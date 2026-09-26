//! Pure word conversion: buffer entries in, erase count + layout hop out.
//!
//! The daemon replays scancodes rather than characters, so a conversion plan
//! is just "erase N, switch to index I, replay these entries". This module
//! computes that plan and tracks the convert/undo chain with no I/O, which
//! keeps it unit-testable without hardware.
//!
//! # Post-conversion buffer policy: freeze, not clear
//!
//! After a conversion the [`InputBuffer`](crate::buffer::InputBuffer) is left
//! unchanged (frozen). Rationale: replay emits the identical scancodes, so
//! the buffer already describes the converted text; clearing it would lose
//! the word needed for undo and would turn a later repeat into a no-op
//! instead of a toggle back. The daemon's own injected keys never re-enter
//! the buffer because the reader skips the virtual device by exact name
//! (`niri-punto`). Any new physical typing invalidates the pending undo via
//! [`Converter::invalidate`], so "repeat the gesture" only undoes an
//! immediately preceding conversion.

use crate::buffer::BufferEntry;
use std::fmt;

/// One conversion step: erase, switch, replay.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConversionPlan {
    /// How many Backspaces to emit.
    pub erase: usize,
    /// Layout index active before this step (for logging and undo).
    pub from_index: u8,
    /// Layout index to switch to by explicit index (never next).
    pub target_index: u8,
    /// Scancodes to replay once the layout-changed event arrives.
    pub replay: Vec<BufferEntry>,
}

impl ConversionPlan {
    /// The same step in the opposite direction: same erase count and replay
    /// list, swapped layout hop. Works because replay re-emits identical
    /// scancodes either way.
    pub fn reversed(&self) -> Self {
        Self {
            erase: self.erase,
            from_index: self.target_index,
            target_index: self.from_index,
            replay: self.replay.clone(),
        }
    }
}

/// Why a conversion was refused. Refusals are reported, never applied.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ConversionError {
    /// The trailing word is empty: nothing to erase or replay.
    Empty,
    /// The current layout is outside a two-layout setup. A configured pair
    /// (ticket 11) will extend this; until then the daemon refuses rather
    /// than guessing.
    Layouts { current: u8, count: usize },
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "nothing to convert: trailing word is empty"),
            Self::Layouts { current, count } => write!(
                f,
                "layout {current} is not one of a 2-layout setup (count {count}): \
                 refusing, needs a configured pair"
            ),
        }
    }
}

impl std::error::Error for ConversionError {}

/// Plan a conversion of `entries` (usually the trailing word) from layout
/// `current` to the other layout of a two-layout setup.
pub fn plan_conversion(
    entries: &[BufferEntry],
    current: u8,
    layout_count: usize,
) -> Result<ConversionPlan, ConversionError> {
    if entries.is_empty() {
        return Err(ConversionError::Empty);
    }
    if layout_count != 2 || current > 1 {
        return Err(ConversionError::Layouts {
            current,
            count: layout_count,
        });
    }
    Ok(ConversionPlan {
        erase: entries.len(),
        from_index: current,
        target_index: 1 - current,
        replay: entries.to_vec(),
    })
}

/// Tracks the convert/undo chain across gestures.
///
/// A fresh gesture plans a new conversion and remembers it; a repeated
/// gesture undoes the remembered step and keeps the reversed step, so a
/// third repeat redoes (toggle chain). Any physical typing between gestures
/// must call [`Converter::invalidate`], cancelling the pending undo.
pub struct Converter {
    last: Option<ConversionPlan>,
}

impl Converter {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Plan a fresh conversion and remember it for a later undo.
    pub fn convert(
        &mut self,
        entries: &[BufferEntry],
        current: u8,
        layout_count: usize,
    ) -> Result<ConversionPlan, ConversionError> {
        let plan = plan_conversion(entries, current, layout_count)?;
        self.last = Some(plan.clone());
        Ok(plan)
    }

    /// Whether a repeated gesture has a conversion to undo.
    pub fn has_pending_undo(&self) -> bool {
        self.last.is_some()
    }

    /// Undo the last step (or redo the undo, toggling back). Returns `None`
    /// when [`Converter::invalidate`] ran since, or no conversion happened.
    pub fn undo(&mut self) -> Option<ConversionPlan> {
        let done = self.last.take()?;
        let back = done.reversed();
        self.last = Some(back.clone());
        Some(back)
    }

    /// New physical input cancels the pending undo.
    pub fn invalidate(&mut self) {
        self.last = None;
    }
}

impl Default for Converter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(scancode: u16) -> BufferEntry {
        BufferEntry {
            scancode,
            shift: false,
        }
    }

    fn word() -> Vec<BufferEntry> {
        // `ghbdtn`-shaped word: six scancodes, layout-agnostic.
        [34, 35, 32, 48, 49, 20].map(entry).to_vec()
    }

    #[test]
    fn word_plans_erase_replay_and_other_index() {
        let plan = plan_conversion(&word(), 0, 2).unwrap();
        assert_eq!(plan.erase, 6);
        assert_eq!((plan.from_index, plan.target_index), (0, 1));
        assert_eq!(plan.replay, word());
    }

    #[test]
    fn conversion_from_second_layout_points_back() {
        let plan = plan_conversion(&word(), 1, 2).unwrap();
        assert_eq!((plan.from_index, plan.target_index), (1, 0));
    }

    #[test]
    fn phrase_slice_plans_too_for_the_ticket_10_seam() {
        let phrase = [30, 31, 57, 32].map(entry).to_vec();
        let plan = plan_conversion(&phrase, 0, 2).unwrap();
        assert_eq!(plan.erase, 4);
        assert_eq!(plan.replay, phrase);
    }

    #[test]
    fn empty_word_is_refused() {
        assert_eq!(plan_conversion(&[], 0, 2), Err(ConversionError::Empty));
    }

    #[test]
    fn short_single_entry_word_converts() {
        let one = [30].map(entry).to_vec();
        let plan = plan_conversion(&one, 0, 2).unwrap();
        assert_eq!(plan.erase, 1);
    }

    #[test]
    fn non_pair_layout_count_is_refused() {
        assert_eq!(
            plan_conversion(&word(), 0, 3),
            Err(ConversionError::Layouts {
                current: 0,
                count: 3
            })
        );
        assert_eq!(
            plan_conversion(&word(), 0, 1),
            Err(ConversionError::Layouts {
                current: 0,
                count: 1
            })
        );
    }

    #[test]
    fn out_of_range_current_index_is_refused() {
        assert_eq!(
            plan_conversion(&word(), 5, 2),
            Err(ConversionError::Layouts {
                current: 5,
                count: 2
            })
        );
    }

    #[test]
    fn undo_reverses_the_hop_keeping_erase_and_replay() {
        let plan = plan_conversion(&word(), 0, 2).unwrap();
        let back = plan.reversed();
        assert_eq!((back.from_index, back.target_index), (1, 0));
        assert_eq!(back.erase, plan.erase);
        assert_eq!(back.replay, plan.replay);
    }

    #[test]
    fn converter_undo_chain_toggles_convert_undo_redo() {
        let mut converter = Converter::new();
        let forward = converter.convert(&word(), 0, 2).unwrap();
        assert!(converter.has_pending_undo());

        let back = converter.undo().unwrap();
        assert_eq!(back, forward.reversed());

        let redo = converter.undo().unwrap();
        assert_eq!(redo, forward);
    }

    #[test]
    fn typing_between_invalidates_the_pending_undo() {
        let mut converter = Converter::new();
        converter.convert(&word(), 0, 2).unwrap();
        converter.invalidate();
        assert!(!converter.has_pending_undo());
        assert_eq!(converter.undo(), None);
    }

    #[test]
    fn failed_conversion_leaves_no_pending_undo() {
        let mut converter = Converter::new();
        assert!(converter.convert(&[], 0, 2).is_err());
        assert!(!converter.has_pending_undo());
    }

    #[test]
    fn fresh_conversion_overwrites_the_remembered_step() {
        let mut converter = Converter::new();
        converter.convert(&word(), 0, 2).unwrap();
        let other = [30, 31].map(entry).to_vec();
        let plan = converter.convert(&other, 1, 2).unwrap();
        assert_eq!(plan.replay, other);
        let back = converter.undo().unwrap();
        assert_eq!((back.from_index, back.target_index), (0, 1));
        assert_eq!(back.replay, other);
    }
}

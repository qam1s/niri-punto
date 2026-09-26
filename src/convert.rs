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
use crate::config::LayoutPair;
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
    /// The current layout is outside the configured pair: either beyond
    /// the pair positions (a third language) or beyond what niri reports.
    /// Carries the pair so the report names it instead of guessing.
    Layouts {
        current: u8,
        count: usize,
        pair: LayoutPair,
    },
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "nothing to convert: trailing word is empty"),
            Self::Layouts {
                current,
                count,
                pair,
            } => write!(
                f,
                "layout {current} is outside the configured pair \
                 (\"{}\", \"{}\", niri reports {count} layouts): refusing",
                pair.first, pair.second,
            ),
        }
    }
}

impl std::error::Error for ConversionError {}

/// Plan a conversion of `entries` (usually the trailing word) from layout
/// `current` to the other layout of the configured `pair`.
///
/// Position in the pair maps to the niri layout index, so the hop is
/// `1 - current`. With more than two niri layouts conversion still works
/// inside the pair; anything outside it (a third language) is refused
/// loudly rather than corrupting text silently.
pub fn plan_conversion(
    entries: &[BufferEntry],
    current: u8,
    layout_count: usize,
    pair: &LayoutPair,
) -> Result<ConversionPlan, ConversionError> {
    if entries.is_empty() {
        return Err(ConversionError::Empty);
    }
    if current > 1 || layout_count < 2 {
        return Err(ConversionError::Layouts {
            current,
            count: layout_count,
            pair: pair.clone(),
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
    pair: LayoutPair,
}

impl Converter {
    pub fn new(pair: LayoutPair) -> Self {
        Self { last: None, pair }
    }

    /// Plan a fresh conversion and remember it for a later undo.
    pub fn convert(
        &mut self,
        entries: &[BufferEntry],
        current: u8,
        layout_count: usize,
    ) -> Result<ConversionPlan, ConversionError> {
        let plan = plan_conversion(entries, current, layout_count, &self.pair)?;
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
        Self::new(LayoutPair::new("us", "ru").expect("us/ru is a valid pair"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> LayoutPair {
        LayoutPair::new("us", "ru").unwrap()
    }

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
        let plan = plan_conversion(&word(), 0, 2, &pair()).unwrap();
        assert_eq!(plan.erase, 6);
        assert_eq!((plan.from_index, plan.target_index), (0, 1));
        assert_eq!(plan.replay, word());
    }

    #[test]
    fn conversion_from_second_layout_points_back() {
        let plan = plan_conversion(&word(), 1, 2, &pair()).unwrap();
        assert_eq!((plan.from_index, plan.target_index), (1, 0));
    }

    #[test]
    fn phrase_slice_plans_too_for_the_ticket_10_seam() {
        let phrase = [30, 31, 57, 32].map(entry).to_vec();
        let plan = plan_conversion(&phrase, 0, 2, &pair()).unwrap();
        assert_eq!(plan.erase, 4);
        assert_eq!(plan.replay, phrase);
    }

    #[test]
    fn empty_word_is_refused() {
        assert_eq!(
            plan_conversion(&[], 0, 2, &pair()),
            Err(ConversionError::Empty)
        );
    }

    #[test]
    fn short_single_entry_word_converts() {
        let one = [30].map(entry).to_vec();
        let plan = plan_conversion(&one, 0, 2, &pair()).unwrap();
        assert_eq!(plan.erase, 1);
    }

    #[test]
    fn conversion_inside_the_pair_survives_extra_layouts() {
        let plan = plan_conversion(&word(), 0, 3, &pair()).unwrap();
        assert_eq!((plan.from_index, plan.target_index), (0, 1));
        let plan = plan_conversion(&word(), 1, 3, &pair()).unwrap();
        assert_eq!((plan.from_index, plan.target_index), (1, 0));
    }

    #[test]
    fn third_language_is_refused_and_names_the_pair() {
        let error = plan_conversion(&word(), 2, 3, &pair()).unwrap_err();
        assert_eq!(
            error,
            ConversionError::Layouts {
                current: 2,
                count: 3,
                pair: pair(),
            }
        );
        let report = error.to_string();
        assert!(report.contains('2'), "{report}");
        assert!(report.contains("\"us\""), "{report}");
        assert!(report.contains("\"ru\""), "{report}");
    }

    #[test]
    fn single_layout_setup_is_refused() {
        assert_eq!(
            plan_conversion(&word(), 0, 1, &pair()),
            Err(ConversionError::Layouts {
                current: 0,
                count: 1,
                pair: pair(),
            })
        );
    }

    #[test]
    fn out_of_range_current_index_is_refused() {
        assert_eq!(
            plan_conversion(&word(), 5, 2, &pair()),
            Err(ConversionError::Layouts {
                current: 5,
                count: 2,
                pair: pair(),
            })
        );
    }

    #[test]
    fn undo_reverses_the_hop_keeping_erase_and_replay() {
        let plan = plan_conversion(&word(), 0, 2, &pair()).unwrap();
        let back = plan.reversed();
        assert_eq!((back.from_index, back.target_index), (1, 0));
        assert_eq!(back.erase, plan.erase);
        assert_eq!(back.replay, plan.replay);
    }

    #[test]
    fn converter_undo_chain_toggles_convert_undo_redo() {
        let mut converter = Converter::new(pair());
        let forward = converter.convert(&word(), 0, 2).unwrap();
        assert!(converter.has_pending_undo());

        let back = converter.undo().unwrap();
        assert_eq!(back, forward.reversed());

        let redo = converter.undo().unwrap();
        assert_eq!(redo, forward);
    }

    #[test]
    fn typing_between_invalidates_the_pending_undo() {
        let mut converter = Converter::new(pair());
        converter.convert(&word(), 0, 2).unwrap();
        converter.invalidate();
        assert!(!converter.has_pending_undo());
        assert_eq!(converter.undo(), None);
    }

    #[test]
    fn failed_conversion_leaves_no_pending_undo() {
        let mut converter = Converter::new(pair());
        assert!(converter.convert(&[], 0, 2).is_err());
        assert!(!converter.has_pending_undo());
    }

    #[test]
    fn fresh_conversion_overwrites_the_remembered_step() {
        let mut converter = Converter::new(pair());
        converter.convert(&word(), 0, 2).unwrap();
        let other = [30, 31].map(entry).to_vec();
        let plan = converter.convert(&other, 1, 2).unwrap();
        assert_eq!(plan.replay, other);
        let back = converter.undo().unwrap();
        assert_eq!((back.from_index, back.target_index), (0, 1));
        assert_eq!(back.replay, other);
    }
}

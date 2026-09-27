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
use crate::scorer::Intended;
use crate::undo::{LayoutCtx, LayoutHop, Reversible, UndoChain};
use std::fmt;

/// One conversion step: erase, switch, replay.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConversionPlan {
    /// How many Backspaces to emit.
    pub erase: usize,
    /// Layout hop by explicit index (never next).
    pub hop: LayoutHop,
    /// Scancodes to replay once the layout-changed event arrives.
    pub replay: Vec<BufferEntry>,
}

impl Reversible for ConversionPlan {
    /// The same step in the opposite direction: same erase count and replay
    /// list, swapped layout hop. Works because replay re-emits identical
    /// scancodes either way.
    fn reversed(&self) -> Self {
        Self {
            erase: self.erase,
            hop: self.hop.swapped(),
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

/// Plan a conversion of `entries` (usually the trailing word) inside `ctx.
///
/// Position in the pair maps to the niri layout index, so the hop is
/// `1 - current`. With more than two niri layouts conversion still works
/// inside the pair; anything outside it (a third language) is refused
/// loudly rather than corrupting text silently.
pub fn plan_conversion(
    entries: &[BufferEntry],
    ctx: LayoutCtx<'_>,
) -> Result<ConversionPlan, ConversionError> {
    if entries.is_empty() {
        return Err(ConversionError::Empty);
    }
    Ok(ConversionPlan {
        erase: entries.len(),
        hop: ctx.hop()?,
        replay: entries.to_vec(),
    })
}

/// Plan a conversion toward an explicit pair position: the target is a
/// confident detector verdict's intended layout, ignoring which layout is
/// current. The empty-word and pair-guard refusals match
/// [`plan_conversion`] exactly, so a decline falls back byte-identical.
pub fn plan_conversion_toward(
    entries: &[BufferEntry],
    target: u8,
    ctx: LayoutCtx<'_>,
) -> Result<ConversionPlan, ConversionError> {
    if entries.is_empty() {
        return Err(ConversionError::Empty);
    }
    Ok(ConversionPlan {
        erase: entries.len(),
        hop: ctx.hop_toward(target)?,
        replay: entries.to_vec(),
    })
}

/// Tracks the convert/undo chain across gestures: a thin wrapper over the
/// shared [`UndoChain`] remembering the configured pair for planning.
pub struct Converter {
    chain: UndoChain<ConversionPlan>,
    pair: LayoutPair,
}

impl Converter {
    pub fn new(pair: LayoutPair) -> Self {
        Self {
            chain: UndoChain::new(),
            pair,
        }
    }

    /// Plan a fresh conversion and remember it for a later undo.
    pub fn convert(
        &mut self,
        entries: &[BufferEntry],
        current: u8,
        layout_count: usize,
    ) -> Result<ConversionPlan, ConversionError> {
        let plan = plan_conversion(
            entries,
            LayoutCtx {
                current,
                count: layout_count,
                pair: &self.pair,
            },
        )?;
        self.chain.remember(plan.clone());
        Ok(plan)
    }

    /// Plan a fresh conversion toward a confident detector verdict's
    /// intended layout (resolved to a pair position through the pair order)
    /// and remember it.
    pub fn convert_toward(
        &mut self,
        entries: &[BufferEntry],
        intended: Intended,
        current: u8,
        layout_count: usize,
    ) -> Result<ConversionPlan, ConversionError> {
        let plan = plan_conversion_toward(
            entries,
            intended.index_in(&self.pair),
            LayoutCtx {
                current,
                count: layout_count,
                pair: &self.pair,
            },
        )?;
        self.chain.remember(plan.clone());
        Ok(plan)
    }

    /// Whether a repeated gesture has a conversion to undo.
    pub fn has_pending_undo(&self) -> bool {
        self.chain.has_pending_undo()
    }

    /// Undo the last step (or redo the undo, toggling back). Returns `None`
    /// when [`Converter::invalidate`] ran since, or no conversion happened.
    pub fn undo(&mut self) -> Option<ConversionPlan> {
        self.chain.undo()
    }

    /// New physical input cancels the pending undo.
    pub fn invalidate(&mut self) {
        self.chain.invalidate();
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
    use crate::undo::LayoutCtx;

    fn pair() -> LayoutPair {
        LayoutPair::new("us", "ru").unwrap()
    }

    fn ctx(pair: &LayoutPair, current: u8, count: usize) -> LayoutCtx<'_> {
        LayoutCtx {
            current,
            count,
            pair,
        }
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
        let plan = plan_conversion(&word(), ctx(&pair(), 0, 2)).unwrap();
        assert_eq!(plan.erase, 6);
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
        assert_eq!(plan.replay, word());
    }

    #[test]
    fn conversion_from_second_layout_points_back() {
        let plan = plan_conversion(&word(), ctx(&pair(), 1, 2)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
    }

    #[test]
    fn phrase_slice_plans_too_for_the_ticket_10_seam() {
        let phrase = [30, 31, 57, 32].map(entry).to_vec();
        let plan = plan_conversion(&phrase, ctx(&pair(), 0, 2)).unwrap();
        assert_eq!(plan.erase, 4);
        assert_eq!(plan.replay, phrase);
    }

    #[test]
    fn toward_ignores_current_but_refuses_like_today() {
        // Diverged current layout (18:55): the target still lands on the
        // intended position, from the diverged current for a correct undo.
        let plan = plan_conversion_toward(&word(), 1, ctx(&pair(), 1, 2)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 1));
        assert_eq!(plan.erase, 6);
        // Refusals match plan_conversion exactly.
        assert_eq!(
            plan_conversion_toward(&[], 1, ctx(&pair(), 0, 2)),
            Err(ConversionError::Empty)
        );
        assert_eq!(
            plan_conversion_toward(&word(), 1, ctx(&pair(), 2, 3)),
            plan_conversion(&word(), ctx(&pair(), 2, 3))
        );
    }

    #[test]
    fn toward_resolves_the_intended_layout_through_a_swapped_pair() {
        use crate::scorer::Intended;
        let swapped = LayoutPair::new("ru", "us").unwrap();
        // Cyrillic verdict lands on position 0 under `layout ru,us`, from
        // the diverged current for a correct undo.
        let mut converter = Converter::new(swapped);
        let plan = converter
            .convert_toward(&word(), Intended::Ru, 1, 2)
            .unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
        assert_eq!(plan.erase, 6);
        let plan = converter
            .convert_toward(&word(), Intended::Us, 0, 2)
            .unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
    }

    #[test]
    fn empty_word_is_refused() {
        assert_eq!(
            plan_conversion(&[], ctx(&pair(), 0, 2)),
            Err(ConversionError::Empty)
        );
    }

    #[test]
    fn short_single_entry_word_converts() {
        let one = [30].map(entry).to_vec();
        let plan = plan_conversion(&one, ctx(&pair(), 0, 2)).unwrap();
        assert_eq!(plan.erase, 1);
    }

    #[test]
    fn conversion_inside_the_pair_survives_extra_layouts() {
        let plan = plan_conversion(&word(), ctx(&pair(), 0, 3)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
        let plan = plan_conversion(&word(), ctx(&pair(), 1, 3)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
    }

    #[test]
    fn third_language_is_refused_and_names_the_pair() {
        let error = plan_conversion(&word(), ctx(&pair(), 2, 3)).unwrap_err();
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
            plan_conversion(&word(), ctx(&pair(), 0, 1)),
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
            plan_conversion(&word(), ctx(&pair(), 5, 2)),
            Err(ConversionError::Layouts {
                current: 5,
                count: 2,
                pair: pair(),
            })
        );
    }

    #[test]
    fn undo_reverses_the_hop_keeping_erase_and_replay() {
        let plan = plan_conversion(&word(), ctx(&pair(), 0, 2)).unwrap();
        let back = plan.reversed();
        assert_eq!((back.hop.from, back.hop.target), (1, 0));
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
        assert_eq!((back.hop.from, back.hop.target), (0, 1));
        assert_eq!(back.replay, other);
    }
}

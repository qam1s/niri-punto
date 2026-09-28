//! Pure word conversion: buffer entries in, erase count + layout hop out.

use crate::buffer::BufferEntry;
use crate::config::LayoutPair;
use crate::scorer::Intended;
use crate::undo::{HasHop, LayoutCtx, LayoutHop, Reversible, UndoChain};
use std::fmt;

/// One conversion step: erase, switch, replay.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConversionPlan {
    pub erase: usize,
    pub hop: LayoutHop,
    pub replay: Vec<BufferEntry>,
}

impl Reversible for ConversionPlan {
    fn reversed(&self) -> Self {
        Self {
            erase: self.erase,
            hop: self.hop.swapped(),
            replay: self.replay.clone(),
        }
    }
}

impl HasHop for ConversionPlan {
    fn hop_target(&self) -> u8 {
        self.hop.target
    }
}

/// Why a conversion was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ConversionError {
    Empty,
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

/// Plan a conversion of `entries` inside `ctx.
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

/// Plan a conversion toward an explicit pair position.
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

/// Tracks the convert/undo chain across gestures.
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

    pub fn convert_toward(
        &mut self,
        entries: &[BufferEntry],
        intended: Intended,
        current: u8,
        layout_count: usize,
        birth: Option<u8>,
    ) -> Result<ConversionPlan, ConversionError> {
        let target = intended.index_in(&self.pair);
        if birth.is_some_and(|b| b == current) && target == current {
            return self.convert(entries, current, layout_count);
        }
        let plan = plan_conversion_toward(
            entries,
            target,
            LayoutCtx {
                current,
                count: layout_count,
                pair: &self.pair,
            },
        )?;
        self.chain.remember(plan.clone());
        Ok(plan)
    }

    pub fn has_pending_undo(&self) -> bool {
        self.chain.has_pending_undo()
    }

    pub fn last_target(&self) -> Option<u8> {
        self.chain.last_target()
    }

    pub fn undo(&mut self) -> Option<ConversionPlan> {
        self.chain.undo()
    }

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
    fn phrase_slice_plans_too() {
        let phrase = [30, 31, 57, 32].map(entry).to_vec();
        let plan = plan_conversion(&phrase, ctx(&pair(), 0, 2)).unwrap();
        assert_eq!(plan.erase, 4);
        assert_eq!(plan.replay, phrase);
    }

    #[test]
    fn toward_ignores_current_but_refuses_like_today() {
        let plan = plan_conversion_toward(&word(), 1, ctx(&pair(), 1, 2)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 1));
        assert_eq!(plan.erase, 6);
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
    fn toward_agreeing_verdict_flips_when_layout_unchanged() {
        use crate::scorer::Intended;
        let mut converter = Converter::new(pair());
        let plan = converter
            .convert_toward(&word(), Intended::Ru, 1, 2, Some(1))
            .unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
    }

    #[test]
    fn toward_agreeing_verdict_holds_when_layout_changed() {
        use crate::scorer::Intended;
        let mut converter = Converter::new(pair());
        let plan = converter
            .convert_toward(&word(), Intended::Ru, 1, 2, Some(0))
            .unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 1));
    }

    #[test]
    fn toward_disagreeing_verdict_holds_when_unchanged() {
        use crate::scorer::Intended;
        let mut converter = Converter::new(pair());
        let plan = converter
            .convert_toward(&word(), Intended::Ru, 0, 2, Some(0))
            .unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
    }

    #[test]
    fn toward_resolves_the_intended_layout_through_a_swapped_pair() {
        use crate::scorer::Intended;
        let swapped = LayoutPair::new("ru", "us").unwrap();
        let mut converter = Converter::new(swapped);
        let plan = converter
            .convert_toward(&word(), Intended::Ru, 1, 2, None)
            .unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
        assert_eq!(plan.erase, 6);
        let plan = converter
            .convert_toward(&word(), Intended::Us, 0, 2, None)
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

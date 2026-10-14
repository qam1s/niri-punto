use crate::buffer::BufferEntry;
use crate::undo::{LayoutCtx, LayoutHop, Reversible, UndoChain};
use std::fmt;

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

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ConversionError {
    Empty,
    Layouts { current: u8, count: usize },
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "nothing to convert: trailing word is empty"),
            Self::Layouts { current, count } => write!(
                f,
                "layout {current} is outside niri's first two layouts \
                 ({count} total): refusing",
            ),
        }
    }
}

impl std::error::Error for ConversionError {}

pub fn plan_conversion(
    entries: &[BufferEntry],
    ctx: LayoutCtx,
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

pub struct Converter {
    chain: UndoChain<ConversionPlan>,
}

impl Converter {
    pub fn new() -> Self {
        Self {
            chain: UndoChain::new(),
        }
    }

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
            },
        )?;
        self.chain.remember(plan.clone());
        Ok(plan)
    }

    pub fn has_pending_undo(&self) -> bool {
        self.chain.has_pending_undo()
    }

    pub fn undo(&mut self) -> Option<ConversionPlan> {
        self.chain.undo()
    }

    pub fn invalidate(&mut self) {
        self.chain.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::undo::LayoutCtx;

    fn ctx(current: u8, count: usize) -> LayoutCtx {
        LayoutCtx { current, count }
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
        let plan = plan_conversion(&word(), ctx(0, 2)).unwrap();
        assert_eq!(plan.erase, 6);
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
        assert_eq!(plan.replay, word());
    }

    #[test]
    fn conversion_from_second_layout_points_back() {
        let plan = plan_conversion(&word(), ctx(1, 2)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
    }

    #[test]
    fn phrase_slice_plans_too() {
        let phrase = [30, 31, 57, 32].map(entry).to_vec();
        let plan = plan_conversion(&phrase, ctx(0, 2)).unwrap();
        assert_eq!(plan.erase, 4);
        assert_eq!(plan.replay, phrase);
    }

    #[test]
    fn empty_word_is_refused() {
        assert_eq!(plan_conversion(&[], ctx(0, 2)), Err(ConversionError::Empty));
    }

    #[test]
    fn short_single_entry_word_converts() {
        let one = [30].map(entry).to_vec();
        let plan = plan_conversion(&one, ctx(0, 2)).unwrap();
        assert_eq!(plan.erase, 1);
    }

    #[test]
    fn conversion_inside_the_pair_survives_extra_layouts() {
        let plan = plan_conversion(&word(), ctx(0, 3)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
        let plan = plan_conversion(&word(), ctx(1, 3)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
    }

    #[test]
    fn third_language_is_refused() {
        let error = plan_conversion(&word(), ctx(2, 3)).unwrap_err();
        assert_eq!(
            error,
            ConversionError::Layouts {
                current: 2,
                count: 3,
            }
        );
        let report = error.to_string();
        assert!(report.contains('2'), "{report}");
        assert!(report.contains('3'), "{report}");
    }

    #[test]
    fn single_layout_setup_is_refused() {
        assert_eq!(
            plan_conversion(&word(), ctx(0, 1)),
            Err(ConversionError::Layouts {
                current: 0,
                count: 1,
            })
        );
    }

    #[test]
    fn out_of_range_current_index_is_refused() {
        assert_eq!(
            plan_conversion(&word(), ctx(5, 2)),
            Err(ConversionError::Layouts {
                current: 5,
                count: 2,
            })
        );
    }

    #[test]
    fn undo_reverses_the_hop_keeping_erase_and_replay() {
        let plan = plan_conversion(&word(), ctx(0, 2)).unwrap();
        let back = plan.reversed();
        assert_eq!((back.hop.from, back.hop.target), (1, 0));
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
        assert_eq!((back.hop.from, back.hop.target), (0, 1));
        assert_eq!(back.replay, other);
    }
}

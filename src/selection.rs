//! Pure selection conversion: pasted text in, converted text + hop out.

use crate::config::LayoutPair;
use crate::convert::ConversionError;
use crate::keymaps;
use crate::scorer::Intended;
use crate::undo::{HasHop, LayoutCtx, LayoutHop, Reversible, UndoChain};

/// One selection step.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SelectionPlan {
    pub hop: LayoutHop,
    pub original: String,
    pub converted: String,
}

impl Reversible for SelectionPlan {
    fn reversed(&self) -> Self {
        Self {
            hop: self.hop.swapped(),
            original: self.converted.clone(),
            converted: self.original.clone(),
        }
    }
}

impl HasHop for SelectionPlan {
    fn hop_target(&self) -> u8 {
        self.hop.target
    }
}

pub fn plan_selection(text: &str, ctx: LayoutCtx<'_>) -> Result<SelectionPlan, ConversionError> {
    if text.is_empty() {
        return Err(ConversionError::Empty);
    }
    Ok(SelectionPlan {
        hop: ctx.hop()?,
        original: text.to_string(),
        converted: keymaps::convert(text, ctx.current != ctx.pair.latin_index()),
    })
}

pub fn plan_selection_toward(
    text: &str,
    intended: Intended,
    ctx: LayoutCtx<'_>,
) -> Result<SelectionPlan, ConversionError> {
    if text.is_empty() {
        return Err(ConversionError::Empty);
    }
    Ok(SelectionPlan {
        hop: ctx.hop_toward(intended.index_in(ctx.pair))?,
        original: text.to_string(),
        converted: keymaps::convert(text, intended == Intended::Us),
    })
}

/// Tracks the convert/undo chain across selection gestures.
pub struct SelectionConverter {
    chain: UndoChain<SelectionPlan>,
    pair: LayoutPair,
}

impl SelectionConverter {
    pub fn new(pair: LayoutPair) -> Self {
        Self {
            chain: UndoChain::new(),
            pair,
        }
    }

    /// Plan a fresh conversion and remember it for a later undo.
    pub fn convert(
        &mut self,
        text: &str,
        current: u8,
        layout_count: usize,
    ) -> Result<SelectionPlan, ConversionError> {
        let plan = plan_selection(
            text,
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
        text: &str,
        intended: Intended,
        current: u8,
        layout_count: usize,
    ) -> Result<SelectionPlan, ConversionError> {
        let plan = plan_selection_toward(
            text,
            intended,
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

    pub fn undo(&mut self) -> Option<SelectionPlan> {
        self.chain.undo()
    }

    pub fn invalidate(&mut self) {
        self.chain.invalidate()
    }
}

impl Default for SelectionConverter {
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

    #[test]
    fn selection_plans_converted_text_and_other_index() {
        let plan = plan_selection("ghbdtn", ctx(&pair(), 0, 2)).unwrap();
        assert_eq!(plan.converted, "привет");
        assert_eq!(plan.original, "ghbdtn");
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
    }

    #[test]
    fn selection_from_second_layout_maps_back() {
        let plan = plan_selection("привет", ctx(&pair(), 1, 2)).unwrap();
        assert_eq!(plan.converted, "ghbdtn");
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
    }

    #[test]
    fn non_latin_selection_survives() {
        let plan = plan_selection("ghbdtn 👋", ctx(&pair(), 0, 2)).unwrap();
        assert_eq!(plan.converted, "привет 👋");
    }

    #[test]
    fn toward_maps_to_the_intended_alphabet_from_any_current() {
        let plan = plan_selection_toward("ghbdtn", Intended::Ru, ctx(&pair(), 1, 2)).unwrap();
        assert_eq!(plan.converted, "привет");
        assert_eq!((plan.hop.from, plan.hop.target), (1, 1));
        let plan = plan_selection_toward("hello", Intended::Us, ctx(&pair(), 1, 2)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
        assert_eq!(
            plan_selection_toward("", Intended::Ru, ctx(&pair(), 0, 2)),
            Err(ConversionError::Empty)
        );
        assert_eq!(
            plan_selection_toward("ghbdtn", Intended::Ru, ctx(&pair(), 2, 3)),
            plan_selection("ghbdtn", ctx(&pair(), 2, 3))
        );
    }

    #[test]
    fn swapped_pair_resolves_hop_target_and_alphabet() {
        let swapped = LayoutPair::new("ru", "us").unwrap();
        let plan = plan_selection_toward("ghbdtn", Intended::Ru, ctx(&swapped, 1, 2)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
        assert_eq!(plan.converted, "привет");
        let plan = plan_selection_toward("привет", Intended::Us, ctx(&swapped, 0, 2)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
        assert_eq!(plan.converted, "ghbdtn");
        let plan = plan_selection("привет", ctx(&swapped, 0, 2)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
        assert_eq!(plan.converted, "ghbdtn");
        let plan = plan_selection("ghbdtn", ctx(&swapped, 1, 2)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (1, 0));
        assert_eq!(plan.converted, "привет");
    }

    #[test]
    fn empty_selection_is_refused() {
        assert_eq!(
            plan_selection("", ctx(&pair(), 0, 2)),
            Err(ConversionError::Empty)
        );
    }

    #[test]
    fn extra_layouts_inside_pair_convert() {
        let plan = plan_selection("ghbdtn", ctx(&pair(), 0, 3)).unwrap();
        assert_eq!((plan.hop.from, plan.hop.target), (0, 1));
    }

    #[test]
    fn single_layout_is_refused() {
        assert_eq!(
            plan_selection("ghbdtn", ctx(&pair(), 0, 1)),
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
            plan_selection("ghbdtn", ctx(&pair(), 5, 2)),
            Err(ConversionError::Layouts {
                current: 5,
                count: 2,
                pair: pair(),
            })
        );
    }

    #[test]
    fn undo_swaps_texts_and_hop() {
        let plan = plan_selection("ghbdtn", ctx(&pair(), 0, 2)).unwrap();
        let back = plan.reversed();
        assert_eq!((back.hop.from, back.hop.target), (1, 0));
        assert_eq!(back.converted, "ghbdtn");
        assert_eq!(back.original, "привет");
    }

    #[test]
    fn converter_undo_chain_toggles_convert_undo_redo() {
        let mut converter = SelectionConverter::new(pair());
        let forward = converter.convert("ghbdtn", 0, 2).unwrap();
        assert!(converter.has_pending_undo());

        let back = converter.undo().unwrap();
        assert_eq!(back, forward.reversed());

        let redo = converter.undo().unwrap();
        assert_eq!(redo, forward);
    }

    #[test]
    fn typing_between_invalidates_the_pending_undo() {
        let mut converter = SelectionConverter::new(pair());
        converter.convert("ghbdtn", 0, 2).unwrap();
        converter.invalidate();
        assert!(!converter.has_pending_undo());
        assert_eq!(converter.undo(), None);
    }

    #[test]
    fn failed_conversion_leaves_no_pending_undo() {
        let mut converter = SelectionConverter::new(pair());
        assert!(converter.convert("", 0, 2).is_err());
        assert!(!converter.has_pending_undo());
    }

    #[test]
    fn fresh_conversion_overwrites_the_remembered_step() {
        let mut converter = SelectionConverter::new(pair());
        converter.convert("ghbdtn", 0, 2).unwrap();
        let plan = converter.convert("lvdk", 0, 2).unwrap();
        assert_eq!(plan.converted, "дмвл");
        let back = converter.undo().unwrap();
        assert_eq!(back.converted, "lvdk");
    }
}

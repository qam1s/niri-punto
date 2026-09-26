//! Pure selection conversion: pasted text in, converted text + hop out.
//!
//! The word path ([`crate::convert`]) replays scancodes; the selection path
//! cannot — the selection may be text the daemon never typed. It maps the
//! clipboard text char-by-char ([`crate::keymaps`]) instead, so any Unicode
//! outside the tables passes through untouched.
//!
//! The undo-chain shape mirrors [`crate::convert::Converter`]: a fresh
//! gesture plans a conversion and remembers it; a repeated gesture undoes the
//! remembered step and keeps the reversed step, so a third repeat redoes.
//! The layout error reuses [`crate::convert::ConversionError`] with the same
//! pair semantics: outside the configured pair is refused loudly.

use crate::config::LayoutPair;
use crate::convert::ConversionError;
use crate::keymaps;
use crate::undo::{LayoutCtx, LayoutHop, Reversible, UndoChain};

/// One selection step: clipboard text to write and layout hop to take.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SelectionPlan {
    /// Layout hop by explicit index (never next).
    pub hop: LayoutHop,
    /// Text read from the selection (pasted back on undo).
    pub original: String,
    /// Text to publish and paste over the selection on this step.
    pub converted: String,
}

impl Reversible for SelectionPlan {
    /// The same step in the opposite direction: texts and hop swapped.
    fn reversed(&self) -> Self {
        Self {
            hop: self.hop.swapped(),
            original: self.converted.clone(),
            converted: self.original.clone(),
        }
    }
}

/// Plan a conversion of the selected `text` inside `ctx`. The current layout
/// names the source alphabet: text in the first layout maps to the second,
/// text in the second maps back. Pair order is the contract: the first entry
/// is the Latin side, the second the Cyrillic side (see the `layouts`
/// config).
pub fn plan_selection(text: &str, ctx: LayoutCtx<'_>) -> Result<SelectionPlan, ConversionError> {
    if text.is_empty() {
        return Err(ConversionError::Empty);
    }
    Ok(SelectionPlan {
        hop: ctx.hop()?,
        original: text.to_string(),
        converted: keymaps::convert(text, ctx.current == 1),
    })
}

/// Tracks the convert/undo chain across selection gestures: a thin wrapper
/// over the shared [`UndoChain`], mirroring [`crate::convert::Converter`].
/// Any physical typing between gestures must call
/// [`SelectionConverter::invalidate`].
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

    /// Whether a repeated gesture has a conversion to undo.
    pub fn has_pending_undo(&self) -> bool {
        self.chain.has_pending_undo()
    }

    /// Undo the last step (or redo the undo, toggling back). Returns `None`
    /// when [`SelectionConverter::invalidate`] ran since.
    pub fn undo(&mut self) -> Option<SelectionPlan> {
        self.chain.undo()
    }

    /// New physical input cancels the pending undo.
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

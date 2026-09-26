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
//! The layout error reuses [`crate::convert::ConversionError`], including the
//! hardcoded two-layout assumption (ticket 11 configures the pair).

use crate::convert::ConversionError;
use crate::keymaps;

/// One selection step: clipboard text to write and layout hop to take.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SelectionPlan {
    /// Layout index active before this step (for logging and undo).
    pub from_index: u8,
    /// Layout index to switch to by explicit index (never next).
    pub target_index: u8,
    /// Text read from the selection (pasted back on undo).
    pub original: String,
    /// Text to publish and paste over the selection on this step.
    pub converted: String,
}

impl SelectionPlan {
    /// The same step in the opposite direction: texts and hop swapped.
    pub fn reversed(&self) -> Self {
        Self {
            from_index: self.target_index,
            target_index: self.from_index,
            original: self.converted.clone(),
            converted: self.original.clone(),
        }
    }
}

/// Plan a conversion of the selected `text` from layout `current` to the
/// other layout of the two-layout setup. The current layout names the source
/// alphabet: US text maps EN->RU, RU text maps RU->EN.
pub fn plan_selection(
    text: &str,
    current: u8,
    layout_count: usize,
) -> Result<SelectionPlan, ConversionError> {
    if text.is_empty() {
        return Err(ConversionError::Empty);
    }
    if layout_count != 2 || current > 1 {
        return Err(ConversionError::Layouts {
            current,
            count: layout_count,
        });
    }
    Ok(SelectionPlan {
        from_index: current,
        target_index: 1 - current,
        original: text.to_string(),
        converted: keymaps::convert(text, current == 1),
    })
}

/// Tracks the convert/undo chain across selection gestures. Same contract as
/// [`crate::convert::Converter`]: any physical typing between gestures must
/// call [`SelectionConverter::invalidate`].
pub struct SelectionConverter {
    last: Option<SelectionPlan>,
}

impl SelectionConverter {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Plan a fresh conversion and remember it for a later undo.
    pub fn convert(
        &mut self,
        text: &str,
        current: u8,
        layout_count: usize,
    ) -> Result<SelectionPlan, ConversionError> {
        let plan = plan_selection(text, current, layout_count)?;
        self.last = Some(plan.clone());
        Ok(plan)
    }

    /// Whether a repeated gesture has a conversion to undo.
    pub fn has_pending_undo(&self) -> bool {
        self.last.is_some()
    }

    /// Undo the last step (or redo the undo, toggling back). Returns `None`
    /// when [`SelectionConverter::invalidate`] ran since.
    pub fn undo(&mut self) -> Option<SelectionPlan> {
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

impl Default for SelectionConverter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_plans_converted_text_and_other_index() {
        let plan = plan_selection("ghbdtn", 0, 2).unwrap();
        assert_eq!(plan.converted, "привет");
        assert_eq!(plan.original, "ghbdtn");
        assert_eq!((plan.from_index, plan.target_index), (0, 1));
    }

    #[test]
    fn selection_from_second_layout_maps_back() {
        let plan = plan_selection("привет", 1, 2).unwrap();
        assert_eq!(plan.converted, "ghbdtn");
        assert_eq!((plan.from_index, plan.target_index), (1, 0));
    }

    #[test]
    fn non_latin_selection_survives() {
        let plan = plan_selection("ghbdtn 👋", 0, 2).unwrap();
        assert_eq!(plan.converted, "привет 👋");
    }

    #[test]
    fn empty_selection_is_refused() {
        assert_eq!(plan_selection("", 0, 2), Err(ConversionError::Empty));
    }

    #[test]
    fn non_pair_layout_count_is_refused() {
        assert_eq!(
            plan_selection("ghbdtn", 0, 3),
            Err(ConversionError::Layouts {
                current: 0,
                count: 3
            })
        );
    }

    #[test]
    fn out_of_range_current_index_is_refused() {
        assert_eq!(
            plan_selection("ghbdtn", 5, 2),
            Err(ConversionError::Layouts {
                current: 5,
                count: 2
            })
        );
    }

    #[test]
    fn undo_swaps_texts_and_hop() {
        let plan = plan_selection("ghbdtn", 0, 2).unwrap();
        let back = plan.reversed();
        assert_eq!((back.from_index, back.target_index), (1, 0));
        assert_eq!(back.converted, "ghbdtn");
        assert_eq!(back.original, "привет");
    }

    #[test]
    fn converter_undo_chain_toggles_convert_undo_redo() {
        let mut converter = SelectionConverter::new();
        let forward = converter.convert("ghbdtn", 0, 2).unwrap();
        assert!(converter.has_pending_undo());

        let back = converter.undo().unwrap();
        assert_eq!(back, forward.reversed());

        let redo = converter.undo().unwrap();
        assert_eq!(redo, forward);
    }

    #[test]
    fn typing_between_invalidates_the_pending_undo() {
        let mut converter = SelectionConverter::new();
        converter.convert("ghbdtn", 0, 2).unwrap();
        converter.invalidate();
        assert!(!converter.has_pending_undo());
        assert_eq!(converter.undo(), None);
    }

    #[test]
    fn failed_conversion_leaves_no_pending_undo() {
        let mut converter = SelectionConverter::new();
        assert!(converter.convert("", 0, 2).is_err());
        assert!(!converter.has_pending_undo());
    }

    #[test]
    fn fresh_conversion_overwrites_the_remembered_step() {
        let mut converter = SelectionConverter::new();
        converter.convert("ghbdtn", 0, 2).unwrap();
        let plan = converter.convert("lvdk", 0, 2).unwrap();
        assert_eq!(plan.converted, "дмвл");
        let back = converter.undo().unwrap();
        assert_eq!(back.converted, "lvdk");
    }
}

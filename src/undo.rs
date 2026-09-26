//! Shared undo chain and layout pair guard for both conversion paths.
//!
//! The word path ([`crate::convert`]) and the selection path
//! ([`crate::selection`]) share one shape: plan a fresh conversion and
//! remember it; a repeated gesture undoes the remembered step and keeps the
//! reversed step, so a third repeat redoes. Both also gate on the same
//! condition: the current layout must sit inside the configured pair.
//! This module holds that shared shape (one generic chain, one pair guard)
//! so the two converters stay thin wrappers.

use crate::config::LayoutPair;
use crate::convert::ConversionError;

/// Layout hop by explicit index (never next): the index active before a
/// step and the index to switch to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LayoutHop {
    pub from: u8,
    pub target: u8,
}

impl LayoutHop {
    /// The same hop in the opposite direction.
    pub fn swapped(&self) -> Self {
        Self {
            from: self.target,
            target: self.from,
        }
    }
}

/// The layout context a conversion is planned in: current index, niri layout
/// count, and the configured pair. Bundles the triple both plan functions
/// need so call sites pass one value, not three arguments.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LayoutCtx<'a> {
    pub current: u8,
    pub count: usize,
    pub pair: &'a LayoutPair,
}

impl LayoutCtx<'_> {
    /// The hop to the other layout of the pair. Refuses loudly when the
    /// current layout sits outside the pair (a third language is active) or
    /// niri reports fewer than two layouts, so text is never corrupted
    /// silently.
    pub fn hop(&self) -> Result<LayoutHop, ConversionError> {
        if self.current > 1 || self.count < 2 {
            return Err(ConversionError::Layouts {
                current: self.current,
                count: self.count,
                pair: self.pair.clone(),
            });
        }
        Ok(LayoutHop {
            from: self.current,
            target: 1 - self.current,
        })
    }
}

/// A plan step that knows its own reverse: undo replays the same content
/// back the other way.
pub trait Reversible: Clone {
    fn reversed(&self) -> Self;
}

/// Tracks the convert/undo chain across gestures.
///
/// A fresh gesture plans a new conversion and remembers it; a repeated
/// gesture undoes the remembered step and keeps the reversed step, so a
/// third repeat redoes (toggle chain). Any physical typing between gestures
/// must call [`UndoChain::invalidate`], cancelling the pending undo.
pub struct UndoChain<T: Reversible> {
    last: Option<T>,
}

impl<T: Reversible> UndoChain<T> {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Remember a fresh conversion for a later undo.
    pub fn remember(&mut self, plan: T) {
        self.last = Some(plan);
    }

    /// Whether a repeated gesture has a conversion to undo.
    pub fn has_pending_undo(&self) -> bool {
        self.last.is_some()
    }

    /// Undo the last step (or redo the undo, toggling back). Returns `None`
    /// when [`UndoChain::invalidate`] ran since, or no conversion happened.
    pub fn undo(&mut self) -> Option<T> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, PartialEq, Eq, Debug)]
    struct Step(LayoutHop);

    impl Reversible for Step {
        fn reversed(&self) -> Self {
            Self(self.0.swapped())
        }
    }

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
    fn hop_points_at_the_other_pair_position() {
        let pair = pair();
        assert_eq!(
            ctx(&pair, 0, 2).hop().unwrap(),
            LayoutHop { from: 0, target: 1 }
        );
        assert_eq!(
            ctx(&pair, 1, 2).hop().unwrap(),
            LayoutHop { from: 1, target: 0 }
        );
    }

    #[test]
    fn hop_inside_the_pair_survives_extra_layouts() {
        let pair = pair();
        assert_eq!(
            ctx(&pair, 1, 3).hop().unwrap(),
            LayoutHop { from: 1, target: 0 }
        );
    }

    #[test]
    fn hop_refuses_a_third_language_and_names_the_pair() {
        let pair = pair();
        assert_eq!(
            ctx(&pair, 2, 3).hop(),
            Err(ConversionError::Layouts {
                current: 2,
                count: 3,
                pair: pair.clone(),
            })
        );
        assert_eq!(
            ctx(&pair, 0, 1).hop(),
            Err(ConversionError::Layouts {
                current: 0,
                count: 1,
                pair: pair.clone(),
            })
        );
    }

    #[test]
    fn chain_toggles_remember_undo_redo() {
        let mut chain = UndoChain::new();
        let forward = Step(LayoutHop { from: 0, target: 1 });
        chain.remember(forward.clone());
        assert!(chain.has_pending_undo());

        let back = chain.undo().unwrap();
        assert_eq!(back, forward.reversed());

        let redo = chain.undo().unwrap();
        assert_eq!(redo, forward);
    }

    #[test]
    fn invalidate_cancels_the_pending_undo() {
        let mut chain = UndoChain::new();
        chain.remember(Step(LayoutHop { from: 0, target: 1 }));
        chain.invalidate();
        assert!(!chain.has_pending_undo());
        assert_eq!(chain.undo(), None);
    }
}

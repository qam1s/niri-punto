//! Shared undo chain and layout pair guard for both conversion paths.

use crate::config::LayoutPair;
use crate::convert::ConversionError;

/// Layout hop by explicit index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LayoutHop {
    pub from: u8,
    pub target: u8,
}

impl LayoutHop {
    pub fn swapped(&self) -> Self {
        Self {
            from: self.target,
            target: self.from,
        }
    }
}

/// The layout context a conversion is planned in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LayoutCtx<'a> {
    pub current: u8,
    pub count: usize,
    pub pair: &'a LayoutPair,
}

impl LayoutCtx<'_> {
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

    pub fn hop_toward(&self, target: u8) -> Result<LayoutHop, ConversionError> {
        debug_assert!(target <= 1, "verdict targets a pair position");
        if self.current > 1 || self.count < 2 || target > 1 {
            return Err(ConversionError::Layouts {
                current: self.current,
                count: self.count,
                pair: self.pair.clone(),
            });
        }
        Ok(LayoutHop {
            from: self.current,
            target,
        })
    }
}

/// A plan step that knows its own reverse.
pub trait Reversible: Clone {
    fn reversed(&self) -> Self;
}

/// A plan step carrying a layout hop.
pub trait HasHop {
    fn hop_target(&self) -> u8;
}

/// Tracks the convert/undo chain across gestures.
pub struct UndoChain<T: Reversible> {
    last: Option<T>,
}

impl<T: Reversible> UndoChain<T> {
    pub fn new() -> Self {
        Self { last: None }
    }

    pub fn remember(&mut self, plan: T) {
        self.last = Some(plan);
    }

    pub fn has_pending_undo(&self) -> bool {
        self.last.is_some()
    }

    pub fn last_target(&self) -> Option<u8>
    where
        T: HasHop,
    {
        self.last.as_ref().map(|step| step.hop_target())
    }

    pub fn undo(&mut self) -> Option<T> {
        let done = self.last.take()?;
        let back = done.reversed();
        self.last = Some(back.clone());
        Some(back)
    }

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
    fn hop_toward_ignores_current_but_keeps_the_pair_guard() {
        let pair = pair();
        assert_eq!(
            ctx(&pair, 1, 2).hop_toward(1).unwrap(),
            LayoutHop { from: 1, target: 1 }
        );
        assert_eq!(
            ctx(&pair, 0, 2).hop_toward(1).unwrap(),
            LayoutHop { from: 0, target: 1 }
        );
        assert_eq!(
            ctx(&pair, 2, 3).hop_toward(1),
            Err(ConversionError::Layouts {
                current: 2,
                count: 3,
                pair: pair.clone(),
            })
        );
        assert_eq!(
            ctx(&pair, 0, 1).hop_toward(1),
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

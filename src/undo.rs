use crate::convert::ConversionError;

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LayoutCtx {
    pub current: u8,
    pub count: usize,
}

impl LayoutCtx {
    pub fn hop(&self) -> Result<LayoutHop, ConversionError> {
        if self.current > 1 || self.count < 2 {
            return Err(ConversionError::Layouts {
                current: self.current,
                count: self.count,
            });
        }
        Ok(LayoutHop {
            from: self.current,
            target: 1 - self.current,
        })
    }
}

pub trait Reversible: Clone {
    fn reversed(&self) -> Self;
}

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

    fn ctx(current: u8, count: usize) -> LayoutCtx {
        LayoutCtx { current, count }
    }

    #[test]
    fn hop_points_at_the_other_pair_position() {
        assert_eq!(ctx(0, 2).hop().unwrap(), LayoutHop { from: 0, target: 1 });
        assert_eq!(ctx(1, 2).hop().unwrap(), LayoutHop { from: 1, target: 0 });
    }

    #[test]
    fn hop_inside_the_pair_survives_extra_layouts() {
        assert_eq!(ctx(1, 3).hop().unwrap(), LayoutHop { from: 1, target: 0 });
    }

    #[test]
    fn hop_refuses_a_third_language() {
        assert_eq!(
            ctx(2, 3).hop(),
            Err(ConversionError::Layouts {
                current: 2,
                count: 3,
            })
        );
        assert_eq!(
            ctx(0, 1).hop(),
            Err(ConversionError::Layouts {
                current: 0,
                count: 1,
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

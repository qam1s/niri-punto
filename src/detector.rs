//! Language detector seam (ADR-0006).
//!
//! 0.1.0 converts only on explicit user gestures, so it needs no detection
//! at all — but the detector lives behind a [`Detector`] trait from the
//! start, with a manual-only stub. When the n-gram scorer (or a small local
//! model) arrives, it implements this trait and slots into the gesture path
//! without touching the buffer, triggers, or injection.

use crate::buffer::BufferEntry;
use crate::scorer::Verdict;

/// Language detector: observes remembered input, optionally scores it.
///
/// [`Detector::score`] reports wrong-layout likelihood for the future
/// auto-conversion mode (out of scope: both implementations decline).
/// [`Detector::verdict`] reports the intended layout for manual-conversion
/// direction: confident verdicts set the hop target, declines keep today's
/// `current_layout` behavior.
pub trait Detector {
    /// Stable name for logs and diagnostics.
    fn name(&self) -> &'static str;
    /// Score `entries` for wrong-layout likelihood, or `None` to decline.
    /// Reserved for the future auto-conversion mode (out of scope): no
    /// caller yet, the manual-direction wiring uses [`Detector::verdict`].
    #[allow(dead_code)]
    fn score(&self, entries: &[BufferEntry]) -> Option<f32>;
    /// Verdict the intended layout of `entries`, or decline.
    fn verdict(&self, entries: &[BufferEntry]) -> Verdict;
}

/// 0.1.0 stub: conversion is manual-only, so detection always declines.
/// Kept as the decline baseline (tests assert decline paths against it);
/// production serves [`BigramDetector`].
#[allow(dead_code)]
pub struct ManualOnly;

impl Detector for ManualOnly {
    fn name(&self) -> &'static str {
        "manual-only"
    }

    fn score(&self, _entries: &[BufferEntry]) -> Option<f32> {
        None
    }

    fn verdict(&self, _entries: &[BufferEntry]) -> Verdict {
        Verdict::Decline
    }
}

/// Content-based direction verdicts from the bigram scorer: delegates to
/// [`crate::scorer::verdict`]. Auto mode stays declined ([`Detector::score`]
/// is `None`); only the manual-conversion direction wiring consults this.
pub struct BigramDetector;

impl Detector for BigramDetector {
    fn name(&self) -> &'static str {
        "bigram"
    }

    fn score(&self, _entries: &[BufferEntry]) -> Option<f32> {
        None
    }

    fn verdict(&self, entries: &[BufferEntry]) -> Verdict {
        crate::scorer::verdict(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_only_declines_every_buffer() {
        let detector = ManualOnly;
        assert_eq!(detector.name(), "manual-only");
        let entries = vec![BufferEntry {
            scancode: 34,
            shift: false,
        }];
        assert_eq!(detector.score(&entries), None);
        assert_eq!(detector.score(&[]), None);
        assert_eq!(detector.verdict(&entries), Verdict::Decline);
        assert_eq!(detector.verdict(&[]), Verdict::Decline);
    }

    #[test]
    fn bigram_verdicts_but_never_auto_scores() {
        use crate::scorer::Intended;
        let detector = BigramDetector;
        assert_eq!(detector.name(), "bigram");
        // `ghbdtn`-shaped word: confident RU verdict, no auto score.
        // (Scancode order matters for bigrams: g,h,b,d,t,n.)
        let entries: Vec<BufferEntry> = [34, 35, 48, 32, 20, 49]
            .map(|scancode| BufferEntry {
                scancode,
                shift: false,
            })
            .to_vec();
        assert_eq!(detector.verdict(&entries), Verdict::Intended(Intended::Ru));
        assert_eq!(detector.score(&entries), None);
        assert_eq!(detector.verdict(&[]), Verdict::Decline);
    }
}

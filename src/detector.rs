//! Language detector.

use crate::buffer::BufferEntry;
use crate::scorer::Verdict;

/// Language detector: observes remembered input, optionally scores it.
pub trait Detector {
    fn name(&self) -> &'static str;
    #[allow(dead_code)]
    fn score(&self, entries: &[BufferEntry]) -> Option<f32>;
    /// Verdict the intended layout of `entries`, or decline.
    fn verdict(&self, entries: &[BufferEntry]) -> Verdict;
}

/// Manual-only stub: conversion is manual, so detection always declines.
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

/// Content-based direction verdicts from the bigram scorer.
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

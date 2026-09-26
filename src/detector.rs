//! Language detector seam (ADR-0006).
//!
//! 0.1.0 converts only on explicit user gestures, so it needs no detection
//! at all — but the detector lives behind a [`Detector`] trait from the
//! start, with a manual-only stub. When the n-gram scorer (or a small local
//! model) arrives, it implements this trait and slots into the gesture path
//! without touching the buffer, triggers, or injection.

use crate::buffer::BufferEntry;

/// Language detector: observes remembered input, optionally scores it.
///
/// Returns `None` when there is nothing to report (the only behavior
/// 0.1.0 needs); a future scorer returns `Some(confidence)` for
/// auto-conversion once that mode exists.
pub trait Detector {
    /// Stable name for logs and diagnostics.
    fn name(&self) -> &'static str;
    /// Score `entries` for wrong-layout likelihood, or `None` to decline.
    fn score(&self, entries: &[BufferEntry]) -> Option<f32>;
}

/// 0.1.0 stub: conversion is manual-only, so detection always declines.
pub struct ManualOnly;

impl Detector for ManualOnly {
    fn name(&self) -> &'static str {
        "manual-only"
    }

    fn score(&self, _entries: &[BufferEntry]) -> Option<f32> {
        None
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
    }
}

//! Remembered input: a log of (scancode, shift) buffer entries.
//!
//! The daemon replays scancodes rather than characters, so the buffer stores
//! raw scancodes plus whether Shift was held — everything a later replay
//! needs, without any layout knowledge. Word boundaries are decided by the
//! caller through a predicate, keeping this module free of key-code tables.

/// One unit of remembered input: a raw scancode plus Shift state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufferEntry {
    /// Raw evdev key scancode (e.g. the code behind `KEY_A`).
    pub scancode: u16,
    /// Whether Shift was held when the key was pressed.
    pub shift: bool,
}

/// Bounded log of buffer entries; oldest entries are evicted past capacity.
pub struct InputBuffer {
    entries: Vec<BufferEntry>,
    capacity: usize,
    /// Layout index when the first entry after a clear arrived (`None`
    /// while empty): anchors the external-switch check — a confident
    /// verdict only overrides the current layout when the layout moved
    /// since typing.
    birth_layout: Option<u8>,
}

/// Default bound: far more than any word or phrase replay needs.
pub const DEFAULT_CAPACITY: usize = 256;

impl InputBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity: capacity.max(1),
            birth_layout: None,
        }
    }

    /// Record the layout the current fill was typed under. Sticks while
    /// entries remain (later switches do not move it); [`clear`](Self::clear)
    /// resets it for the next fill.
    pub fn note_birth(&mut self, layout: u8) {
        if self.birth_layout.is_none() {
            self.birth_layout = Some(layout);
        }
    }

    /// Layout recorded by [`note_birth`](Self::note_birth), if any fill
    /// has started since the last clear.
    pub fn birth_layout(&self) -> Option<u8> {
        self.birth_layout
    }

    pub fn push(&mut self, entry: BufferEntry) {
        if self.entries.len() >= self.capacity {
            let overflow = self.entries.len() - self.capacity + 1;
            self.entries.drain(..overflow);
        }
        self.entries.push(entry);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.birth_layout = None;
    }

    // `len`/`entries` are the read seam for tickets 09/10 (selection,
    // phrase); unused by the word-only loop of ticket 08.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn entries(&self) -> &[BufferEntry] {
        &self.entries
    }

    /// Entries since the last boundary (the current word): the suffix after
    /// the most recent entry matching `is_boundary`.
    pub fn trailing_word(&self, is_boundary: impl Fn(u16) -> bool) -> &[BufferEntry] {
        let cut = self
            .entries
            .iter()
            .rposition(|e| is_boundary(e.scancode))
            .map(|i| i + 1)
            .unwrap_or(0);
        &self.entries[cut..]
    }

    /// Whole-buffer scope for phrase conversion: everything remembered
    /// since the last [`clear`](Self::clear), including word boundaries.
    /// Bounded by Esc-clear and capacity eviction, never by word edges.
    pub fn phrase(&self) -> &[BufferEntry] {
        &self.entries
    }
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(scancode: u16) -> BufferEntry {
        BufferEntry {
            scancode,
            shift: false,
        }
    }

    const SPACE: u16 = 57; // stand-in boundary scancode for tests

    fn is_boundary(code: u16) -> bool {
        code == SPACE
    }

    #[test]
    fn empty_buffer_has_no_word() {
        let buf = InputBuffer::default();
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.trailing_word(is_boundary), &[]);
    }

    #[test]
    fn word_is_suffix_after_last_boundary() {
        let mut buf = InputBuffer::default();
        for code in [30, 48, SPACE, 31, 32] {
            buf.push(entry(code));
        }
        let word: Vec<u16> = buf
            .trailing_word(is_boundary)
            .iter()
            .map(|e| e.scancode)
            .collect();
        assert_eq!(word, vec![31, 32]);
    }

    #[test]
    fn whole_buffer_is_word_without_boundary() {
        let mut buf = InputBuffer::default();
        for code in [30, 48, 31] {
            buf.push(entry(code));
        }
        assert_eq!(buf.trailing_word(is_boundary).len(), 3);
    }

    #[test]
    fn trailing_boundary_means_empty_word() {
        let mut buf = InputBuffer::default();
        for code in [30, SPACE] {
            buf.push(entry(code));
        }
        assert_eq!(buf.trailing_word(is_boundary), &[]);
        assert_eq!(buf.len(), 2); // history kept for phrase scope
    }

    #[test]
    fn phrase_covers_whole_buffer_across_word_boundaries() {
        let mut buf = InputBuffer::default();
        for code in [30, 48, SPACE, 31, 32] {
            buf.push(entry(code));
        }
        // The word scope sees only the suffix; the phrase scope everything.
        assert_eq!(buf.trailing_word(is_boundary).len(), 2);
        let phrase: Vec<u16> = buf.phrase().iter().map(|e| e.scancode).collect();
        assert_eq!(phrase, vec![30, 48, SPACE, 31, 32]);
    }

    #[test]
    fn phrase_keeps_trailing_boundary_for_full_replay() {
        let mut buf = InputBuffer::default();
        for code in [30, SPACE] {
            buf.push(entry(code));
        }
        assert_eq!(buf.trailing_word(is_boundary), &[]);
        assert_eq!(buf.phrase().len(), 2);
    }

    #[test]
    fn phrase_is_empty_after_clear() {
        let mut buf = InputBuffer::default();
        buf.push(entry(30));
        buf.clear();
        assert_eq!(buf.phrase(), &[]);
    }

    #[test]
    fn shift_flag_is_preserved() {
        let mut buf = InputBuffer::default();
        buf.push(BufferEntry {
            scancode: 30,
            shift: true,
        });
        assert!(buf.entries()[0].shift);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let mut buf = InputBuffer::new(3);
        for code in [10, 11, 12, 13] {
            buf.push(entry(code));
        }
        let codes: Vec<u16> = buf.entries().iter().map(|e| e.scancode).collect();
        assert_eq!(codes, vec![11, 12, 13]);
    }

    #[test]
    fn clear_empties() {
        let mut buf = InputBuffer::default();
        buf.push(entry(30));
        buf.clear();
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn birth_layout_sticks_for_the_fill_and_resets_on_clear() {
        let mut buf = InputBuffer::default();
        assert_eq!(buf.birth_layout(), None);
        buf.note_birth(1);
        buf.push(entry(30));
        // A later switch does not move the anchor.
        buf.note_birth(0);
        assert_eq!(buf.birth_layout(), Some(1));
        buf.clear();
        assert_eq!(buf.birth_layout(), None);
        buf.note_birth(0);
        assert_eq!(buf.birth_layout(), Some(0));
    }
}

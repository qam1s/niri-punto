//! Remembered input: a log of (scancode, shift) entries.

/// One unit of remembered input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufferEntry {
    pub scancode: u16,
    pub shift: bool,
}

/// Bounded log of buffer entries; oldest entries are evicted past capacity.
pub struct InputBuffer {
    entries: Vec<BufferEntry>,
    capacity: usize,
    /// Layout index when the first entry after a clear arrived.
    birth_layout: Option<u8>,
}

pub const DEFAULT_CAPACITY: usize = 256;

impl InputBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity: capacity.max(1),
            birth_layout: None,
        }
    }

    pub fn note_birth(&mut self, layout: u8) {
        if self.birth_layout.is_none() {
            self.birth_layout = Some(layout);
        }
    }

    pub fn birth_layout(&self) -> Option<u8> {
        self.birth_layout
    }

    pub fn reanchor(&mut self, layout: u8) {
        self.birth_layout = Some(layout);
    }

    pub fn push(&mut self, entry: BufferEntry) {
        if self.entries.len() >= self.capacity {
            let overflow = self.entries.len() - self.capacity + 1;
            self.entries.drain(..overflow);
        }
        self.entries.push(entry);
    }

    pub fn pop(&mut self) {
        self.entries.pop();
        if self.entries.is_empty() {
            self.birth_layout = None;
        }
    }

    pub fn pop_word(&mut self, is_boundary: impl Fn(u16) -> bool) {
        let cut = self
            .entries
            .iter()
            .rposition(|e| is_boundary(e.scancode))
            .map(|i| i + 1)
            .unwrap_or(0);
        self.entries.truncate(cut);
        if self.entries.is_empty() {
            self.birth_layout = None;
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.birth_layout = None;
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn entries(&self) -> &[BufferEntry] {
        &self.entries
    }

    pub fn trailing_word(&self, is_boundary: impl Fn(u16) -> bool) -> &[BufferEntry] {
        let cut = self
            .entries
            .iter()
            .rposition(|e| is_boundary(e.scancode))
            .map(|i| i + 1)
            .unwrap_or(0);
        &self.entries[cut..]
    }

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

    const SPACE: u16 = 57;

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
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn phrase_covers_whole_buffer_across_word_boundaries() {
        let mut buf = InputBuffer::default();
        for code in [30, 48, SPACE, 31, 32] {
            buf.push(entry(code));
        }
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
        buf.note_birth(0);
        assert_eq!(buf.birth_layout(), Some(1));
        buf.clear();
        assert_eq!(buf.birth_layout(), None);
        buf.note_birth(0);
        assert_eq!(buf.birth_layout(), Some(0));
    }

    #[test]
    fn pop_drops_the_most_recent_entry() {
        let mut buf = InputBuffer::default();
        for code in [30, 48, 31] {
            buf.push(entry(code));
        }
        buf.pop();
        let codes: Vec<u16> = buf.entries().iter().map(|e| e.scancode).collect();
        assert_eq!(codes, vec![30, 48]);
    }

    #[test]
    fn pop_on_empty_is_a_noop() {
        let mut buf = InputBuffer::default();
        buf.pop();
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.birth_layout(), None);
    }

    #[test]
    fn pop_to_empty_resets_the_birth_anchor() {
        let mut buf = InputBuffer::default();
        buf.note_birth(1);
        buf.push(entry(30));
        buf.pop();
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.birth_layout(), None);
    }

    #[test]
    fn pop_word_drops_the_trailing_word() {
        let mut buf = InputBuffer::default();
        for code in [30, 48, SPACE, 31, 32] {
            buf.push(entry(code));
        }
        buf.pop_word(is_boundary);
        let codes: Vec<u16> = buf.entries().iter().map(|e| e.scancode).collect();
        assert_eq!(codes, vec![30, 48, SPACE]);
    }

    #[test]
    fn pop_word_without_boundary_clears_all() {
        let mut buf = InputBuffer::default();
        buf.note_birth(1);
        for code in [30, 48] {
            buf.push(entry(code));
        }
        buf.pop_word(is_boundary);
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.birth_layout(), None);
    }
}

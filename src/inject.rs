//! uinput injector: erase with Backspace, replay recorded scancodes.

use crate::buffer::BufferEntry;
use crate::reader::OWN_DEVICE_NAME;
use evdev::{AttributeSet, KeyCode, KeyEvent, uinput::VirtualDevice};
use std::io;

/// One key transition to emit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyStroke {
    pub scancode: u16,
    pub value: i32,
}

fn tap(scancode: u16, out: &mut Vec<KeyStroke>) {
    out.push(KeyStroke { scancode, value: 1 });
    out.push(KeyStroke { scancode, value: 0 });
}

/// Backspace down/up pairs that delete `count` characters.
pub fn erase_strokes(count: usize) -> Vec<KeyStroke> {
    let mut out = Vec::with_capacity(count * 2);
    for _ in 0..count {
        tap(KeyCode::KEY_BACKSPACE.code(), &mut out);
    }
    out
}

pub fn replay_strokes(entries: &[BufferEntry]) -> Vec<KeyStroke> {
    let shift = KeyCode::KEY_LEFTSHIFT.code();
    let mut out = Vec::with_capacity(entries.len() * 2 + 2);
    let mut shift_down = false;
    for entry in entries {
        if entry.shift && !shift_down {
            out.push(KeyStroke {
                scancode: shift,
                value: 1,
            });
            shift_down = true;
        } else if !entry.shift && shift_down {
            out.push(KeyStroke {
                scancode: shift,
                value: 0,
            });
            shift_down = false;
        }
        tap(entry.scancode, &mut out);
    }
    if shift_down {
        out.push(KeyStroke {
            scancode: shift,
            value: 0,
        });
    }
    out
}

/// A sink for key transitions: erase and replay.
pub trait Emitter {
    fn erase(&mut self, count: usize) -> io::Result<()>;
    fn replay(&mut self, entries: &[BufferEntry]) -> io::Result<()>;
}

/// uinput device that emits corrections.
pub struct Injector {
    device: VirtualDevice,
}

impl Injector {
    pub fn open() -> io::Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for code in 0..0x300 {
            keys.insert(KeyCode(code));
        }
        let device = VirtualDevice::builder()?
            .name(OWN_DEVICE_NAME)
            .with_keys(&keys)?
            .build()?;
        Ok(Self { device })
    }

    fn emit_all(&mut self, strokes: &[KeyStroke]) -> io::Result<()> {
        for stroke in strokes {
            self.device
                .emit(&[*KeyEvent::new(KeyCode(stroke.scancode), stroke.value)])?;
        }
        Ok(())
    }

    pub fn erase(&mut self, count: usize) -> io::Result<()> {
        self.emit_all(&erase_strokes(count))
    }

    pub fn replay(&mut self, entries: &[BufferEntry]) -> io::Result<()> {
        self.emit_all(&replay_strokes(entries))
    }
}

impl Emitter for Injector {
    fn erase(&mut self, count: usize) -> io::Result<()> {
        Injector::erase(self, count)
    }

    fn replay(&mut self, entries: &[BufferEntry]) -> io::Result<()> {
        Injector::replay(self, entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(scancode: u16) -> BufferEntry {
        BufferEntry {
            scancode,
            shift: false,
        }
    }

    fn shifted(scancode: u16) -> BufferEntry {
        BufferEntry {
            scancode,
            shift: true,
        }
    }

    #[test]
    fn erase_emits_one_backspace_tap_per_char() {
        let strokes = erase_strokes(3);
        let backspace = KeyCode::KEY_BACKSPACE.code();
        assert_eq!(strokes.len(), 6);
        for pair in strokes.chunks_exact(2) {
            assert_eq!(
                pair,
                &[
                    KeyStroke {
                        scancode: backspace,
                        value: 1
                    },
                    KeyStroke {
                        scancode: backspace,
                        value: 0
                    },
                ]
            );
        }
    }

    #[test]
    fn erase_zero_emits_nothing() {
        assert!(erase_strokes(0).is_empty());
    }

    #[test]
    fn replay_plain_entries_tap_each_scancode() {
        let strokes = replay_strokes(&[plain(30), plain(48)]);
        assert_eq!(
            strokes,
            vec![
                KeyStroke {
                    scancode: 30,
                    value: 1
                },
                KeyStroke {
                    scancode: 30,
                    value: 0
                },
                KeyStroke {
                    scancode: 48,
                    value: 1
                },
                KeyStroke {
                    scancode: 48,
                    value: 0
                },
            ]
        );
    }

    #[test]
    fn replay_drives_shift_around_shifted_entries() {
        let shift = KeyCode::KEY_LEFTSHIFT.code();
        let strokes = replay_strokes(&[plain(30), shifted(31), shifted(32), plain(33)]);
        assert_eq!(
            strokes,
            vec![
                KeyStroke {
                    scancode: 30,
                    value: 1
                },
                KeyStroke {
                    scancode: 30,
                    value: 0
                },
                KeyStroke {
                    scancode: shift,
                    value: 1
                },
                KeyStroke {
                    scancode: 31,
                    value: 1
                },
                KeyStroke {
                    scancode: 31,
                    value: 0
                },
                KeyStroke {
                    scancode: 32,
                    value: 1
                },
                KeyStroke {
                    scancode: 32,
                    value: 0
                },
                KeyStroke {
                    scancode: shift,
                    value: 0
                },
                KeyStroke {
                    scancode: 33,
                    value: 1
                },
                KeyStroke {
                    scancode: 33,
                    value: 0
                },
            ]
        );
    }

    #[test]
    fn replay_releases_trailing_shift() {
        let shift = KeyCode::KEY_LEFTSHIFT.code();
        let strokes = replay_strokes(&[shifted(30)]);
        assert_eq!(
            strokes.last(),
            Some(&KeyStroke {
                scancode: shift,
                value: 0
            })
        );
    }

    #[test]
    fn replay_empty_emits_nothing() {
        assert!(replay_strokes(&[]).is_empty());
    }
}

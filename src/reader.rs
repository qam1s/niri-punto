//! evdev reader: keyboard devices, hotplug, and dry-run dispatch.
//!
//! Devices are opened read-only and never grabbed, so the daemon can crash
//! or stop without ever blocking the user's keyboard. The daemon's own
//! future uinput device is excluded by name so injected keys never loop
//! back into the buffer.

use crate::buffer::BufferEntry;
use crate::trigger::Key;
use evdev::{Device, EventType, KeyCode};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// Name of the daemon's own virtual device (created from the next ticket
/// on, when injection lands). Anything with this name is never opened.
pub const OWN_DEVICE_NAME: &str = "niri-punto";

/// How often the device directory is rescanned for hotplug changes.
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// A raw key transition from one hardware device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RawKey {
    pub scancode: u16,
    /// evdev value: 0 = release, 1 = press, 2 = autorepeat.
    pub value: i32,
}

enum DeviceMsg {
    Key(RawKey),
    Disconnected(PathBuf),
}

/// Map a scancode to the trigger key space.
pub fn classify(scancode: u16) -> Key {
    if scancode == KeyCode::KEY_LEFTSHIFT.code() {
        Key::ShiftLeft
    } else if scancode == KeyCode::KEY_RIGHTSHIFT.code() {
        Key::ShiftRight
    } else if scancode == KeyCode::KEY_LEFTCTRL.code() {
        Key::CtrlLeft
    } else if scancode == KeyCode::KEY_RIGHTCTRL.code() {
        Key::CtrlRight
    } else if scancode == KeyCode::KEY_LEFTMETA.code() {
        Key::MetaLeft
    } else if scancode == KeyCode::KEY_RIGHTMETA.code() {
        Key::MetaRight
    } else if scancode == KeyCode::KEY_LEFTALT.code() {
        Key::AltLeft
    } else if scancode == KeyCode::KEY_RIGHTALT.code() {
        Key::AltRight
    } else if scancode == KeyCode::KEY_FN.code() {
        Key::Fn
    } else {
        Key::Other
    }
}

/// Scancodes that end a word: space, enter, keypad enter, tab.
pub fn is_word_boundary(scancode: u16) -> bool {
    scancode == KeyCode::KEY_SPACE.code()
        || scancode == KeyCode::KEY_ENTER.code()
        || scancode == KeyCode::KEY_KPENTER.code()
        || scancode == KeyCode::KEY_TAB.code()
}

/// Scancode that drops the buffered history.
pub fn is_reset(scancode: u16) -> bool {
    scancode == KeyCode::KEY_ESC.code()
}

/// Scancode that deletes the character before the cursor: mirrored as a
/// buffer pop, never recorded (recording it desyncs erase/replay counts
/// from the visible text).
pub fn is_backspace(scancode: u16) -> bool {
    scancode == KeyCode::KEY_BACKSPACE.code()
}

/// Scancodes that move the cursor or edit ahead of it: arrows, Home/End,
/// PageUp/PageDown, Delete, Insert. The buffer no longer describes the
/// screen, so the caller clears it instead of recording.
pub fn is_navigation(scancode: u16) -> bool {
    scancode == KeyCode::KEY_LEFT.code()
        || scancode == KeyCode::KEY_RIGHT.code()
        || scancode == KeyCode::KEY_UP.code()
        || scancode == KeyCode::KEY_DOWN.code()
        || scancode == KeyCode::KEY_HOME.code()
        || scancode == KeyCode::KEY_END.code()
        || scancode == KeyCode::KEY_PAGEUP.code()
        || scancode == KeyCode::KEY_PAGEDOWN.code()
        || scancode == KeyCode::KEY_DELETE.code()
        || scancode == KeyCode::KEY_INSERT.code()
}

/// Action keys: F1 through F24. They trigger app or compositor actions
/// (refresh, fullscreen, ...), so the screen state after them is
/// unpredictable and the caller drops the history instead of recording.
pub fn is_function(scancode: u16) -> bool {
    scancode == KeyCode::KEY_F1.code()
        || scancode == KeyCode::KEY_F2.code()
        || scancode == KeyCode::KEY_F3.code()
        || scancode == KeyCode::KEY_F4.code()
        || scancode == KeyCode::KEY_F5.code()
        || scancode == KeyCode::KEY_F6.code()
        || scancode == KeyCode::KEY_F7.code()
        || scancode == KeyCode::KEY_F8.code()
        || scancode == KeyCode::KEY_F9.code()
        || scancode == KeyCode::KEY_F10.code()
        || scancode == KeyCode::KEY_F11.code()
        || scancode == KeyCode::KEY_F12.code()
        || scancode == KeyCode::KEY_F13.code()
        || scancode == KeyCode::KEY_F14.code()
        || scancode == KeyCode::KEY_F15.code()
        || scancode == KeyCode::KEY_F16.code()
        || scancode == KeyCode::KEY_F17.code()
        || scancode == KeyCode::KEY_F18.code()
        || scancode == KeyCode::KEY_F19.code()
        || scancode == KeyCode::KEY_F20.code()
        || scancode == KeyCode::KEY_F21.code()
        || scancode == KeyCode::KEY_F22.code()
        || scancode == KeyCode::KEY_F23.code()
        || scancode == KeyCode::KEY_F24.code()
}

/// The CapsLock key itself: never text. CapsLock also flips the case
/// semantics of later letters in ways the shift flag cannot model
/// exactly (letters invert, digits do not), so the caller clears the
/// history rather than replaying a wrong case.
pub fn is_caps_lock(scancode: u16) -> bool {
    scancode == KeyCode::KEY_CAPSLOCK.code()
}

/// Copy keys for the plain-copy exception: Ctrl+C and Ctrl+Insert move
/// no text and no cursor, so the history survives them (every other
/// Ctrl shortcut clears it: select-all, cut, paste, undo, ...).
pub fn is_copy_key(scancode: u16) -> bool {
    scancode == KeyCode::KEY_C.code() || scancode == KeyCode::KEY_INSERT.code()
}

/// Key name to scancode for `chord` combos: lowercase ASCII letters and
/// top-row digits. Anything else is rejected at config load.
pub fn scancode_by_name(name: &str) -> Option<u16> {
    let code = match name {
        "a" => KeyCode::KEY_A,
        "b" => KeyCode::KEY_B,
        "c" => KeyCode::KEY_C,
        "d" => KeyCode::KEY_D,
        "e" => KeyCode::KEY_E,
        "f" => KeyCode::KEY_F,
        "g" => KeyCode::KEY_G,
        "h" => KeyCode::KEY_H,
        "i" => KeyCode::KEY_I,
        "j" => KeyCode::KEY_J,
        "k" => KeyCode::KEY_K,
        "l" => KeyCode::KEY_L,
        "m" => KeyCode::KEY_M,
        "n" => KeyCode::KEY_N,
        "o" => KeyCode::KEY_O,
        "p" => KeyCode::KEY_P,
        "q" => KeyCode::KEY_Q,
        "r" => KeyCode::KEY_R,
        "s" => KeyCode::KEY_S,
        "t" => KeyCode::KEY_T,
        "u" => KeyCode::KEY_U,
        "v" => KeyCode::KEY_V,
        "w" => KeyCode::KEY_W,
        "x" => KeyCode::KEY_X,
        "y" => KeyCode::KEY_Y,
        "z" => KeyCode::KEY_Z,
        "0" => KeyCode::KEY_0,
        "1" => KeyCode::KEY_1,
        "2" => KeyCode::KEY_2,
        "3" => KeyCode::KEY_3,
        "4" => KeyCode::KEY_4,
        "5" => KeyCode::KEY_5,
        "6" => KeyCode::KEY_6,
        "7" => KeyCode::KEY_7,
        "8" => KeyCode::KEY_8,
        "9" => KeyCode::KEY_9,
        _ => return None,
    };
    Some(code.code())
}

/// Both side scancodes of a modifier key name, for binds of bare
/// modifiers (`Mod+Shift` fires on either Shift press under Mod).
pub fn modifier_scancodes(name: &str) -> Option<(u16, u16)> {
    let (left, right) = match name {
        "mod" => (KeyCode::KEY_LEFTMETA, KeyCode::KEY_RIGHTMETA),
        "shift" => (KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_RIGHTSHIFT),
        "ctrl" => (KeyCode::KEY_LEFTCTRL, KeyCode::KEY_RIGHTCTRL),
        _ => return None,
    };
    Some((left.code(), right.code()))
}

/// Whether a device with this name must be skipped. The daemon's own
/// future uinput device is matched by exact name; nameless devices are
/// kept (real keyboards always report a name, and skipping them would
/// silently lose input).
pub fn should_skip_device(name: Option<&str>) -> bool {
    name == Some(OWN_DEVICE_NAME)
}

/// A key press worth remembering: press or autorepeat of a non-modifier.
pub fn is_typing_key(key: Key, value: i32) -> bool {
    key == Key::Other && (value == 1 || value == 2)
}

pub fn buffer_entry_for(scancode: u16, shift_held: bool) -> BufferEntry {
    BufferEntry {
        scancode,
        shift: shift_held,
    }
}

/// Candidate device nodes: `/dev/input/event*` entries that exist now.
fn candidate_nodes(input_dir: &Path) -> Vec<PathBuf> {
    let mut nodes = Vec::new();
    let Ok(dir) = std::fs::read_dir(input_dir) else {
        return nodes;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("event") {
            nodes.push(entry.path());
        }
    }
    nodes.sort();
    nodes
}

/// Try to open one node as a readable keyboard. Returns `None` for
/// anything unusable: missing nodes, non-key devices, permission errors,
/// and the daemon's own virtual device. Never grabs.
fn open_keyboard(path: &Path) -> Option<Device> {
    let device = Device::open(path).ok()?;
    if !device.supported_events().contains(EventType::KEY) {
        return None;
    }
    if should_skip_device(device.name()) {
        return None;
    }
    Some(device)
}

fn device_thread(path: PathBuf, tx: Sender<DeviceMsg>) {
    let Some(mut device) = open_keyboard(&path) else {
        return;
    };
    let name = device.name().unwrap_or("?").to_string();
    eprintln!("input: + {} ({name})", path.display());
    loop {
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break, // unplugged or lost: report and exit
        };
        for event in events {
            if event.event_type() != EventType::KEY {
                continue;
            }
            let msg = DeviceMsg::Key(RawKey {
                scancode: event.code(),
                value: event.value(),
            });
            if tx.send(msg).is_err() {
                return; // daemon is gone
            }
        }
    }
    eprintln!("input: - {} ({name})", path.display());
    let _ = tx.send(DeviceMsg::Disconnected(path));
}

/// Blocking hotplug reader. Opens every keyboard under `input_dir`,
/// picks up devices that appear later, and drops ones that disappear —
/// all without restart. Yields raw key transitions; the caller owns the
/// buffer and the trigger machine.
pub struct Reader {
    tx: Sender<DeviceMsg>,
    rx: Receiver<DeviceMsg>,
    live: HashSet<PathBuf>,
    last_rescan: Instant,
}

/// One short-poll outcome: a key, idle (try again), or every device gone.
pub enum Poll {
    Key(RawKey),
    Idle,
    Gone,
}

impl Reader {
    pub fn open(input_dir: &Path) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut reader = Self {
            tx,
            rx,
            live: HashSet::new(),
            last_rescan: Instant::now(),
        };
        let tx = reader.tx.clone();
        reader.rescan(input_dir, &tx);
        reader
    }

    /// Wait up to `timeout` for one key transition so the caller can
    /// multiplex other work (control-socket requests) between polls.
    /// Hotplug rescans run at most every [`RESCAN_INTERVAL`], except after
    /// a device disconnect, which always rescans immediately.
    pub fn poll(&mut self, input_dir: &Path, timeout: Duration) -> Poll {
        match self.rx.recv_timeout(timeout) {
            Ok(DeviceMsg::Key(key)) => Poll::Key(key),
            Ok(DeviceMsg::Disconnected(path)) => {
                self.live.remove(&path);
                Poll::Idle
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if self.last_rescan.elapsed() >= RESCAN_INTERVAL {
                    let tx = self.tx.clone();
                    self.rescan(input_dir, &tx);
                    self.last_rescan = Instant::now();
                }
                Poll::Idle
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let tx = self.tx.clone();
                self.rescan(input_dir, &tx);
                self.last_rescan = Instant::now();
                if self.live.is_empty() {
                    Poll::Gone
                } else {
                    Poll::Idle
                }
            }
        }
    }

    fn rescan(&mut self, input_dir: &Path, tx: &Sender<DeviceMsg>) {
        for path in candidate_nodes(input_dir) {
            if self.live.contains(&path) {
                continue;
            }
            // Probe before spawning so dead ends never occupy the live set.
            if open_keyboard(&path).is_none() {
                continue;
            }
            let tx = tx.clone();
            let thread_path = path.clone();
            std::thread::Builder::new()
                .name(format!("evdev-{}", path.display()))
                .spawn(move || device_thread(thread_path, tx))
                .ok();
            // `device_thread` re-opens and logs; record liveness here so a
            // device that vanishes between probe and spawn is cleaned up
            // via its Disconnected message instead of lingering forever.
            self.live.insert(path);
        }
    }

    /// Devices currently (believed) open.
    pub fn live_devices(&self) -> &HashSet<PathBuf> {
        &self.live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_and_ctrls_classify() {
        assert_eq!(classify(KeyCode::KEY_LEFTSHIFT.code()), Key::ShiftLeft);
        assert_eq!(classify(KeyCode::KEY_RIGHTSHIFT.code()), Key::ShiftRight);
        assert_eq!(classify(KeyCode::KEY_LEFTCTRL.code()), Key::CtrlLeft);
        assert_eq!(classify(KeyCode::KEY_RIGHTCTRL.code()), Key::CtrlRight);
        assert_eq!(classify(KeyCode::KEY_LEFTMETA.code()), Key::MetaLeft);
        assert_eq!(classify(KeyCode::KEY_RIGHTMETA.code()), Key::MetaRight);
        assert_eq!(classify(KeyCode::KEY_LEFTALT.code()), Key::AltLeft);
        assert_eq!(classify(KeyCode::KEY_RIGHTALT.code()), Key::AltRight);
        assert_eq!(classify(KeyCode::KEY_FN.code()), Key::Fn);
        assert_eq!(classify(KeyCode::KEY_A.code()), Key::Other);
    }

    #[test]
    fn word_boundaries() {
        for code in [
            KeyCode::KEY_SPACE.code(),
            KeyCode::KEY_ENTER.code(),
            KeyCode::KEY_KPENTER.code(),
            KeyCode::KEY_TAB.code(),
        ] {
            assert!(is_word_boundary(code), "code {code}");
        }
        assert!(!is_word_boundary(KeyCode::KEY_A.code()));
        assert!(!is_word_boundary(KeyCode::KEY_LEFTSHIFT.code()));
    }

    #[test]
    fn escape_resets() {
        assert!(is_reset(KeyCode::KEY_ESC.code()));
        assert!(!is_reset(KeyCode::KEY_A.code()));
    }

    #[test]
    fn backspace_is_detected() {
        assert!(is_backspace(KeyCode::KEY_BACKSPACE.code()));
        assert!(!is_backspace(KeyCode::KEY_A.code()));
        assert!(!is_backspace(KeyCode::KEY_DELETE.code()));
    }

    #[test]
    fn function_keys_are_detected() {
        for code in [
            KeyCode::KEY_F1.code(),
            KeyCode::KEY_F5.code(),
            KeyCode::KEY_F10.code(),
            KeyCode::KEY_F11.code(),
            KeyCode::KEY_F12.code(),
        ] {
            assert!(is_function(code), "code {code}");
        }
        assert!(!is_function(KeyCode::KEY_A.code()));
        assert!(!is_function(KeyCode::KEY_BACKSPACE.code()));
    }

    #[test]
    fn caps_lock_and_copy_keys_are_detected() {
        assert!(is_caps_lock(KeyCode::KEY_CAPSLOCK.code()));
        assert!(!is_caps_lock(KeyCode::KEY_A.code()));
        assert!(is_copy_key(KeyCode::KEY_C.code()));
        assert!(is_copy_key(KeyCode::KEY_INSERT.code()));
        assert!(!is_copy_key(KeyCode::KEY_X.code()));
        assert!(!is_copy_key(KeyCode::KEY_V.code()));
    }

    #[test]
    fn navigation_covers_cursor_and_forward_edit_keys() {
        for code in [
            KeyCode::KEY_LEFT.code(),
            KeyCode::KEY_RIGHT.code(),
            KeyCode::KEY_UP.code(),
            KeyCode::KEY_DOWN.code(),
            KeyCode::KEY_HOME.code(),
            KeyCode::KEY_END.code(),
            KeyCode::KEY_PAGEUP.code(),
            KeyCode::KEY_PAGEDOWN.code(),
            KeyCode::KEY_DELETE.code(),
            KeyCode::KEY_INSERT.code(),
        ] {
            assert!(is_navigation(code), "code {code}");
        }
        assert!(!is_navigation(KeyCode::KEY_A.code()));
        assert!(!is_navigation(KeyCode::KEY_BACKSPACE.code()));
        assert!(!is_navigation(KeyCode::KEY_SPACE.code()));
    }

    #[test]
    fn modifier_names_map_to_both_sides() {
        assert_eq!(
            modifier_scancodes("mod"),
            Some((KeyCode::KEY_LEFTMETA.code(), KeyCode::KEY_RIGHTMETA.code()))
        );
        assert_eq!(
            modifier_scancodes("shift"),
            Some((
                KeyCode::KEY_LEFTSHIFT.code(),
                KeyCode::KEY_RIGHTSHIFT.code()
            ))
        );
        assert_eq!(modifier_scancodes("alt"), None);
        assert_eq!(modifier_scancodes("l"), None);
    }

    #[test]
    fn key_names_map_to_scancodes() {
        assert_eq!(scancode_by_name("a"), Some(KeyCode::KEY_A.code()));
        assert_eq!(scancode_by_name("l"), Some(KeyCode::KEY_L.code()));
        assert_eq!(scancode_by_name("z"), Some(KeyCode::KEY_Z.code()));
        assert_eq!(scancode_by_name("0"), Some(KeyCode::KEY_0.code()));
        assert_eq!(scancode_by_name("9"), Some(KeyCode::KEY_9.code()));
        assert_eq!(scancode_by_name("L"), None);
        assert_eq!(scancode_by_name("space"), None);
        assert_eq!(scancode_by_name(""), None);
    }

    #[test]
    fn own_virtual_device_is_skipped_by_name() {
        assert!(should_skip_device(Some(OWN_DEVICE_NAME)));
        assert!(!should_skip_device(Some("AT Translated Set 2 keyboard")));
        assert!(!should_skip_device(Some("niri-punto-clone")));
        assert!(!should_skip_device(None));
    }

    #[test]
    fn typing_means_press_or_repeat_of_other_keys() {
        assert!(is_typing_key(Key::Other, 1));
        assert!(is_typing_key(Key::Other, 2));
        assert!(!is_typing_key(Key::Other, 0));
        assert!(!is_typing_key(Key::ShiftLeft, 1));
        assert!(!is_typing_key(Key::CtrlLeft, 1));
        assert!(!is_typing_key(Key::MetaLeft, 1));
        assert!(!is_typing_key(Key::AltLeft, 1));
        assert!(!is_typing_key(Key::AltRight, 1));
        assert!(!is_typing_key(Key::Fn, 1));
        assert!(!is_typing_key(Key::Fn, 2));
    }

    #[test]
    fn unreadable_input_dir_yields_no_candidates() {
        let nodes = candidate_nodes(Path::new("/nonexistent-dir-for-tests"));
        assert!(nodes.is_empty());
    }
}

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
    }

    #[test]
    fn unreadable_input_dir_yields_no_candidates() {
        let nodes = candidate_nodes(Path::new("/nonexistent-dir-for-tests"));
        assert!(nodes.is_empty());
    }
}

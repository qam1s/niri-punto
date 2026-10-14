use crate::buffer::BufferEntry;
use crate::trigger::Key;
use evdev::{Device, EventType, KeyCode};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

pub const OWN_DEVICE_NAME: &str = "niri-punto";

pub const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RawKey {
    pub scancode: u16,
    pub value: i32,
}

enum DeviceMsg {
    Key(RawKey),
    Disconnected(PathBuf),
}

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

pub fn is_word_boundary(scancode: u16) -> bool {
    scancode == KeyCode::KEY_SPACE.code()
        || scancode == KeyCode::KEY_ENTER.code()
        || scancode == KeyCode::KEY_KPENTER.code()
        || scancode == KeyCode::KEY_TAB.code()
}

pub fn is_reset(scancode: u16) -> bool {
    scancode == KeyCode::KEY_ESC.code()
}

pub fn is_backspace(scancode: u16) -> bool {
    scancode == KeyCode::KEY_BACKSPACE.code()
}

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

pub fn is_caps_lock(scancode: u16) -> bool {
    scancode == KeyCode::KEY_CAPSLOCK.code()
}

pub fn is_copy_key(scancode: u16) -> bool {
    scancode == KeyCode::KEY_C.code() || scancode == KeyCode::KEY_INSERT.code()
}

#[allow(dead_code)]
pub fn modifier_scancodes(name: &str) -> Option<(u16, u16)> {
    let (left, right) = match name {
        "mod" => (KeyCode::KEY_LEFTMETA, KeyCode::KEY_RIGHTMETA),
        "shift" => (KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_RIGHTSHIFT),
        "ctrl" => (KeyCode::KEY_LEFTCTRL, KeyCode::KEY_RIGHTCTRL),
        _ => return None,
    };
    Some((left.code(), right.code()))
}

pub fn should_skip_device(name: Option<&str>) -> bool {
    name == Some(OWN_DEVICE_NAME)
}

pub fn is_typing_key(key: Key, value: i32) -> bool {
    key == Key::Other && (value == 1 || value == 2)
}

pub fn buffer_entry_for(scancode: u16, shift_held: bool) -> BufferEntry {
    BufferEntry {
        scancode,
        shift: shift_held,
    }
}

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
            Err(_) => break,
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
                return;
            }
        }
    }
    eprintln!("input: - {} ({name})", path.display());
    let _ = tx.send(DeviceMsg::Disconnected(path));
}

pub struct Reader {
    tx: Sender<DeviceMsg>,
    rx: Receiver<DeviceMsg>,
    live: HashSet<PathBuf>,
    last_rescan: Instant,
}

#[derive(Debug, PartialEq)]
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
            if open_keyboard(&path).is_none() {
                continue;
            }
            let tx = tx.clone();
            let thread_path = path.clone();
            std::thread::Builder::new()
                .name(format!("evdev-{}", path.display()))
                .spawn(move || device_thread(thread_path, tx))
                .ok();
            self.live.insert(path);
        }
    }

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

    #[test]
    fn candidate_nodes_lists_sorted_event_entries() {
        let dir = std::env::temp_dir().join("niri-punto-test-reader-candidates");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["event10", "mouse0", "event2", "event1"] {
            std::fs::write(dir.join(name), b"fake").unwrap();
        }
        let nodes = candidate_nodes(&dir);
        let names: Vec<String> = nodes
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["event1", "event10", "event2"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn buffer_entry_records_shift_state() {
        assert_eq!(
            buffer_entry_for(30, true),
            BufferEntry {
                scancode: 30,
                shift: true
            }
        );
        assert_eq!(
            buffer_entry_for(30, false),
            BufferEntry {
                scancode: 30,
                shift: false
            }
        );
    }

    #[test]
    fn ctrl_names_map_to_both_sides() {
        assert_eq!(
            modifier_scancodes("ctrl"),
            Some((KeyCode::KEY_LEFTCTRL.code(), KeyCode::KEY_RIGHTCTRL.code()))
        );
    }

    #[test]
    fn extended_function_keys_are_detected() {
        for code in [
            KeyCode::KEY_F2.code(),
            KeyCode::KEY_F3.code(),
            KeyCode::KEY_F4.code(),
            KeyCode::KEY_F6.code(),
            KeyCode::KEY_F7.code(),
            KeyCode::KEY_F8.code(),
            KeyCode::KEY_F9.code(),
            KeyCode::KEY_F13.code(),
            KeyCode::KEY_F14.code(),
            KeyCode::KEY_F15.code(),
            KeyCode::KEY_F16.code(),
            KeyCode::KEY_F17.code(),
            KeyCode::KEY_F18.code(),
            KeyCode::KEY_F19.code(),
            KeyCode::KEY_F20.code(),
            KeyCode::KEY_F21.code(),
            KeyCode::KEY_F22.code(),
            KeyCode::KEY_F23.code(),
            KeyCode::KEY_F24.code(),
        ] {
            assert!(is_function(code), "code {code}");
        }
    }

    #[test]
    fn missing_device_is_not_a_keyboard() {
        assert!(open_keyboard(Path::new("/nonexistent-dir-for-tests/event0")).is_none());
        let dir = std::env::temp_dir().join("niri-punto-test-reader-plain");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("event0");
        std::fs::write(&plain, b"not-an-evdev-device").unwrap();
        assert!(open_keyboard(&plain).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reader_opens_empty_dir_with_no_live_devices() {
        let dir = std::env::temp_dir().join("niri-punto-test-reader-open");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let reader = Reader::open(&dir);
        assert!(reader.live_devices().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_on_empty_channel_idles_then_reports_gone() {
        use std::sync::mpsc;
        let dir = std::env::temp_dir().join("niri-punto-test-reader-poll");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let mut reader = Reader::open(&dir);
        assert!(reader.live_devices().is_empty());
        assert!(matches!(
            reader.poll(&dir, Duration::from_millis(5)),
            Poll::Idle
        ));
        drop(reader);
        let (tx, rx) = mpsc::channel::<DeviceMsg>();
        drop(tx);
        let mut reader = Reader {
            tx: mpsc::channel::<DeviceMsg>().0,
            rx,
            live: HashSet::new(),
            last_rescan: Instant::now(),
        };
        assert!(matches!(
            reader.poll(&dir, Duration::from_millis(5)),
            Poll::Gone
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_gone_is_distinct_from_idle() {
        assert_eq!(Poll::Idle, Poll::Idle);
        assert_ne!(Poll::Gone, Poll::Idle);
        assert_eq!(
            Poll::Key(RawKey {
                scancode: 30,
                value: 1
            }),
            Poll::Key(RawKey {
                scancode: 30,
                value: 1
            })
        );
        assert_ne!(
            Poll::Key(RawKey {
                scancode: 30,
                value: 1
            }),
            Poll::Key(RawKey {
                scancode: 31,
                value: 1
            })
        );
        assert!(!format!("{:?}", Poll::Gone).is_empty());
        assert!(!format!("{:?}", Poll::Idle).is_empty());
        assert!(
            !format!(
                "{:?}",
                Poll::Key(RawKey {
                    scancode: 1,
                    value: 0
                })
            )
            .is_empty()
        );
    }

    #[test]
    fn poll_delivers_queued_keys_and_disconnects() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<DeviceMsg>();
        let mut reader = Reader {
            tx: tx.clone(),
            rx,
            live: HashSet::new(),
            last_rescan: Instant::now(),
        };
        tx.send(DeviceMsg::Key(RawKey {
            scancode: 30,
            value: 1,
        }))
        .unwrap();
        let dir = std::env::temp_dir().join("niri-punto-test-reader-queue");
        assert_eq!(
            reader.poll(&dir, Duration::from_secs(1)),
            Poll::Key(RawKey {
                scancode: 30,
                value: 1
            })
        );
        let path = PathBuf::from("/dev/input/event9");
        reader.live.insert(path.clone());
        tx.send(DeviceMsg::Disconnected(path)).unwrap();
        assert!(matches!(
            reader.poll(&dir, Duration::from_secs(1)),
            Poll::Idle
        ));
        assert!(reader.live_devices().is_empty());
    }

    #[test]
    fn rescan_ignores_missing_and_plain_files() {
        let dir = std::env::temp_dir().join("niri-punto-test-reader-rescan");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("event0"), b"not-an-evdev-device").unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<DeviceMsg>();
        let mut reader = Reader {
            tx: tx.clone(),
            rx,
            live: HashSet::new(),
            last_rescan: Instant::now(),
        };
        reader.rescan(&dir, &tx);
        assert!(reader.live_devices().is_empty());
        reader.rescan(Path::new("/nonexistent-dir-for-tests"), &tx);
        assert!(reader.live_devices().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn timeout_rescans_after_the_interval() {
        let dir = std::env::temp_dir().join("niri-punto-test-reader-rescan-iv");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<DeviceMsg>();
        let mut reader = Reader {
            tx,
            rx,
            live: HashSet::new(),
            last_rescan: Instant::now() - RESCAN_INTERVAL - Duration::from_secs(1),
        };
        assert!(matches!(
            reader.poll(&dir, Duration::from_millis(5)),
            Poll::Idle
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn disconnect_with_live_devices_idles() {
        use std::sync::mpsc;
        let dir = std::env::temp_dir().join("niri-punto-test-reader-live-gone");
        let (tx, rx) = mpsc::channel::<DeviceMsg>();
        drop(tx);
        let mut live = HashSet::new();
        live.insert(dir.join("event0"));
        let mut reader = Reader {
            tx: mpsc::channel::<DeviceMsg>().0,
            rx,
            live,
            last_rescan: Instant::now(),
        };
        assert!(matches!(
            reader.poll(&dir, Duration::from_millis(5)),
            Poll::Idle
        ));
    }

    #[test]
    fn rescan_skips_already_live_paths() {
        let dir = std::env::temp_dir().join("niri-punto-test-reader-live-skip");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("event0"), b"not-an-evdev-device").unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<DeviceMsg>();
        let mut live = HashSet::new();
        live.insert(dir.join("event0"));
        let mut reader = Reader {
            tx: tx.clone(),
            rx,
            live,
            last_rescan: Instant::now(),
        };
        reader.rescan(&dir, &tx);
        assert_eq!(reader.live_devices().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}

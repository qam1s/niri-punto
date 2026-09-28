//! niri IPC client: layout index reads, index switches, event barrier.

use niri_ipc::{
    Action, Event, KeyboardLayouts, LayoutSwitchTarget, Request, Response, socket::Socket,
};
use std::io;
use std::time::Duration;

/// Base delay for IPC reconnect backoff.
pub const RECONNECT_BASE_MS: u64 = 500;
/// Backoff never waits longer than this between attempts.
pub const RECONNECT_MAX_MS: u64 = 10_000;
/// Reconnect attempts before giving up.
pub const RECONNECT_ATTEMPTS: u32 = 6;

/// Backoff delay before reconnect `attempt` (0-based).
pub fn backoff_delay(attempt: u32) -> Duration {
    let doubled = RECONNECT_BASE_MS.saturating_mul(1u64 << attempt.min(20));
    Duration::from_millis(doubled.min(RECONNECT_MAX_MS))
}

/// Layout backend: layout index reads, index switches, event barrier.
pub trait LayoutBackend {
    fn current_layout(&mut self) -> io::Result<(u8, usize)>;
    fn switch_to(&mut self, index: u8) -> io::Result<()>;
    fn wait_for_layout(&mut self, target: u8) -> io::Result<()>;
    fn recover_barrier(&mut self, target: u8) -> io::Result<bool>;
    fn focused_window_id(&mut self) -> io::Result<Option<u64>>;
}

/// Client holding the request socket and the event-stream reader.
pub struct IpcClient {
    requests: Socket,
    read_event: Box<dyn FnMut() -> io::Result<Event>>,
}

impl IpcClient {
    pub fn connect() -> io::Result<Self> {
        let requests = Socket::connect()?;
        let read_event = subscribe_events()?;
        Ok(Self {
            requests,
            read_event,
        })
    }

    /// Connect with backoff, for compositor restarts.
    pub fn connect_with_retry() -> io::Result<Self> {
        let mut last = io::Error::other("no attempts ran");
        for attempt in 0..RECONNECT_ATTEMPTS {
            match Self::connect() {
                Ok(client) => return Ok(client),
                Err(error) => {
                    last = error;
                    if attempt + 1 < RECONNECT_ATTEMPTS {
                        eprintln!(
                            "niri IPC connect failed ({last}); retrying in {:?}",
                            backoff_delay(attempt)
                        );
                        std::thread::sleep(backoff_delay(attempt));
                    }
                }
            }
        }
        Err(last)
    }

    /// Full layout state: names in index order plus the current index.
    pub fn layouts(&mut self) -> io::Result<KeyboardLayouts> {
        match self.requests.send(Request::KeyboardLayouts) {
            Ok(Ok(Response::KeyboardLayouts(layouts))) => Ok(layouts),
            Ok(Ok(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "niri answered KeyboardLayouts with an unexpected response",
            )),
            Ok(Err(message)) => Err(io::Error::other(format!(
                "niri reported an error: {message}"
            ))),
            Err(error) => Err(error),
        }
    }
}

impl LayoutBackend for IpcClient {
    fn current_layout(&mut self) -> io::Result<(u8, usize)> {
        let layouts = self.layouts()?;
        Ok((layouts.current_idx, layouts.names.len()))
    }

    fn focused_window_id(&mut self) -> io::Result<Option<u64>> {
        match self.requests.send(Request::FocusedWindow) {
            Ok(Ok(Response::FocusedWindow(window))) => Ok(window.map(|w| w.id)),
            Ok(Ok(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "niri answered FocusedWindow with an unexpected response",
            )),
            Ok(Err(message)) => Err(io::Error::other(format!(
                "niri reported an error: {message}"
            ))),
            Err(error) => Err(error),
        }
    }

    fn switch_to(&mut self, index: u8) -> io::Result<()> {
        let action = Action::SwitchLayout {
            layout: LayoutSwitchTarget::Index(index),
        };
        match self.requests.send(Request::Action(action)) {
            Ok(Ok(Response::Handled)) => Ok(()),
            Ok(Ok(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "niri answered SwitchLayout with an unexpected response",
            )),
            Ok(Err(message)) => Err(io::Error::other(format!(
                "niri refused the layout switch: {message}"
            ))),
            Err(error) => Err(error),
        }
    }

    fn wait_for_layout(&mut self, target: u8) -> io::Result<()> {
        loop {
            let event = (self.read_event)()?;
            if event_is_layout(&event, target) {
                return Ok(());
            }
        }
    }

    fn recover_barrier(&mut self, target: u8) -> io::Result<bool> {
        let mut last = io::Error::other("no attempts ran");
        for attempt in 0..RECONNECT_ATTEMPTS {
            match subscribe_events() {
                Ok(read_event) => {
                    self.read_event = read_event;
                    let (current, _) = LayoutBackend::current_layout(self)?;
                    return Ok(current == target);
                }
                Err(error) => {
                    last = error;
                    if attempt + 1 < RECONNECT_ATTEMPTS {
                        std::thread::sleep(backoff_delay(attempt));
                    }
                }
            }
        }
        Err(last)
    }
}

fn subscribe_events() -> io::Result<Box<dyn FnMut() -> io::Result<Event>>> {
    let mut events = Socket::connect()?;
    match events.send(Request::EventStream) {
        Ok(Ok(Response::Handled)) => {}
        Ok(Ok(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "niri refused the event stream with an unexpected response",
            ));
        }
        Ok(Err(message)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("niri refused the event stream: {message}"),
            ));
        }
        Err(error) => return Err(error),
    }
    Ok(Box::new(events.read_events()))
}

/// Whether `event` confirms `target` is now the active layout.
pub fn event_is_layout(event: &Event, target: u8) -> bool {
    match event {
        Event::KeyboardLayoutSwitched { idx } => *idx == target,
        Event::KeyboardLayoutsChanged { keyboard_layouts } => {
            keyboard_layouts.current_idx == target
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use niri_ipc::KeyboardLayouts;

    fn changed(idx: u8) -> Event {
        Event::KeyboardLayoutsChanged {
            keyboard_layouts: KeyboardLayouts {
                names: vec!["us".to_string(), "ru".to_string()],
                current_idx: idx,
            },
        }
    }

    #[test]
    fn switched_event_matches_target_only() {
        assert!(event_is_layout(
            &Event::KeyboardLayoutSwitched { idx: 1 },
            1
        ));
        assert!(!event_is_layout(
            &Event::KeyboardLayoutSwitched { idx: 0 },
            1
        ));
    }

    #[test]
    fn changed_event_matches_current_idx_only() {
        assert!(event_is_layout(&changed(1), 1));
        assert!(!event_is_layout(&changed(0), 1));
    }

    #[test]
    fn unrelated_events_never_match() {
        assert!(!event_is_layout(&Event::WindowClosed { id: 7 }, 0));
        assert!(!event_is_layout(&Event::ConfigLoaded { failed: false }, 0));
    }

    #[test]
    fn backoff_doubles_from_the_base_and_caps() {
        use std::time::Duration;
        assert_eq!(backoff_delay(0), Duration::from_millis(500));
        assert_eq!(backoff_delay(1), Duration::from_millis(1000));
        assert_eq!(backoff_delay(2), Duration::from_millis(2000));
        assert_eq!(backoff_delay(3), Duration::from_millis(4000));
        assert_eq!(backoff_delay(4), Duration::from_millis(8000));
        assert_eq!(backoff_delay(5), Duration::from_millis(10_000));
        assert_eq!(backoff_delay(6), Duration::from_millis(10_000));
        assert_eq!(backoff_delay(u32::MAX), Duration::from_millis(10_000));
    }
}

//! niri IPC client: layout index reads, index switches, event barrier.
//!
//! Hardware-dependent: needs `$NIRI_SOCKET` (i.e. running inside niri).
//! Opens two connections because an event-stream connection never takes
//! further requests: one long-lived event stream plus one request socket.
//! Layout switches always name an explicit index, never next/prev.
//!
//! [`event_is_layout`] is pure and unit-tested; the rest needs a compositor.

use niri_ipc::{
    Action, Event, KeyboardLayouts, LayoutSwitchTarget, Request, Response, socket::Socket,
};
use std::io;
use std::time::Duration;

/// Base delay for IPC reconnect backoff; doubles per attempt, capped.
pub const RECONNECT_BASE_MS: u64 = 500;
/// Backoff never waits longer than this between attempts.
pub const RECONNECT_MAX_MS: u64 = 10_000;
/// Reconnect attempts before giving up (then the daemon exits and the
/// systemd unit restarts it as a last resort).
pub const RECONNECT_ATTEMPTS: u32 = 6;

/// Backoff delay before reconnect `attempt` (0-based): exponential in
/// [`RECONNECT_BASE_MS`], capped at [`RECONNECT_MAX_MS`].
pub fn backoff_delay(attempt: u32) -> Duration {
    let doubled = RECONNECT_BASE_MS.saturating_mul(1u64 << attempt.min(20));
    Duration::from_millis(doubled.min(RECONNECT_MAX_MS))
}

/// Layout backend: layout index reads, index switches, event barrier. The
/// production implementation is [`IpcClient`] (needs `$NIRI_SOCKET`); tests
/// slot in a fake without a compositor.
pub trait LayoutBackend {
    /// Current layout index and configured layout count.
    fn current_layout(&mut self) -> io::Result<(u8, usize)>;
    /// Switch to `index` by explicit index.
    fn switch_to(&mut self, index: u8) -> io::Result<()>;
    /// Block until niri reports `target` as active.
    fn wait_for_layout(&mut self, target: u8) -> io::Result<()>;
    /// Re-establish a dead event stream, then check `target`. See
    /// [`IpcClient::recover_barrier`].
    fn recover_barrier(&mut self, target: u8) -> io::Result<bool>;
}

/// Client holding the request socket and the event-stream reader.
pub struct IpcClient {
    requests: Socket,
    read_event: Box<dyn FnMut() -> io::Result<Event>>,
}

impl IpcClient {
    /// Connect both sockets and subscribe to the event stream.
    pub fn connect() -> io::Result<Self> {
        let requests = Socket::connect()?;
        let read_event = subscribe_events()?;
        Ok(Self {
            requests,
            read_event,
        })
    }

    /// Connect with backoff, for compositor restarts: retry
    /// [`RECONNECT_ATTEMPTS`] times before giving up (the caller exits and
    /// the systemd unit restarts the daemon as a last resort).
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
    /// [`current_layout`](LayoutBackend::current_layout) is the index/count
    /// projection `doctor` reports.
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
    /// Current layout index and configured layout count.
    fn current_layout(&mut self) -> io::Result<(u8, usize)> {
        let layouts = self.layouts()?;
        Ok((layouts.current_idx, layouts.names.len()))
    }

    /// Switch to `index` by explicit index. Rejects out-of-range u8 upstream;
    /// the caller passes indices learned from niri itself.
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

    /// Block until niri reports `target` as active. Skips unrelated events;
    /// never sleeps. An I/O error means the event stream died — the caller
    /// recovers via [`IpcClient::recover_barrier`] instead of exiting.
    fn wait_for_layout(&mut self, target: u8) -> io::Result<()> {
        loop {
            let event = (self.read_event)()?;
            if event_is_layout(&event, target) {
                return Ok(());
            }
        }
    }

    /// Re-establish a dead event stream with backoff, then check whether
    /// `target` is already active. Returns `Ok(true)` when the barrier is
    /// satisfied after recovery, `Ok(false)` when the stream is back but
    /// the layout sits elsewhere, and `Err` when reconnects ran out (the
    /// caller fails the conversion; exiting stays the last resort).
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

/// Subscribe one event-stream connection: the second socket, which never
/// takes further requests afterwards.
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

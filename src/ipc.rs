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

/// Client holding the request socket and the event-stream reader.
pub struct IpcClient {
    requests: Socket,
    read_event: Box<dyn FnMut() -> io::Result<Event>>,
}

impl IpcClient {
    /// Connect both sockets and subscribe to the event stream.
    pub fn connect() -> io::Result<Self> {
        let requests = Socket::connect()?;
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
        Ok(Self {
            requests,
            read_event: Box::new(events.read_events()),
        })
    }

    /// Full layout state: names in index order plus the current index.
    /// [`current_layout`] is the index/count projection `doctor` reports.
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

    /// Current layout index and configured layout count.
    pub fn current_layout(&mut self) -> io::Result<(u8, usize)> {
        let layouts = self.layouts()?;
        Ok((layouts.current_idx, layouts.names.len()))
    }

    /// Switch to `index` by explicit index. Rejects out-of-range u8 upstream;
    /// the caller passes indices learned from niri itself.
    pub fn switch_to(&mut self, index: u8) -> io::Result<()> {
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
    /// never sleeps. An I/O error means the compositor went away — the caller
    /// should exit and let the service restart.
    pub fn wait_for_layout(&mut self, target: u8) -> io::Result<()> {
        loop {
            let event = (self.read_event)()?;
            if event_is_layout(&event, target) {
                return Ok(());
            }
        }
    }
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
}

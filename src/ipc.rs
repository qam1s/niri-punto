use niri_ipc::{
    Action, Event, KeyboardLayouts, LayoutSwitchTarget, Request, Response, socket::Socket,
};
use std::io;
use std::time::Duration;

pub const RECONNECT_BASE_MS: u64 = 500;
pub const RECONNECT_MAX_MS: u64 = 10_000;
pub const RECONNECT_ATTEMPTS: u32 = 6;

pub fn backoff_delay(attempt: u32) -> Duration {
    let doubled = RECONNECT_BASE_MS.saturating_mul(1u64 << attempt.min(20));
    Duration::from_millis(doubled.min(RECONNECT_MAX_MS))
}

pub trait LayoutBackend {
    fn current_layout(&mut self) -> io::Result<(u8, usize, Vec<String>)>;
    fn switch_to(&mut self, index: u8) -> io::Result<()>;
    fn wait_for_layout(&mut self, target: u8) -> io::Result<()>;
    fn recover_barrier(&mut self, target: u8) -> io::Result<bool>;
    fn focused_window_id(&mut self) -> io::Result<Option<u64>>;
}

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

    pub fn connect_with_retry() -> io::Result<Self> {
        Self::connect_with_retry_attempts(RECONNECT_ATTEMPTS)
    }

    fn connect_with_retry_attempts(attempts: u32) -> io::Result<Self> {
        let mut last = io::Error::other("no attempts ran");
        for attempt in 0..attempts {
            match Self::connect() {
                Ok(client) => return Ok(client),
                Err(error) => {
                    last = error;
                    if attempt + 1 < attempts {
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

    fn recover_barrier_attempts(&mut self, target: u8, attempts: u32) -> io::Result<bool> {
        let mut last = io::Error::other("no attempts ran");
        for attempt in 0..attempts {
            match subscribe_events() {
                Ok(read_event) => {
                    self.read_event = read_event;
                    let (current, _, _) = LayoutBackend::current_layout(self)?;
                    return Ok(current == target);
                }
                Err(error) => {
                    last = error;
                    if attempt + 1 < attempts {
                        std::thread::sleep(backoff_delay(attempt));
                    }
                }
            }
        }
        Err(last)
    }

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
    fn current_layout(&mut self) -> io::Result<(u8, usize, Vec<String>)> {
        let layouts = self.layouts()?;
        Ok((layouts.current_idx, layouts.names.len(), layouts.names))
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
        self.recover_barrier_attempts(target, RECONNECT_ATTEMPTS)
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
    use niri_ipc::Reply;
    use std::sync::Arc;

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

    const CLOSE_SENTINEL: &str = "__fake_niri_close_without_reply__";

    fn auto_reply(request: &Request) -> Reply {
        match request {
            Request::KeyboardLayouts => Ok(Response::KeyboardLayouts(KeyboardLayouts {
                names: vec!["us".to_string(), "ru".to_string()],
                current_idx: 0,
            })),
            Request::FocusedWindow => Ok(Response::FocusedWindow(None)),
            Request::Action(_) => Ok(Response::Handled),
            Request::EventStream => Ok(Response::Handled),
            _ => Err("unsupported in fake niri".to_string()),
        }
    }

    struct FakeNiri {
        path: std::path::PathBuf,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeNiri {
        fn start(
            overrides: Vec<Reply>,
            stream_reply: Option<Reply>,
            events: Vec<Event>,
            raw_stream: Option<String>,
        ) -> Self {
            static IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = IDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "niri-punto-fake-niri-{}-{id}.sock",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_clone = stop.clone();
            let queue = Arc::new(std::sync::Mutex::new(
                overrides
                    .into_iter()
                    .collect::<std::collections::VecDeque<_>>(),
            ));
            let thread = std::thread::spawn(move || {
                while !stop_clone.load(std::sync::atomic::Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let queue = queue.clone();
                            let events = events.clone();
                            let stream_reply = stream_reply.clone();
                            let raw_stream = raw_stream.clone();
                            std::thread::spawn(move || {
                                serve_conn(
                                    stream,
                                    &queue,
                                    stream_reply,
                                    &events,
                                    raw_stream.as_deref(),
                                )
                            });
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                path,
                stop,
                thread: Some(thread),
            }
        }

        fn socket_env(&self) -> (crate::test_env::EnvGuard, Option<std::ffi::OsString>) {
            let guard = crate::test_env::lock();
            let prior = std::env::var_os("NIRI_SOCKET");
            unsafe {
                std::env::set_var("NIRI_SOCKET", &self.path);
            }
            (guard, prior)
        }
    }

    impl Drop for FakeNiri {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                thread.join().ok();
            }
            std::fs::remove_file(&self.path).ok();
        }
    }

    fn restore_socket(guard: crate::test_env::EnvGuard, prior: Option<std::ffi::OsString>) {
        crate::test_env::restore("NIRI_SOCKET", prior);
        drop(guard);
    }

    fn serve_conn(
        stream: std::os::unix::net::UnixStream,
        queue: &std::sync::Mutex<std::collections::VecDeque<Reply>>,
        stream_reply: Option<Reply>,
        events: &[Event],
        raw_stream: Option<&str>,
    ) {
        use std::io::{BufRead, Write};
        let reader_stream = stream.try_clone().unwrap();
        let mut reader = std::io::BufReader::new(reader_stream);
        let mut writer = stream;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            let Ok(request) = serde_json::from_str::<Request>(&line) else {
                break;
            };
            if matches!(request, Request::EventStream) {
                if let Some(raw) = raw_stream {
                    let _ = writer.write_all(raw.as_bytes());
                    let _ = writer.write_all(b"\n");
                    break;
                }
                let reply = stream_reply.clone().unwrap_or(Ok(Response::Handled));
                if is_close(&reply) {
                    break;
                }
                let mut encoded = serde_json::to_string(&reply).unwrap();
                encoded.push('\n');
                if writer.write_all(encoded.as_bytes()).is_err() {
                    break;
                }
                if matches!(reply, Ok(Response::Handled)) {
                    for event in events {
                        let mut encoded = serde_json::to_string(event).unwrap();
                        encoded.push('\n');
                        if writer.write_all(encoded.as_bytes()).is_err() {
                            break;
                        }
                    }
                }
                break;
            }
            let reply = queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| auto_reply(&request));
            if is_close(&reply) {
                break;
            }
            let mut encoded = serde_json::to_string(&reply).unwrap();
            encoded.push('\n');
            if writer.write_all(encoded.as_bytes()).is_err() {
                break;
            }
        }
    }

    fn is_close(reply: &Reply) -> bool {
        matches!(reply, Err(message) if message == CLOSE_SENTINEL)
    }

    fn close_override() -> Reply {
        Err(CLOSE_SENTINEL.to_string())
    }

    #[test]
    fn auto_reply_answers_known_requests() {
        assert!(auto_reply(&Request::KeyboardLayouts).is_ok());
        assert!(auto_reply(&Request::FocusedWindow).is_ok());
        assert!(auto_reply(&Request::EventStream).is_ok());
        assert!(auto_reply(&Request::Version).is_err());
        assert!(auto_reply(&Request::Outputs).is_err());
    }

    #[test]
    fn connect_fails_without_a_socket_path() {
        let _env = crate::test_env::lock();
        let prior = std::env::var_os("NIRI_SOCKET");
        unsafe {
            std::env::remove_var("NIRI_SOCKET");
        }
        assert!(IpcClient::connect().is_err());
        assert!(IpcClient::connect_with_retry_attempts(1).is_err());
        assert!(IpcClient::connect_with_retry_attempts(0).is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn connect_fails_on_a_missing_socket_file() {
        let _env = crate::test_env::lock();
        let prior = std::env::var_os("NIRI_SOCKET");
        unsafe {
            std::env::set_var(
                "NIRI_SOCKET",
                std::env::temp_dir().join("niri-punto-no-such-socket.sock"),
            );
        }
        assert!(IpcClient::connect().is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn connect_and_retry_succeed_against_fake_niri() {
        let server = FakeNiri::start(vec![], None, vec![], None);
        let (_env, prior) = server.socket_env();
        assert!(IpcClient::connect().is_ok());
        assert!(IpcClient::connect_with_retry().is_ok());
        restore_socket(_env, prior);
    }

    #[test]
    fn layouts_roundtrip_through_fake_niri() {
        let server = FakeNiri::start(vec![], None, vec![], None);
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        let layouts = client.layouts().unwrap();
        assert_eq!(layouts.names, vec!["us".to_string(), "ru".to_string()]);
        assert_eq!(layouts.current_idx, 0);
        assert_eq!(client.current_layout().unwrap().0, 0);
        restore_socket(_env, prior);
    }

    #[test]
    fn layouts_rejects_an_unexpected_response() {
        let server = FakeNiri::start(vec![Ok(Response::Handled)], None, vec![], None);
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        let error = client.layouts().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        restore_socket(_env, prior);
    }

    #[test]
    fn layouts_reports_a_niri_error() {
        let server = FakeNiri::start(vec![Err("busy".to_string())], None, vec![], None);
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        let error = client.layouts().unwrap_err();
        assert!(error.to_string().contains("busy"), "{error}");
        restore_socket(_env, prior);
    }

    #[test]
    fn layouts_reports_a_broken_pipe() {
        let server = FakeNiri::start(vec![close_override()], None, vec![], None);
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        assert!(client.layouts().is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn focused_window_reports_none_and_errors() {
        let server = FakeNiri::start(
            vec![
                Ok(Response::FocusedWindow(None)),
                Ok(Response::Handled),
                Err("denied".to_string()),
                close_override(),
            ],
            None,
            vec![],
            None,
        );
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        assert_eq!(client.focused_window_id().unwrap(), None);
        let error = client.focused_window_id().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let error = client.focused_window_id().unwrap_err();
        assert!(error.to_string().contains("denied"), "{error}");
        assert!(client.focused_window_id().is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn switch_reports_ok_and_errors() {
        let server = FakeNiri::start(
            vec![
                Ok(Response::Handled),
                Ok(Response::KeyboardLayouts(KeyboardLayouts {
                    names: vec!["us".to_string()],
                    current_idx: 0,
                })),
                Err("refused".to_string()),
                close_override(),
            ],
            None,
            vec![],
            None,
        );
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        assert!(client.switch_to(1).is_ok());
        let error = client.switch_to(1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let error = client.switch_to(1).unwrap_err();
        assert!(error.to_string().contains("refused"), "{error}");
        assert!(client.switch_to(1).is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn wait_returns_on_the_matching_event() {
        let server = FakeNiri::start(
            vec![],
            None,
            vec![
                Event::WindowClosed { id: 7 },
                Event::KeyboardLayoutSwitched { idx: 1 },
            ],
            None,
        );
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        assert!(client.wait_for_layout(1).is_ok());
        restore_socket(_env, prior);
    }

    #[test]
    fn wait_reports_a_dead_stream() {
        let server = FakeNiri::start(vec![], None, vec![], None);
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        assert!(client.wait_for_layout(1).is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn recover_resubscribes_and_compares() {
        let server = FakeNiri::start(vec![], None, vec![], None);
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        assert!(client.recover_barrier(0).unwrap());
        assert!(!client.recover_barrier(1).unwrap());
        assert!(client.recover_barrier_attempts(0, 1).unwrap());
        restore_socket(_env, prior);
    }

    #[test]
    fn recover_reports_a_dead_socket_with_one_attempt() {
        let server = FakeNiri::start(vec![], None, vec![], None);
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        unsafe {
            std::env::set_var(
                "NIRI_SOCKET",
                std::env::temp_dir().join("niri-punto-no-such-socket.sock"),
            );
        }
        assert!(client.recover_barrier_attempts(0, 1).is_err());
        assert!(client.recover_barrier_attempts(0, 0).is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn event_stream_rejects_a_wrong_response() {
        let server = FakeNiri::start(
            vec![],
            Some(Ok(Response::Version("26.4.0".to_string()))),
            vec![],
            None,
        );
        let (_env, prior) = server.socket_env();
        assert!(IpcClient::connect().is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn event_stream_reports_a_niri_refusal() {
        let server = FakeNiri::start(vec![], Some(Err("no stream".to_string())), vec![], None);
        let (_env, prior) = server.socket_env();
        assert!(IpcClient::connect().is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn event_stream_reports_garbage_bytes() {
        let server = FakeNiri::start(vec![], None, vec![], Some("not json".to_string()));
        let (_env, prior) = server.socket_env();
        assert!(IpcClient::connect().is_err());
        restore_socket(_env, prior);
    }

    #[test]
    fn retry_sleeps_once_on_two_attempts() {
        let _env = crate::test_env::lock();
        let prior = std::env::var_os("NIRI_SOCKET");
        unsafe {
            std::env::set_var(
                "NIRI_SOCKET",
                std::env::temp_dir().join("niri-punto-no-such-socket.sock"),
            );
        }
        let start = std::time::Instant::now();
        assert!(IpcClient::connect_with_retry_attempts(2).is_err());
        assert!(start.elapsed() >= backoff_delay(0));
        restore_socket(_env, prior);
    }

    #[test]
    fn recover_sleeps_once_on_two_attempts() {
        let server = FakeNiri::start(vec![], None, vec![], None);
        let (_env, prior) = server.socket_env();
        let mut client = IpcClient::connect().unwrap();
        unsafe {
            std::env::set_var(
                "NIRI_SOCKET",
                std::env::temp_dir().join("niri-punto-no-such-socket.sock"),
            );
        }
        let start = std::time::Instant::now();
        assert!(client.recover_barrier_attempts(0, 2).is_err());
        assert!(start.elapsed() >= backoff_delay(0));
        restore_socket(_env, prior);
    }
}

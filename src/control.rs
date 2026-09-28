//! Control socket: niri-bind subcommands talk to the running daemon.

use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

/// Which conversion a niri bind asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlKind {
    Word,
    Selection,
}

impl ControlKind {
    pub fn as_line(self) -> &'static str {
        match self {
            Self::Word => "convert-word\n",
            Self::Selection => "convert-selection\n",
        }
    }
}

pub fn parse_request(line: &str) -> Option<ControlKind> {
    match line.trim() {
        "convert-word" => Some(ControlKind::Word),
        "convert-selection" => Some(ControlKind::Selection),
        _ => None,
    }
}

/// Path of the daemon's control socket.
pub fn socket_path() -> PathBuf {
    if let Ok(path) = env::var("NIRI_PUNTO_SOCKET") {
        return PathBuf::from(path);
    }
    if let Ok(dir) = env::var("XDG_RUNTIME_DIR") {
        return Path::new(&dir).join("niri-punto.sock");
    }
    env::temp_dir().join("niri-punto.sock")
}

pub fn is_live(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Bind the control socket for `run`.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if is_live(path) {
        return Err(already_running(path));
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => Err(already_running(path)),
        Err(error) => Err(error),
    }
}

fn already_running(path: &Path) -> io::Error {
    io::Error::other(format!(
        "niri-punto is already running (control socket {} is live); \
         not starting a second daemon",
        path.display()
    ))
}

/// Ask the running daemon for one conversion.
pub fn send(path: &Path, kind: ControlKind) -> io::Result<String> {
    let stream = UnixStream::connect(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("no running daemon at {}: {error}", path.display()),
        )
    })?;
    let mut stream = stream;
    stream.write_all(kind.as_line().as_bytes())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    Ok(reply.trim_end().to_string())
}

/// One bind request forwarded to the daemon's main loop for conversion.
pub struct ControlRequest {
    pub kind: ControlKind,
    pub reply: Sender<String>,
}

fn read_request_line(stream: &UnixStream) -> Option<String> {
    let mut line = String::new();
    match BufReader::new(stream).read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(line),
        Err(_) => None,
    }
}

fn write_reply(stream: &UnixStream, message: &str) {
    let mut stream = stream;
    let _ = writeln!(stream, "{message}");
}

/// Serve a single connection.
pub fn serve_one(stream: UnixStream, requests: &Sender<ControlRequest>) -> bool {
    let Some(line) = read_request_line(&stream) else {
        return false;
    };
    let Some(kind) = parse_request(&line) else {
        write_reply(&stream, &format!("err unknown command: {}", line.trim()));
        return true;
    };
    if kind == ControlKind::Selection {
        write_reply(&stream, "err selection conversion is not wired yet");
        return true;
    }
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    if requests.send(ControlRequest { kind, reply: tx }).is_err() {
        write_reply(&stream, "err daemon is shutting down");
        return true;
    }
    match rx.recv() {
        Ok(answer) => write_reply(&stream, &answer),
        Err(_) => write_reply(&stream, "err daemon is shutting down"),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream as PairStream;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn known_commands_parse_with_or_without_newline() {
        assert_eq!(parse_request("convert-word"), Some(ControlKind::Word));
        assert_eq!(parse_request("convert-word\n"), Some(ControlKind::Word));
        assert_eq!(
            parse_request("convert-selection\n"),
            Some(ControlKind::Selection)
        );
    }

    #[test]
    fn unknown_commands_are_rejected() {
        assert_eq!(parse_request(""), None);
        assert_eq!(parse_request("convert-word extra"), None);
        assert_eq!(parse_request("CONVERT-WORD"), None);
        assert_eq!(parse_request("doctor"), None);
    }

    #[test]
    fn request_lines_roundtrip_through_parse() {
        assert_eq!(
            parse_request(ControlKind::Word.as_line()),
            Some(ControlKind::Word)
        );
        assert_eq!(
            parse_request(ControlKind::Selection.as_line()),
            Some(ControlKind::Selection)
        );
    }

    #[test]
    fn socket_path_override_wins_for_tests() {
        let prior = env::var_os("NIRI_PUNTO_SOCKET");
        unsafe {
            env::set_var("NIRI_PUNTO_SOCKET", "/tmp/niri-punto-test-override.sock");
        }
        assert_eq!(
            socket_path(),
            PathBuf::from("/tmp/niri-punto-test-override.sock")
        );
        unsafe {
            match prior {
                Some(value) => env::set_var("NIRI_PUNTO_SOCKET", value),
                None => env::remove_var("NIRI_PUNTO_SOCKET"),
            }
        }
    }

    fn test_socket(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "niri-punto-test-{}-{}.sock",
            name,
            std::process::id()
        ))
    }

    fn recv_line(stream: &PairStream) -> String {
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .expect("reply arrives");
        line.trim_end().to_string()
    }

    #[test]
    fn word_request_is_forwarded_and_answer_relayed() {
        let (client, server) = PairStream::pair().unwrap();
        let (tx, rx) = channel::<ControlRequest>();
        let thread = std::thread::spawn(move || serve_one(server, &tx));

        let mut client = client;
        client.write_all(b"convert-word\n").unwrap();
        let request = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(request.kind, ControlKind::Word);
        request
            .reply
            .send("ok erase 6 then replay".to_string())
            .unwrap();
        assert_eq!(recv_line(&client), "ok erase 6 then replay");
        thread.join().unwrap();
    }

    #[test]
    fn selection_request_is_answered_without_main_loop() {
        let (client, server) = PairStream::pair().unwrap();
        let (tx, rx) = channel::<ControlRequest>();
        let thread = std::thread::spawn(move || serve_one(server, &tx));

        let mut client = client;
        client.write_all(b"convert-selection\n").unwrap();
        assert!(recv_line(&client).starts_with("err "));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        thread.join().unwrap();
    }

    #[test]
    fn unknown_command_gets_an_error_line() {
        let (client, server) = PairStream::pair().unwrap();
        let (tx, _rx) = channel::<ControlRequest>();
        let thread = std::thread::spawn(move || serve_one(server, &tx));

        let mut client = client;
        client.write_all(b"doctor\n").unwrap();
        let reply = recv_line(&client);
        assert!(reply.starts_with("err unknown command"), "{reply}");
        thread.join().unwrap();
    }

    #[test]
    fn bind_is_live_and_send_roundtrip() {
        let path = test_socket("live");
        let _ = std::fs::remove_file(&path);
        assert!(!is_live(&path));

        let listener = bind(&path).unwrap();
        assert!(is_live(&path));
        assert!(bind(&path).is_err());

        let (tx, rx) = channel::<ControlRequest>();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                if serve_one(stream, &tx) {
                    break;
                }
            }
        });
        let relay = std::thread::spawn(move || {
            let request = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            request.reply.send("ok converted".to_string()).unwrap();
        });
        assert_eq!(send(&path, ControlKind::Word).unwrap(), "ok converted");
        relay.join().unwrap();
        thread.join().unwrap();

        let missing = test_socket("missing-10");
        assert!(send(&missing, ControlKind::Word).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stale_socket_file_is_reclaimed() {
        let path = test_socket("stale");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "stale").unwrap();
        assert!(!is_live(&path));
        let listener = bind(&path).unwrap();
        assert!(is_live(&path));
        drop(listener);
        let _ = std::fs::remove_file(&path);
    }
}

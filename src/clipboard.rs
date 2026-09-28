//! Clipboard access for the selection path via `wl-copy`/`wl-paste`.

use std::io;
use std::process::{Command, Stdio};

const INSTALL_HINT: &str = "install the wl-clipboard package";

fn missing(binary: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{binary} not found: {INSTALL_HINT}"),
    )
}

pub trait Clipboard {
    fn read_selection(&mut self) -> io::Result<String>;
    fn write_selection(&mut self, text: &str) -> io::Result<()>;
}

/// Production clipboard over `wl-paste`/`wl-copy`.
pub struct WlClipboard;

impl Clipboard for WlClipboard {
    fn read_selection(&mut self) -> io::Result<String> {
        read_with("wl-paste", &["--primary", "--no-newline"])
    }

    fn write_selection(&mut self, text: &str) -> io::Result<()> {
        write_with("wl-copy", &[], text)
    }
}

fn read_with(binary: &str, args: &[&str]) -> io::Result<String> {
    let output = Command::new(binary).args(args).output().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            missing(binary)
        } else {
            error
        }
    })?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{binary} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        )));
    }
    String::from_utf8(output.stdout).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{binary} printed non-UTF-8: {error}"),
        )
    })
}

fn write_with(binary: &str, args: &[&str], text: &str) -> io::Result<()> {
    use std::io::Write as _;
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                missing(binary)
            } else {
                error
            }
        })?;
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        match stdin.write_all(text.as_bytes()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error),
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{binary} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_captures_stub_output() {
        let text = read_with("sh", &["-c", "printf 'hi'"]).unwrap();
        assert_eq!(text, "hi");
    }

    #[test]
    fn read_reports_a_missing_binary_with_install_hint() {
        let error = read_with("/nonexistent-wl-paste-for-tests", &[]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let message = error.to_string();
        assert!(
            message.contains("/nonexistent-wl-paste-for-tests"),
            "{message}"
        );
        assert!(message.contains("wl-clipboard"), "{message}");
    }

    #[test]
    fn read_reports_a_failing_binary() {
        let error = read_with("sh", &["-c", "echo nope >&2; exit 1"]).unwrap_err();
        assert!(error.to_string().contains("nope"), "{error}");
    }

    #[test]
    fn write_pipes_to_a_stub_binary() {
        write_with("sh", &["-c", "cat >/dev/null"], "ghbdtn").unwrap();
    }

    #[test]
    fn write_reports_a_missing_binary_with_install_hint() {
        let error = write_with("/nonexistent-wl-copy-for-tests", &[], "x").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("wl-clipboard"));
    }

    #[test]
    fn write_reports_a_failing_binary() {
        let error = write_with("false", &[], "x").unwrap_err();
        assert!(error.to_string().contains("false"), "{error}");
    }
}

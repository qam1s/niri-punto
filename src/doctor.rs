//! `doctor`: one command to check devices, permissions, socket, layouts.
//!
//! The layout-correspondence analysis is pure ([`check_layouts`]) and
//! unit-tested; the gathering helpers below it are thin wrappers over the
//! filesystem, udev-rule presence, and niri IPC. Exit status is 0 when every
//! check passes, 1 otherwise.

use crate::config::{self, LayoutPair, RULE_FILE_NAME};
use crate::ipc::IpcClient;
use std::fmt;
use std::path::{Path, PathBuf};

/// Where keyboard nodes live.
pub const INPUT_DIR: &str = "/dev/input";
/// Where `setup` installs the uaccess rule.
pub const RULE_DEST: &str = "/etc/udev/rules.d/99-niri-punto.rules";

/// A layout correspondence finding between the configured pair and niri.
///
/// These are notes, never failures: with more than two niri layouts
/// conversion still works inside the pair, and a third active language is
/// transient state. Name equality is deliberately NOT checked: niri reports
/// xkb descriptive names (`English (US)` for config code `us`), so only the
/// printed index<->code table lets the user verify the order visually.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LayoutFinding {
    /// niri reports a layout count other than the pair size.
    CountMismatch { actual: usize },
    /// The current index is outside the pair (a third language active).
    CurrentOutsidePair { current: u8, count: usize },
}

impl fmt::Display for LayoutFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CountMismatch { actual } => write!(
                f,
                "niri reports {actual} layouts, config holds a pair: \
                 conversion works inside the pair only"
            ),
            Self::CurrentOutsidePair { current, count } => write!(
                f,
                "current layout {current} (of {count}) is outside the pair: \
                 conversion will refuse until you switch back"
            ),
        }
    }
}

/// Compare the configured pair against niri's layout state. Empty means
/// nothing worth mentioning; anything returned is a note (see
/// [`LayoutFinding`]).
pub fn check_layouts(_pair: &LayoutPair, names: &[String], current: u8) -> Vec<LayoutFinding> {
    let mut findings = Vec::new();
    if names.len() != 2 {
        findings.push(LayoutFinding::CountMismatch {
            actual: names.len(),
        });
    }
    if current > 1 {
        findings.push(LayoutFinding::CurrentOutsidePair {
            current,
            count: names.len(),
        });
    }
    findings
}

/// How the udev rule fares. Pure over the file content (or its absence).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RuleCheck {
    /// Rule present and grants `uaccess`.
    Ok,
    /// Rule file missing: device access needs the manual install step.
    Missing,
    /// Rule present but does not mention `uaccess` (edited? foreign?).
    NoUaccess,
}

impl fmt::Display for RuleCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => write!(f, "ok (uaccess rule installed)"),
            Self::Missing => write!(
                f,
                "MISSING: run `niri-punto setup` (or install {RULE_FILE_NAME} by hand)"
            ),
            Self::NoUaccess => write!(f, "PRESENT but no uaccess tag: reinstall the shipped rule"),
        }
    }
}

/// Assess rule content: `None` when the file could not be read.
pub fn assess_rule(content: Option<&str>) -> RuleCheck {
    match content {
        Some(text) if text.contains("uaccess") => RuleCheck::Ok,
        Some(_) => RuleCheck::NoUaccess,
        None => RuleCheck::Missing,
    }
}

/// Read the installed rule, if any.
pub fn read_rule() -> Option<String> {
    std::fs::read_to_string(RULE_DEST).ok()
}

/// One input node probe: path plus whether it opens as a keyboard.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeviceProbe {
    pub path: PathBuf,
    pub readable_keyboard: bool,
}

/// List `/dev/input/event*` nodes and probe readability. Opening is
/// read-only and never grabs. Testable with any directory: an empty or
/// missing dir yields an empty probe list.
pub fn probe_input_dir(input_dir: &Path) -> Vec<DeviceProbe> {
    let mut probes = Vec::new();
    let Ok(dir) = std::fs::read_dir(input_dir) else {
        return probes;
    };
    let mut nodes: Vec<PathBuf> = dir
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event"))
        })
        .collect();
    nodes.sort();
    for path in nodes {
        probes.push(DeviceProbe {
            readable_keyboard: is_readable_keyboard(&path),
            path,
        });
    }
    probes
}

fn is_readable_keyboard(path: &Path) -> bool {
    let Ok(device) = evdev::Device::open(path) else {
        return false;
    };
    device.supported_events().contains(evdev::EventType::KEY)
}

/// Print one `key: value` line of the checklist.
fn line(key: &str, value: impl fmt::Display) {
    println!("{key}: {value}");
}

/// Run the full checklist. Returns the process exit code.
pub fn run() -> i32 {
    let mut failed = false;

    let config_path = config::config_path();
    let pair = match config::load() {
        Ok(pair) => {
            line(
                "config",
                format!(
                    "{} (\"{}\", \"{}\")",
                    config_path.display(),
                    pair.first,
                    pair.second
                ),
            );
            Some(pair)
        }
        Err(error) => {
            failed = true;
            line("config", format!("{}: {error}", config_path.display()));
            println!("  hint: run `niri-punto setup` to write the default config");
            None
        }
    };

    let probes = probe_input_dir(Path::new(INPUT_DIR));
    let readable = probes
        .iter()
        .filter(|probe| probe.readable_keyboard)
        .count();
    if probes.is_empty() {
        failed = true;
        line("devices", format!("{INPUT_DIR}: no event nodes found"));
    } else {
        line(
            "devices",
            format!(
                "{readable} readable keyboard(s) of {} event node(s)",
                probes.len()
            ),
        );
        for probe in &probes {
            println!(
                "  {}: {}",
                probe.path.display(),
                if probe.readable_keyboard {
                    "readable"
                } else {
                    "not a readable keyboard (permission? non-key device?)"
                }
            );
        }
        if readable == 0 {
            failed = true;
            println!("  hint: missing uaccess rule or session ACL? see `permissions` below");
        }
    }

    let rule = assess_rule(read_rule().as_deref());
    if rule != RuleCheck::Ok {
        failed = true;
    }
    line("permissions", format!("{RULE_DEST}: {rule}"));

    let layouts = match std::env::var("NIRI_SOCKET") {
        Ok(socket) => {
            line("socket", format!("$NIRI_SOCKET={socket}"));
            match IpcClient::connect().and_then(|mut client| client.layouts()) {
                Ok(layouts) => {
                    line(
                        "niri layouts",
                        format!(
                            "[{}] current={}",
                            layouts.names.join(", "),
                            layouts.current_idx
                        ),
                    );
                    Some(layouts)
                }
                Err(error) => {
                    failed = true;
                    line("niri layouts", format!("query failed: {error}"));
                    None
                }
            }
        }
        Err(_) => {
            failed = true;
            line("socket", "$NIRI_SOCKET is unset: run inside niri");
            None
        }
    };

    match (pair, layouts) {
        (Some(pair), Some(layouts)) => {
            report_correspondence(&pair, &layouts.names, layouts.current_idx);
        }
        (Some(_), None) => println!("  layouts: correspondence skipped (no niri data)"),
        (None, _) => println!("  layouts: correspondence skipped (no config)"),
    }

    exit_code(failed)
}

/// Layout correspondence section: the index<->code table plus any notes.
/// Notes never fail the run; only missing data upstream does.
fn report_correspondence(pair: &LayoutPair, names: &[String], current: u8) {
    for index in 0..=1u8 {
        let configured = pair.get(index).expect("pair holds indices 0 and 1");
        let actual = names.get(index as usize).cloned().unwrap_or_default();
        println!("  index {index}: niri \"{actual}\" <-> config \"{configured}\"");
    }
    for finding in check_layouts(pair, names, current) {
        println!("  note: {finding}");
    }
}

fn exit_code(failed: bool) -> i32 {
    if failed { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    fn pair() -> LayoutPair {
        LayoutPair::new("us", "ru").unwrap()
    }

    #[test]
    fn matching_pair_has_no_findings() {
        assert!(check_layouts(&pair(), &names(&["us", "ru"]), 0).is_empty());
        assert!(check_layouts(&pair(), &names(&["us", "ru"]), 1).is_empty());
    }

    #[test]
    fn descriptive_niri_names_are_not_a_finding() {
        // niri reports xkb descriptions ("English (US)"), never the codes,
        // so order is for the user's eyes only — not a checkable fact.
        let findings = check_layouts(&pair(), &names(&["English (US)", "Russian"]), 1);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn count_mismatch_is_a_note_not_a_failure() {
        let findings = check_layouts(&pair(), &names(&["us", "ru", "de"]), 0);
        assert_eq!(findings, vec![LayoutFinding::CountMismatch { actual: 3 }]);
    }

    #[test]
    fn single_layout_is_just_a_count_note() {
        let findings = check_layouts(&pair(), &names(&["us"]), 0);
        assert_eq!(findings, vec![LayoutFinding::CountMismatch { actual: 1 }]);
    }

    #[test]
    fn current_third_language_is_reported() {
        let findings = check_layouts(&pair(), &names(&["us", "ru", "de"]), 2);
        assert!(findings.contains(&LayoutFinding::CurrentOutsidePair {
            current: 2,
            count: 3
        }));
    }

    #[test]
    fn rule_assessment() {
        assert_eq!(assess_rule(None), RuleCheck::Missing);
        assert_eq!(
            assess_rule(Some("KERNEL==\"event*\", TAG+=\"uaccess\"\n")),
            RuleCheck::Ok
        );
        assert_eq!(assess_rule(Some("# empty rule\n")), RuleCheck::NoUaccess);
    }

    #[test]
    fn empty_or_missing_input_dir_probes_nothing() {
        let dir = std::env::temp_dir().join("niri-punto-test-doctor-empty");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(probe_input_dir(&dir).is_empty());
        assert!(probe_input_dir(Path::new("/nonexistent-dir-for-tests")).is_empty());
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn finding_messages_name_the_facts() {
        let report = LayoutFinding::CurrentOutsidePair {
            current: 2,
            count: 3,
        }
        .to_string();
        assert!(report.contains('2'), "{report}");
        let report = LayoutFinding::CountMismatch { actual: 3 }.to_string();
        assert!(report.contains('3'), "{report}");
    }
}

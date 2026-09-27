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
pub const RULE_DEST: &str = "/etc/udev/rules.d/70-niri-punto.rules";
/// Where `setup` installs the uinput modules-load entry.
pub const MODULES_DEST: &str = "/etc/modules-load.d/niri-punto.conf";
/// The injection device the daemon opens.
pub const UINPUT_NODE: &str = "/dev/uinput";
/// sysfs presence of the uinput driver (missing = dead static node).
pub const UINPUT_SYSFS: &str = "/sys/class/misc/uinput";

/// A layout correspondence finding between the configured pair and niri.
///
/// Name equality is deliberately NOT checked: niri reports xkb descriptive
/// names (`English (US)` for config code `us`), so only the printed
/// index<->code table lets the user verify the order visually. What IS
/// checked — count and the active index — is reported explicitly, with the
/// pair codes and the active niri name inline, so a mismatch never passes
/// silently.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LayoutFinding {
    /// niri reports a layout count other than the pair size.
    CountMismatch { actual: usize, pair: LayoutPair },
    /// The current index is outside the pair (a third language active).
    CurrentOutsidePair {
        current: u8,
        count: usize,
        active: String,
        pair: LayoutPair,
    },
}

impl fmt::Display for LayoutFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CountMismatch { actual, pair } => write!(
                f,
                "niri reports {actual} layouts, config holds pair \
                 (\"{}\", \"{}\"): conversion works inside the pair only",
                pair.first, pair.second,
            ),
            Self::CurrentOutsidePair {
                current,
                count,
                active,
                pair,
            } => write!(
                f,
                "current layout {current} (\"{active}\", of {count}) is outside \
                 the configured pair (\"{}\", \"{}\"): a third language is \
                 active, conversion will refuse until you switch back",
                pair.first, pair.second,
            ),
        }
    }
}

/// Compare the configured pair against niri's layout state. Empty means the
/// count matches and the active index sits inside the pair; anything
/// returned is a note (see [`LayoutFinding`]).
pub fn check_layouts(pair: &LayoutPair, names: &[String], current: u8) -> Vec<LayoutFinding> {
    let mut findings = Vec::new();
    if names.len() != 2 {
        findings.push(LayoutFinding::CountMismatch {
            actual: names.len(),
            pair: pair.clone(),
        });
    }
    if current > 1 {
        findings.push(LayoutFinding::CurrentOutsidePair {
            current,
            count: names.len(),
            active: names
                .get(current as usize)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            pair: pair.clone(),
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

/// How /dev/uinput fares. Pure over the node/sysfs paths, so tests use
/// scratch dirs: opening is read-only and side-effect free.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UinputCheck {
    /// Opens read-only: driver loaded and access granted.
    Ok,
    /// Node missing entirely.
    Missing,
    /// Present but not openable. `driver_loaded` tells a missing driver
    /// (dead static node: `sudo modprobe uinput`) apart from missing
    /// access (rule/ACL: rerun `setup`).
    NotUsable { driver_loaded: bool },
}

impl fmt::Display for UinputCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => write!(f, "ok (driver loaded, readable)"),
            Self::Missing => write!(f, "MISSING: no /dev/uinput on this system"),
            Self::NotUsable {
                driver_loaded: false,
            } => write!(
                f,
                "driver not loaded (dead static node): run `sudo modprobe uinput`, \
                 or rerun `niri-punto setup` for the persistent modules-load entry"
            ),
            Self::NotUsable {
                driver_loaded: true,
            } => write!(
                f,
                "present but not readable: missing uaccess ACL? rerun `niri-punto setup`"
            ),
        }
    }
}

/// Probe the injection device without side effects.
pub fn check_uinput(node: &Path, sysfs: &Path) -> UinputCheck {
    if !node.exists() {
        return UinputCheck::Missing;
    }
    if std::fs::File::open(node).is_ok() {
        return UinputCheck::Ok;
    }
    UinputCheck::NotUsable {
        driver_loaded: sysfs.exists(),
    }
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

    let uinput = check_uinput(Path::new(UINPUT_NODE), Path::new(UINPUT_SYSFS));
    if uinput != UinputCheck::Ok {
        failed = true;
    }
    line("uinput", format!("{UINPUT_NODE}: {uinput}"));

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
/// Notes never fail the run; only missing data upstream does. The table
/// always prints, and a clean check says so explicitly — correspondence
/// never passes silently.
fn report_correspondence(pair: &LayoutPair, names: &[String], current: u8) {
    for index in 0..=1u8 {
        let configured = pair.get(index).expect("pair holds indices 0 and 1");
        let actual = names.get(index as usize).cloned().unwrap_or_default();
        println!("  index {index}: niri \"{actual}\" <-> config \"{configured}\"");
    }
    let findings = check_layouts(pair, names, current);
    if findings.is_empty() {
        println!("  layouts: count and active index sit inside the pair (order is positional)");
    }
    for finding in findings {
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
        assert_eq!(
            findings,
            vec![LayoutFinding::CountMismatch {
                actual: 3,
                pair: pair(),
            }]
        );
    }

    #[test]
    fn single_layout_is_just_a_count_note() {
        let findings = check_layouts(&pair(), &names(&["us"]), 0);
        assert_eq!(
            findings,
            vec![LayoutFinding::CountMismatch {
                actual: 1,
                pair: pair(),
            }]
        );
    }

    #[test]
    fn current_third_language_is_reported() {
        let findings = check_layouts(&pair(), &names(&["us", "ru", "de"]), 2);
        assert!(findings.contains(&LayoutFinding::CurrentOutsidePair {
            current: 2,
            count: 3,
            active: "de".to_string(),
            pair: pair(),
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
            active: "de".to_string(),
            pair: pair(),
        }
        .to_string();
        assert!(report.contains('2'), "{report}");
        assert!(report.contains("de"), "{report}");
        assert!(report.contains("\"us\""), "{report}");
        assert!(report.contains("\"ru\""), "{report}");
        assert!(report.contains("third language"), "{report}");
        let report = LayoutFinding::CountMismatch {
            actual: 3,
            pair: pair(),
        }
        .to_string();
        assert!(report.contains('3'), "{report}");
        assert!(report.contains("\"us\""), "{report}");
        assert!(report.contains("\"ru\""), "{report}");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("niri-punto-doctor-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn uinput_missing_node_is_missing() {
        let dir = scratch("uinput-missing");
        assert_eq!(
            check_uinput(&dir.join("uinput"), &dir.join("sysfs")),
            UinputCheck::Missing
        );
    }

    #[test]
    fn uinput_openable_node_is_ok() {
        let dir = scratch("uinput-ok");
        let node = dir.join("uinput");
        std::fs::write(&node, b"fake").unwrap();
        assert_eq!(check_uinput(&node, &dir.join("sysfs")), UinputCheck::Ok);
    }

    #[test]
    fn uinput_unreadable_node_names_driver_state() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("uinput-perms");
        let node = dir.join("uinput");
        std::fs::write(&node, b"fake").unwrap();
        let mut permissions = std::fs::metadata(&node).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&node, permissions).unwrap();
        let sysfs = dir.join("sysfs");
        assert_eq!(
            check_uinput(&node, &sysfs),
            UinputCheck::NotUsable {
                driver_loaded: false
            }
        );
        std::fs::create_dir_all(&sysfs).unwrap();
        assert_eq!(
            check_uinput(&node, &sysfs),
            UinputCheck::NotUsable {
                driver_loaded: true
            }
        );
        let report = UinputCheck::NotUsable {
            driver_loaded: false,
        }
        .to_string();
        assert!(report.contains("modprobe"), "{report}");
    }
}

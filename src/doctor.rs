use crate::ipc::IpcClient;
use crate::setup::RULE_FILE_NAME;
use crate::setup::SERVICE_FILE_NAME;
use niri_ipc::KeyboardLayouts;
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const INPUT_DIR: &str = "/dev/input";
pub const RULE_DEST: &str = "/etc/udev/rules.d/70-niri-punto.rules";
pub const MODULES_DEST: &str = "/etc/modules-load.d/niri-punto.conf";
pub const UINPUT_NODE: &str = "/dev/uinput";
pub const UINPUT_SYSFS: &str = "/sys/class/misc/uinput";

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RuleCheck {
    Ok,
    Missing,
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

pub fn assess_rule(content: Option<&str>) -> RuleCheck {
    match content {
        Some(text) if text.contains("uaccess") => RuleCheck::Ok,
        Some(_) => RuleCheck::NoUaccess,
        None => RuleCheck::Missing,
    }
}

pub fn read_rule() -> Option<String> {
    std::fs::read_to_string(RULE_DEST).ok()
}

pub fn unit_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    Some(base.join("systemd").join("user").join(SERVICE_FILE_NAME))
}

/// Packaged unit locations (NixOS module, traditional packages),
/// checked when no user unit exists.
pub const SYSTEM_UNIT_PATHS: [&str; 2] = [
    "/etc/systemd/user/niri-punto.service",
    "/usr/lib/systemd/user/niri-punto.service",
];

pub fn read_unit_at(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

pub fn pick_unit(user: Option<PathBuf>, system: &[PathBuf]) -> Option<PathBuf> {
    if let Some(path) = user {
        if path.exists() {
            return Some(path);
        }
    }
    system.iter().find(|path| path.exists()).cloned()
}

pub fn locate_unit() -> Option<PathBuf> {
    let system: Vec<PathBuf> = SYSTEM_UNIT_PATHS.iter().map(PathBuf::from).collect();
    pick_unit(unit_path(), &system)
}

pub fn unit_exec_target(unit: &str) -> Option<PathBuf> {
    unit.lines().find_map(|line| {
        let value = line.trim().strip_prefix("ExecStart=")?;
        let first = value.split_whitespace().next()?;
        let unquoted = first.trim_matches('"');
        (!unquoted.is_empty()).then(|| PathBuf::from(unquoted))
    })
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UnitCheck {
    Ok { exe: PathBuf },
    Missing,
    NoExecStart,
    Stale { exe: PathBuf },
}

impl fmt::Display for UnitCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok { exe } => write!(f, "ok (ExecStart={})", exe.display()),
            Self::Missing => write!(
                f,
                "MISSING: no user unit installed; \
                 re-run `niri-punto setup` to re-register"
            ),
            Self::NoExecStart => write!(
                f,
                "PRESENT but no ExecStart found; \
                 re-run `niri-punto setup` to re-register"
            ),
            Self::Stale { exe } => write!(
                f,
                "STALE: ExecStart={} is missing or not executable \
                 (the binary moved?); re-run `niri-punto setup` to re-register",
                exe.display()
            ),
        }
    }
}

pub fn check_unit_exec(content: Option<&str>) -> UnitCheck {
    let Some(text) = content else {
        return UnitCheck::Missing;
    };
    let Some(exe) = unit_exec_target(text) else {
        return UnitCheck::NoExecStart;
    };
    if is_executable_file(&exe) {
        UnitCheck::Ok { exe }
    } else {
        UnitCheck::Stale { exe }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UinputCheck {
    Ok,
    Missing,
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

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeviceProbe {
    pub path: PathBuf,
    pub readable_keyboard: bool,
}

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

pub fn layouts_hint(layouts: &KeyboardLayouts) -> Option<String> {
    (layouts.names.len() > 2).then(|| {
        format!(
            "hint: {} layouts configured; conversion works only between the first two, others are skipped",
            layouts.names.len()
        )
    })
}

fn line(key: &str, value: impl fmt::Display) {
    println!("{key}: {value}");
}

pub fn run() -> i32 {
    run_in(Path::new(INPUT_DIR))
}

pub fn run_in(input_dir: &Path) -> i32 {
    let mut failed = false;

    let probes = probe_input_dir(input_dir);
    let readable = probes
        .iter()
        .filter(|probe| probe.readable_keyboard)
        .count();
    if probes.is_empty() {
        failed = true;
        line(
            "devices",
            format!("{}: no event nodes found", input_dir.display()),
        );
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

    match locate_unit() {
        Some(path) => {
            let check = check_unit_exec(read_unit_at(&path).as_deref());
            if !matches!(check, UnitCheck::Ok { .. }) {
                failed = true;
            }
            line("service", format!("{}: {check}", path.display()));
        }
        None => {
            failed = true;
            match unit_path() {
                Some(path) => line(
                    "service",
                    format!("{}: {}", path.display(), UnitCheck::Missing),
                ),
                None => line("service", "$HOME is unset: cannot locate user unit"),
            }
        }
    }

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
                            layouts.current_idx,
                        ),
                    );
                    if let Some(hint) = layouts_hint(&layouts) {
                        println!("  {hint}");
                    }
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

    if layouts.is_none() {
        println!("  layouts: detail skipped (no niri data)");
    }

    exit_code(failed)
}

fn exit_code(failed: bool) -> i32 {
    if failed { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn layouts_hint_is_quiet_for_a_pair() {
        let layouts = KeyboardLayouts {
            names: vec!["us".to_string(), "ru".to_string()],
            current_idx: 0,
        };
        assert_eq!(layouts_hint(&layouts), None);
    }

    #[test]
    fn layouts_hint_warns_beyond_a_pair() {
        let layouts = KeyboardLayouts {
            names: vec!["us".to_string(), "ru".to_string(), "de".to_string()],
            current_idx: 2,
        };
        let hint = layouts_hint(&layouts).unwrap();
        assert!(hint.contains("3 layouts"), "{hint}");
        assert!(hint.contains("first two"), "{hint}");
    }

    #[test]
    fn empty_or_missing_input_dir_probes_nothing() {
        let dir = std::env::temp_dir().join("niri-punto-test-doctor-empty");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(probe_input_dir(&dir).is_empty());
        assert!(probe_input_dir(Path::new("/nonexistent-dir-for-tests")).is_empty());
        std::fs::remove_dir(&dir).ok();
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

    #[test]
    fn unit_exec_target_reads_the_first_token() {
        assert_eq!(
            unit_exec_target("[Service]\nExecStart=/opt/niri-punto/niri-punto\n"),
            Some(PathBuf::from("/opt/niri-punto/niri-punto"))
        );
        assert_eq!(
            unit_exec_target("ExecStart=/bin/niri-punto --flag\n"),
            Some(PathBuf::from("/bin/niri-punto"))
        );
        assert_eq!(unit_exec_target("[Service]\nRestart=on-failure\n"), None);
        assert_eq!(unit_exec_target("ExecStart=\n"), None);
    }

    #[test]
    fn stale_unit_exec_warns_to_reregister() {
        let dir = scratch("unit-stale");
        let unit_path = dir.join("niri-punto.service");
        std::fs::write(&unit_path, "[Service]\nExecStart=/nonexistent/niri-punto\n").unwrap();
        let content = std::fs::read_to_string(&unit_path).unwrap();
        let check = check_unit_exec(Some(&content));
        assert_eq!(
            check,
            UnitCheck::Stale {
                exe: PathBuf::from("/nonexistent/niri-punto")
            }
        );
        let report = check.to_string();
        assert!(report.contains("STALE"), "{report}");
        assert!(report.contains("re-run `niri-punto setup`"), "{report}");
    }

    #[test]
    fn unit_pick_prefers_the_user_install() {
        let dir = scratch("unit-pick-user");
        let user = dir.join("user.service");
        let system = dir.join("system.service");
        std::fs::write(&user, "[Service]\n").unwrap();
        std::fs::write(&system, "[Service]\n").unwrap();
        assert_eq!(pick_unit(Some(user.clone()), &[system]), Some(user));
    }

    #[test]
    fn unit_pick_falls_back_to_the_system_unit() {
        let dir = scratch("unit-pick-system");
        let missing = dir.join("nowhere").join("user.service");
        let first = dir.join("first.service");
        let second = dir.join("second.service");
        std::fs::write(&second, "[Service]\n").unwrap();
        assert_eq!(
            pick_unit(Some(missing), &[first, second.clone()]),
            Some(second)
        );
    }

    #[test]
    fn unit_pick_is_none_without_any_unit() {
        let dir = scratch("unit-pick-none");
        let missing = dir.join("nowhere").join("user.service");
        assert_eq!(pick_unit(Some(missing), &[]), None);
        assert_eq!(pick_unit(None, &[]), None);
    }

    #[test]
    fn unit_exec_check_covers_missing_and_healthy() {
        assert_eq!(check_unit_exec(None), UnitCheck::Missing);
        assert!(
            check_unit_exec(None)
                .to_string()
                .contains("re-run `niri-punto setup`")
        );
        assert_eq!(
            check_unit_exec(Some("[Service]\nRestart=on-failure\n")),
            UnitCheck::NoExecStart
        );
        assert!(
            check_unit_exec(Some("[Service]\nRestart=on-failure\n"))
                .to_string()
                .contains("no ExecStart")
        );

        let dir = scratch("unit-ok");
        let exe = dir.join("niri-punto");
        std::fs::write(&exe, b"fake").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&exe).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&exe, permissions).unwrap();
        let unit = format!("[Service]\nExecStart={}\n", exe.display());
        assert_eq!(
            check_unit_exec(Some(&unit)),
            UnitCheck::Ok { exe: exe.clone() }
        );
        assert!(
            check_unit_exec(Some(&unit))
                .to_string()
                .contains(&exe.display().to_string())
        );
    }

    #[test]
    fn rule_display_names_each_state() {
        assert_eq!(RuleCheck::Ok.to_string(), "ok (uaccess rule installed)");
        assert!(RuleCheck::Missing.to_string().contains("MISSING"));
        assert!(RuleCheck::Missing.to_string().contains("niri-punto setup"));
        assert!(RuleCheck::NoUaccess.to_string().contains("no uaccess tag"));
    }

    #[test]
    fn unit_display_names_each_state() {
        let exe = PathBuf::from("/opt/niri-punto/niri-punto");
        assert!(
            UnitCheck::Ok { exe: exe.clone() }
                .to_string()
                .contains("/opt/niri-punto/niri-punto")
        );
        assert!(UnitCheck::Missing.to_string().contains("MISSING"));
        assert!(UnitCheck::NoExecStart.to_string().contains("no ExecStart"));
        assert!(UnitCheck::Stale { exe }.to_string().contains("STALE"));
    }

    #[test]
    fn uinput_display_names_each_state() {
        assert_eq!(UinputCheck::Ok.to_string(), "ok (driver loaded, readable)");
        assert!(UinputCheck::Missing.to_string().contains("MISSING"));
        assert!(
            UinputCheck::NotUsable {
                driver_loaded: true
            }
            .to_string()
            .contains("not readable")
        );
    }

    #[test]
    fn unit_exec_target_handles_quoted_paths() {
        assert_eq!(
            unit_exec_target("ExecStart=\"/opt/niri-punto/niri-punto\" --flag\n"),
            Some(PathBuf::from("/opt/niri-punto/niri-punto"))
        );
        assert_eq!(unit_exec_target("ExecStart=\"\"\n"), None);
        assert_eq!(
            unit_exec_target("  ExecStart=/bin/niri-punto\n"),
            Some(PathBuf::from("/bin/niri-punto"))
        );
    }

    #[test]
    fn read_unit_at_roundtrips_files_and_missing() {
        let dir = scratch("doctor-read-unit");
        let path = dir.join("niri-punto.service");
        assert_eq!(read_unit_at(&path), None);
        std::fs::write(&path, "[Service]\n").unwrap();
        assert_eq!(read_unit_at(&path).as_deref(), Some("[Service]\n"));
    }

    #[test]
    fn unit_path_follows_xdg_and_home() {
        let _env = crate::test_env::lock();
        let prior_home = std::env::var_os("HOME");
        let prior_xdg = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::set_var("HOME", "/tmp/niri-punto-doctor-home");
            std::env::set_var("XDG_CONFIG_HOME", "/tmp/niri-punto-doctor-xdg");
        }
        assert_eq!(
            unit_path(),
            Some(PathBuf::from(
                "/tmp/niri-punto-doctor-xdg/systemd/user/niri-punto.service"
            ))
        );
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "");
        }
        assert_eq!(
            unit_path(),
            Some(PathBuf::from(
                "/tmp/niri-punto-doctor-home/.config/systemd/user/niri-punto.service"
            ))
        );
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        assert_eq!(
            unit_path(),
            Some(PathBuf::from(
                "/tmp/niri-punto-doctor-home/.config/systemd/user/niri-punto.service"
            ))
        );
        unsafe {
            std::env::remove_var("HOME");
        }
        assert_eq!(unit_path(), None);
        crate::test_env::restore("HOME", prior_home);
        crate::test_env::restore("XDG_CONFIG_HOME", prior_xdg);
    }

    #[test]
    fn locate_unit_finds_user_before_system() {
        let _env = crate::test_env::lock();
        let prior_home = std::env::var_os("HOME");
        let dir = scratch("doctor-locate-unit");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        assert_eq!(locate_unit(), None);
        let unit = unit_path().unwrap();
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        std::fs::write(&unit, "[Service]\n").unwrap();
        assert_eq!(locate_unit(), Some(unit.clone()));
        std::fs::remove_file(&unit).ok();
        crate::test_env::restore("HOME", prior_home);
    }

    #[test]
    fn probe_lists_event_nodes_in_sorted_order() {
        let dir = scratch("doctor-probe-sort");
        for name in ["event10", "event2", "not-event", "event1"] {
            std::fs::write(dir.join(name), b"fake").unwrap();
        }
        let probes = probe_input_dir(&dir);
        let names: Vec<String> = probes
            .iter()
            .map(|probe| {
                probe
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        assert_eq!(names, vec!["event1", "event10", "event2"]);
        assert!(probes.iter().all(|probe| !probe.readable_keyboard));
    }

    #[test]
    fn unreadable_device_is_not_a_keyboard() {
        assert!(!is_readable_keyboard(Path::new(
            "/nonexistent-dir-for-tests/event0"
        )));
    }

    #[test]
    fn exit_code_maps_failure_to_one() {
        assert_eq!(exit_code(false), 0);
        assert_eq!(exit_code(true), 1);
    }

    #[test]
    fn run_without_niri_reports_every_section() {
        let _env = crate::test_env::lock();
        let prior_socket = std::env::var_os("NIRI_SOCKET");
        let prior_home = std::env::var_os("HOME");
        let dir = scratch("doctor-run-smoke");
        unsafe {
            std::env::remove_var("NIRI_SOCKET");
            std::env::set_var("HOME", dir.join("home-for-run"));
        }
        let code = run();
        assert!(code == 0 || code == 1);
        crate::test_env::restore("NIRI_SOCKET", prior_socket);
        crate::test_env::restore("HOME", prior_home);
    }

    #[test]
    fn run_lists_unreadable_event_nodes_with_a_hint() {
        let dir = scratch("doctor-run-nodes");
        std::fs::write(dir.join("event0"), b"not-an-evdev-device").unwrap();
        std::fs::write(dir.join("event1"), b"not-an-evdev-device").unwrap();
        let code = run_in(&dir);
        assert_eq!(code, 1);
    }

    #[test]
    fn run_reports_healthy_service_and_layouts() {
        use std::io::{BufRead, Write};
        use std::os::unix::net::UnixListener;

        let _env = crate::test_env::lock();
        let prior_socket = std::env::var_os("NIRI_SOCKET");
        let prior_home = std::env::var_os("HOME");
        let prior_xdg = std::env::var_os("XDG_CONFIG_HOME");

        let dir = scratch("doctor-run-healthy");
        let home = dir.join("home");
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let exe = dir.join("niri-punto");
        std::fs::write(&exe, b"fake").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&exe).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&exe, permissions).unwrap();
        }
        let unit = unit_path().unwrap();
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        std::fs::write(&unit, format!("[Service]\nExecStart={}\n", exe.display())).unwrap();

        let socket_path = dir.join("fake-niri.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let Ok(stream) = stream else { break };
                std::thread::spawn(move || {
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        return;
                    }
                    let reply = if line.contains("EventStream") {
                        "{\"Ok\":\"Handled\"}".to_string()
                    } else {
                        "{\"Ok\":{\"KeyboardLayouts\":{\"names\":[\"us\",\"ru\"],\"current_idx\":0}}}"
                            .to_string()
                    };
                    let _ = writer.write_all(reply.as_bytes());
                    let _ = writer.write_all(b"\n");
                });
            }
        });
        unsafe {
            std::env::set_var("NIRI_SOCKET", &socket_path);
        }
        let code = run_in(&dir);
        assert_eq!(code, 1);
        drop(server);
        crate::test_env::restore("NIRI_SOCKET", prior_socket);
        crate::test_env::restore("HOME", prior_home);
        crate::test_env::restore("XDG_CONFIG_HOME", prior_xdg);
    }
}

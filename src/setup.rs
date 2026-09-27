//! `setup`: fresh-machine install without a package manager.
//!
//! Everything installs at user level except the udev rule: the binary goes
//! to `~/.local/bin`, the unit to `~/.config/systemd/user/`, the default
//! config to `$XDG_CONFIG_HOME/niri-punto/` (never overwriting). Only the
//! udev-rule step escalates via `sudo`; `--no-udev` skips it and prints the
//! manual command instead.
//!
//! File helpers take explicit paths and are unit-tested against temp dirs;
//! commands go through [`Runner`] so `--dry-run` and tests observe without
//! executing.

use crate::config::{self, DEFAULT_CONFIG, RULE_FILE_NAME};
use crate::doctor;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Shipped unit and rule, embedded so `setup` works from the bare binary.
/// The same files live under `contrib/` in the tarball for manual install.
pub const UNIT_SOURCE: &str = include_str!("../contrib/niri-punto.service");
pub const RULE_SOURCE: &str = include_str!("../contrib/99-niri-punto.rules");

pub const SERVICE_FILE_NAME: &str = "niri-punto.service";

/// CLI flags for `setup`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Options {
    pub no_udev: bool,
    pub dry_run: bool,
}

/// Resolved install locations.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Paths {
    /// `~/.local/bin`.
    pub bin_dir: PathBuf,
    /// `~/.config/systemd/user/niri-punto.service`.
    pub unit_path: PathBuf,
    /// `$XDG_CONFIG_HOME/niri-punto/config.kdl`.
    pub config_path: PathBuf,
    /// The running binary (copy source).
    pub exe: PathBuf,
}

/// Pure constructor over explicit homes; [`resolve`] is the env wrapper.
pub fn paths_for(home: &Path, xdg_config_home: Option<&str>, exe: PathBuf) -> Paths {
    Paths {
        bin_dir: home.join(".local").join("bin"),
        unit_path: {
            let base = match xdg_config_home {
                Some(dir) if !dir.is_empty() => PathBuf::from(dir),
                _ => home.join(".config"),
            };
            base.join("systemd").join("user").join(SERVICE_FILE_NAME)
        },
        config_path: config::config_path_for(home, xdg_config_home),
        exe,
    }
}

/// Resolve install locations from `$HOME` / `$XDG_CONFIG_HOME`.
pub fn resolve() -> io::Result<Paths> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("$HOME is unset: cannot locate user install dirs"))?;
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let exe = std::env::current_exe()?;
    Ok(paths_for(&home, xdg.as_deref(), exe))
}

/// One external command. Display renders the shell-ish form.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Command {
    pub prog: String,
    pub args: Vec<String>,
}

impl Command {
    fn new(prog: &str, args: &[&str]) -> Self {
        Self {
            prog: prog.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
        }
    }
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.prog)?;
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

/// Executes (or observes) the privileged and daemon-reload steps.
pub trait Runner {
    fn run(&self, command: &Command) -> io::Result<()>;
}

/// Really spawns the command.
pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&self, command: &Command) -> io::Result<()> {
        let status = std::process::Command::new(&command.prog)
            .args(&command.args)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("command failed: {command}")))
        }
    }
}

/// Copy the running binary to `~/.local/bin`, creating the dir and
/// preserving executability.
pub fn install_binary(exe: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(exe, dest)?;
    let mut permissions = std::fs::metadata(dest)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(dest, permissions)?;
    Ok(())
}

/// Write the shipped user unit, creating the dir.
pub fn install_unit(dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, UNIT_SOURCE)?;
    Ok(())
}

/// Write the default config unless one already exists. Returns true when
/// the file was created; an existing file is never touched.
pub fn write_default_config(dest: &Path) -> io::Result<bool> {
    if dest.exists() {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, DEFAULT_CONFIG)?;
    Ok(true)
}

/// Rule shipped next to the binary in the tarball layout, if present.
pub fn shipped_rule_src(exe: &Path) -> Option<PathBuf> {
    let candidate = exe.parent()?.join("contrib").join(RULE_FILE_NAME);
    candidate.is_file().then_some(candidate)
}

/// Stage the embedded rule text so `sudo install` (or the manual command)
/// has a source path even outside the tarball layout.
pub fn stage_rule(dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(RULE_FILE_NAME);
    std::fs::write(&path, RULE_SOURCE)?;
    Ok(path)
}

/// Resolve the rule source: shipped file wins, embedded staging is fallback.
pub fn rule_source(exe: &Path, staging_dir: &Path) -> io::Result<PathBuf> {
    if let Some(shipped) = shipped_rule_src(exe) {
        return Ok(shipped);
    }
    stage_rule(staging_dir)
}

/// User-level daemon reload + enable. No privilege involved.
pub fn user_commands() -> Vec<Command> {
    vec![
        Command::new("systemctl", &["--user", "daemon-reload"]),
        Command::new(
            "systemctl",
            &["--user", "enable", "--now", SERVICE_FILE_NAME],
        ),
    ]
}

/// The escalated udev step: install the rule, reload, trigger.
/// The trigger replays `add` (scoped to the input subsystem): logind writes
/// uaccess ACLs on add events, so already-present devices become readable
/// without a reboot or replug. A bare `trigger` (change) only retags them.
pub fn udev_commands(rule_src: &Path) -> Vec<Command> {
    let src = rule_src.to_string_lossy().to_string();
    vec![
        Command::new("sudo", &["install", "-m", "644", &src, doctor::RULE_DEST]),
        Command::new("sudo", &["udevadm", "control", "--reload-rules"]),
        Command::new(
            "sudo",
            &[
                "udevadm",
                "trigger",
                "--action=add",
                "--subsystem-match=input",
            ],
        ),
    ]
}

/// Manual fallback printed with `--no-udev`.
pub fn manual_udev_command(rule_src: &Path) -> String {
    udev_commands(rule_src)
        .iter()
        .map(|command| command.to_string())
        .collect::<Vec<_>>()
        .join(" && ")
}

/// Warn when the binary dir is not on `$PATH`.
pub fn path_warning(bin_dir: &Path) -> Option<String> {
    let on_path = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir == bin_dir));
    (!on_path).then(|| {
        format!(
            "warning: {} is not on $PATH; add it or invoke the binary by full path",
            bin_dir.display()
        )
    })
}

/// Staging dir for the embedded rule fallback.
pub fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("niri-punto-setup")
}

/// Run the install. Returns the process exit code.
pub fn run(options: Options, paths: &Paths, runner: &dyn Runner) -> i32 {
    let binary_dest = paths.bin_dir.join("niri-punto");
    if !step("binary", &binary_dest, options.dry_run, || {
        install_binary(&paths.exe, &binary_dest)
    }) {
        return 1;
    }

    if !step("unit", &paths.unit_path, options.dry_run, || {
        install_unit(&paths.unit_path)
    }) {
        return 1;
    }

    if options.dry_run {
        println!(
            "[dry-run] write default config {}",
            paths.config_path.display()
        );
    } else {
        match write_default_config(&paths.config_path) {
            Ok(true) => println!("config: wrote {}", paths.config_path.display()),
            Ok(false) => println!("config: kept existing {}", paths.config_path.display()),
            Err(error) => {
                eprintln!("config: {error}");
                return 1;
            }
        }
    }

    // Dry run resolves the rule path without staging files.
    let rule_src = if options.dry_run {
        shipped_rule_src(&paths.exe).unwrap_or_else(|| staging_dir().join(RULE_FILE_NAME))
    } else {
        match rule_source(&paths.exe, &staging_dir()) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("udev rule: {error}");
                return 1;
            }
        }
    };
    if options.no_udev {
        println!("udev rule: skipped (--no-udev); install by hand:");
        println!("  {}", manual_udev_command(&rule_src));
    } else {
        println!("udev rule: needs root; escalating just this step");
        for command in udev_commands(&rule_src) {
            println!("+ {command}");
            if let Err(error) = exec(runner, options.dry_run, &command) {
                eprintln!("udev rule: {error}");
                eprintln!("  fallback: {}", manual_udev_command(&rule_src));
                return 1;
            }
        }
    }

    for command in user_commands() {
        println!("+ {command}");
        if let Err(error) = exec(runner, options.dry_run, &command) {
            eprintln!("service: {error}");
            return 1;
        }
    }

    if let Some(warning) = path_warning(&paths.bin_dir) {
        println!("{warning}");
    }
    println!("verify: run `niri-punto doctor`");
    0
}

/// Run one external step: print on dry runs, delegate otherwise.
fn exec(runner: &dyn Runner, dry_run: bool, command: &Command) -> io::Result<()> {
    if dry_run {
        println!("[dry-run] {command}");
        return Ok(());
    }
    runner.run(command)
}

/// Print a `name: dest` line and run the file step (or its dry-run echo).
/// Returns false after reporting a failure.
fn step(name: &str, dest: &Path, dry_run: bool, install: impl FnOnce() -> io::Result<()>) -> bool {
    if dry_run {
        println!("[dry-run] install {name} -> {}", dest.display());
        return true;
    }
    match install() {
        Ok(()) => {
            println!("{name}: installed -> {}", dest.display());
            true
        }
        Err(error) => {
            eprintln!("{name}: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeRunner {
        commands: RefCell<Vec<Command>>,
    }

    impl FakeRunner {
        fn new() -> Self {
            Self {
                commands: RefCell::new(Vec::new()),
            }
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, command: &Command) -> io::Result<()> {
            self.commands.borrow_mut().push(command.clone());
            Ok(())
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("niri-punto-test-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_exe(dir: &Path) -> PathBuf {
        let exe = dir.join("niri-punto");
        std::fs::write(&exe, b"fake-binary").unwrap();
        exe
    }

    fn test_paths(tag: &str) -> (PathBuf, Paths) {
        let root = scratch(tag);
        let home = root.join("home");
        let exe = fake_exe(&root);
        let paths = paths_for(&home, Some(root.join("xcfg").to_str().unwrap()), exe);
        (root, paths)
    }

    #[test]
    fn default_config_is_valid_and_pairs_us_ru() {
        let pair = config::parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(pair.first, "us");
        assert_eq!(pair.second, "ru");
    }

    #[test]
    fn embedded_unit_is_a_graphical_user_unit_with_restart() {
        assert!(UNIT_SOURCE.contains("PartOf=graphical-session.target"));
        assert!(UNIT_SOURCE.contains("Restart=on-failure"));
        assert!(UNIT_SOURCE.contains("ExecStart=%h/.local/bin/niri-punto"));
        assert!(UNIT_SOURCE.contains("WantedBy=graphical-session.target"));
    }

    #[test]
    fn embedded_rule_grants_uaccess() {
        assert!(RULE_SOURCE.contains("uaccess"));
        assert!(RULE_SOURCE.contains("SUBSYSTEM==\"input\""));
        // The daemon injects through /dev/uinput (root-only by default,
        // tagged by no stock rule): without this line it exits on start.
        assert!(RULE_SOURCE.contains("KERNEL==\"uinput\""));
    }

    #[test]
    fn default_config_is_never_clobbered() {
        let dir = scratch("setup-config");
        let dest = dir.join("config.kdl");
        assert!(write_default_config(&dest).unwrap());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), DEFAULT_CONFIG);
        std::fs::write(&dest, "layouts \"de\" \"fr\"\n").unwrap();
        assert!(!write_default_config(&dest).unwrap());
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "layouts \"de\" \"fr\"\n"
        );
    }

    #[test]
    fn unit_install_writes_shipped_content() {
        let dir = scratch("setup-unit");
        let dest = dir.join("systemd").join("user").join(SERVICE_FILE_NAME);
        install_unit(&dest).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), UNIT_SOURCE);
    }

    #[test]
    fn binary_install_copies_and_marks_executable() {
        let dir = scratch("setup-bin");
        let exe = fake_exe(&dir);
        let dest = dir.join("bin").join("niri-punto");
        install_binary(&exe, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"fake-binary");
        assert_ne!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o111,
            0
        );
    }

    #[test]
    fn shipped_rule_wins_over_staging() {
        let dir = scratch("setup-rule");
        let exe = fake_exe(&dir);
        std::fs::create_dir_all(dir.join("contrib")).unwrap();
        let shipped = dir.join("contrib").join(RULE_FILE_NAME);
        std::fs::write(&shipped, "# shipped\n").unwrap();
        assert_eq!(rule_source(&exe, &dir.join("stage")).unwrap(), shipped);

        let dir2 = scratch("setup-rule-fallback");
        let exe2 = fake_exe(&dir2);
        let staged = rule_source(&exe2, &dir2.join("stage")).unwrap();
        assert_eq!(std::fs::read_to_string(&staged).unwrap(), RULE_SOURCE);
    }

    #[test]
    fn only_the_udev_step_needs_sudo() {
        assert!(user_commands().iter().all(|command| command.prog != "sudo"));
        let udev = udev_commands(Path::new("/tmp/99-niri-punto.rules"));
        assert!(!udev.is_empty());
        assert!(udev.iter().all(|command| command.prog == "sudo"));
        let manual = manual_udev_command(Path::new("/tmp/99-niri-punto.rules"));
        assert!(manual.contains("sudo install -m 644"));
        assert!(manual.contains(doctor::RULE_DEST));
        assert!(manual.contains("udevadm control --reload-rules"));
        // Add (not change) events are what make logind write uaccess ACLs
        // onto already-present devices (no reboot/replug needed).
        assert!(manual.contains("--action=add"));
        assert!(manual.contains("--subsystem-match=input"));
    }

    #[test]
    fn full_run_installs_user_steps_and_escalates_udev() {
        let (_root, paths) = test_paths("setup-run");
        let runner = FakeRunner::new();
        let code = run(Options::default(), &paths, &runner);
        assert_eq!(code, 0);
        assert!(paths.bin_dir.join("niri-punto").is_file());
        assert_eq!(
            std::fs::read_to_string(&paths.unit_path).unwrap(),
            UNIT_SOURCE
        );
        assert_eq!(
            std::fs::read_to_string(&paths.config_path).unwrap(),
            DEFAULT_CONFIG
        );
        let commands = runner.commands.borrow();
        assert_eq!(commands.len(), 5); // 3 udev (sudo) + 2 user systemctl
        assert!(commands[..3].iter().all(|command| command.prog == "sudo"));
        assert!(
            commands[3..]
                .iter()
                .all(|command| command.prog == "systemctl")
        );
    }

    #[test]
    fn rerun_keeps_config_and_reinstalls_the_rest() {
        let (_root, paths) = test_paths("setup-rerun");
        let runner = FakeRunner::new();
        assert_eq!(run(Options::default(), &paths, &runner), 0);
        std::fs::write(&paths.config_path, "layouts \"de\" \"fr\"\n").unwrap();
        assert_eq!(run(Options::default(), &paths, &runner), 0);
        assert_eq!(
            std::fs::read_to_string(&paths.config_path).unwrap(),
            "layouts \"de\" \"fr\"\n"
        );
    }

    #[test]
    fn no_udev_runs_no_sudo_commands() {
        let (_root, paths) = test_paths("setup-no-udev");
        let runner = FakeRunner::new();
        let options = Options {
            no_udev: true,
            dry_run: false,
        };
        assert_eq!(run(options, &paths, &runner), 0);
        let commands = runner.commands.borrow();
        assert!(commands.iter().all(|command| command.prog != "sudo"));
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn dry_run_writes_nothing_and_spawns_nothing() {
        let (_root, paths) = test_paths("setup-dry");
        let runner = FakeRunner::new();
        let options = Options {
            no_udev: false,
            dry_run: true,
        };
        assert_eq!(run(options, &paths, &runner), 0);
        assert!(!paths.bin_dir.join("niri-punto").exists());
        assert!(!paths.unit_path.exists());
        assert!(!paths.config_path.exists());
        assert!(runner.commands.borrow().is_empty());
    }
}

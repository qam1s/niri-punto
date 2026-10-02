//! `update`: refresh the cargo install, re-point the unit, restart.
//!
//! `update` installs the released crates.io build; `update-dev` installs
//! the freshest main build. Packaged installs (`/usr/bin`) belong to the
//! package manager and are refused; udev and config are `setup` territory
//! and stay untouched.

use crate::setup::{self, Command, Options, Paths, Runner};
use crate::uninstall;
use std::io;
use std::path::PathBuf;

fn cmd(prog: &str, args: &[&str]) -> Command {
    Command {
        prog: prog.to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    }
}

/// Git source of the release the install docs point at.
pub const REPO: &str = "https://github.com/qam1s/niri-punto";

/// Rebuild the binary from the released crates.io version.
pub fn install_command() -> Command {
    cmd("cargo", &["install", "niri-punto", "--locked"])
}

/// Rebuild the binary from the freshest main.
pub fn install_dev_command() -> Command {
    cmd("cargo", &["install", "--git", REPO, "--locked"])
}

/// Ask git for the current revision of the remote main branch.
pub fn ls_remote_main_command() -> Command {
    cmd("git", &["ls-remote", REPO, "refs/heads/main"])
}

/// Pick up the rewritten unit file.
pub fn reload_command() -> Command {
    cmd("systemctl", &["--user", "daemon-reload"])
}

/// Run the fresh binary now.
pub fn restart_command() -> Command {
    cmd(
        "systemctl",
        &["--user", "restart", setup::SERVICE_FILE_NAME],
    )
}

/// Binary the refreshed unit should start: the cargo copy when it exists,
/// otherwise the currently running exe.
pub fn refreshed_exe(paths: &Paths) -> PathBuf {
    uninstall::cargo_copy().unwrap_or_else(|| paths.exe.clone())
}

fn exec(runner: &dyn Runner, dry_run: bool, command: &Command) -> io::Result<()> {
    println!("+ {command}");
    if dry_run {
        return Ok(());
    }
    runner.run(command)
}

/// Run the update. Returns the process exit code.
pub fn run(options: Options, paths: &Paths, runner: &dyn Runner) -> i32 {
    run_with(options, paths, runner, install_command())
}

/// Run the dev update: same steps, but the binary comes from main's
/// freshest commit instead of the released crates.io version. Cargo
/// rebuilds git installs unconditionally, so skip it when the installed
/// dev build already sits on the current main revision.
pub fn run_dev(options: Options, paths: &Paths, runner: &dyn Runner) -> i32 {
    if !options.dry_run && !setup::is_packaged(&paths.exe) {
        if let Some(remote) = remote_main_rev(runner) {
            if installed_dev_rev().as_deref() == Some(remote.as_str()) {
                println!("niri-punto is already the latest dev build ({remote})");
                return 0;
            }
        }
    }
    run_with(options, paths, runner, install_dev_command())
}

/// Cargo home, where install metadata lives.
fn cargo_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CARGO_HOME") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo"))
}

/// Revision of the previously installed git main build, read from cargo's
/// install metadata. `None` when main was never installed from git or the
/// metadata is unreadable; the update then just reinstalls.
pub fn installed_dev_rev() -> Option<String> {
    let text = std::fs::read_to_string(cargo_home()?.join(".crates2.json")).ok()?;
    let needle = format!("git+{REPO}#");
    let rest = &text[text.find(&needle)? + needle.len()..];
    let rev = rest.get(..40)?;
    rev.chars()
        .all(|c| c.is_ascii_hexdigit())
        .then(|| rev.to_string())
}

/// Current revision of the remote main branch; `None` when git is
/// unavailable or its output is unexpected.
pub fn remote_main_rev(runner: &dyn Runner) -> Option<String> {
    let output = runner.capture(&ls_remote_main_command()).ok()?;
    let rev = output.split_whitespace().next()?;
    (rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit())).then(|| rev.to_string())
}

fn run_with(options: Options, paths: &Paths, runner: &dyn Runner, install: Command) -> i32 {
    if setup::is_packaged(&paths.exe) {
        println!(
            "packaged install detected ({}): files belong to the package, \
             use your package manager to update",
            paths.exe.display()
        );
        return 1;
    }
    if let Err(error) = exec(runner, options.dry_run, &install) {
        if error.kind() == io::ErrorKind::NotFound {
            eprintln!("update: cargo not found; install the Rust toolchain first");
        } else {
            eprintln!("update: {error}");
        }
        return 1;
    }
    let exe = refreshed_exe(paths);
    if options.dry_run {
        println!("[dry-run] unit ExecStart={}", exe.display());
    } else {
        if let Err(error) = setup::install_unit(&paths.unit_path, &exe) {
            eprintln!("unit: {error}");
            return 1;
        }
        println!("unit: ExecStart={}", exe.display());
    }
    if options.dry_run {
        println!(
            "[dry-run] write default config {}",
            paths.config_path.display()
        );
    } else {
        match setup::write_default_config(&paths.config_path) {
            Ok(true) => println!("config: wrote {}", paths.config_path.display()),
            Ok(false) => println!("config: kept existing {}", paths.config_path.display()),
            Err(error) => {
                eprintln!("config: {error}");
                return 1;
            }
        }
    }
    if options.dry_run {
        println!("[dry-run] {}", reload_command());
        println!("[dry-run] {}", restart_command());
        return 0;
    }
    if let Err(error) = runner.run(&reload_command()) {
        eprintln!("service: {error}");
        return 1;
    }
    if let Err(error) = runner.run(&restart_command()) {
        eprintln!("service: {error}");
        return 1;
    }
    println!("updated {}", exe.display());
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeRunner {
        commands: RefCell<Vec<Command>>,
        remote: Option<String>,
    }

    impl FakeRunner {
        fn new() -> Self {
            Self {
                commands: RefCell::new(Vec::new()),
                remote: None,
            }
        }

        fn with_remote(rev: &str) -> Self {
            Self {
                commands: RefCell::new(Vec::new()),
                remote: Some(rev.to_string()),
            }
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, command: &Command) -> io::Result<()> {
            self.commands.borrow_mut().push(command.clone());
            Ok(())
        }

        fn capture(&self, _command: &Command) -> io::Result<String> {
            self.remote
                .clone()
                .map(|rev| format!("{rev}\trefs/heads/main\n"))
                .ok_or_else(|| io::Error::other("no remote"))
        }
    }

    struct FailingRunner;

    impl Runner for FailingRunner {
        fn run(&self, _command: &Command) -> io::Result<()> {
            Err(io::Error::other("runner broke"))
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("niri-punto-test-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Point `CARGO_HOME` at a scratch dir for one test; restores on drop.
    /// Tests run in parallel threads, so the lock serializes env access.
    struct CargoHomeGuard {
        prior: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CargoHomeGuard {
        fn point_at(dir: &std::path::Path) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let prior = std::env::var_os("CARGO_HOME");
            unsafe {
                std::env::set_var("CARGO_HOME", dir);
            }
            Self { prior, _lock: lock }
        }

        fn empty(tag: &str) -> Self {
            Self::point_at(&scratch(tag))
        }
    }

    impl Drop for CargoHomeGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(value) => std::env::set_var("CARGO_HOME", value),
                    None => std::env::remove_var("CARGO_HOME"),
                }
            }
        }
    }

    fn installed_paths(tag: &str) -> (PathBuf, Paths) {
        let root = scratch(tag);
        let home = root.join("home");
        let xcfg = root.join("xcfg");
        let exe = root.join("niri-punto");
        std::fs::write(&exe, b"fake-binary").unwrap();
        let paths = setup::paths_for(&home, Some(xcfg.to_str().unwrap()), exe);
        std::fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
        std::fs::write(&paths.config_path, "layout \"us\" \"ru\"\n").unwrap();
        (root, paths)
    }

    #[test]
    fn install_pulls_the_released_crate() {
        let command = install_command();
        assert_eq!(command.prog, "cargo");
        assert_eq!(
            command.args,
            vec![
                "install".to_string(),
                "niri-punto".to_string(),
                "--locked".to_string(),
            ]
        );
    }

    #[test]
    fn install_dev_pulls_locked_main_from_the_repo() {
        let command = install_dev_command();
        assert_eq!(command.prog, "cargo");
        assert_eq!(
            command.args,
            vec![
                "install".to_string(),
                "--git".to_string(),
                REPO.to_string(),
                "--locked".to_string(),
            ]
        );
    }

    #[test]
    fn dev_run_reinstalls_from_main_then_restarts() {
        let (_root, paths) = installed_paths("update-dev-run");
        let cargo = scratch("update-dev-run-home");
        std::fs::create_dir_all(cargo.join("bin")).unwrap();
        let copy = cargo.join("bin").join("niri-punto");
        std::fs::write(&copy, b"cargo-binary").unwrap();
        let _env = CargoHomeGuard::point_at(&cargo);
        let runner = FakeRunner::new();
        let options = Options::default();
        assert_eq!(run_dev(options, &paths, &runner), 0);
        let unit = std::fs::read_to_string(&paths.unit_path).unwrap();
        assert!(unit.contains(&format!("ExecStart={}", copy.display())));
        let commands = runner.commands.borrow();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0], install_dev_command());
        assert_eq!(commands[1], reload_command());
        assert_eq!(commands[2], restart_command());
    }

    #[test]
    fn dev_run_refuses_packaged_installs() {
        let (_root, mut paths) = installed_paths("update-dev-packaged");
        paths.exe = PathBuf::from("/usr/bin/niri-punto");
        let _env = CargoHomeGuard::empty("update-dev-packaged-home");
        let runner = FakeRunner::new();
        assert_eq!(run_dev(Options::default(), &paths, &runner), 1);
        assert!(runner.commands.borrow().is_empty());
    }

    const REV: &str = "79eaeb4a0a7a6fdc0ca1934bb793468312ef3374";
    const OTHER_REV: &str = "0123456789abcdef0123456789abcdef01234567";

    fn crates_json(cargo: &std::path::Path, key: &str) {
        let text = format!(r#"{{"installs":{{"{key}":{{}}}}}}"#);
        std::fs::write(cargo.join(".crates2.json"), text).unwrap();
    }

    #[test]
    fn installed_dev_rev_reads_the_git_entry() {
        let cargo = scratch("update-rev-home");
        crates_json(&cargo, &format!("niri-punto 0.3.8 (git+{REPO}#{REV})"));
        let _env = CargoHomeGuard::point_at(&cargo);
        assert_eq!(installed_dev_rev().as_deref(), Some(REV));
    }

    #[test]
    fn installed_dev_rev_is_none_for_registry_installs() {
        let cargo = scratch("update-rev-registry-home");
        crates_json(
            &cargo,
            "niri-punto 0.3.8 (registry+https://github.com/rust-lang/crates.io-index)",
        );
        let _env = CargoHomeGuard::point_at(&cargo);
        assert_eq!(installed_dev_rev(), None);
    }

    #[test]
    fn installed_dev_rev_is_none_without_metadata() {
        let _env = CargoHomeGuard::empty("update-rev-missing-home");
        assert_eq!(installed_dev_rev(), None);
    }

    #[test]
    fn remote_main_rev_parses_ls_remote_output() {
        let runner = FakeRunner::with_remote(REV);
        assert_eq!(remote_main_rev(&runner).as_deref(), Some(REV));
        let runner = FakeRunner::new();
        assert_eq!(remote_main_rev(&runner), None);
    }

    #[test]
    fn dev_run_skips_install_when_main_is_unchanged() {
        let (_root, paths) = installed_paths("update-dev-skip");
        let cargo = scratch("update-dev-skip-home");
        crates_json(&cargo, &format!("niri-punto 0.3.8 (git+{REPO}#{REV})"));
        let _env = CargoHomeGuard::point_at(&cargo);
        let runner = FakeRunner::with_remote(REV);
        assert_eq!(run_dev(Options::default(), &paths, &runner), 0);
        assert!(runner.commands.borrow().is_empty());
    }

    #[test]
    fn dev_run_installs_when_main_moved() {
        let (_root, paths) = installed_paths("update-dev-moved");
        let cargo = scratch("update-dev-moved-home");
        crates_json(&cargo, &format!("niri-punto 0.3.8 (git+{REPO}#{REV})"));
        let _env = CargoHomeGuard::point_at(&cargo);
        let runner = FakeRunner::with_remote(OTHER_REV);
        assert_eq!(run_dev(Options::default(), &paths, &runner), 0);
        let commands = runner.commands.borrow();
        assert_eq!(commands[0], install_dev_command());
    }

    #[test]
    fn service_commands_reload_then_restart() {
        assert_eq!(
            reload_command().to_string(),
            "systemctl --user daemon-reload"
        );
        assert_eq!(
            restart_command().to_string(),
            "systemctl --user restart niri-punto.service"
        );
    }

    #[test]
    fn refreshed_exe_prefers_the_cargo_copy() {
        let (_root, paths) = installed_paths("update-exe");
        let cargo = scratch("update-exe-home");
        std::fs::create_dir_all(cargo.join("bin")).unwrap();
        let copy = cargo.join("bin").join("niri-punto");
        std::fs::write(&copy, b"cargo-binary").unwrap();
        let _env = CargoHomeGuard::point_at(&cargo);
        assert_eq!(refreshed_exe(&paths), copy);
    }

    #[test]
    fn refreshed_exe_falls_back_to_the_running_exe() {
        let (_root, paths) = installed_paths("update-exe-fallback");
        let _env = CargoHomeGuard::empty("update-exe-fallback-home");
        assert_eq!(refreshed_exe(&paths), paths.exe);
    }

    #[test]
    fn full_run_reinstalls_repoints_config_and_restarts() {
        let (_root, paths) = installed_paths("update-run");
        let cargo = scratch("update-run-home");
        std::fs::create_dir_all(cargo.join("bin")).unwrap();
        let copy = cargo.join("bin").join("niri-punto");
        std::fs::write(&copy, b"cargo-binary").unwrap();
        let _env = CargoHomeGuard::point_at(&cargo);
        let runner = FakeRunner::new();
        let options = Options::default();
        assert_eq!(run(options, &paths, &runner), 0);
        let unit = std::fs::read_to_string(&paths.unit_path).unwrap();
        assert!(unit.contains(&format!("ExecStart={}", copy.display())));
        let commands = runner.commands.borrow();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0], install_command());
        assert_eq!(commands[1], reload_command());
        assert_eq!(commands[2], restart_command());
    }

    #[test]
    fn dry_run_touches_nothing_and_spawns_nothing() {
        let (_root, paths) = installed_paths("update-dry");
        std::fs::remove_file(&paths.config_path).unwrap();
        let _env = CargoHomeGuard::empty("update-dry-home");
        let runner = FakeRunner::new();
        let options = Options {
            dry_run: true,
            ..Default::default()
        };
        assert_eq!(run(options, &paths, &runner), 0);
        assert!(runner.commands.borrow().is_empty());
        assert!(!paths.unit_path.exists());
        assert!(!paths.config_path.exists());
    }

    #[test]
    fn packaged_install_is_refused() {
        let (_root, mut paths) = installed_paths("update-packaged");
        paths.exe = PathBuf::from("/usr/bin/niri-punto");
        let _env = CargoHomeGuard::empty("update-packaged-home");
        let runner = FakeRunner::new();
        assert_eq!(run(Options::default(), &paths, &runner), 1);
        assert!(runner.commands.borrow().is_empty());
    }

    #[test]
    fn failing_cargo_install_exits_nonzero() {
        let (_root, paths) = installed_paths("update-cargo-fail");
        let _env = CargoHomeGuard::empty("update-cargo-fail-home");
        assert_eq!(run(Options::default(), &paths, &FailingRunner), 1);
        assert!(!paths.unit_path.exists());
    }
}

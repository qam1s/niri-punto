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

pub const REPO: &str = "https://github.com/qam1s/niri-punto";

pub fn install_command() -> Command {
    cmd("cargo", &["install", "niri-punto", "--locked"])
}

pub fn install_dev_command() -> Command {
    cmd("cargo", &["install", "--git", REPO, "--locked"])
}

pub fn ls_remote_main_command() -> Command {
    cmd("git", &["ls-remote", REPO, "refs/heads/main"])
}

pub fn reload_command() -> Command {
    cmd("systemctl", &["--user", "daemon-reload"])
}

pub fn restart_command() -> Command {
    cmd(
        "systemctl",
        &["--user", "restart", setup::SERVICE_FILE_NAME],
    )
}

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

pub fn run(options: Options, paths: &Paths, runner: &dyn Runner) -> i32 {
    run_with(options, paths, runner, install_command())
}

/// Cargo rebuilds git installs unconditionally, so skip when already on the current main revision.
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

fn cargo_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CARGO_HOME") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo"))
}

pub fn installed_dev_rev() -> Option<String> {
    let text = std::fs::read_to_string(cargo_home()?.join(".crates2.json")).ok()?;
    let needle = format!("git+{REPO}#");
    let rest = &text[text.find(&needle)? + needle.len()..];
    let rev = rest.get(..40)?;
    rev.chars()
        .all(|c| c.is_ascii_hexdigit())
        .then(|| rev.to_string())
}

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
    use crate::test_env::{CargoHomeGuard, scratch};
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

    fn installed_paths(tag: &str) -> (PathBuf, Paths) {
        let root = scratch(tag);
        let home = root.join("home");
        let xcfg = root.join("xcfg");
        let exe = root.join("niri-punto");
        std::fs::write(&exe, b"fake-binary").unwrap();
        let paths = setup::paths_for(&home, Some(xcfg.to_str().unwrap()), exe);
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
    fn full_run_reinstalls_repoints_and_restarts() {
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
        let _env = CargoHomeGuard::empty("update-dry-home");
        let runner = FakeRunner::new();
        let options = Options {
            dry_run: true,
            ..Default::default()
        };
        assert_eq!(run(options, &paths, &runner), 0);
        assert!(runner.commands.borrow().is_empty());
        assert!(!paths.unit_path.exists());
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

    #[test]
    fn missing_cargo_reports_the_toolchain_hint() {
        struct MissingCargo;
        impl Runner for MissingCargo {
            fn run(&self, command: &Command) -> io::Result<()> {
                if command.prog == "cargo" {
                    return Err(io::Error::new(io::ErrorKind::NotFound, "no cargo"));
                }
                Ok(())
            }
        }
        let (_root, paths) = installed_paths("update-cargo-missing");
        let _env = CargoHomeGuard::empty("update-cargo-missing-home");
        assert_eq!(run(Options::default(), &paths, &MissingCargo), 1);
    }

    struct FailAfterInstall {
        fail_arg: &'static str,
    }

    impl Runner for FailAfterInstall {
        fn run(&self, command: &Command) -> io::Result<()> {
            if command.prog == "systemctl" && command.args.iter().any(|arg| arg == self.fail_arg) {
                return Err(io::Error::other("service broke"));
            }
            Ok(())
        }
    }

    #[test]
    fn failing_reload_exits_nonzero() {
        let (_root, paths) = installed_paths("update-reload-fail");
        let _env = CargoHomeGuard::empty("update-reload-fail-home");
        let runner = FailAfterInstall {
            fail_arg: "daemon-reload",
        };
        assert_eq!(run(Options::default(), &paths, &runner), 1);
    }

    #[test]
    fn failing_restart_exits_nonzero() {
        let (_root, paths) = installed_paths("update-restart-fail");
        let _env = CargoHomeGuard::empty("update-restart-fail-home");
        let runner = FailAfterInstall {
            fail_arg: "restart",
        };
        assert_eq!(run(Options::default(), &paths, &runner), 1);
    }

    #[test]
    fn installed_rev_rejects_short_and_dirty_values() {
        let cargo = scratch("update-rev-short-home");
        crates_json(&cargo, &format!("niri-punto 0.3.8 (git+{REPO}#abc123)"));
        {
            let _env = CargoHomeGuard::point_at(&cargo);
            assert_eq!(installed_dev_rev(), None);
        }
        let cargo = scratch("update-rev-nonhex-home");
        crates_json(
            &cargo,
            &format!(
                "niri-punto 0.3.8 (git+{REPO}#{})",
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
            ),
        );
        {
            let _env = CargoHomeGuard::point_at(&cargo);
            assert_eq!(installed_dev_rev(), None);
        }
    }

    #[test]
    fn remote_rev_rejects_short_and_dirty_values() {
        let runner = FakeRunner::with_remote("abc123");
        assert_eq!(remote_main_rev(&runner), None);
        let runner = FakeRunner::with_remote("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz");
        assert_eq!(remote_main_rev(&runner), None);
        struct EmptyRunner;
        impl Runner for EmptyRunner {
            fn run(&self, _command: &Command) -> io::Result<()> {
                Ok(())
            }
            fn capture(&self, _command: &Command) -> io::Result<String> {
                Ok("".to_string())
            }
        }
        assert_eq!(remote_main_rev(&EmptyRunner), None);
    }

    #[test]
    fn ls_remote_command_names_main() {
        assert_eq!(
            ls_remote_main_command().to_string(),
            format!("git ls-remote {REPO} refs/heads/main")
        );
    }

    #[test]
    fn dev_dry_run_skips_rev_check_and_writes_nothing() {
        let (_root, paths) = installed_paths("update-dev-dry");
        let _env = CargoHomeGuard::empty("update-dev-dry-home");
        let runner = FakeRunner::with_remote(REV);
        let options = Options {
            dry_run: true,
            ..Default::default()
        };
        assert_eq!(run_dev(options, &paths, &runner), 0);
        assert!(runner.commands.borrow().is_empty());
        assert!(!paths.unit_path.exists());
    }

    #[test]
    fn cargo_home_falls_back_to_home_dir() {
        let _env = crate::test_env::lock();
        let prior_cargo = std::env::var_os("CARGO_HOME");
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("CARGO_HOME", "/tmp/niri-punto-update-cargo");
            std::env::set_var("HOME", "/tmp/niri-punto-update-home");
        }
        assert_eq!(
            cargo_home(),
            Some(PathBuf::from("/tmp/niri-punto-update-cargo"))
        );
        unsafe {
            std::env::remove_var("CARGO_HOME");
        }
        assert_eq!(
            cargo_home(),
            Some(PathBuf::from("/tmp/niri-punto-update-home/.cargo"))
        );
        crate::test_env::restore("CARGO_HOME", prior_cargo);
        crate::test_env::restore("HOME", prior_home);
    }

    #[test]
    fn failing_unit_repoint_exits_nonzero() {
        let root = scratch("update-unit-fail");
        let blocker = root.join("blocker");
        std::fs::write(&blocker, b"in the way").unwrap();
        let home = root.join("home");
        let exe = root.join("niri-punto");
        std::fs::write(&exe, b"fake-binary").unwrap();
        let mut paths = setup::paths_for(&home, Some(root.join("xcfg").to_str().unwrap()), exe);
        paths.unit_path = blocker.join("sub").join("niri-punto.service");
        let _env = CargoHomeGuard::empty("update-unit-fail-home");
        let runner = FakeRunner::new();
        assert_eq!(run(Options::default(), &paths, &runner), 1);
        assert_eq!(runner.commands.borrow().len(), 1);
    }
}

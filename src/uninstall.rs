use crate::doctor;
use crate::setup::{self, Command, Options, Paths, Runner};
use std::io;
use std::path::{Path, PathBuf};

fn cmd(prog: &str, args: &[&str]) -> Command {
    Command {
        prog: prog.to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    }
}

pub fn user_stop() -> Command {
    cmd(
        "systemctl",
        &["--user", "disable", "--now", setup::SERVICE_FILE_NAME],
    )
}

pub fn user_reload() -> Command {
    cmd("systemctl", &["--user", "daemon-reload"])
}

pub fn udev_commands() -> Vec<Command> {
    vec![
        cmd("sudo", &["rm", "-f", doctor::RULE_DEST]),
        cmd("sudo", &["rm", "-f", doctor::MODULES_DEST]),
        cmd("sudo", &["rm", "-f", setup::STALE_RULE_DEST]),
        cmd("sudo", &["udevadm", "control", "--reload-rules"]),
        cmd(
            "sudo",
            &[
                "udevadm",
                "trigger",
                "--action=add",
                "--subsystem-match=input",
                "--subsystem-match=misc",
            ],
        ),
    ]
}

pub const PACKAGE_NAME: &str = "niri-punto";

pub fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
}

pub fn cargo_copy() -> Option<PathBuf> {
    let copy = cargo_home()?.join("bin").join(PACKAGE_NAME);
    copy.is_file().then_some(copy)
}

pub fn cargo_command() -> Command {
    cmd("cargo", &["uninstall", PACKAGE_NAME])
}

pub fn manual_udev_command() -> String {
    udev_commands()
        .iter()
        .map(|command| command.to_string())
        .collect::<Vec<_>>()
        .join(" && ")
}

fn remove(path: &Path, label: &str, dry_run: bool) -> bool {
    if dry_run {
        println!("[dry-run] remove {label} {}", path.display());
        return true;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {
            println!("{label}: removed {}", path.display());
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            println!("{label}: already gone ({})", path.display());
            true
        }
        Err(error) => {
            eprintln!("{label}: {error}");
            false
        }
    }
}

fn exec(runner: &dyn Runner, dry_run: bool, command: &Command) -> io::Result<()> {
    println!("+ {command}");
    if dry_run {
        println!("[dry-run] {command}");
        return Ok(());
    }
    runner.run(command)
}

pub fn run(options: Options, paths: &Paths, runner: &dyn Runner) -> i32 {
    let packaged = setup::is_packaged(&paths.exe);
    if packaged {
        println!(
            "packaged install detected ({}): files belong to the package, \
             use your package manager to remove them",
            paths.exe.display()
        );
    }
    if let Err(error) = exec(runner, options.dry_run, &user_stop()) {
        eprintln!("service: {error}");
        return 1;
    }

    let binary_dest = paths.bin_dir.join("niri-punto");
    if packaged {
        println!("binary: owned by the package, skipped");
    } else if !remove(&binary_dest, "binary", options.dry_run) {
        return 1;
    }
    if packaged {
        println!("unit: owned by the package, skipped");
    } else if !remove(&paths.unit_path, "unit", options.dry_run) {
        return 1;
    }
    if let Err(error) = exec(runner, options.dry_run, &user_reload()) {
        eprintln!("service: {error}");
        return 1;
    }

    if packaged {
        println!("udev rule: owned by the package, skipped");
    } else if options.no_udev {
        println!("udev rule: skipped (--no-udev); remove by hand:");
        println!("  {}", manual_udev_command());
    } else {
        println!("udev rule: needs root; escalating just this step");
        for command in udev_commands() {
            if let Err(error) = exec(runner, options.dry_run, &command) {
                eprintln!("udev rule: {error}");
                eprintln!("  fallback: {}", manual_udev_command());
                return 1;
            }
        }
    }

    match cargo_copy() {
        None => println!("cargo: no copy installed, skipped"),
        Some(copy) => {
            println!("cargo: removing {}", copy.display());
            if let Err(error) = exec(runner, options.dry_run, &cargo_command()) {
                if error.kind() == io::ErrorKind::NotFound {
                    println!("cargo: not found, kept {}", copy.display());
                } else {
                    eprintln!("cargo: {error}");
                    eprintln!("  fallback: cargo uninstall {PACKAGE_NAME}");
                    return 1;
                }
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::{CargoHomeGuard, scratch};
    use std::cell::RefCell;
    use std::path::PathBuf;

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

    fn installed_paths(tag: &str) -> (PathBuf, Paths) {
        let root = scratch(tag);
        let home = root.join("home");
        let xcfg = root.join("xcfg");
        let exe = root.join("niri-punto");
        std::fs::write(&exe, b"fake-binary").unwrap();
        let mut paths = setup::paths_for(&home, Some(xcfg.to_str().unwrap()), exe.clone());
        std::fs::create_dir_all(paths.bin_dir.clone()).unwrap();
        std::fs::copy(&exe, paths.bin_dir.join("niri-punto")).unwrap();
        std::fs::create_dir_all(paths.unit_path.parent().unwrap()).unwrap();
        std::fs::write(&paths.unit_path, "[Unit]\n").unwrap();
        paths.exe = paths.bin_dir.join("niri-punto");
        (root, paths)
    }

    #[test]
    fn full_run_removes_user_files_and_escalates_udev() {
        let (_root, paths) = installed_paths("uninstall-run");
        let _env = CargoHomeGuard::empty("uninstall-env-run");
        let runner = FakeRunner::new();
        assert_eq!(run(Options::default(), &paths, &runner), 0);
        assert!(!paths.bin_dir.join("niri-punto").exists());
        assert!(!paths.unit_path.exists());
        let commands = runner.commands.borrow();
        assert_eq!(commands.len(), 7);
        assert_eq!(commands[0].prog, "systemctl");
        assert!(commands[0].args.contains(&"disable".to_string()));
        assert_eq!(commands[1].prog, "systemctl");
        assert!(commands[1].args.contains(&"daemon-reload".to_string()));
        assert!(commands[2..].iter().all(|command| command.prog == "sudo"));
    }

    #[test]
    fn missing_files_still_exit_zero() {
        let root = scratch("uninstall-missing");
        let _env = CargoHomeGuard::empty("uninstall-env-missing");
        let home = root.join("home");
        let paths = setup::paths_for(
            &home,
            Some(root.join("xcfg").to_str().unwrap()),
            root.join("niri-punto"),
        );
        let runner = FakeRunner::new();
        assert_eq!(run(Options::default(), &paths, &runner), 0);
    }

    #[test]
    fn packaged_run_keeps_owned_files_but_disables_the_service() {
        let (_root, paths) = installed_paths("uninstall-packaged");
        let _env = CargoHomeGuard::empty("uninstall-env-packaged");
        let mut packaged = paths;
        packaged.exe = PathBuf::from("/usr/bin/niri-punto");
        let runner = FakeRunner::new();
        let options = Options {
            no_udev: true,
            dry_run: false,
        };
        assert_eq!(run(options, &packaged, &runner), 0);
        assert!(packaged.bin_dir.join("niri-punto").exists());
        assert!(packaged.unit_path.exists());
        let commands = runner.commands.borrow();
        assert!(commands.iter().all(|command| command.prog != "sudo"));
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn no_udev_runs_no_sudo_commands() {
        let (_root, paths) = installed_paths("uninstall-no-udev");
        let _env = CargoHomeGuard::empty("uninstall-env-no-udev");
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
    fn dry_run_removes_nothing_and_spawns_nothing() {
        let (_root, paths) = installed_paths("uninstall-dry");
        let _env = CargoHomeGuard::empty("uninstall-env-dry");
        let runner = FakeRunner::new();
        let options = Options {
            no_udev: false,
            dry_run: true,
        };
        assert_eq!(run(options, &paths, &runner), 0);
        assert!(paths.bin_dir.join("niri-punto").exists());
        assert!(paths.unit_path.exists());
        assert!(runner.commands.borrow().is_empty());
    }

    #[test]
    fn cargo_copy_is_removed_through_cargo_itself() {
        let (_root, paths) = installed_paths("uninstall-cargo");
        let cargo = scratch("uninstall-cargo-home");
        std::fs::create_dir_all(cargo.join("bin")).unwrap();
        let copy = cargo.join("bin").join("niri-punto");
        std::fs::write(&copy, b"cargo-binary").unwrap();
        let _env = CargoHomeGuard::point_at(&cargo);
        let runner = FakeRunner::new();
        assert_eq!(run(Options::default(), &paths, &runner), 0);
        let commands = runner.commands.borrow();
        assert_eq!(commands.len(), 8);
        let last = commands.last().unwrap();
        assert_eq!(last.prog, "cargo");
        assert_eq!(
            last.args,
            vec!["uninstall".to_string(), "niri-punto".to_string()]
        );
    }

    struct FailingRunner {
        kind: io::ErrorKind,
    }

    impl Runner for FailingRunner {
        fn run(&self, command: &Command) -> io::Result<()> {
            if command.prog == "cargo" {
                return Err(io::Error::new(self.kind, "cargo broke"));
            }
            Ok(())
        }
    }

    #[test]
    fn missing_cargo_keeps_the_copy_and_exits_zero() {
        let (_root, paths) = installed_paths("uninstall-cargo-missing");
        let cargo = scratch("uninstall-cargo-missing-home");
        std::fs::create_dir_all(cargo.join("bin")).unwrap();
        let copy = cargo.join("bin").join("niri-punto");
        std::fs::write(&copy, b"cargo-binary").unwrap();
        let _env = CargoHomeGuard::point_at(&cargo);
        let runner = FailingRunner {
            kind: io::ErrorKind::NotFound,
        };
        assert_eq!(run(Options::default(), &paths, &runner), 0);
        assert!(copy.exists());
    }

    #[test]
    fn failing_cargo_uninstall_exits_nonzero() {
        let (_root, paths) = installed_paths("uninstall-cargo-fail");
        let cargo = scratch("uninstall-cargo-fail-home");
        std::fs::create_dir_all(cargo.join("bin")).unwrap();
        std::fs::write(cargo.join("bin").join("niri-punto"), b"cargo-binary").unwrap();
        let _env = CargoHomeGuard::point_at(&cargo);
        let runner = FailingRunner {
            kind: io::ErrorKind::Other,
        };
        assert_eq!(run(Options::default(), &paths, &runner), 1);
    }

    #[test]
    fn manual_fallback_covers_both_rule_files() {
        let manual = manual_udev_command();
        assert!(manual.contains(doctor::RULE_DEST));
        assert!(manual.contains(doctor::MODULES_DEST));
        assert!(manual.contains(setup::STALE_RULE_DEST));
    }

    #[test]
    fn service_commands_name_systemctl() {
        assert_eq!(user_stop().prog, "systemctl");
        assert!(user_stop().args.contains(&"disable".to_string()));
        assert_eq!(user_reload().to_string(), "systemctl --user daemon-reload");
        assert_eq!(udev_commands().len(), 5);
        assert!(udev_commands().iter().all(|command| command.prog == "sudo"));
        assert_eq!(cargo_command().to_string(), "cargo uninstall niri-punto");
    }

    #[test]
    fn cargo_home_falls_back_to_home_dir() {
        let _env = crate::test_env::lock();
        let prior_cargo = std::env::var_os("CARGO_HOME");
        let prior_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("CARGO_HOME", "/tmp/niri-punto-cargo-home");
            std::env::set_var("HOME", "/tmp/niri-punto-home");
        }
        assert_eq!(
            cargo_home(),
            Some(PathBuf::from("/tmp/niri-punto-cargo-home"))
        );
        unsafe {
            std::env::remove_var("CARGO_HOME");
        }
        assert_eq!(
            cargo_home(),
            Some(PathBuf::from("/tmp/niri-punto-home/.cargo"))
        );
        unsafe {
            std::env::remove_var("HOME");
        }
        assert_eq!(cargo_copy(), None);
        crate::test_env::restore("CARGO_HOME", prior_cargo);
        crate::test_env::restore("HOME", prior_home);
    }

    #[test]
    fn cargo_copy_needs_an_existing_binary() {
        let cargo = scratch("uninstall-cargo-copy-absent");
        let _env = CargoHomeGuard::point_at(&cargo);
        assert_eq!(cargo_copy(), None);
    }

    struct FailOnRunner {
        fail_on: &'static str,
    }

    impl Runner for FailOnRunner {
        fn run(&self, command: &Command) -> io::Result<()> {
            if command.prog == self.fail_on {
                return Err(io::Error::other(format!("{} broke", command.prog)));
            }
            self_commands_push(command);
            Ok(())
        }
    }

    fn self_commands_push(_command: &Command) {}

    #[test]
    fn failing_stop_exits_nonzero() {
        let (_root, paths) = installed_paths("uninstall-stop-fail");
        let _env = CargoHomeGuard::empty("uninstall-env-stop-fail");
        let runner = FailOnRunner {
            fail_on: "systemctl",
        };
        assert_eq!(run(Options::default(), &paths, &runner), 1);
    }

    #[test]
    fn failing_udev_step_exits_nonzero() {
        let (_root, paths) = installed_paths("uninstall-udev-fail");
        let _env = CargoHomeGuard::empty("uninstall-env-udev-fail");
        let runner = FailOnRunner { fail_on: "sudo" };
        assert_eq!(run(Options::default(), &paths, &runner), 1);
    }

    #[test]
    fn unreadable_file_blocks_removal() {
        let dir = scratch("uninstall-remove-fail");
        let blocked = dir.join("blocked");
        std::fs::create_dir_all(&blocked).unwrap();
        assert!(!remove(&blocked, "unit", false));
        assert!(remove(&dir.join("missing"), "unit", false));
        assert!(remove(&blocked, "unit", true));
    }

    #[test]
    fn failing_reload_exits_nonzero() {
        struct ReloadFails {
            calls: RefCell<usize>,
        }
        impl Runner for ReloadFails {
            fn run(&self, _command: &Command) -> io::Result<()> {
                let mut calls = self.calls.borrow_mut();
                *calls += 1;
                if *calls == 2 {
                    return Err(io::Error::other("reload broke"));
                }
                Ok(())
            }
        }
        let (_root, paths) = installed_paths("uninstall-reload-fail");
        let _env = CargoHomeGuard::empty("uninstall-env-reload-fail");
        let runner = ReloadFails {
            calls: RefCell::new(0),
        };
        assert_eq!(run(Options::default(), &paths, &runner), 1);
    }

    #[test]
    fn blocked_binary_path_exits_nonzero() {
        let (_root, paths) = installed_paths("uninstall-blocked-binary");
        let _env = CargoHomeGuard::empty("uninstall-env-blocked-binary");
        let binary = paths.bin_dir.join("niri-punto");
        std::fs::remove_file(&binary).unwrap();
        std::fs::create_dir_all(&binary).unwrap();
        let runner = FakeRunner::new();
        assert_eq!(run(Options::default(), &paths, &runner), 1);
    }

    #[test]
    fn blocked_unit_path_exits_nonzero() {
        let (_root, paths) = installed_paths("uninstall-blocked-unit");
        let _env = CargoHomeGuard::empty("uninstall-env-blocked-unit");
        std::fs::remove_file(&paths.unit_path).unwrap();
        std::fs::create_dir_all(&paths.unit_path).unwrap();
        let runner = FakeRunner::new();
        assert_eq!(run(Options::default(), &paths, &runner), 1);
    }
}

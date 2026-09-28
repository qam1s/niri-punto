//! `uninstall`: remove what `setup` installed, keep the config.

use crate::doctor;
use crate::setup::{self, Command, Options, Paths, Runner};
use std::io;
use std::path::Path;

fn cmd(prog: &str, args: &[&str]) -> Command {
    Command {
        prog: prog.to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    }
}

/// Stop + disable the user unit, then forget it after the file is gone.
pub fn user_stop() -> Command {
    cmd(
        "systemctl",
        &["--user", "disable", "--now", setup::SERVICE_FILE_NAME],
    )
}

/// Reload after the unit file is removed.
pub fn user_reload() -> Command {
    cmd("systemctl", &["--user", "daemon-reload"])
}

/// The escalated cleanup of root-owned files.
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

/// Manual fallback printed with `--no-udev`.
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

/// Run the uninstall. Returns the process exit code.
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

    println!("config: kept {}", paths.config_path.display());
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
    0
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("niri-punto-test-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        std::fs::create_dir_all(paths.config_path.parent().unwrap()).unwrap();
        std::fs::write(&paths.config_path, "layout \"us\" \"ru\"\n").unwrap();
        paths.exe = paths.bin_dir.join("niri-punto");
        (root, paths)
    }

    #[test]
    fn full_run_removes_user_files_and_escalates_udev() {
        let (_root, paths) = installed_paths("uninstall-run");
        let runner = FakeRunner::new();
        assert_eq!(run(Options::default(), &paths, &runner), 0);
        assert!(!paths.bin_dir.join("niri-punto").exists());
        assert!(!paths.unit_path.exists());
        assert!(paths.config_path.exists());
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
    fn manual_fallback_covers_both_rule_files() {
        let manual = manual_udev_command();
        assert!(manual.contains(doctor::RULE_DEST));
        assert!(manual.contains(doctor::MODULES_DEST));
        assert!(manual.contains(setup::STALE_RULE_DEST));
    }
}

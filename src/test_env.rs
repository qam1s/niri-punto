//! Tests run in parallel threads, so every test that mutates process-global
//! environment must hold the shared lock for the whole mutation. Module-local
//! locks do not serialize against other modules touching the same variable.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

pub struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
}

pub fn lock() -> EnvGuard {
    EnvGuard {
        _lock: ENV_LOCK.lock().unwrap(),
    }
}

pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niri-punto-test-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Restore a process-global variable saved with `std::env::var_os`.
/// Callers must hold [`lock`] (directly or through a guard) across
/// the save/mutate/restore sequence.
pub fn restore(key: &str, prior: Option<std::ffi::OsString>) {
    unsafe {
        match prior {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

pub struct CargoHomeGuard {
    prior: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl CargoHomeGuard {
    pub fn point_at(dir: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap();
        let prior = std::env::var_os("CARGO_HOME");
        unsafe {
            std::env::set_var("CARGO_HOME", dir);
        }
        Self { prior, _lock: lock }
    }

    pub fn empty(tag: &str) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_handles_prior_and_missing_values() {
        let _lock = lock();
        unsafe {
            std::env::set_var("NIRI_PUNTO_RESTORE_PROBE", "present");
        }
        let prior = std::env::var_os("NIRI_PUNTO_RESTORE_PROBE");
        restore("NIRI_PUNTO_RESTORE_PROBE", prior);
        assert_eq!(
            std::env::var_os("NIRI_PUNTO_RESTORE_PROBE").as_deref(),
            Some(std::ffi::OsStr::new("present"))
        );
        unsafe {
            std::env::remove_var("NIRI_PUNTO_RESTORE_PROBE");
        }
        let prior = std::env::var_os("NIRI_PUNTO_RESTORE_PROBE");
        restore("NIRI_PUNTO_RESTORE_PROBE", prior);
        assert_eq!(std::env::var_os("NIRI_PUNTO_RESTORE_PROBE"), None);
    }

    #[test]
    fn guard_restores_missing_cargo_home() {
        let prior = {
            let _lock = lock();
            std::env::var_os("CARGO_HOME")
        };
        {
            let _lock = lock();
            unsafe {
                std::env::remove_var("CARGO_HOME");
            }
        }
        {
            let _env = CargoHomeGuard::empty("test-env-drop-none");
            assert!(std::env::var_os("CARGO_HOME").is_some());
        }
        assert_eq!(std::env::var_os("CARGO_HOME"), None);
        assert!(scratch("test-env-scratch").is_dir());
        {
            let _lock = lock();
            restore("CARGO_HOME", prior);
        }
    }
}

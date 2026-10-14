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

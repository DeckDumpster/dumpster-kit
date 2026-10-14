//! Shared scaffolding for this crate's unit tests. Compiled out of any real
//! build.

use std::path::Path;

use crate::master::KeyEnv;
use dumpster_kit_tenancy::AppPrefix;

/// The names a test app derives: `DUMPSTAR`, so every literal the old
/// crate's tests held still holds.
pub fn env() -> KeyEnv {
    KeyEnv::new(&AppPrefix::new("DUMPSTAR").unwrap())
}
use std::sync::{Mutex, MutexGuard};

/// Serialises every test that reads or writes the process environment. The
/// key file's location comes from an environment variable, and the whole
/// point of several tests below is what happens when it points at nothing —
/// which is not a thing to have two tests disagreeing about at once.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Points [`&env().key_file_var`] at `path` for the life of the
/// guard, then puts the previous value back.
pub struct EnvGuard {
    previous: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    /// Take the lock and set the variable.
    pub fn set(path: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var(&env().key_file_var).ok();
        unsafe { std::env::set_var(&env().key_file_var, path) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => unsafe { std::env::set_var(&env().key_file_var, v) },
            None => unsafe { std::env::remove_var(&env().key_file_var) },
        }
    }
}

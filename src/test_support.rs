use std::sync::{Mutex, MutexGuard};

/// Environment variables are process-global, so any test that overrides one
/// (`HOME` to point `dirs::home_dir()` at a temp dir, `GITHUB_CLIENT_ID` to
/// configure OAuth) must serialize against every *other* test doing the
/// same — not just other tests in the same module. A single shared lock,
/// used by every module's tests, is what makes that true; per-module locks
/// look safe in isolation but let tests in different modules race on the
/// same env vars when run in parallel.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Sets env vars for the duration of a test, restoring the previous values
/// (or absence) on drop. Holds the global env lock the whole time.
pub struct EnvVarsGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Vec<(String, Option<String>)>,
}

impl EnvVarsGuard {
    pub fn set(vars: &[(&str, &str)]) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = vars
            .iter()
            .map(|(name, value)| {
                let old = std::env::var(name).ok();
                unsafe { std::env::set_var(name, value) };
                (name.to_string(), old)
            })
            .collect();
        EnvVarsGuard {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for EnvVarsGuard {
    fn drop(&mut self) {
        for (name, old) in &self.previous {
            match old {
                Some(v) => unsafe { std::env::set_var(name, v) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

pub struct HomeEnvGuard(#[allow(dead_code)] EnvVarsGuard);

impl HomeEnvGuard {
    pub fn set(path: &std::path::Path) -> Self {
        HomeEnvGuard(EnvVarsGuard::set(&[("HOME", path.to_str().unwrap())]))
    }
}

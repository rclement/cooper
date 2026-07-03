use std::sync::Mutex;

/// `HOME` is a process-global env var, so any test that overrides it (to
/// point `dirs::home_dir()` at a temp dir) must serialize against every
/// *other* test doing the same — not just other tests in the same module.
/// A single shared lock, used by every module's tests, is what makes that
/// true; per-module locks look safe in isolation but let tests in different
/// modules race on the same env var when run in parallel.
static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

pub struct HomeEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<String>,
}

impl HomeEnvGuard {
    pub fn set(path: &std::path::Path) -> Self {
        let lock = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", path) };
        HomeEnvGuard {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

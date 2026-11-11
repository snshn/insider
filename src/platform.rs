//! Picks the hardware prober for the OS insider runs on.
//!
//! All probers are plain std code (files and spawned tools), so every one of
//! them builds everywhere. `INSIDER_PLATFORM=linux|macos|windows` overrides
//! the choice, which together with `INSIDER_SYSROOT` lets you replay a capture
//! from one OS on another.

use crate::model::Snapshot;
use crate::{probe, probe_macos, probe_windows};

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub use probe::Options;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

pub fn os() -> Os {
    match std::env::var("INSIDER_PLATFORM")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "linux" => Os::Linux,
        "macos" | "mac" | "darwin" => Os::Macos,
        "windows" | "win" => Os::Windows,
        _ if cfg!(target_os = "macos") => Os::Macos,
        _ if cfg!(windows) => Os::Windows,
        _ => Os::Linux,
    }
}

/// How to get the extra details that need elevated rights.
pub fn elevate_hint() -> &'static str {
    match os() {
        Os::Linux => "More detail (RAM timings, CPU power): run with sudo.",
        Os::Macos => "More detail (temperatures, fans, clocks, power): run with sudo.",
        Os::Windows => "More detail (temperatures): run as Administrator.",
    }
}

/// A slow-to-compute value (e.g. `system_profiler` output) kept fresh on a
/// background thread so `gather()` never waits for it.
pub struct Refresh<T> {
    cur: Arc<Mutex<(Arc<T>, u64)>>,
}

impl<T: Send + Sync + 'static> Refresh<T> {
    /// Starts with `initial`; `f` runs right away in the background and then
    /// every `every`, until the `Refresh` is dropped.
    pub fn new(initial: T, every: Duration, f: impl Fn() -> T + Send + 'static) -> Self {
        let cur = Arc::new(Mutex::new((Arc::new(initial), 0u64)));
        let weak = Arc::downgrade(&cur);
        thread::spawn(move || loop {
            let v = Arc::new(f());
            match weak.upgrade() {
                Some(c) => {
                    if let Ok(mut g) = c.lock() {
                        g.0 = v;
                        g.1 += 1;
                    }
                }
                None => break,
            }
            thread::sleep(every);
        });
        Refresh { cur }
    }

    pub fn get(&self) -> Arc<T> {
        self.cur
            .lock()
            .map(|g| g.0.clone())
            .unwrap_or_else(|e| e.into_inner().0.clone())
    }

    /// Wait (up to `max`) for the first background refresh to land.
    pub fn wait(&self, max: Duration) {
        let t = Instant::now();
        while t.elapsed() < max && self.cur.lock().map_or(true, |g| g.1 == 0) {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

pub enum Prober {
    Linux(probe::Prober),
    Macos(probe_macos::Prober),
    Windows(probe_windows::Prober),
}

impl Prober {
    pub fn new(opts: Options) -> Self {
        match os() {
            Os::Linux => Prober::Linux(probe::Prober::new(opts)),
            Os::Macos => Prober::Macos(probe_macos::Prober::new(opts)),
            Os::Windows => Prober::Windows(probe_windows::Prober::new(opts)),
        }
    }

    pub fn gather(&mut self) -> Snapshot {
        match self {
            Prober::Linux(p) => p.gather(),
            Prober::Macos(p) => p.gather(),
            Prober::Windows(p) => p.gather(),
        }
    }

    /// Block until slow, background-collected data is in (for `--dump`).
    pub fn settle(&self) {
        match self {
            Prober::Linux(_) => {}
            Prober::Macos(p) => p.settle(),
            Prober::Windows(p) => p.settle(),
        }
    }
}

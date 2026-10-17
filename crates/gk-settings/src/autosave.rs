//! Debounced saver: persists a value at most every `interval`, only when its
//! serialized form changed.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;

use crate::io::fnv1a64;

pub const DEFAULT_AUTOSAVE_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct Autosave<T> {
    last_hash: Option<u64>,
    last_save: Option<Instant>,
    interval: Duration,
    _marker: std::marker::PhantomData<fn(&T)>,
}

impl<T> Default for Autosave<T> {
    fn default() -> Self {
        Self {
            last_hash: None,
            last_save: None,
            interval: DEFAULT_AUTOSAVE_INTERVAL,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: Serialize> Autosave<T> {
    /// Starts tracking `initial` as already persisted (no save until it changes).
    pub fn new(initial: &T) -> Self {
        Self {
            last_hash: hash_of(initial),
            ..Self::default()
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// True when `value` differs from what was last saved.
    pub fn is_dirty(&self, value: &T) -> bool {
        hash_of(value) != self.last_hash
    }

    /// Call every frame. Returns true if `save` ran and succeeded.
    pub fn maybe_save(&mut self, value: &T, save: impl FnOnce(&T) -> Result<()>) -> bool {
        self.maybe_save_at(Instant::now(), value, save)
    }

    /// Same as [`maybe_save`](Self::maybe_save) with an explicit clock (tests).
    pub fn maybe_save_at(
        &mut self,
        now: Instant,
        value: &T,
        save: impl FnOnce(&T) -> Result<()>,
    ) -> bool {
        if self
            .last_save
            .is_some_and(|t| now.saturating_duration_since(t) < self.interval)
        {
            return false;
        }
        let hash = hash_of(value);
        if hash == self.last_hash {
            return false;
        }
        self.run(now, hash, value, save)
    }

    /// Saves immediately if changed, ignoring the debounce (e.g. on exit).
    pub fn flush(&mut self, value: &T, save: impl FnOnce(&T) -> Result<()>) -> bool {
        let hash = hash_of(value);
        if hash == self.last_hash {
            return false;
        }
        self.run(Instant::now(), hash, value, save)
    }

    fn run(
        &mut self,
        now: Instant,
        hash: Option<u64>,
        value: &T,
        save: impl FnOnce(&T) -> Result<()>,
    ) -> bool {
        // Throttle retries on failure too.
        self.last_save = Some(now);
        match save(value) {
            Ok(()) => {
                self.last_hash = hash;
                true
            }
            Err(e) => {
                eprintln!("gk-settings: autosave failed: {e:#}");
                false
            }
        }
    }
}

fn hash_of<T: Serialize>(value: &T) -> Option<u64> {
    toml::to_string(value)
        .ok()
        .map(|s| fnv1a64(s.as_bytes()))
}

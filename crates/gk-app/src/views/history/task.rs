//! Minimal background job with a keyed result slot, for work the session
//! does not cache itself (commit comparisons, file history, ...).

use parking_lot::Mutex;
use std::sync::Arc;

type Slot<T> = Arc<Mutex<Option<Result<Arc<T>, String>>>>;

pub enum TaskState<T> {
    Idle,
    Running,
    Done(Arc<T>),
    Failed(String),
}

/// Runs at most one job per key; a new key supersedes the previous job.
pub struct Task<K, T> {
    key: Option<K>,
    ticket: Arc<Mutex<u64>>,
    slot: Slot<T>,
}

impl<K, T> Default for Task<K, T> {
    fn default() -> Self {
        Self {
            key: None,
            ticket: Arc::new(Mutex::new(0)),
            slot: Arc::new(Mutex::new(None)),
        }
    }
}

impl<K: PartialEq + Clone, T: Send + Sync + 'static> Task<K, T> {
    /// Returns the state for `key`, starting `job` on a worker thread when
    /// the key changed.
    pub fn get(
        &mut self,
        key: &K,
        ctx: &egui::Context,
        job: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) -> TaskState<T> {
        if self.key.as_ref() != Some(key) {
            self.start(key.clone(), ctx, job);
        }
        self.state()
    }

    pub fn start(
        &mut self,
        key: K,
        ctx: &egui::Context,
        job: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) {
        self.key = Some(key);
        *self.slot.lock() = None;
        let ticket = {
            let mut t = self.ticket.lock();
            *t += 1;
            *t
        };
        let (slot, tickets, ctx) = (self.slot.clone(), self.ticket.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = job().map(Arc::new).map_err(|e| format!("{e:#}"));
            if *tickets.lock() == ticket {
                *slot.lock() = Some(r);
            }
            ctx.request_repaint();
        });
    }

    pub fn state(&self) -> TaskState<T> {
        if self.key.is_none() {
            return TaskState::Idle;
        }
        match &*self.slot.lock() {
            None => TaskState::Running,
            Some(Ok(v)) => TaskState::Done(v.clone()),
            Some(Err(e)) => TaskState::Failed(e.clone()),
        }
    }
}

/// Fire-and-forget work whose result is consumed once by the UI (e.g. a
/// message to copy to the clipboard).
pub struct Oneshot<T> {
    slot: Arc<Mutex<Option<T>>>,
}

impl<T> Default for Oneshot<T> {
    fn default() -> Self {
        Self {
            slot: Arc::new(Mutex::new(None)),
        }
    }
}

impl<T: Send + 'static> Oneshot<T> {
    pub fn spawn(&self, ctx: &egui::Context, job: impl FnOnce() -> Option<T> + Send + 'static) {
        let (slot, ctx) = (self.slot.clone(), ctx.clone());
        std::thread::spawn(move || {
            if let Some(v) = job() {
                *slot.lock() = Some(v);
                ctx.request_repaint();
            }
        });
    }

    pub fn take(&self) -> Option<T> {
        self.slot.lock().take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait<K: PartialEq + Clone, T: Send + Sync + 'static>(t: &Task<K, T>) -> TaskState<T> {
        let start = Instant::now();
        loop {
            match t.state() {
                TaskState::Running if start.elapsed() < Duration::from_secs(5) => {
                    std::thread::yield_now()
                }
                s => return s,
            }
        }
    }

    #[test]
    fn newer_key_wins() {
        let ctx = egui::Context::default();
        let mut t: Task<u32, u32> = Task::default();
        assert!(matches!(t.state(), TaskState::Idle));
        let _ = t.get(&1, &ctx, || {
            std::thread::sleep(Duration::from_millis(50));
            Ok(1)
        });
        let _ = t.get(&2, &ctx, || Ok(2));
        assert!(matches!(wait(&t), TaskState::Done(v) if *v == 2));
        std::thread::sleep(Duration::from_millis(80));
        assert!(matches!(t.state(), TaskState::Done(v) if *v == 2));
        let _ = t.get(&3, &ctx, || anyhow::bail!("nope"));
        assert!(matches!(wait(&t), TaskState::Failed(e) if e == "nope"));
    }
}

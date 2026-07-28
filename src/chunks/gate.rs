//! The two pieces of synchronisation every chunk fetch goes through.
//!
//! A [`Semaphore`] bounds how many requests ArenaNet ever sees from us at once,
//! whatever the client has in flight. A [`Slot`] is how several readers that
//! want the same chunk share one download instead of racing for it.
//!
//! Both are a mutex and a condvar and nothing else. The store's threads block
//! on network I/O for tens of milliseconds at a time, so what parking one costs
//! is far below anything that shows up in a measurement, and neither an async
//! runtime nor a lock-free queue has to exist for them.

use std::sync::{Arc, Condvar, Mutex};

use crate::error::{Error, Result};

/// A chunk fetch that several readers may be waiting on.
pub(super) struct Slot {
    state: Mutex<Option<std::result::Result<Arc<Vec<u8>>, String>>>,
    ready: Condvar,
}

impl Slot {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    pub(super) fn fulfil(&self, result: std::result::Result<Arc<Vec<u8>>, String>) {
        *self.state.lock().unwrap() = Some(result);
        self.ready.notify_all();
    }

    pub(super) fn wait(&self) -> Result<Arc<Vec<u8>>> {
        let mut state = self.state.lock().unwrap();
        while state.is_none() {
            state = self.ready.wait(state).unwrap();
        }
        match state.as_ref().expect("filled") {
            Ok(bytes) => Ok(Arc::clone(bytes)),
            Err(detail) => Err(Error::Transport {
                url: "chunk".into(),
                detail: detail.clone(),
            }),
        }
    }
}

pub(super) struct Semaphore {
    available: Mutex<usize>,
    released: Condvar,
}

impl Semaphore {
    pub(super) fn new(count: usize) -> Self {
        Self {
            available: Mutex::new(count),
            released: Condvar::new(),
        }
    }

    pub(super) fn acquire(&self) -> Permit<'_> {
        let mut available = self.available.lock().unwrap();
        while *available == 0 {
            available = self.released.wait(available).unwrap();
        }
        *available -= 1;
        Permit { semaphore: self }
    }

    /// How many permits nobody is holding. Only the tests ask — the store's own
    /// code has no business acting on a count that is stale the moment it reads
    /// it.
    #[cfg(test)]
    pub(super) fn free(&self) -> usize {
        *self.available.lock().unwrap()
    }
}

pub(super) struct Permit<'a> {
    semaphore: &'a Semaphore,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.semaphore.available.lock().unwrap() += 1;
        self.semaphore.released.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn semaphore_bounds_concurrency() {
        let sem = Arc::new(Semaphore::new(2));
        let peak = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(AtomicUsize::new(0));

        std::thread::scope(|scope| {
            for _ in 0..16 {
                let (sem, peak, live) = (Arc::clone(&sem), Arc::clone(&peak), Arc::clone(&live));
                scope.spawn(move || {
                    let _permit = sem.acquire();
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    live.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn slot_wakes_every_waiter() {
        let slot = Arc::new(Slot::new());
        let payload = Arc::new(vec![7u8; 4]);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let slot = Arc::clone(&slot);
                scope.spawn(move || assert_eq!(*slot.wait().unwrap(), vec![7u8; 4]));
            }
            slot.fulfil(Ok(payload));
        });
    }

    #[test]
    fn slot_propagates_failure() {
        let slot = Slot::new();
        slot.fulfil(Err("boom".into()));
        assert!(slot.wait().is_err());
    }
}

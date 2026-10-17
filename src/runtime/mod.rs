//! glib⇄tokio bridge. A tokio runtime runs beside the glib main loop; UI handlers
//! spawn `innertube` futures and receive results back on the glib thread via
//! `async-channel` + `glib::spawn_future_local`. `Generation` implements the
//! stale-result drop: bump it per new request, capture the value at spawn, and
//! discard any result whose captured value is not the latest.

use std::cell::Cell;
use std::rc::Rc;

/// Monotonic request counter, one per logically-superseding action (e.g. "search",
/// "resolve"). Cloneable handle over shared state; lives on the glib (main) thread.
#[derive(Clone, Default)]
pub struct Generation(Rc<Cell<u64>>);

impl Generation {
    /// Bump to start a new request; returns the token the caller must present later.
    pub fn bump(&self) -> u64 {
        let next = self.0.get() + 1;
        self.0.set(next);
        next
    }

    /// The latest issued token without bumping. Capture it to detect a later supersession
    /// (e.g. a load-more that a fresh search should invalidate).
    pub fn current(&self) -> u64 {
        self.0.get()
    }

    /// True iff `token` is still the latest bump (i.e. no newer request started).
    /// Token `0` is the reserved "no request issued" sentinel (`bump` starts at 1)
    /// and is never current, so a defaulted/uninitialized token never passes.
    pub fn is_current(&self, token: u64) -> bool {
        token != 0 && self.0.get() == token
    }
}

use std::future::Future;
use tokio::runtime::Runtime;

/// Owns the tokio runtime that lives beside the glib loop. Created once in `main`
/// and shared (clone the `Rc`) into UI handlers.
#[derive(Clone)]
pub struct AsyncRuntime(Rc<Runtime>);

impl AsyncRuntime {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self(Rc::new(
            tokio::runtime::Builder::new_multi_thread().enable_all().build()?,
        )))
    }

    /// Spawn a long-lived background future on tokio (no result delivered back). Used for
    /// persistent services like the MPRIS D-Bus connection.
    pub fn spawn_bg<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.0.spawn(fut);
    }

    /// Spawn `fut` on tokio and deliver its output to `on_done` on the glib main thread.
    /// Gate `on_done` on a captured `Generation` token to drop superseded results.
    pub fn spawn<F, T, G>(&self, fut: F, on_done: G)
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
        G: FnOnce(T) + 'static,
    {
        drop(self.spawn_abortable(fut, on_done)); // detach: dropping a JoinHandle doesn't abort
    }

    /// Like [`AsyncRuntime::spawn`], but returns the tokio `JoinHandle`. `abort()` drops the
    /// future mid-await and cancels its in-flight network I/O. `on_done` never runs for an
    /// aborted task (its sender drops unfired).
    pub fn spawn_abortable<F, T, G>(&self, fut: F, on_done: G) -> tokio::task::JoinHandle<()>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
        G: FnOnce(T) + 'static,
    {
        let (tx, rx) = async_channel::bounded(1);
        let handle = self.0.spawn(async move {
            let _ = tx.send(fut.await).await;
        });
        gtk4::glib::spawn_future_local(async move {
            if let Ok(value) = rx.recv().await {
                on_done(value);
            }
        });
        handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_token_is_current_older_is_stale() {
        let g = Generation::default();
        let first = g.bump();
        let second = g.bump();
        assert!(!g.is_current(first), "superseded token must be stale");
        assert!(g.is_current(second), "latest token must be current");
    }
}

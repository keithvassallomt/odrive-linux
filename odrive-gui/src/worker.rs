//! Tiny worker-thread helper for long-running agent shell-outs.
//!
//! Pattern: hand `work` to a fresh OS thread, then poll a channel from
//! the GTK main loop on a 100ms timer until the result arrives. When it
//! does, fire `on_done` on the main thread (so it can freely capture
//! GTK widgets) and stop polling.
//!
//! Why this pattern instead of glib::MainContext::spawn_local + an
//! async runtime? OdriveAgent's CLI calls are synchronous via
//! std::process::Command::output() — there's no async path through to
//! odrive itself. A worker thread + channel is the simplest unblocker
//! and adds no new dependencies. 100ms poll is well below human
//! perception of lag and well above the ~16ms frame budget, so the
//! main loop stays smooth.
use adw::gtk::glib;
use libadwaita as adw;
use std::cell::RefCell;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Run `work` on a worker thread; once it finishes, call `on_done` with
/// its result on the GTK main thread. The result type `T` must be
/// `Send` (it crosses the thread boundary), but `on_done` itself can
/// freely capture `!Send` GTK widgets — it runs on the main thread.
pub fn spawn<T, F, G>(work: F, on_done: G)
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    G: FnOnce(T) + 'static,
{
    let (tx, rx) = mpsc::sync_channel::<T>(1);
    thread::spawn(move || {
        // If the receiver was dropped (window closed during work),
        // sending fails — that's fine, just discard the result.
        let _ = tx.send(work());
    });

    // Wrap on_done in RefCell<Option<>> so the FnMut closure
    // timeout_add_local takes can `.take()` it on the firing tick.
    let on_done = RefCell::new(Some(on_done));
    glib::timeout_add_local(Duration::from_millis(100), move || {
        match rx.try_recv() {
            Ok(result) => {
                if let Some(cb) = on_done.borrow_mut().take() {
                    cb(result);
                }
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            // Sender hung up without sending (worker panicked). Stop
            // the timer; the caller never gets a callback. Acceptable
            // — the work was a one-shot CLI call and a panic in
            // OdriveAgent shell-out is a real bug that would surface
            // in the worker's stderr / agent log.
            Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        }
    });
}

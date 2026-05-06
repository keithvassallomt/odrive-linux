//! Shared `Adw.Toast` builders so every module emits feedback with the
//! same timeout and priority semantics.
//!
//! Default `Adw.Toast::new(...)` doesn't set an explicit timeout —
//! Adw applies its 5 s default. That's longer than GNOME apps usually
//! want (Files / Software dismiss in ~3 s) and made the manager feel
//! sluggish on success messages. These helpers normalise timing and
//! lift error toasts to High priority so a follow-up success toast
//! doesn't supersede them before the user has read the failure.
use libadwaita as adw;
#[allow(unused_imports)]
use adw::prelude::*; // brings ToastExt methods (set_timeout, set_priority) into scope
use adw::{Toast, ToastPriority};

const DEFAULT_TIMEOUT: u32 = 3;

/// Standard feedback toast (success / informational). 3 s timeout.
pub fn toast(msg: &str) -> Toast {
    let t = Toast::new(msg);
    t.set_timeout(DEFAULT_TIMEOUT);
    t
}

/// Error toast — 3 s timeout but marked High priority so it sits in
/// the queue ahead of any success toast that follows. The user sees
/// the failure even when the calling code emits a "completed" toast
/// in a parallel branch.
pub fn error_toast(msg: &str) -> Toast {
    let t = toast(msg);
    t.set_priority(ToastPriority::High);
    t
}

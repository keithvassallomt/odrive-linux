//! Panel indicator (StatusNotifierItem) backed by the `ksni` crate.
//!
//! Lives inside the GUI process — when the GUI quits, the icon
//! disappears. Future "daemon mode" (icon visible whether the window is
//! open or not) is out of scope for now.
//!
//! Threading: ksni runs its own background thread for D-Bus and
//! invokes our menu callbacks from there. Callbacks therefore can't
//! touch GTK widgets directly. They post events on a
//! `std::sync::mpsc` channel; a `glib::timeout_add_local` poll on the
//! GTK main loop drains the channel and runs the GTK-side action
//! (presenting the window, shelling out to xdg-open, toggling
//! agent start/stop on a worker thread, or `app.quit()`).
//!
//! Quit closes the Manager window only — the agent is independent
//! infrastructure (typically managed by the user-level systemd unit
//! the wizard installed) and must outlive the GUI.
//!
//! Pause / Resume is the heavy-handed approximation the upstream
//! forces on us: there's no `odrive pause` command, so we toggle
//! `agent.stop()` / `agent.start()`. That kills any in-flight
//! transfers — documented in CLAUDE.md alongside the rest of the
//! upstream limitations.
use crate::worker::spawn as worker_spawn;
use adw::gtk::glib;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem};
use libadwaita as adw;
use odrive_core::OdriveAgent;
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

/// One-way command from the indicator menu (ksni thread) to the GTK
/// main thread, where everything that touches GTK or interactive
/// state actually happens.
#[derive(Debug, Clone, Copy)]
enum TrayEvent {
    OpenFolder,
    OpenManager,
    TogglePause,
    Quit,
}

pub struct OdriveTray {
    /// Last known agent state. Updated from the GTK main thread on
    /// the 5s poll via `Handle::update`. Drives the menu label flip
    /// between "Pause sync" and "Resume sync".
    is_running: bool,
    /// Send half of the GTK-bound event channel.
    tx: mpsc::Sender<TrayEvent>,
    /// Pre-rendered ARGB32 pixmaps. The GNOME appindicator extension
    /// won't resolve `icon_name` reliably (especially for symbolic
    /// SVGs), so we ship raw pixel data the host can blit directly.
    /// Filled at install() time on the GTK main thread.
    pixmap: Vec<Icon>,
}

impl ksni::Tray for OdriveTray {
    fn id(&self) -> String {
        "ai.openclaw.odrive-linux".into()
    }

    fn icon_name(&self) -> String {
        // Adwaita ships this in `symbolic/places/`. Reads
        // semantically as "remote folder" — what we want for a
        // cloud-sync manager.
        "folder-remote-symbolic".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.pixmap.clone()
    }

    fn title(&self) -> String {
        if self.is_running {
            "odrive — running".into()
        } else {
            "odrive — paused".into()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let pause_label = if self.is_running { "Pause sync" } else { "Resume sync" };
        vec![
            StandardItem {
                label: "Open odrive folder".into(),
                icon_name: "folder-symbolic".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayEvent::OpenFolder);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open odrive Manager".into(),
                icon_name: "preferences-system-symbolic".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayEvent::OpenManager);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: pause_label.into(),
                icon_name: if self.is_running {
                    "media-playback-pause-symbolic".into()
                } else {
                    "media-playback-start-symbolic".into()
                },
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayEvent::TogglePause);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Render an icon-theme entry into the SNI pixmap format
/// (ARGB32, network byte order — i.e. A, R, G, B per pixel).
/// Returns an empty vec on any failure; the indicator still renders
/// from `icon_name` on hosts that handle it. Must be called from the
/// GTK main thread (gdk_pixbuf and IconTheme aren't thread-safe).
fn render_pixmap(name: &str, size: i32) -> Vec<Icon> {
    use adw::gtk::gdk_pixbuf::Pixbuf;
    use adw::gtk::prelude::*;
    use adw::gtk::{IconLookupFlags, IconTheme, TextDirection};

    let Some(display) = adw::gtk::gdk::Display::default() else {
        return Vec::new();
    };
    let theme = IconTheme::for_display(&display);
    let paintable = theme.lookup_icon(
        name,
        &[],
        size,
        1,
        TextDirection::None,
        IconLookupFlags::empty(),
    );
    let Some(file) = paintable.file() else {
        return Vec::new();
    };
    let Some(path) = file.path() else {
        return Vec::new();
    };
    let Ok(pixbuf) = Pixbuf::from_file_at_size(&path, size, size) else {
        return Vec::new();
    };

    let bytes = pixbuf.read_pixel_bytes();
    let raw: &[u8] = bytes.as_ref();
    let stride = pixbuf.rowstride() as usize;
    let width = pixbuf.width() as usize;
    let height = pixbuf.height() as usize;
    let channels = pixbuf.n_channels() as usize;

    let mut argb = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let i = y * stride + x * channels;
            let r = raw[i];
            let g = raw[i + 1];
            let b = raw[i + 2];
            let a = if channels == 4 { raw[i + 3] } else { 0xff };
            argb.extend_from_slice(&[a, r, g, b]);
        }
    }

    vec![Icon {
        width: width as i32,
        height: height as i32,
        data: argb,
    }]
}

/// Install the indicator. Spawns the ksni background thread, installs
/// the GTK-side event drain on the main loop, and starts a 5s
/// `is_running` poll that mirrors agent state into the tray label.
pub fn install(
    app: &adw::gtk::Application,
    window: &adw::ApplicationWindow,
    agent: Rc<OdriveAgent>,
) {
    let (tx, rx) = mpsc::channel::<TrayEvent>();
    let initial_running = agent.is_running();
    // 24px is the typical GNOME panel size; the host scales as needed.
    let pixmap = render_pixmap("folder-remote-symbolic", 24);

    let tray = OdriveTray {
        is_running: initial_running,
        tx,
        pixmap,
    };

    // Spawn ksni on its own thread. Returns a handle we use to push
    // state updates from the GTK thread (mainly: the 5s is_running
    // poll re-renders the menu when the agent's state changes).
    let handle: Handle<OdriveTray> = match tray.spawn() {
        Ok(h) => h,
        Err(e) => {
            eprintln!(
                "Tray indicator unavailable ({e}). On stock GNOME this needs the \
                 gnome-shell-extension-appindicator extension. The Manager works without it."
            );
            return;
        }
    };

    // Drain channel events on the GTK main thread.
    let app_for_events = app.clone();
    let window_for_events = window.clone();
    let agent_for_events = agent.clone();
    let handle_for_events = handle.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        while let Ok(event) = rx.try_recv() {
            handle_event(
                event,
                &app_for_events,
                &window_for_events,
                &agent_for_events,
                &handle_for_events,
            );
        }
        glib::ControlFlow::Continue
    });

    // Mirror agent state into the tray every 5s. Same cadence as the
    // dashboard's poll so the two surfaces never visibly diverge.
    let agent_for_poll = agent.clone();
    let handle_for_poll = handle.clone();
    glib::timeout_add_seconds_local(5, move || {
        let running = agent_for_poll.is_running();
        handle_for_poll.update(|t: &mut OdriveTray| {
            t.is_running = running;
        });
        glib::ControlFlow::Continue
    });
}

fn handle_event(
    event: TrayEvent,
    app: &adw::gtk::Application,
    window: &adw::ApplicationWindow,
    agent: &Rc<OdriveAgent>,
    handle: &Handle<OdriveTray>,
) {
    use adw::prelude::*;
    match event {
        TrayEvent::OpenFolder => {
            // First mount's local path is the canonical "odrive folder".
            // No mounts yet → quietly do nothing; the Manager window's
            // empty state covers the fix path.
            if let Ok(mounts) = agent.get_mounts() {
                if let Some(first) = mounts.first() {
                    let _ = Command::new("xdg-open").arg(&first.local_path).spawn();
                }
            }
        }
        TrayEvent::OpenManager => {
            window.present();
        }
        TrayEvent::TogglePause => {
            // start() and stop() are quick (mostly): start() may sleep
            // 2s waiting for the daemon to come up. Move them to a
            // worker so the indicator's click feedback is instant and
            // the GTK loop keeps painting if the user pokes the menu
            // again immediately after.
            let agent_clone = agent.as_ref().clone();
            let was_running = agent.is_running();
            let handle_for_done = handle.clone();
            worker_spawn(
                move || {
                    if was_running {
                        let _ = agent_clone.stop();
                    } else {
                        let _ = agent_clone.start();
                    }
                    agent_clone.is_running()
                },
                move |new_state: bool| {
                    handle_for_done.update(|t: &mut OdriveTray| {
                        t.is_running = new_state;
                    });
                },
            );
        }
        TrayEvent::Quit => {
            // Quit just closes the Manager window. The agent is
            // independent infrastructure (typically managed by the
            // user-level systemd unit the wizard installed) and must
            // outlive the GUI; calling `agent.shutdown()` here would
            // kill the daemon and stop sync until next login. To stop
            // sync explicitly the user uses Pause from this same menu.
            app.quit();
        }
    }
}

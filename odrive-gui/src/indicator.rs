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
//! Animation: while the agent is doing work
//! (`SyncActivity::is_active`) we cycle through the 16 frames bundled
//! under `odrive-icons/tray-icons/animated/<color>/` (installed by
//! `odrive-cli install-handlers` as `odrive-tray-<color>-active-<N>`).
//! A 2s activity poll detects idle↔active transitions; while active, an
//! 80ms animation timer advances the frame index and pushes the next
//! pixmap via `Handle::update`. Colours without bundled animation
//! frames (currently darkgrey/grey) stay on the static icon — we
//! detect "no animation available" by attempting to render frame 1 and
//! finding no theme entry for it.
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
use odrive_core::{OdriveAgent, OdriveConfig, DEFAULT_TRAY_ICON_COLOR, TRAY_ICON_COLORS};
use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

/// Panel-icon size in logical pixels. Standard GNOME panel cell; the
/// host scales as needed for high-DPI.
const ICON_SIZE: i32 = 24;

/// Animation tick interval. 16 frames × 80ms ≈ 1.28 s/loop.
const ANIM_TICK_MS: u64 = 80;

/// Activity-poll interval. The dashboard's 5s poll is too slow to catch
/// short refreshes (the agent often spends <2s on a folder refresh), so
/// the indicator polls at its own faster cadence. A `get_sync_activity`
/// call is a single `odrive status` shell-out, same shape as the
/// dashboard already runs.
const ACTIVITY_POLL_SECS: u32 = 2;

/// How many frames `install-handlers` deposits per animated colour.
/// Hardcoded here rather than discovered via the icon theme because
/// `IconTheme::lookup_icon` doesn't expose enumeration — we'd have to
/// probe each `<color>-active-<N>` until lookup fails. 16 is the
/// upstream asset bundle's count for every animated colour.
const ANIMATION_FRAMES: u32 = 16;

/// Resolve a stored colour string to the icon-theme name we registered
/// via `install-handlers`. Unknown values fall through to the default
/// (`pink`) — matches the config-load behaviour and means a typo in
/// `~/.config/odrive-linux/config.toml` doesn't yield an empty tray.
fn icon_name_for_color(color: &str) -> String {
    let resolved = if TRAY_ICON_COLORS.iter().any(|c| *c == color) {
        color
    } else {
        DEFAULT_TRAY_ICON_COLOR
    };
    format!("odrive-tray-{}", resolved)
}

/// One pre-rendered icon: the theme name (so `icon_name()` keeps
/// reflecting the current frame for hosts that resolve by name) and
/// the ARGB32 pixmap (for hosts that blit raw pixel data).
#[derive(Clone)]
struct RenderedIcon {
    name: String,
    pixmap: Vec<Icon>,
}

/// Cache of rendered icons for the *currently selected colour*. Lives
/// behind an `Rc<RefCell<>>` shared between the activity poll, the
/// animation tick, and the `TrayController` colour swap. All three run
/// on the GTK main thread, so RefCell is safe.
struct RenderedSet {
    /// Idle icon — what we show when no work is in flight.
    idle: RenderedIcon,
    /// Animation frames in order. Empty when this colour has no
    /// bundled animation (darkgrey/grey today); the animation timer
    /// short-circuits in that case.
    frames: Vec<RenderedIcon>,
    /// Index into `frames` for the next frame to push.
    frame_idx: usize,
}

impl RenderedSet {
    fn for_color(color: &str) -> Self {
        let idle_name = icon_name_for_color(color);
        let idle = RenderedIcon {
            pixmap: render_pixmap(&idle_name, ICON_SIZE),
            name: idle_name,
        };

        // Probe each frame name in turn. `render_pixmap` returns an
        // empty Vec when the icon-theme entry doesn't resolve, which is
        // how we distinguish "this colour has animation frames
        // installed" from "it doesn't". We bail at the first miss
        // rather than continuing past gaps — a partial install would
        // be a packaging bug, not something to paper over.
        let mut frames = Vec::with_capacity(ANIMATION_FRAMES as usize);
        for n in 1..=ANIMATION_FRAMES {
            let name = format!("{}-active-{}", icon_name_for_color(color), n);
            let pixmap = render_pixmap(&name, ICON_SIZE);
            if pixmap.is_empty() {
                frames.clear();
                break;
            }
            frames.push(RenderedIcon { name, pixmap });
        }

        Self { idle, frames, frame_idx: 0 }
    }

    fn has_animation(&self) -> bool {
        !self.frames.is_empty()
    }
}

/// Opaque handle returned from `install`. Lets the Settings page swap
/// the tray icon's colour live without depending on `ksni` types.
pub struct TrayController {
    handle: Option<Handle<OdriveTray>>,
    rendered: Rc<RefCell<RenderedSet>>,
}

impl TrayController {
    /// Re-render every icon for the new colour and push the idle frame
    /// to the running tray. If sync is currently active, the animation
    /// timer picks up the new frame set on its next tick (≤ 80 ms),
    /// so a colour change mid-animation flickers briefly through the
    /// idle frame and then resumes animating in the new colour.
    /// No-op if the indicator failed to spawn (e.g. no SNI host on the
    /// bus). Must be called from the GTK main thread because
    /// `render_pixmap` touches gdk_pixbuf.
    pub fn set_icon_color(&self, color: &str) {
        let Some(handle) = &self.handle else { return; };
        let new_set = RenderedSet::for_color(color);
        let idle = new_set.idle.clone();
        *self.rendered.borrow_mut() = new_set;
        push_icon(handle, &idle);
    }
}

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
    /// the activity poll via `Handle::update`. Drives the menu label
    /// flip between "Pause sync" and "Resume sync".
    is_running: bool,
    /// Send half of the GTK-bound event channel.
    tx: mpsc::Sender<TrayEvent>,
    /// Pre-rendered ARGB32 pixmap. The GNOME appindicator extension
    /// won't resolve `icon_name` reliably (especially for symbolic
    /// SVGs), so we ship raw pixel data the host can blit directly.
    /// Updated from the GTK main thread by the animation tick and by
    /// `TrayController::set_icon_color`.
    pixmap: Vec<Icon>,
    /// The icon-theme name backing the current pixmap. Stored so
    /// hosts that *do* honour names re-resolve to the same frame.
    icon_name_cached: String,
}

impl ksni::Tray for OdriveTray {
    fn id(&self) -> String {
        "ai.openclaw.odrive-linux".into()
    }

    fn icon_name(&self) -> String {
        self.icon_name_cached.clone()
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

/// Push an icon (name + pixmap) to the running tray. Wraps the
/// `Handle::update` boilerplate so the activity poll, animation tick,
/// and colour swap all converge on one path.
fn push_icon(handle: &Handle<OdriveTray>, icon: &RenderedIcon) {
    let icon = icon.clone();
    handle.update(move |t: &mut OdriveTray| {
        t.icon_name_cached = icon.name;
        t.pixmap = icon.pixmap;
    });
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
/// the GTK-side event drain on the main loop, and starts the activity
/// + animation timers. Returns a `TrayController` the Settings page
/// can hold onto to swap the icon colour without re-importing `ksni`.
pub fn install(
    app: &adw::gtk::Application,
    window: &adw::ApplicationWindow,
    agent: Rc<OdriveAgent>,
) -> TrayController {
    let (tx, rx) = mpsc::channel::<TrayEvent>();
    let initial_running = agent.is_running();
    let cfg = OdriveConfig::load();
    let rendered = Rc::new(RefCell::new(RenderedSet::for_color(&cfg.tray_icon_color)));
    let initial_idle = rendered.borrow().idle.clone();

    let tray = OdriveTray {
        is_running: initial_running,
        tx,
        pixmap: initial_idle.pixmap.clone(),
        icon_name_cached: initial_idle.name.clone(),
    };

    let handle: Handle<OdriveTray> = match tray.spawn() {
        Ok(h) => h,
        Err(e) => {
            eprintln!(
                "Tray indicator unavailable ({e}). On stock GNOME this needs the \
                 gnome-shell-extension-appindicator extension. The Manager works without it."
            );
            return TrayController { handle: None, rendered };
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

    // Animation source-id holder. The activity poll starts the timer
    // on idle→active transitions and stops it (via `.take()` +
    // `.remove()`) on active→idle. Lives in an Rc<RefCell<>> so both
    // the activity poll and the timer's own closure can see it.
    let anim_source: Rc<RefCell<Option<glib::SourceId>>> =
        Rc::new(RefCell::new(None));

    // Activity + agent-state poll. Runs every ACTIVITY_POLL_SECS,
    // calls `get_sync_activity` once, updates is_running and decides
    // whether to start/stop the animation timer.
    let agent_for_poll = agent.clone();
    let handle_for_poll = handle.clone();
    let rendered_for_poll = rendered.clone();
    let anim_for_poll = anim_source.clone();
    glib::timeout_add_seconds_local(ACTIVITY_POLL_SECS, move || {
        // get_sync_activity is a `get_status` shell-out under the
        // hood. Errors mean the agent isn't reachable — treat as
        // "not active" (and not running) so we don't spin the
        // animation against a dead daemon.
        let (running, active) = match agent_for_poll.get_sync_activity() {
            Ok(a) => (true, a.is_active()),
            Err(_) => (agent_for_poll.is_running(), false),
        };
        handle_for_poll.update(move |t: &mut OdriveTray| {
            t.is_running = running;
        });

        let currently_animating = anim_for_poll.borrow().is_some();
        let has_animation = rendered_for_poll.borrow().has_animation();

        if active && !currently_animating && has_animation {
            let handle_for_anim = handle_for_poll.clone();
            let rendered_for_anim = rendered_for_poll.clone();
            let source = glib::timeout_add_local(
                Duration::from_millis(ANIM_TICK_MS),
                move || {
                    // Recompute the next frame inside the borrow so we
                    // hold the RefCell only briefly. The frame data is
                    // cloned out before push_icon's Handle::update so
                    // we don't keep the borrow across the cross-thread
                    // call.
                    let next = {
                        let mut r = rendered_for_anim.borrow_mut();
                        if r.frames.is_empty() {
                            None
                        } else {
                            r.frame_idx = (r.frame_idx + 1) % r.frames.len();
                            Some(r.frames[r.frame_idx].clone())
                        }
                    };
                    if let Some(icon) = next {
                        push_icon(&handle_for_anim, &icon);
                    }
                    glib::ControlFlow::Continue
                },
            );
            *anim_for_poll.borrow_mut() = Some(source);
        } else if !active && currently_animating {
            if let Some(src) = anim_for_poll.borrow_mut().take() {
                src.remove();
            }
            // Reset to frame 0 so the next active stretch starts fresh.
            let idle = {
                let mut r = rendered_for_poll.borrow_mut();
                r.frame_idx = 0;
                r.idle.clone()
            };
            push_icon(&handle_for_poll, &idle);
        }

        glib::ControlFlow::Continue
    });

    TrayController { handle: Some(handle), rendered }
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

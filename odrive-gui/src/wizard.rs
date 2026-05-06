//! Onboarding wizard window. Walks the user through up to four phases:
//!
//!   1. Install — agent binaries on disk and executable
//!   2. Service — `odriveagent` running (start once OR auto-start on login)
//!   3. Login — `odrive authenticate <key>` succeeded
//!   4. Mount (optional) — at least one local mount registered
//!
//! Each phase is gated on a precondition; a phase is shown only if its
//! precondition fails. After every successful page, the wizard re-checks
//! all preconditions and advances to the next failing one (or closes if
//! everything's satisfied).
//!
//! Long-running operations (install download, agent start, mount) run
//! synchronously on the GTK main thread for now — the UI will briefly
//! freeze during these. If that becomes painful, move to a worker thread
//! and post results back via `glib::idle_add_local` (same pattern noted
//! in CLAUDE.md for the dashboard's poll).
use libadwaita as adw;
use adw::prelude::*;
use adw::gtk as gtk;
use adw::{
    ApplicationWindow, EntryRow, HeaderBar, NavigationPage, NavigationView,
    PreferencesGroup, ToastOverlay, ToolbarView,
};
use gtk::{
    gio, glib, Align, Application, Box as GtkBox, Button, FileDialog, Image, Justification,
    Label, Orientation,
};
use crate::toasts::{error_toast, toast};

/// Mascot icon name resolved against hicolor; the same illustration the
/// About dialog uses. Single visual identity across every wizard page.
const WIZARD_ICON: &str = "odrive-linux-mascot";

/// Hero icon size for the wizard pages. Adw.StatusPage's CSS pins the
/// icon at 128 px and dims it via `color-mix(currentColor, transparent)`,
/// which works fine for symbolic glyphs but renders our full-colour
/// raster mascot smaller than intended (the dim layer plus the shared
/// 128-px slot for both compact and hero modes makes the illustration
/// read as an accent badge rather than the page art). Building the
/// hero column directly with explicit `set_pixel_size` sidesteps both.
const WIZARD_ICON_SIZE: i32 = 192;

/// Pixel width every wizard action button gets via `set_size_request`.
/// 280 reads as comfortable for the longest label ("Start at login (and
/// survive reboot)") without dwarfing single-word buttons; homogeneous
/// rendering inside a vertical Box plus this floor produces visually
/// uniform widths.
const WIZARD_BTN_WIDTH: i32 = 280;

/// Build a wizard action button: pill style, uniform width, optionally
/// the suggested-action variant for the recommended choice. Centralises
/// the styling so a future tweak lands in one place rather than 7+.
fn action_button(label: &str, suggested: bool) -> Button {
    let btn = Button::builder()
        .label(label)
        .halign(Align::Center)
        .build();
    btn.set_size_request(WIZARD_BTN_WIDTH, -1);
    btn.add_css_class("pill");
    if suggested {
        btn.add_css_class("suggested-action");
    }
    btn
}

/// Compose the hero column for a wizard page: large mascot + bold title
/// + dimmed description + caller-supplied actions, vertically centred.
/// Replaces Adw.StatusPage for the wizard because StatusPage's bundled
/// CSS dims the icon and ties title styling to a deep descendant
/// selector that's brittle when the page contains a custom child layout.
/// Hand-built with `Image::set_pixel_size` for the icon and the
/// public `.title-1` / `.body` / `.dim-label` Adw style classes for the
/// labels, giving us crisp hero typography with no mystery scaling.
fn hero_page(title_text: &str, description_text: &str, actions: &GtkBox) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 12);
    column.set_halign(Align::Center);
    column.set_valign(Align::Center);
    column.set_margin_top(36);
    column.set_margin_bottom(36);
    column.set_margin_start(36);
    column.set_margin_end(36);

    let icon = Image::from_icon_name(WIZARD_ICON);
    icon.set_pixel_size(WIZARD_ICON_SIZE);
    icon.set_margin_bottom(12);
    column.append(&icon);

    let title = Label::new(Some(title_text));
    title.add_css_class("title-1");
    title.set_wrap(true);
    title.set_justify(Justification::Center);
    title.set_max_width_chars(40);
    column.append(&title);

    let description = Label::new(Some(description_text));
    description.add_css_class("body");
    description.add_css_class("dim-label");
    description.set_wrap(true);
    description.set_justify(Justification::Center);
    description.set_max_width_chars(50);
    description.set_margin_bottom(12);
    column.append(&description);

    column.append(actions);
    column
}

use crate::worker;
use odrive_core::{OdriveAgent, OdriveConfig, OdriveError};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// Show the onboarding wizard. `on_complete` fires exactly once when the
/// wizard window closes (either because all phases finished, or because
/// the user closed it manually). The caller is expected to build the
/// dashboard from `on_complete`.
pub fn show<F>(app: &Application, on_complete: F)
where
    F: Fn() + 'static,
{
    let agent = Rc::new(RefCell::new(OdriveAgent::new()));
    let on_complete: Rc<dyn Fn()> = Rc::new(on_complete);

    let nav = NavigationView::new();
    let overlay = ToastOverlay::new();
    overlay.set_child(Some(&nav));

    // ToolbarView is the modern shell — gives the HeaderBar proper
    // background-blending behaviour and (crucially) docks the
    // ToastOverlay's bottom edge at the window content's bottom edge,
    // so toasts always appear at the window bottom regardless of which
    // page is currently visible. The earlier manual Box + HeaderBar
    // pattern positioned toasts at the bottom of the NavigationView's
    // content, which on a tall window with centered StatusPage content
    // ended up hugging the StatusPage rather than the window frame.
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&HeaderBar::new());
    toolbar.set_content(Some(&overlay));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("odrive Manager — Setup")
        .default_width(640)
        .default_height(480)
        .content(&toolbar)
        .build();

    // Closing the window — for any reason — completes the wizard. The
    // dashboard then re-runs the same precondition checks and either
    // shows itself or surfaces remaining gaps as empty-state CTAs.
    {
        let on_complete = on_complete.clone();
        window.connect_close_request(move |_| {
            on_complete();
            glib::Propagation::Proceed
        });
    }

    push_next(&nav, &agent, &overlay, &window);
    window.present();
}

/// Determine which phase still needs the user's attention and push the
/// corresponding page, or close the window if everything is satisfied.
fn push_next(
    nav: &NavigationView,
    agent: &Rc<RefCell<OdriveAgent>>,
    overlay: &ToastOverlay,
    window: &ApplicationWindow,
) {
    let next = {
        let a = agent.borrow();
        if !bins_present(&a) {
            Phase::Install
        } else if !a.is_running() {
            Phase::Service
        } else if !a.is_authenticated() {
            Phase::Login
        } else if a.get_mounts().map(|m| m.is_empty()).unwrap_or(true) {
            Phase::Mount
        } else {
            Phase::Done
        }
    };

    match next {
        Phase::Install => nav.push(&install_page(nav, agent, overlay, window)),
        Phase::Service => nav.push(&service_page(nav, agent, overlay, window)),
        Phase::Login => nav.push(&login_page(nav, agent, overlay, window)),
        Phase::Mount => nav.push(&mount_page(nav, agent, overlay, window)),
        Phase::Done => window.close(),
    }
}

#[derive(Copy, Clone, Debug)]
enum Phase {
    Install,
    Service,
    Login,
    Mount,
    Done,
}

fn bins_present(agent: &OdriveAgent) -> bool {
    let bin_dir = agent.agent_bin_dir();
    Path::new(&format!("{}/odrive", bin_dir)).exists()
        && Path::new(&format!("{}/odriveagent", bin_dir)).exists()
}

// ---------------------------------------------------------------------------
// Install page
// ---------------------------------------------------------------------------

fn install_page(
    nav: &NavigationView,
    agent: &Rc<RefCell<OdriveAgent>>,
    overlay: &ToastOverlay,
    window: &ApplicationWindow,
) -> NavigationPage {
    let pick_btn = action_button("Specify custom location", false);
    let install_btn = action_button("Install for me", true);

    let actions = GtkBox::new(Orientation::Vertical, 12);
    actions.set_halign(Align::Center);
    actions.append(&pick_btn);
    actions.append(&install_btn);

    let body = hero_page(
        "Install odrive",
        &format!(
            "Couldn't find odrive at {}. Either point us at an existing install, or let us run the official installer.",
            agent.borrow().agent_bin_dir(),
        ),
        &actions,
    );

    {
        let nav = nav.clone();
        let agent = agent.clone();
        let overlay = overlay.clone();
        let window = window.clone();
        pick_btn.connect_clicked(move |_| {
            pick_custom_location(&nav, &agent, &overlay, &window);
        });
    }

    {
        let nav = nav.clone();
        let agent = agent.clone();
        let overlay = overlay.clone();
        let window = window.clone();
        install_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            btn.set_label("Installing… (this may take a moment)");
            // Move the install pipeline to a worker thread so the GTK
            // main loop keeps painting during the curl+tar run.
            let agent_for_worker = agent.borrow().clone();
            let nav_for_done = nav.clone();
            let agent_for_done = agent.clone();
            let overlay_for_done = overlay.clone();
            let window_for_done = window.clone();
            let btn_for_done = btn.clone();
            worker::spawn(
                move || agent_for_worker.install_official(),
                move |result: Result<(), OdriveError>| {
                    btn_for_done.set_sensitive(true);
                    btn_for_done.set_label("Install for me");
                    match result {
                        Ok(_) => {
                            overlay_for_done.add_toast(toast("odrive installed"));
                            push_next(&nav_for_done, &agent_for_done, &overlay_for_done, &window_for_done);
                        }
                        Err(e) => {
                            overlay_for_done
                                .add_toast(error_toast(&format!("Install failed: {}", e)));
                        }
                    }
                },
            );
        });
    }

    NavigationPage::builder()
        .title("Install")
        .child(&body)
        .can_pop(false)
        .build()
}

fn pick_custom_location(
    nav: &NavigationView,
    agent: &Rc<RefCell<OdriveAgent>>,
    overlay: &ToastOverlay,
    window: &ApplicationWindow,
) {
    let dialog = FileDialog::builder()
        .title("Pick the folder containing the odrive bins")
        .modal(true)
        .build();

    // Open at the currently-configured location even if the bins aren't
    // there yet — gives the user a sensible starting point.
    let initial = gio::File::for_path(agent.borrow().agent_bin_dir());
    dialog.set_initial_folder(Some(&initial));

    let nav = nav.clone();
    let agent = agent.clone();
    let overlay = overlay.clone();
    let window_for_cb = window.clone();
    dialog.select_folder(Some(window), gio::Cancellable::NONE, move |result| {
        let folder = match result {
            Ok(f) => f,
            Err(_) => return, // user cancelled
        };
        let Some(path) = folder.path() else {
            overlay.add_toast(error_toast("Selected folder has no usable path"));
            return;
        };
        let bin_dir = path.to_string_lossy().to_string();
        let trial = agent.borrow().with_new_bin_dir(bin_dir.clone());
        let odrive_bin = format!("{}/odrive", bin_dir);
        let agent_bin = format!("{}/odriveagent", bin_dir);
        if !Path::new(&odrive_bin).exists() || !Path::new(&agent_bin).exists() {
            overlay.add_toast(error_toast(
                "That folder doesn't contain odrive and odriveagent — pick the bin/ directory.",
            ));
            return;
        }
        // Persist the choice and swap the active agent. Load-modify-save
        // so we don't clobber other fields (tray_icon_color, etc.) the
        // user may have set elsewhere.
        let mut cfg = OdriveConfig::load();
        cfg.agent_bin_dir = bin_dir;
        if let Err(e) = cfg.save() {
            overlay.add_toast(error_toast(&format!("Could not save config: {}", e)));
            return;
        }
        *agent.borrow_mut() = trial;
        overlay.add_toast(toast("Custom location saved"));
        push_next(&nav, &agent, &overlay, &window_for_cb);
    });
}

// ---------------------------------------------------------------------------
// Service page
// ---------------------------------------------------------------------------

fn service_page(
    nav: &NavigationView,
    agent: &Rc<RefCell<OdriveAgent>>,
    overlay: &ToastOverlay,
    window: &ApplicationWindow,
) -> NavigationPage {
    let once_btn = action_button("Start once", false);
    let auto_btn = action_button("Start at login (and survive reboot)", true);

    let actions = GtkBox::new(Orientation::Vertical, 12);
    actions.set_halign(Align::Center);
    actions.append(&once_btn);
    actions.append(&auto_btn);

    let body = hero_page(
        "Start the agent",
        "odriveagent isn't running. How do you want to start it?",
        &actions,
    );

    {
        let nav = nav.clone();
        let agent = agent.clone();
        let overlay = overlay.clone();
        let window = window.clone();
        once_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            btn.set_label("Starting…");
            // `agent.start()` blocks ~2s polling is_running internally,
            // so it must not run on the GTK main thread or the UI
            // freezes. Worker pattern matches the Install button.
            let agent_for_worker = agent.borrow().clone();
            let nav_for_done = nav.clone();
            let agent_for_done = agent.clone();
            let overlay_for_done = overlay.clone();
            let window_for_done = window.clone();
            let btn_for_done = btn.clone();
            worker::spawn(
                move || agent_for_worker.start(),
                move |result: Result<(), OdriveError>| {
                    btn_for_done.set_sensitive(true);
                    btn_for_done.set_label("Start once");
                    match result {
                        Ok(_) => {
                            overlay_for_done.add_toast(toast("Agent started"));
                            advance_when_ready(
                                &nav_for_done,
                                &agent_for_done,
                                &overlay_for_done,
                                &window_for_done,
                            );
                        }
                        Err(e) => overlay_for_done
                            .add_toast(error_toast(&format!("Start failed: {}", e))),
                    }
                },
            );
        });
    }

    {
        let nav = nav.clone();
        let agent = agent.clone();
        let overlay = overlay.clone();
        let window = window.clone();
        auto_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            btn.set_label("Enabling…");
            // enable_autostart shells out to systemctl + loginctl; the
            // latter may sit on a polkit prompt for enable-linger. Run
            // it on a worker so the GTK main loop keeps painting and
            // the polkit dialog can render. After it returns Ok we
            // still have to wait for the agent's IPC to bind —
            // `enable --now` returns when ExecStart launches, not when
            // the daemon is healthy — so poll is_running briefly
            // before advancing the wizard.
            let agent_for_worker = agent.borrow().clone();
            let nav_for_done = nav.clone();
            let agent_for_done = agent.clone();
            let overlay_for_done = overlay.clone();
            let window_for_done = window.clone();
            let btn_for_done = btn.clone();
            worker::spawn(
                move || enable_autostart(&agent_for_worker),
                move |result: Result<(), String>| {
                    btn_for_done.set_sensitive(true);
                    btn_for_done.set_label("Start at login (and survive reboot)");
                    match result {
                        Ok(_) => {
                            overlay_for_done.add_toast(toast("Auto-start enabled"));
                            advance_when_ready(
                                &nav_for_done,
                                &agent_for_done,
                                &overlay_for_done,
                                &window_for_done,
                            );
                        }
                        Err(e) => overlay_for_done
                            .add_toast(error_toast(&format!("Auto-start failed: {}", e))),
                    }
                },
            );
        });
    }

    NavigationPage::builder()
        .title("Service")
        .child(&body)
        .can_pop(false)
        .build()
}

/// Write the systemd unit, enable+start it, then enable linger so it
/// survives logout/reboot. Linger may trigger a polkit prompt at the OS
/// level the first time; we don't surface that as a separate UI step.
fn enable_autostart(agent: &OdriveAgent) -> Result<(), String> {
    agent.write_systemd_unit().map_err(|e| e.to_string())?;
    agent.enable_systemd_unit().map_err(|e| e.to_string())?;
    agent.enable_linger().map_err(|e| e.to_string())?;
    Ok(())
}

/// Call `push_next` once `is_running()` returns true, polling at 500ms
/// for up to ~10s. `systemctl --user enable --now odrive.service` and
/// `systemctl --user start` both return as soon as ExecStart launches,
/// but the agent itself needs another moment to bind its IPC — during
/// that window `is_running()` (which requires both `pgrep` AND a clean
/// `odrive status` exit) returns false. Calling `push_next` immediately
/// races that window: push_next would re-evaluate the precondition,
/// see `is_running` still false, and push another copy of the same
/// Service page on top of itself, which looks indistinguishable from
/// "nothing happened" to the user. The first poll fires at 500ms; the
/// happy path takes 1–2 polls. After 20 ticks we surface a diagnostic
/// toast and stop — the user can then re-click or check
/// `systemctl --user status odrive.service` directly.
fn advance_when_ready(
    nav: &NavigationView,
    agent: &Rc<RefCell<OdriveAgent>>,
    overlay: &ToastOverlay,
    window: &ApplicationWindow,
) {
    use std::cell::Cell;
    let nav = nav.clone();
    let agent = agent.clone();
    let overlay = overlay.clone();
    let window = window.clone();
    let attempts = Rc::new(Cell::new(0u32));
    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        if agent.borrow().is_running() {
            push_next(&nav, &agent, &overlay, &window);
            return glib::ControlFlow::Break;
        }
        let n = attempts.get() + 1;
        attempts.set(n);
        if n >= 20 {
            overlay.add_toast(error_toast(
                "Agent didn't come online in time. Check `systemctl --user status odrive.service`.",
            ));
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

// ---------------------------------------------------------------------------
// Login page
// ---------------------------------------------------------------------------

fn login_page(
    nav: &NavigationView,
    agent: &Rc<RefCell<OdriveAgent>>,
    overlay: &ToastOverlay,
    window: &ApplicationWindow,
) -> NavigationPage {
    let get_code_btn = action_button("Get auth code", false);
    let submit_btn = action_button("Sign in", true);

    // PreferencesGroup gives the EntryRow the rounded-card border +
    // shadow GNOME settings rows have, so the field reads as a defined
    // input control instead of melting into the page background. Also
    // forces a sensible width — the bare EntryRow stretched across the
    // full content width and looked unbalanced next to the centered
    // buttons above and below it.
    let entry_row = EntryRow::builder().title("Auth code").build();
    let entry_group = PreferencesGroup::new();
    entry_group.add(&entry_row);
    entry_group.set_halign(Align::Center);
    entry_group.set_size_request(WIZARD_BTN_WIDTH, -1);

    let actions = GtkBox::new(Orientation::Vertical, 12);
    actions.set_halign(Align::Center);
    actions.append(&get_code_btn);
    actions.append(&entry_group);
    actions.append(&submit_btn);

    let body = hero_page(
        "Sign in to odrive",
        "Get an authentication code from your odrive account, then paste it below.",
        &actions,
    );

    {
        let overlay = overlay.clone();
        get_code_btn.connect_clicked(move |_| {
            // xdg-open is the de-facto opener on Linux; gio::AppInfo with
            // launch_default_for_uri is GTK-native but xdg-open is simpler
            // and equivalent on every desktop we care about.
            let r = std::process::Command::new("xdg-open")
                .arg("https://www.odrive.com/account/authcodes")
                .spawn();
            if let Err(e) = r {
                overlay.add_toast(error_toast(&format!("Couldn't open browser: {}", e)));
            }
        });
    }

    {
        let nav = nav.clone();
        let agent = agent.clone();
        let overlay = overlay.clone();
        let window = window.clone();
        let entry_row = entry_row.clone();
        submit_btn.connect_clicked(move |btn| {
            let code = entry_row.text().trim().to_string();
            if code.is_empty() {
                overlay.add_toast(error_toast("Paste your auth code first"));
                return;
            }
            btn.set_sensitive(false);
            let result = agent.borrow().authenticate(&code);
            btn.set_sensitive(true);
            match result {
                Ok(_) => {
                    overlay.add_toast(toast("Signed in"));
                    push_next(&nav, &agent, &overlay, &window);
                }
                Err(e) => overlay.add_toast(error_toast(&format!("Sign-in failed: {}", e))),
            }
        });
    }

    NavigationPage::builder()
        .title("Sign in")
        .child(&body)
        .can_pop(false)
        .build()
}

// ---------------------------------------------------------------------------
// Mount page (optional)
// ---------------------------------------------------------------------------

fn mount_page(
    nav: &NavigationView,
    agent: &Rc<RefCell<OdriveAgent>>,
    overlay: &ToastOverlay,
    window: &ApplicationWindow,
) -> NavigationPage {
    let default_path = agent.borrow().default_mount_path();

    let default_btn = action_button(&format!("Use default ({})", default_path), true);
    let pick_btn = action_button("Choose a different folder", false);
    let skip_btn = action_button("Skip — I'll mount later", false);

    let actions = GtkBox::new(Orientation::Vertical, 12);
    actions.set_halign(Align::Center);
    actions.append(&default_btn);
    actions.append(&pick_btn);
    actions.append(&skip_btn);

    let body = hero_page(
        "Mount your odrive root (optional)",
        &format!(
            "Pick a local folder to mirror your odrive cloud into. Default is {}.",
            default_path,
        ),
        &actions,
    );

    {
        let nav = nav.clone();
        let agent = agent.clone();
        let overlay = overlay.clone();
        let window = window.clone();
        let default_path = default_path.clone();
        default_btn.connect_clicked(move |_| {
            // `odrive mount` creates the local dir if it's missing, but
            // it errors before doing so when the *parent* directory
            // doesn't exist. ~/odrive's parent is $HOME which is a
            // given, so a bare mount call is safe; create_dir_all is a
            // belt-and-braces guard for users who customised
            // `default_mount_path` to something deeper.
            if let Err(e) = std::fs::create_dir_all(&default_path) {
                overlay.add_toast(error_toast(&format!("Could not create {}: {}", default_path, e)));
                return;
            }
            match agent.borrow().mount(&default_path, "/") {
                Ok(_) => {
                    overlay.add_toast(toast("Mount created"));
                    push_next(&nav, &agent, &overlay, &window);
                }
                Err(e) => overlay.add_toast(error_toast(&format!("Mount failed: {}", e))),
            }
        });
    }

    {
        let nav = nav.clone();
        let agent = agent.clone();
        let overlay = overlay.clone();
        let window = window.clone();
        pick_btn.connect_clicked(move |_| {
            run_mount_picker(&nav, &agent, &overlay, &window);
        });
    }

    {
        let window = window.clone();
        skip_btn.connect_clicked(move |_| {
            // Closing fires the on_complete callback, which builds the
            // dashboard. The empty-state mount banner there will offer
            // the same flow if/when the user wants it later.
            window.close();
        });
    }

    NavigationPage::builder()
        .title("Mount")
        .child(&body)
        .can_pop(false)
        .build()
}

fn run_mount_picker(
    nav: &NavigationView,
    agent: &Rc<RefCell<OdriveAgent>>,
    overlay: &ToastOverlay,
    window: &ApplicationWindow,
) {
    let dialog = FileDialog::builder()
        .title("Pick the local folder for your odrive root")
        .modal(true)
        .build();

    // Default to ~/odrive — create the directory hint via a gio::File so
    // the picker opens there even when it doesn't yet exist.
    let default_path = agent.borrow().default_mount_path();
    let initial = gio::File::for_path(&default_path);
    dialog.set_initial_folder(Some(&initial));

    let nav = nav.clone();
    let agent = agent.clone();
    let overlay = overlay.clone();
    let window_for_cb = window.clone();
    dialog.select_folder(Some(window), gio::Cancellable::NONE, move |result| {
        let folder = match result {
            Ok(f) => f,
            Err(_) => return,
        };
        let Some(path) = folder.path() else {
            overlay.add_toast(error_toast("Selected folder has no usable path"));
            return;
        };
        let local = path.to_string_lossy().to_string();
        match agent.borrow().mount(&local, "/") {
            Ok(_) => {
                overlay.add_toast(toast("Mount created"));
                push_next(&nav, &agent, &overlay, &window_for_cb);
            }
            Err(e) => overlay.add_toast(error_toast(&format!("Mount failed: {}", e))),
        }
    });
}


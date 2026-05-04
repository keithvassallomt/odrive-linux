mod indicator;
mod mount_detail;
mod settings_page;
mod wizard;
mod worker;

use libadwaita as adw;
use adw::prelude::*;
use adw::gtk as gtk;
use adw::{
    ActionRow, ApplicationWindow, HeaderBar, MessageDialog, NavigationPage,
    NavigationView, PreferencesGroup, PreferencesPage, ResponseAppearance,
    StatusPage, Toast, ToastOverlay, ToolbarView, WindowTitle,
};
use gtk::{gdk, gio, Application, Button, CssProvider, MenuButton};
use odrive_core::{OdriveAgent, OdriveDb};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

fn main() {
    let application = Application::builder()
        .application_id("ai.openclaw.odrive-linux")
        .build();

    application.connect_startup(|_| {
        install_app_css();
    });

    application.connect_activate(|app| {
        let agent = OdriveAgent::new();
        if needs_wizard(&agent) {
            // Wizard runs first; when it closes (any way), build the
            // dashboard. The dashboard re-runs the same precondition
            // checks at construction time, so anything still missing
            // surfaces as an empty-state CTA there.
            let app_for_complete = app.clone();
            wizard::show(app, move || present_dashboard(&app_for_complete));
        } else {
            present_dashboard(app);
        }
    });

    application.run();
}

/// True iff at least one of the four wizard phases still has work to do.
/// The "Mount" precondition is included even though it's optional —
/// reaching the wizard with no mounts simply lets the user opt into the
/// Mount page; they can still skip it from there.
fn needs_wizard(agent: &OdriveAgent) -> bool {
    let bin_dir = agent.agent_bin_dir();
    let odrive_bin = format!("{}/odrive", bin_dir);
    let agent_bin = format!("{}/odriveagent", bin_dir);
    if !Path::new(&odrive_bin).exists() || !Path::new(&agent_bin).exists() {
        return true;
    }
    if !agent.is_running() {
        return true;
    }
    if !agent.is_authenticated() {
        return true;
    }
    if agent.get_mounts().map(|m| m.is_empty()).unwrap_or(true) {
        return true;
    }
    false
}

fn present_dashboard(app: &Application) {
    let agent = Rc::new(OdriveAgent::new());

    let nav = NavigationView::new();
    let overlay = ToastOverlay::new();
    overlay.set_child(Some(&nav));

    let dashboard_page = build_dashboard_page(app.clone(), agent.clone(), overlay.clone(), nav.clone());
    nav.push(&dashboard_page);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("odrive Manager")
        .default_width(820)
        .default_height(520)
        .content(&overlay)
        .build();

    // Panel indicator: lives for the lifetime of this dashboard window.
    // If the StatusNotifierItem host isn't available (stock GNOME
    // without the appindicator extension) install() logs a warning
    // and the Manager runs without a tray icon.
    indicator::install(app, &window, agent.clone());

    window.present();
}

/// App-wide CSS overrides. We only make the smallest set of tweaks
/// libadwaita doesn't already do for us:
///   - Dim group descriptions and row subtitles so titles win the
///     visual weight contest (Yaru's defaults render both in the same
///     bright white).
///   - Add real space between PreferencesGroups and a touch of vertical
///     padding inside each row so the content feels less crammed at
///     density-1.
fn install_app_css() {
    let css = CssProvider::new();
    // Class-based selectors that don't depend on libadwaita's exact
    // widget-tree depth. `.description` is the class libadwaita applies
    // to the group description label; `.subtitle` is on row subtitles.
    // (My earlier `preferencesgroup > box > label.description` selector
    // was one level too shallow — the description sits inside a nested
    // header box, not directly under the outer vertical box.)
    // Single-line rows would otherwise collapse to ~25-30px tall — a
    // 40px floor lets a one-line "Tenders" row breathe without making
    // a two-line title+subtitle row visibly inflated.
    css.load_from_string(
        "preferencesgroup .description { opacity: 0.55; }\n\
         row .subtitle { opacity: 0.6; }\n\
         preferencesgroup { margin-bottom: 18px; }\n\
         preferencesgroup .boxed-list { margin-top: 12px; }\n\
         .boxed-list > row { min-height: 40px; padding-left: 6px; padding-right: 6px; }\n\
         preferencespage > scrolledwindow > viewport > clamp { margin-top: 6px; }\n",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn build_dashboard_page(
    app: Application,
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    nav: NavigationView,
) -> NavigationPage {
    // ToolbarView is the modern shell — it handles background blending
    // and scroll-edge styling between the HeaderBar and the
    // PreferencesPage content automatically.
    let toolbar = ToolbarView::new();

    let header = HeaderBar::new();
    let title = WindowTitle::new("odrive Manager", "");
    header.set_title_widget(Some(&title));

    // Primary menu (hamburger) on the right — current GNOME idiom for
    // app-level commands. Houses Preferences and About; future entries
    // (Pause / Resume, Quit on the panel-indicator side) will land here
    // too.
    let menu = primary_menu(&nav, &agent, &overlay);
    let menu_btn = MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu.0)
        .primary(true)
        .tooltip_text("Main Menu")
        .build();
    install_menu_actions(&app, menu.1);
    header.pack_start(&menu_btn);

    toolbar.add_top_bar(&header);

    // PreferencesPage normally clamps content to 600px and centres it; on
    // GNOME 49's Adw build the default top padding under a HeaderBar is
    // tight, so add explicit breathing room. We also nudge the title from
    // each group up so they don't kiss the headerbar.
    let page = PreferencesPage::new();
    page.set_margin_top(12);

    // ----- Agent group -----
    let agent_group = PreferencesGroup::builder()
        .title("Agent")
        .description("Daemon lifecycle and the local placeholder index.")
        .build();

    let status_row = ActionRow::builder()
        .title("Status")
        .subtitle("Checking…")
        .build();
    let start_stop_btn = Button::builder()
        .label("Start")
        .valign(gtk::Align::Center)
        .build();
    start_stop_btn.add_css_class("pill");
    status_row.add_suffix(&start_stop_btn);
    agent_group.add(&status_row);

    let db_row = ActionRow::builder()
        .title("Placeholder database")
        .subtitle("0 tracked items")
        .build();
    let scan_btn = Button::builder()
        .label("Scan now")
        .valign(gtk::Align::Center)
        .build();
    scan_btn.add_css_class("pill");
    db_row.add_suffix(&scan_btn);
    agent_group.add(&db_row);

    page.add(&agent_group);

    // ----- Mounts group -----
    let mounts_group = PreferencesGroup::builder()
        .title("Mounts")
        .description("Local folders mirrored from your odrive cloud account.")
        .build();
    page.add(&mounts_group);

    toolbar.set_content(Some(&page));

    // Track every widget we've added to `mounts_group` so we can remove
    // exactly those on the next tick. `PreferencesGroup.first_child()`
    // returns its internal scaffolding box, not the rows we added —
    // walking that tree blind triggers Adwaita-CRITICAL warnings.
    let mounted_children: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));

    // Update closure — refreshes the agent group, then rebuilds the
    // mounts group from scratch each tick. Rebuilding from scratch
    // keeps the wiring simple: each mount's row carries its own
    // closures referencing its own path, with no need to diff.
    let update_ui = {
        let agent = agent.clone();
        let status_row = status_row.clone();
        let start_stop_btn = start_stop_btn.clone();
        let db_row = db_row.clone();
        let mounts_group = mounts_group.clone();
        let overlay = overlay.clone();
        let mounted_children = mounted_children.clone();
        let nav_for_rows = nav.clone();
        move || {
            let is_running = agent.is_running();
            status_row.set_subtitle(if is_running { "Running" } else { "Stopped" });
            start_stop_btn.set_label(if is_running { "Stop" } else { "Start" });
            if is_running {
                start_stop_btn.remove_css_class("suggested-action");
            } else {
                start_stop_btn.add_css_class("suggested-action");
            }

            if let Ok(db) = OdriveDb::open(agent.get_db_path()) {
                let count = db.count_placeholders().unwrap_or(0);
                db_row.set_subtitle(&format!("{} tracked items", count));
            }

            // Drop the previous tick's children, then rebuild.
            for child in mounted_children.borrow_mut().drain(..) {
                mounts_group.remove(&child);
            }

            match agent.get_mounts() {
                Ok(mounts) if mounts.is_empty() => {
                    let empty = StatusPage::builder()
                        .icon_name("folder-symbolic")
                        .title("No mounts yet")
                        .description("Set up a mount through the onboarding wizard, or restart the app to launch it.")
                        .build();
                    empty.add_css_class("compact");
                    mounts_group.add(&empty);
                    mounted_children.borrow_mut().push(empty.upcast::<gtk::Widget>());
                }
                Ok(mounts) => {
                    for mount in mounts {
                        let row = build_mount_row(
                            agent.clone(),
                            overlay.clone(),
                            nav_for_rows.clone(),
                            mount.local_path.clone(),
                            mount.remote_path.clone(),
                            mount.status.clone(),
                        );
                        mounts_group.add(&row);
                        mounted_children.borrow_mut().push(row.upcast::<gtk::Widget>());
                    }
                }
                Err(_) => {
                    let err = StatusPage::builder()
                        .icon_name("dialog-error-symbolic")
                        .title("Couldn't list mounts")
                        .description("The agent didn't respond to a status query. Try starting the agent.")
                        .build();
                    err.add_css_class("compact");
                    mounts_group.add(&err);
                    mounted_children.borrow_mut().push(err.upcast::<gtk::Widget>());
                }
            }
        }
    };

    update_ui();

    // Background poll — refreshes status, placeholder count, and the
    // mount list every 5s so external state changes (a sync completing,
    // the agent restarting, a fresh mount) surface without requiring the
    // user to click. Each tick runs the same synchronous shell-outs the
    // button handlers do, so on a slow agent response the UI may briefly
    // stutter. If that becomes visible, move the IO to a worker thread
    // and post results back via glib::idle_add_local.
    gtk::glib::timeout_add_seconds_local(5, {
        let update = update_ui.clone();
        move || {
            update();
            gtk::glib::ControlFlow::Continue
        }
    });

    start_stop_btn.connect_clicked({
        let agent = agent.clone();
        let update = update_ui.clone();
        let overlay = overlay.clone();
        move |_| {
            if agent.is_running() {
                let _ = agent.stop();
            } else {
                let _ = agent.start();
            }
            update();
            overlay.add_toast(Toast::new("Status updated"));
        }
    });

    scan_btn.connect_clicked({
        let agent = agent.clone();
        let update = update_ui.clone();
        let overlay = overlay.clone();
        move |btn| {
            btn.set_sensitive(false);
            btn.set_label("Scanning…");
            let agent_for_worker = agent.as_ref().clone();
            let mount_path = agent.default_mount_path();
            let overlay_for_done = overlay.clone();
            let update_for_done = update.clone();
            let btn_for_done = btn.clone();
            worker::spawn(
                move || agent_for_worker.scan_placeholders(&mount_path),
                move |result| {
                    btn_for_done.set_sensitive(true);
                    btn_for_done.set_label("Scan now");
                    match result {
                        Ok(count) => {
                            overlay_for_done
                                .add_toast(Toast::new(&format!("Found {} placeholders", count)));
                            update_for_done();
                        }
                        Err(e) => overlay_for_done
                            .add_toast(Toast::new(&format!("Scan failed: {}", e))),
                    }
                },
            );
        }
    });

    NavigationPage::builder()
        .title("odrive Manager")
        .child(&toolbar)
        .can_pop(false)
        .build()
}

/// Build a single mount entry. The row is `activatable` and on click
/// pushes the mount-detail page onto the same NavigationView, where
/// the user can drill into folders and set per-folder sync rules.
fn build_mount_row(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    nav: NavigationView,
    local_path: String,
    remote_path: String,
    status: String,
) -> ActionRow {
    let row = ActionRow::builder()
        .title(&local_path)
        .subtitle(&format!("{} • {}", remote_path, status))
        .activatable(true)
        .build();

    // Leading folder icon for visual hierarchy. Bump the pixel size from
    // the default 16px to 24px so it doesn't look squished inside the
    // row's rounded corner; symbolic icons scale crisply at this size.
    // margin_start nudges the icon off the row's left edge; margin_end
    // separates the icon from the title (otherwise they touch).
    let icon = adw::gtk::Image::from_icon_name("folder-symbolic");
    icon.set_pixel_size(24);
    icon.set_margin_start(6);
    icon.set_margin_end(8);
    row.add_prefix(&icon);

    // Trailing chevron — visual cue that the row drills into a detail
    // page. Sits before the unmount button so the destructive icon is
    // closer to the row edge.
    let chevron = adw::gtk::Image::from_icon_name("go-next-symbolic");
    chevron.set_margin_start(6);
    row.add_suffix(&chevron);

    let unmount_btn = Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Unmount")
        .valign(gtk::Align::Center)
        .build();
    unmount_btn.add_css_class("flat");
    unmount_btn.add_css_class("circular");
    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let local_path = local_path.clone();
        unmount_btn.connect_clicked(move |btn| {
            confirm_and_unmount(
                btn.upcast_ref::<gtk::Widget>(),
                agent.clone(),
                overlay.clone(),
                local_path.clone(),
            );
        });
    }
    row.add_suffix(&unmount_btn);

    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let nav = nav.clone();
        let local_path = local_path.clone();
        row.connect_activated(move |_| {
            let page = mount_detail::build_mount_root(
                agent.clone(),
                overlay.clone(),
                nav.clone(),
                local_path.clone(),
            );
            nav.push(&page);
        });
    }

    row
}

/// Pop a destructive-style confirmation dialog before calling
/// `agent.unmount`. We don't try to delete already-synced files — the
/// upstream's behaviour is to leave them on disk, and matching that is
/// less surprising than offering a "wipe everything" option that the
/// user might fire by accident.
fn confirm_and_unmount(
    parent: &gtk::Widget,
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    local_path: String,
) {
    let window = parent
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok());
    let dialog = MessageDialog::builder()
        .heading("Remove this mount?")
        .body(format!(
            "This unmounts {} from odrive. Already-synced files stay on disk; placeholders for unsynced files become inert. You can re-mount later from the Manager.",
            local_path
        ))
        .modal(true)
        .build();
    if let Some(w) = window.as_ref() {
        dialog.set_transient_for(Some(w));
    }
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("unmount", "Unmount");
    dialog.set_response_appearance("unmount", ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let local_path_for_cb = local_path.clone();
    dialog.connect_response(None, move |dlg, response| {
        if response == "unmount" {
            match agent.unmount(&local_path_for_cb) {
                Ok(_) => overlay.add_toast(Toast::new("Mount removed")),
                Err(e) => overlay.add_toast(Toast::new(&format!("Unmount failed: {}", e))),
            }
        }
        dlg.close();
    });
    dialog.present();
}

/// Build the primary menu model and the action callbacks that back its
/// items. Returns the model and a vector of (action_name, callback)
/// pairs to be installed on the application via
/// `install_menu_actions`.
fn primary_menu(
    nav: &NavigationView,
    agent: &Rc<OdriveAgent>,
    overlay: &ToastOverlay,
) -> (gio::Menu, Vec<(String, Box<dyn Fn() + 'static>)>) {
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some("_Preferences"), Some("app.preferences"));
    section.append(Some("_About odrive Manager"), Some("app.about"));
    menu.append_section(None, &section);

    let mut actions: Vec<(String, Box<dyn Fn() + 'static>)> = Vec::new();

    // Preferences action → push the settings page onto the nav stack.
    {
        let nav = nav.clone();
        let agent = agent.clone();
        let overlay = overlay.clone();
        actions.push((
            "preferences".to_string(),
            Box::new(move || {
                let page = settings_page::build(agent.clone(), overlay.clone());
                nav.push(&page);
            }),
        ));
    }

    // About action → fire a minimal Adw.AboutWindow. The version string
    // is pulled from Cargo so a release bump propagates without code
    // edits.
    {
        let overlay = overlay.clone();
        actions.push((
            "about".to_string(),
            Box::new(move || {
                let about = adw::AboutWindow::builder()
                    .application_name("odrive Manager")
                    .application_icon("folder-remote-symbolic")
                    .version(env!("CARGO_PKG_VERSION"))
                    .developer_name("odrive-linux contributors")
                    .website("https://www.odrive.com")
                    .license_type(gtk::License::MitX11)
                    .modal(true)
                    .build();
                if let Some(root) = overlay
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok())
                {
                    about.set_transient_for(Some(&root));
                }
                about.present();
            }),
        ));
    }

    (menu, actions)
}

fn install_menu_actions(app: &Application, actions: Vec<(String, Box<dyn Fn() + 'static>)>) {
    for (name, callback) in actions {
        let action = gio::SimpleAction::new(&name, None);
        action.connect_activate(move |_, _| callback());
        app.add_action(&action);
    }
}

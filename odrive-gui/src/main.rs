mod settings_page;
mod wizard;

use libadwaita as adw;
use adw::prelude::*;
use adw::gtk as gtk;
use adw::{
    ActionRow, ApplicationWindow, HeaderBar, MessageDialog, NavigationPage,
    NavigationView, ResponseAppearance, Toast, ToastOverlay,
};
use gtk::{Application, Box, ListBox, Orientation, Button, Label};
use odrive_core::{OdriveAgent, OdriveDb};
use std::path::Path;
use std::rc::Rc;

fn main() {
    let application = Application::builder()
        .application_id("ai.openclaw.odrive-linux")
        .build();

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

    let dashboard_page = build_dashboard_page(agent.clone(), overlay.clone(), nav.clone());
    nav.push(&dashboard_page);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("odrive Manager")
        .default_width(640)
        .default_height(480)
        .content(&overlay)
        .build();

    window.present();
}

fn build_dashboard_page(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    nav: NavigationView,
) -> NavigationPage {
    let outer = Box::new(Orientation::Vertical, 0);

    // Header with a trailing gear button that pushes the Settings page.
    let header = HeaderBar::new();
    let settings_btn = Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text("Global Settings")
        .build();
    settings_btn.add_css_class("flat");
    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let nav = nav.clone();
        settings_btn.connect_clicked(move |_| {
            let page = settings_page::build(agent.clone(), overlay.clone());
            nav.push(&page);
        });
    }
    header.pack_end(&settings_btn);
    outer.append(&header);

    // Status Group
    let list = ListBox::new();
    list.add_css_class("boxed-list");
    list.set_margin_top(24);
    list.set_margin_bottom(12);
    list.set_margin_start(24);
    list.set_margin_end(24);

    let status_row = ActionRow::builder()
        .title("Agent Status")
        .build();

    let status_label = Label::builder()
        .label("Checking...")
        .valign(gtk::Align::Center)
        .build();
    status_row.add_suffix(&status_label);

    let start_stop_btn = Button::builder()
        .label("Start")
        .valign(gtk::Align::Center)
        .build();
    status_row.add_suffix(&start_stop_btn);

    list.append(&status_row);

    // Placeholder Group
    let db_row = ActionRow::builder()
        .title("Placeholder Database")
        .subtitle("0 tracked items")
        .build();

    let scan_btn = Button::builder()
        .label("Scan Now")
        .valign(gtk::Align::Center)
        .build();
    db_row.add_suffix(&scan_btn);
    list.append(&db_row);

    outer.append(&list);

    // Mounts List
    let mount_list_title = Label::builder()
        .label("Active Mounts")
        .xalign(0.0)
        .margin_start(28)
        .margin_top(12)
        .build();
    mount_list_title.add_css_class("heading");
    outer.append(&mount_list_title);

    let mount_list = ListBox::new();
    mount_list.add_css_class("boxed-list");
    mount_list.set_margin_top(6);
    mount_list.set_margin_bottom(24);
    mount_list.set_margin_start(24);
    mount_list.set_margin_end(24);
    outer.append(&mount_list);

    // Update function — refreshes status, placeholder count, and rebuilds
    // the mount list. Each mount row gets a trailing "Unmount" button
    // that pops a confirmation dialog before calling the agent.
    let update_ui = {
        let agent = agent.clone();
        let status_label = status_label.clone();
        let start_stop_btn = start_stop_btn.clone();
        let db_row = db_row.clone();
        let mount_list = mount_list.clone();
        let overlay_for_unmount = overlay.clone();
        move || {
            let is_running = agent.is_running();
            status_label.set_label(if is_running { "Running" } else { "Stopped" });
            start_stop_btn.set_label(if is_running { "Stop" } else { "Start" });

            if let Ok(db) = OdriveDb::open(agent.get_db_path()) {
                let count = db.count_placeholders().unwrap_or(0);
                db_row.set_subtitle(&format!("{} tracked items", count));
            }

            // Refresh mount list
            while let Some(child) = mount_list.first_child() {
                mount_list.remove(&child);
            }

            if let Ok(mounts) = agent.get_mounts() {
                if mounts.is_empty() {
                    let empty_row = ActionRow::builder()
                        .title("No active mounts")
                        .build();
                    mount_list.append(&empty_row);
                } else {
                    for mount in mounts {
                        let row = ActionRow::builder()
                            .title(&mount.local_path)
                            .subtitle(&format!("Remote: {} ({})", mount.remote_path, mount.status))
                            .build();
                        let unmount_btn = Button::builder()
                            .label("Unmount")
                            .valign(gtk::Align::Center)
                            .build();
                        unmount_btn.add_css_class("flat");
                        {
                            let agent = agent.clone();
                            let overlay = overlay_for_unmount.clone();
                            let local = mount.local_path.clone();
                            unmount_btn.connect_clicked(move |btn| {
                                confirm_and_unmount(
                                    btn.upcast_ref::<gtk::Widget>(),
                                    agent.clone(),
                                    overlay.clone(),
                                    local.clone(),
                                );
                            });
                        }
                        row.add_suffix(&unmount_btn);
                        mount_list.append(&row);
                    }
                }
            } else {
                let error_row = ActionRow::builder()
                    .title("Unable to retrieve mounts")
                    .build();
                mount_list.append(&error_row);
            }
        }
    };

    // Initial update
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

    // Button actions
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
        move |_| {
            let mount_path = agent.default_mount_path();
            match agent.scan_placeholders(&mount_path) {
                Ok(count) => {
                    overlay.add_toast(Toast::new(&format!("Found {} placeholders", count)));
                    update();
                }
                Err(e) => overlay.add_toast(Toast::new(&format!("Scan failed: {}", e))),
            }
        }
    });

    NavigationPage::builder()
        .title("odrive Manager")
        .child(&outer)
        .can_pop(false)
        .build()
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

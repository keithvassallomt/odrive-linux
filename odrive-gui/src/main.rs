use libadwaita as adw;
use adw::prelude::*;
use adw::gtk as gtk;
use adw::{ActionRow, ApplicationWindow, HeaderBar, Toast, ToastOverlay};
use gtk::{Application, Box, ListBox, Orientation, Button, Label};
use odrive_core::{OdriveAgent, OdriveDb};
use std::rc::Rc;

fn main() {
    let application = Application::builder()
        .application_id("ai.openclaw.odrive-linux")
        .build();

    application.connect_activate(|app| {
        let agent = Rc::new(OdriveAgent::new());
        
        let overlay = ToastOverlay::new();
        let content = Box::new(Orientation::Vertical, 0);
        overlay.set_child(Some(&content));

        // Header
        let header = HeaderBar::new();
        content.append(&header);

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

        content.append(&list);

        // Mounts List
        let mount_list_title = Label::builder()
            .label("Active Mounts")
            .xalign(0.0)
            .margin_start(28)
            .margin_top(12)
            .build();
        mount_list_title.add_css_class("heading");
        content.append(&mount_list_title);

        let mount_list = ListBox::new();
        mount_list.add_css_class("boxed-list");
        mount_list.set_margin_top(6);
        mount_list.set_margin_bottom(24);
        mount_list.set_margin_start(24);
        mount_list.set_margin_end(24);
        content.append(&mount_list);

        // Update function
        let update_ui = {
            let agent = agent.clone();
            let status_label = status_label.clone();
            let start_stop_btn = start_stop_btn.clone();
            let db_row = db_row.clone();
            let mount_list = mount_list.clone();
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

        let window = ApplicationWindow::builder()
            .application(app)
            .title("odrive Manager")
            .default_width(600)
            .default_height(400)
            .content(&overlay)
            .build();

        window.present();
    });

    application.run();
}

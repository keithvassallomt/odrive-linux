use libadwaita as adw;
use adw::prelude::*;
use adw::gtk as gtk;
use adw::{ActionRow, ApplicationWindow, HeaderBar, Toast, ToastOverlay};
use gtk::{Application, Box, ListBox, Orientation, Button, Label};
use odrive_core::OdriveAgent;
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
        list.set_margin_bottom(24);
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

        // Scan Group
        let scan_row = ActionRow::builder()
            .title("Placeholder Database")
            .subtitle("Scan local mount for placeholders")
            .build();

        let scan_btn = Button::builder()
            .label("Scan Now")
            .valign(gtk::Align::Center)
            .build();
        scan_row.add_suffix(&scan_btn);
        list.append(&scan_row);

        content.append(&list);

        // Update function
        let update_ui = {
            let agent = agent.clone();
            let status_label = status_label.clone();
            let start_stop_btn = start_stop_btn.clone();
            move || {
                let is_running = agent.is_running();
                status_label.set_label(if is_running { "Running" } else { "Stopped" });
                start_stop_btn.set_label(if is_running { "Stop" } else { "Start" });
            }
        };

        // Initial update
        update_ui();

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
            let overlay = overlay.clone();
            move |_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/home/keith".to_string());
                let mount_path = format!("{}/odrive", home);
                match agent.scan_placeholders(&mount_path) {
                    Ok(count) => overlay.add_toast(Toast::new(&format!("Found {} placeholders", count))),
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

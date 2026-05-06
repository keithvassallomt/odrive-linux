//! Backup tab — the dashboard's second visible tab.
//!
//! odrive's backup model is one-way local→remote on a fixed schedule
//! (24 h default; the agent's internal `backupIntervalMinutes` knob has
//! no public CLI/IPC verb so we don't expose schedule overrides).
//! Modified files end up date-stamped in the destination; users
//! "restore" by browsing the destination through the odrive web app or
//! the storage source's own client (per the upstream User Manual).
//!
//! ## Surface we have to drive the UI
//!
//! - `odrive backup <local> <remote>` — register a job. Premium-gated
//!   server-side; failures bubble up as toasts.
//! - `odrive removebackup <jobId>` — drop a job locally.
//! - `odrive backupnow` — kick the scheduler. Bulk only — neither the
//!   CLI nor the IPC has a per-job force-run.
//! - `odrive status --backups` → `BackupJob` list (jobId / localPath /
//!   remotePath / status). The IPC carries richer per-job fields
//!   (`processing`, `size`, `percentComplete`) but the CLI strips them;
//!   we don't surface progress in v1.
//! - `lastBackupTime` / `timeTillNextBackup` (pre-formatted strings) —
//!   only reachable via direct IPC. `OdriveAgent::get_backup_schedule`
//!   uses the in-tree `AgentIpc::status` primitive for this read.
//!
//! ## What we don't surface (intentional)
//!
//! - Per-job progress bar (would need a second IPC pump alongside the
//!   CLI shell-out path; trade-off: defer until users ask for it).
//! - Schedule override (no public verb — only an internal advanced
//!   property the agent reads from disk on startup).
//! - Remote-folder browsing for the destination picker (no IPC for it;
//!   the SEE-encrypted access token wall already closed that door).
//!   Users type a remote path; the dialog has a shortcut button to
//!   open odrive's web manager so they can copy the path visually.

use crate::worker;
use adw::gtk::glib;
use adw::prelude::*;
use adw::{
    ActionRow, EntryRow, HeaderBar, MessageDialog, PreferencesGroup, PreferencesPage,
    ResponseAppearance, Toast, ToastOverlay, ToolbarView, Window,
};
use libadwaita as adw;
use odrive_core::{BackupJob, BackupSchedule, OdriveAgent};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// Build the Backup tab content. Returns a `PreferencesPage` matching
/// the Trash / Mount & Sync idiom: actions live in a header
/// PreferencesGroup, the dynamic body in a second group below.
pub fn build_backup_page(agent: Rc<OdriveAgent>, overlay: ToastOverlay) -> PreferencesPage {
    let page = PreferencesPage::new();
    page.set_margin_top(12);

    let actions_group = PreferencesGroup::new();

    let header_row = ActionRow::builder()
        .title("Backup")
        .subtitle("Folders backed up to remote storage.")
        .build();
    let backup_now_btn = adw::gtk::Button::builder()
        .label("Back up now")
        .tooltip_text("Trigger every registered backup job immediately")
        .valign(adw::gtk::Align::Center)
        .build();
    let add_btn = adw::gtk::Button::builder()
        .label("Add backup…")
        .tooltip_text("Register a new backup job")
        .css_classes(["suggested-action"])
        .valign(adw::gtk::Align::Center)
        .build();
    let header_suffix = adw::gtk::Box::builder()
        .orientation(adw::gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(adw::gtk::Align::Center)
        .build();
    header_suffix.append(&backup_now_btn);
    header_suffix.append(&add_btn);
    header_row.add_suffix(&header_suffix);
    actions_group.add(&header_row);

    let schedule_row = ActionRow::builder()
        .title("Schedule")
        .subtitle("—")
        .build();
    actions_group.add(&schedule_row);
    page.add(&actions_group);

    let jobs_group = PreferencesGroup::new();
    page.add(&jobs_group);
    let rows: Rc<RefCell<Vec<ActionRow>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let backup_now_for_cb = backup_now_btn.clone();
        backup_now_btn.connect_clicked(move |_| {
            let agent_inner = (*agent).clone();
            let overlay_w = overlay.clone();
            backup_now_for_cb.set_sensitive(false);
            let backup_now_reset = backup_now_for_cb.clone();
            worker::spawn(
                move || agent_inner.backup_now().map(|_| ()),
                move |result| {
                    backup_now_reset.set_sensitive(true);
                    let toast = match result {
                        Ok(()) => Toast::new("Backup queued — running now."),
                        Err(e) => Toast::new(&format!("Couldn't trigger backup: {}", e)),
                    };
                    overlay_w.add_toast(toast);
                },
            );
        });
    }

    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let add_for_cb = add_btn.clone();
        add_btn.connect_clicked(move |_| {
            present_add_backup_dialog(agent.clone(), overlay.clone(), &add_for_cb);
        });
    }

    repopulate(&agent, &overlay, &jobs_group, &rows, &schedule_row);
    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let jobs_group = jobs_group.clone();
        let rows = rows.clone();
        let schedule_row = schedule_row.clone();
        glib::timeout_add_seconds_local(5, move || {
            repopulate(&agent, &overlay, &jobs_group, &rows, &schedule_row);
            glib::ControlFlow::Continue
        });
    }

    page
}

fn repopulate(
    agent: &Rc<OdriveAgent>,
    overlay: &ToastOverlay,
    group: &PreferencesGroup,
    rows: &Rc<RefCell<Vec<ActionRow>>>,
    schedule_row: &ActionRow,
) {
    // Schedule strip — IPC-direct read; failure-tolerant, falls back
    // to em-dash so a transient agent hiccup doesn't make the row
    // flicker between text and an error.
    match agent.get_backup_schedule() {
        Ok(BackupSchedule {
            last_backup_time,
            time_till_next,
        }) => {
            schedule_row.set_subtitle(&format_schedule(
                last_backup_time.as_deref(),
                time_till_next.as_deref(),
            ));
        }
        Err(_) => {
            schedule_row.set_subtitle("—");
        }
    }

    let jobs = agent.get_backup_jobs().unwrap_or_default();

    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }

    if jobs.is_empty() {
        let row = ActionRow::builder()
            .title("No backups configured")
            .subtitle("Click \"Add backup…\" to copy a local folder to remote storage.")
            .build();
        group.add(&row);
        rows.borrow_mut().push(row);
        return;
    }

    for job in &jobs {
        let row = build_job_row(agent, overlay, job);
        group.add(&row);
        rows.borrow_mut().push(row);
    }
}

fn format_schedule(last: Option<&str>, next: Option<&str>) -> String {
    match (last, next) {
        (Some(l), Some(n)) => format!("Last: {}  •  {}", l.trim(), n.trim()),
        (Some(l), None) => format!("Last: {}", l.trim()),
        (None, Some(n)) => n.trim().to_string(),
        (None, None) => "—".to_string(),
    }
}

fn build_job_row(
    agent: &Rc<OdriveAgent>,
    overlay: &ToastOverlay,
    job: &BackupJob,
) -> ActionRow {
    let basename = Path::new(&job.local_path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| job.local_path.clone());

    let subtitle = format!(
        "{}  →  {}",
        job.local_path,
        if job.remote_path.is_empty() {
            "(remote)"
        } else {
            &job.remote_path
        }
    );

    let row = ActionRow::builder()
        .title(glib::markup_escape_text(&basename).to_string())
        .subtitle(glib::markup_escape_text(&subtitle).to_string())
        .build();

    // Status badge — simple text label so we don't need extra theming.
    if !job.status.is_empty() {
        let status_label = adw::gtk::Label::builder()
            .label(&job.status)
            .css_classes(["dim-label", "caption"])
            .valign(adw::gtk::Align::Center)
            .build();
        row.add_suffix(&status_label);
    }

    let suffix_box = adw::gtk::Box::builder()
        .orientation(adw::gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(adw::gtk::Align::Center)
        .build();

    let open_btn = adw::gtk::Button::builder()
        .icon_name("web-browser-symbolic")
        .tooltip_text("Open the odrive web manager")
        .css_classes(["flat"])
        .valign(adw::gtk::Align::Center)
        .build();
    open_btn.connect_clicked(|_| {
        let _ = adw::gtk::glib::spawn_command_line_async(
            "xdg-open https://www.odrive.com/account/myodrive",
        );
    });
    suffix_box.append(&open_btn);

    let remove_btn = adw::gtk::Button::builder()
        .label("Remove")
        .tooltip_text("Stop running this backup. Already-backed-up files in remote stay where they are.")
        .css_classes(["flat", "destructive-action"])
        .valign(adw::gtk::Align::Center)
        .build();
    {
        let agent = agent.clone();
        let overlay_w = overlay.clone();
        let job = job.clone();
        let row_anchor = row.clone();
        remove_btn.connect_clicked(move |_| {
            confirm_and_remove(agent.clone(), overlay_w.clone(), job.clone(), &row_anchor);
        });
    }
    suffix_box.append(&remove_btn);

    row.add_suffix(&suffix_box);
    row
}

fn confirm_and_remove(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    job: BackupJob,
    anchor: &ActionRow,
) {
    let window = anchor
        .root()
        .and_then(|r| r.downcast::<adw::gtk::Window>().ok());
    let dialog = MessageDialog::builder()
        .heading("Remove this backup?")
        .body(format!(
            "Stops backing up {} to {}. Already-uploaded files in the destination stay where they are.",
            job.local_path, job.remote_path
        ))
        .modal(true)
        .build();
    if let Some(w) = window.as_ref() {
        dialog.set_transient_for(Some(w));
    }
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_response_appearance("remove", ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |dlg, response| {
        if response == "remove" {
            let agent_inner = (*agent).clone();
            let overlay_w = overlay.clone();
            let job_id = job.job_id.clone();
            worker::spawn(
                move || agent_inner.remove_backup_job(&job_id).map(|_| ()),
                move |result| {
                    let toast = match result {
                        Ok(()) => Toast::new("Backup removed."),
                        Err(e) => Toast::new(&format!("Couldn't remove backup: {}", e)),
                    };
                    overlay_w.add_toast(toast);
                },
            );
        }
        dlg.close();
    });
    dialog.present();
}

/// "Add backup…" — modal `Adw.Window` shaped like a small GNOME
/// Settings sheet: HeaderBar with Cancel (left) + Save (right,
/// suggested-action, disabled until the form is valid), PreferencesPage
/// with native Adw row widgets (`ActionRow` for the folder picker,
/// `EntryRow` for the remote path). Replaces an earlier
/// `Adw.MessageDialog`-with-extra-child approach that crammed bare
/// labels and a Gtk.Entry into the message dialog body — visually
/// jarring next to the rest of the app.
fn present_add_backup_dialog(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    anchor: &adw::gtk::Button,
) {
    let parent = anchor
        .root()
        .and_then(|r| r.downcast::<adw::gtk::Window>().ok());

    let win = Window::builder()
        .title("Add backup")
        .modal(true)
        .default_width(520)
        .default_height(420)
        .build();
    if let Some(p) = parent.as_ref() {
        win.set_transient_for(Some(p));
    }

    // --- Header bar with Cancel / Save buttons ----------------------------
    let header = HeaderBar::new();
    let cancel_btn = adw::gtk::Button::builder().label("Cancel").build();
    let save_btn = adw::gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);

    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);

    let page = PreferencesPage::new();
    page.set_margin_top(6);

    // --- Source group: local folder picker --------------------------------
    let source_group = PreferencesGroup::builder()
        .title("Source")
        .description("The local folder you want backed up.")
        .build();
    let local_row = ActionRow::builder()
        .title("Local folder")
        .subtitle("(none chosen)")
        .build();
    let local_pick_btn = adw::gtk::Button::builder()
        .label("Choose folder…")
        .valign(adw::gtk::Align::Center)
        .build();
    local_row.add_suffix(&local_pick_btn);
    source_group.add(&local_row);
    page.add(&source_group);

    // --- Destination group: remote path entry -----------------------------
    let dest_group = PreferencesGroup::builder()
        .title("Destination")
        .description(
            "The remote path where backed-up versions will be stored. \
             Type the path manually — odrive doesn't expose a remote folder \
             picker — or open the web manager to copy one.",
        )
        .build();
    let remote_row = EntryRow::builder()
        .title("Remote path")
        .build();
    // EntryRow doesn't have a placeholder property; the .title is the
    // floating label. Seed an example by setting the gtk::Entry's
    // placeholder via the underlying widget when first focused.
    let open_web_btn = adw::gtk::Button::builder()
        .icon_name("web-browser-symbolic")
        .tooltip_text("Open the odrive web manager so you can copy a remote path.")
        .css_classes(["flat"])
        .valign(adw::gtk::Align::Center)
        .build();
    open_web_btn.connect_clicked(|_| {
        let _ = adw::gtk::glib::spawn_command_line_async(
            "xdg-open https://www.odrive.com/account/myodrive",
        );
    });
    remote_row.add_suffix(&open_web_btn);
    dest_group.add(&remote_row);
    page.add(&dest_group);

    // --- Restore note (per upstream docs) ---------------------------------
    let note_group = PreferencesGroup::new();
    let note_row = ActionRow::builder()
        .title("How to restore")
        .subtitle(
            "Open the destination folder in odrive's web app and download the \
             version of the file you want.",
        )
        .build();
    note_group.add(&note_row);
    page.add(&note_group);

    toolbar.set_content(Some(&page));
    win.set_content(Some(&toolbar));

    // --- Wiring: validation + handlers ------------------------------------
    let local_path: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    {
        let win_for_pick = win.clone();
        let local_row_for_pick = local_row.clone();
        let local_path_for_pick = local_path.clone();
        let remote_row_for_pick = remote_row.clone();
        let save_btn_for_pick = save_btn.clone();
        local_pick_btn.connect_clicked(move |_| {
            let local_row_inner = local_row_for_pick.clone();
            let local_path_w = local_path_for_pick.clone();
            let remote_row_w = remote_row_for_pick.clone();
            let save_btn_w = save_btn_for_pick.clone();
            let file_dialog = adw::gtk::FileDialog::builder()
                .title("Choose backup source folder")
                .modal(true)
                .build();
            let cancellable: Option<&adw::gtk::gio::Cancellable> = None;
            file_dialog.select_folder(Some(&win_for_pick), cancellable, move |result| {
                if let Ok(folder) = result {
                    if let Some(p) = folder.path() {
                        let p_str = p.to_string_lossy().into_owned();
                        local_row_inner.set_subtitle(&p_str);
                        *local_path_w.borrow_mut() = Some(p_str);
                        save_btn_w.set_sensitive(!remote_row_w.text().is_empty());
                    }
                }
            });
        });
    }

    {
        let local_path_for_remote = local_path.clone();
        let save_btn_for_remote = save_btn.clone();
        remote_row.connect_changed(move |entry| {
            let local_set = local_path_for_remote.borrow().is_some();
            let remote_set = !entry.text().is_empty();
            save_btn_for_remote.set_sensitive(local_set && remote_set);
        });
    }

    {
        let win_for_cancel = win.clone();
        cancel_btn.connect_clicked(move |_| {
            win_for_cancel.close();
        });
    }

    {
        let agent_for_save = agent.clone();
        let overlay_for_save = overlay.clone();
        let local_path_for_save = local_path.clone();
        let remote_row_for_save = remote_row.clone();
        let win_for_save = win.clone();
        save_btn.connect_clicked(move |_| {
            let local = local_path_for_save.borrow().clone();
            let remote = remote_row_for_save.text().to_string();
            let trimmed = remote.trim().to_string();
            if let (Some(local), false) = (local.clone(), trimmed.is_empty()) {
                let agent_inner = (*agent_for_save).clone();
                let overlay_w = overlay_for_save.clone();
                let win_close = win_for_save.clone();
                worker::spawn(
                    move || agent_inner.add_backup_job(&local, &trimmed).map(|_| ()),
                    move |result| {
                        let toast = match result {
                            Ok(()) => Toast::new("Backup added."),
                            Err(e) => Toast::new(&format!("Couldn't add backup: {}", e)),
                        };
                        overlay_w.add_toast(toast);
                        win_close.close();
                    },
                );
            }
        });
    }

    win.present();
}


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
    ActionRow, MessageDialog, PreferencesGroup, PreferencesPage, ResponseAppearance, Toast,
    ToastOverlay,
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
        .tooltip_text("Open the destination in odrive's web app")
        .css_classes(["flat"])
        .valign(adw::gtk::Align::Center)
        .build();
    {
        let remote_path = job.remote_path.clone();
        let overlay_w = overlay.clone();
        open_btn.connect_clicked(move |_| {
            // The web app accepts a remote path verbatim under
            // /browse/. We don't have the mount table here (backups
            // aren't tied to a mount) so we lean on the
            // already-percent-encoded build-web-url helper directly.
            let path = remote_path.trim_start_matches('/');
            let url = format!(
                "https://www.odrive.com/browse/{}",
                percent_encode_minimal(path)
            );
            let _ = adw::gtk::glib::spawn_command_line_async(&format!(
                "xdg-open {}",
                shell_escape(&url)
            ));
            overlay_w.add_toast(Toast::new("Opening backup destination…"));
        });
    }
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

/// "Add backup…" — modal MessageDialog with an extra-child VBox
/// containing the local folder picker + remote path entry. We use
/// MessageDialog rather than pushing a NavigationPage so the form
/// stays visually distinct from the rest of the dashboard.
fn present_add_backup_dialog(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    anchor: &adw::gtk::Button,
) {
    let window = anchor
        .root()
        .and_then(|r| r.downcast::<adw::gtk::Window>().ok());

    let dialog = MessageDialog::builder()
        .heading("Add backup")
        .body("Choose a local folder and the remote destination it should be backed up to.")
        .modal(true)
        .build();
    if let Some(w) = window.as_ref() {
        dialog.set_transient_for(Some(w));
    }

    let content = adw::gtk::Box::builder()
        .orientation(adw::gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(8)
        .margin_bottom(8)
        .build();

    // --- Local folder picker -----------------------------------------------
    let local_label = adw::gtk::Label::builder()
        .label("Local folder")
        .halign(adw::gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    content.append(&local_label);

    let local_row = adw::gtk::Box::builder()
        .orientation(adw::gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let local_path: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let local_display = adw::gtk::Label::builder()
        .label("(none chosen)")
        .halign(adw::gtk::Align::Start)
        .hexpand(true)
        .ellipsize(adw::gtk::pango::EllipsizeMode::Middle)
        .css_classes(["dim-label"])
        .build();
    let local_pick_btn = adw::gtk::Button::builder()
        .label("Choose folder…")
        .build();
    local_row.append(&local_display);
    local_row.append(&local_pick_btn);
    content.append(&local_row);

    // --- Remote path entry --------------------------------------------------
    let remote_label = adw::gtk::Label::builder()
        .label("Remote path")
        .halign(adw::gtk::Align::Start)
        .margin_top(8)
        .css_classes(["heading"])
        .build();
    content.append(&remote_label);

    let remote_entry = adw::gtk::Entry::builder()
        .placeholder_text("e.g. /Google Drive/Backups/Documents")
        .build();
    content.append(&remote_entry);

    let hint_row = adw::gtk::Box::builder()
        .orientation(adw::gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let hint_label = adw::gtk::Label::builder()
        .label(
            "The agent doesn't expose a remote-folder picker — type the path or open the web manager to copy one.",
        )
        .halign(adw::gtk::Align::Start)
        .hexpand(true)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    let open_web_btn = adw::gtk::Button::builder()
        .icon_name("web-browser-symbolic")
        .tooltip_text("Open the odrive web manager")
        .css_classes(["flat"])
        .valign(adw::gtk::Align::Center)
        .build();
    open_web_btn.connect_clicked(|_| {
        let _ = adw::gtk::glib::spawn_command_line_async("xdg-open https://www.odrive.com/browse/");
    });
    hint_row.append(&hint_label);
    hint_row.append(&open_web_btn);
    content.append(&hint_row);

    // --- Restore note (per upstream docs) ----------------------------------
    let restore_note = adw::gtk::Label::builder()
        .label(
            "To restore later, open the destination folder and download the version you want.",
        )
        .halign(adw::gtk::Align::Start)
        .wrap(true)
        .xalign(0.0)
        .margin_top(4)
        .css_classes(["dim-label", "caption"])
        .build();
    content.append(&restore_note);

    dialog.set_extra_child(Some(&content));

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("save", "Save");
    dialog.set_response_appearance("save", ResponseAppearance::Suggested);
    dialog.set_response_enabled("save", false);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");

    // Save is enabled only when both local and remote have been
    // populated. Keep the predicate cheap — re-check on every change.
    let dialog_for_local = dialog.clone();
    let dialog_for_remote = dialog.clone();
    let local_path_for_pick = local_path.clone();
    let local_display_for_pick = local_display.clone();
    let remote_entry_for_pick = remote_entry.clone();
    let dialog_parent = window.clone();
    local_pick_btn.connect_clicked(move |_| {
        let dialog_inner = dialog_for_local.clone();
        let local_path_w = local_path_for_pick.clone();
        let local_display_w = local_display_for_pick.clone();
        let remote_entry_w = remote_entry_for_pick.clone();
        let file_dialog = adw::gtk::FileDialog::builder()
            .title("Choose backup source folder")
            .modal(true)
            .build();
        let parent = dialog_parent.clone();
        let cancellable: Option<&adw::gtk::gio::Cancellable> = None;
        file_dialog.select_folder(parent.as_ref(), cancellable, move |result| {
            if let Ok(folder) = result {
                if let Some(p) = folder.path() {
                    let p_str = p.to_string_lossy().into_owned();
                    local_display_w.set_label(&p_str);
                    local_display_w.remove_css_class("dim-label");
                    *local_path_w.borrow_mut() = Some(p_str);
                    let remote_set = !remote_entry_w.text().is_empty();
                    dialog_inner.set_response_enabled("save", remote_set);
                }
            }
        });
    });

    let local_path_for_remote = local_path.clone();
    remote_entry.connect_changed(move |entry| {
        let local_set = local_path_for_remote.borrow().is_some();
        let remote_set = !entry.text().is_empty();
        dialog_for_remote.set_response_enabled("save", local_set && remote_set);
    });

    let agent_for_save = agent.clone();
    let overlay_for_save = overlay.clone();
    let local_path_for_save = local_path.clone();
    let remote_entry_for_save = remote_entry.clone();
    dialog.connect_response(None, move |dlg, response| {
        if response == "save" {
            let local = local_path_for_save.borrow().clone();
            let remote = remote_entry_for_save.text().to_string();
            if let (Some(local), remote) = (local, remote) {
                let trimmed = remote.trim().to_string();
                if !trimmed.is_empty() {
                    let agent_inner = (*agent_for_save).clone();
                    let overlay_w = overlay_for_save.clone();
                    worker::spawn(
                        move || agent_inner.add_backup_job(&local, &trimmed).map(|_| ()),
                        move |result| {
                            let toast = match result {
                                Ok(()) => Toast::new("Backup added."),
                                Err(e) => Toast::new(&format!("Couldn't add backup: {}", e)),
                            };
                            overlay_w.add_toast(toast);
                        },
                    );
                }
            }
        }
        dlg.close();
    });
    dialog.present();
}

/// Bare-bones percent-encoder for path segments so `xdg-open` gets a
/// clean URL when we synthesize the destination web URL. Identical
/// rule to `odrive_core::build_web_url`'s encoder (`-_.~/` plus
/// alphanumerics passed through). We don't reach into odrive-core for
/// it because that helper is bundled with mount lookup; a job's
/// `remote_path` is already a slash-prefixed remote path.
fn percent_encode_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let safe = matches!(b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' | b'/');
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Tiny single-arg shell-quoter. Only used to wrap a URL that already
/// went through percent-encoding, so we just need to defend against the
/// odd "&" or "?" landing in glib::spawn_command_line_async's tokenizer.
fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

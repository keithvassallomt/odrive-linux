//! Trash tab — the dashboard's third visible tab.
//!
//! Built around `odrive status --trash` (read) and `odrive restoretrash`
//! / `odrive emptytrash` (mutate). Both mutating ops are bulk-only at
//! the agent IPC: no per-item form exists. macOS/Windows desktop GUIs
//! reach `TrashController.restore_delete(o2Path)` via in-process
//! Python; the JSON socket the Linux Manager talks to does not expose
//! it. Confirmed by decompiling `ProtocolCommands.pyc` and probing the
//! live agent.
//!
//! ## Per-item Restore workaround
//!
//! The user explicitly opted in to this trade-off. Per-row "Restore":
//!   1. Capture the current trash list.
//!   2. Call `agent.restore_trash()` — restores ALL items as
//!      placeholders.
//!   3. For each item the user did NOT pick, delete its placeholder on
//!      disk (`<path>.cloud` for files, `<path>.cloudf` for folder
//!      placeholders — the agent recreates them as empty files via
//!      `apply_local_add_empty_file`).
//!   4. The agent's periodic local scan (~30 min cadence) detects the
//!      re-deletions and puts those items back in trash. Until that
//!      next scan, the trash list looks empty even though only one
//!      item was meant to be restored.
//!
//! That trailing 30-minute window is unavoidable from outside the
//! agent — there's no manual "scan now" command. Communicate it in
//! the confirm dialog and the post-action toast so users aren't
//! surprised by the gap.
//!
//! Per-item permanent delete is NOT offered. The mirror workaround
//! (empty-all → re-delete only the chosen item locally) would
//! permanently destroy the items the user wanted kept. Bulk
//! "Empty Trash" with a clear confirmation is the only delete UI.

use crate::worker;
use adw::gtk::glib;
use adw::prelude::*;
use adw::{
    ActionRow, MessageDialog, PreferencesGroup, PreferencesPage, ResponseAppearance, Toast,
    ToastOverlay,
};
use libadwaita as adw;
use odrive_core::{OdriveAgent, TrashItem};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// Build the Trash tab content. Returns a `PreferencesPage` to slot
/// into the dashboard's `ViewStack` like the Mount & Sync tab does.
///
/// The page holds a single `PreferencesGroup` with the trash actions
/// in the header suffix and one row per trashed item. Rendering is
/// driven by `repopulate`, which is called once at construction and
/// again on a 5 s timer for the lifetime of the page.
pub fn build_trash_page(agent: Rc<OdriveAgent>, overlay: ToastOverlay) -> PreferencesPage {
    let page = PreferencesPage::new();
    page.set_margin_top(12);

    // Page-level action bar: a single ActionRow inside a header-less
    // PreferencesGroup, with the buttons as suffix widgets. This gives
    // them a normal ListBoxRow vertical extent (~40 px) instead of
    // letting them inherit the group-header height the way
    // `set_header_suffix` does — that path produced absurdly tall
    // buttons whenever the group description wrapped to multiple lines.
    let actions_group = PreferencesGroup::new();
    let actions_row = ActionRow::builder()
        .title("Trash")
        .subtitle("Items removed locally that the agent has trashed.")
        .build();
    let restore_all_btn = adw::gtk::Button::builder()
        .label("Restore All")
        .tooltip_text("Restore every item in the trash as a placeholder")
        .valign(adw::gtk::Align::Center)
        .build();
    let empty_btn = adw::gtk::Button::builder()
        .label("Empty Trash")
        .tooltip_text("Permanently delete every item in the trash")
        .css_classes(["destructive-action"])
        .valign(adw::gtk::Align::Center)
        .build();
    let suffix_box = adw::gtk::Box::builder()
        .orientation(adw::gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(adw::gtk::Align::Center)
        .build();
    suffix_box.append(&restore_all_btn);
    suffix_box.append(&empty_btn);
    actions_row.add_suffix(&suffix_box);
    actions_group.add(&actions_row);
    page.add(&actions_group);

    let group = PreferencesGroup::new();
    page.add(&group);

    // Track ActionRows so each tick can swap them rather than rebuilding
    // the whole group (avoids flicker and lets a per-row Restore in
    // flight retain its widget reference).
    let rows: Rc<RefCell<Vec<ActionRow>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let group = group.clone();
        let rows = rows.clone();
        let restore_all_for_cb = restore_all_btn.clone();
        let empty_for_cb = empty_btn.clone();
        restore_all_btn.connect_clicked(move |btn| {
            confirm_and_run_bulk(
                &agent,
                &overlay,
                &group,
                &rows,
                &restore_all_for_cb,
                &empty_for_cb,
                BulkAction::Restore,
                btn,
            );
        });
    }
    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let group = group.clone();
        let rows = rows.clone();
        let restore_all_for_cb = restore_all_btn.clone();
        let empty_for_cb = empty_btn.clone();
        empty_btn.connect_clicked(move |btn| {
            confirm_and_run_bulk(
                &agent,
                &overlay,
                &group,
                &rows,
                &restore_all_for_cb,
                &empty_for_cb,
                BulkAction::Empty,
                btn,
            );
        });
    }

    // Initial paint + the 5 s refresh poll. The poll lives for the
    // process — the Trash tab is part of the long-lived dashboard
    // window, same lifetime model as the Mount & Sync poll.
    repopulate(
        &agent,
        &overlay,
        &group,
        &rows,
        &restore_all_btn,
        &empty_btn,
    );
    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let group = group.clone();
        let rows = rows.clone();
        let restore_all_btn = restore_all_btn.clone();
        let empty_btn = empty_btn.clone();
        glib::timeout_add_seconds_local(5, move || {
            repopulate(
                &agent,
                &overlay,
                &group,
                &rows,
                &restore_all_btn,
                &empty_btn,
            );
            glib::ControlFlow::Continue
        });
    }

    page
}

/// Refresh the listing. Called once at construction and on every tick.
/// Re-reads `odrive status --trash` synchronously — this is a cheap
/// status read, not worth a worker thread (parity with the existing
/// Mount & Sync poll).
fn repopulate(
    agent: &Rc<OdriveAgent>,
    overlay: &ToastOverlay,
    group: &PreferencesGroup,
    rows: &Rc<RefCell<Vec<ActionRow>>>,
    restore_all_btn: &adw::gtk::Button,
    empty_btn: &adw::gtk::Button,
) {
    let items = match agent.get_trash_items() {
        Ok(items) => items,
        Err(_) => Vec::new(), // Agent down / not authenticated → empty list rather than error toast on every tick.
    };

    // Drop existing rows. ActionRow inherits ListBoxRow which is what
    // PreferencesGroup keeps; `remove` is the documented way.
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }

    let empty = items.is_empty();
    restore_all_btn.set_sensitive(!empty);
    empty_btn.set_sensitive(!empty);

    if empty {
        let row = ActionRow::builder()
            .title("Trash is empty")
            .subtitle("Items appear here after the agent's periodic local scan detects a deletion.")
            .build();
        group.add(&row);
        rows.borrow_mut().push(row);
        return;
    }

    for item in &items {
        let row = build_item_row(agent, overlay, &items, item);
        group.add(&row);
        rows.borrow_mut().push(row);
    }
}

fn build_item_row(
    agent: &Rc<OdriveAgent>,
    overlay: &ToastOverlay,
    all_items: &[TrashItem],
    item: &TrashItem,
) -> ActionRow {
    let basename = Path::new(&item.local_path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| item.local_path.clone());
    let parent = Path::new(&item.local_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let row = ActionRow::builder()
        .title(glib::markup_escape_text(&basename).to_string())
        .subtitle(glib::markup_escape_text(&parent).to_string())
        .build();

    let restore_btn = adw::gtk::Button::builder()
        .label("Restore")
        .valign(adw::gtk::Align::Center)
        .css_classes(["flat"])
        .build();

    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let target = item.clone();
        let snapshot: Vec<TrashItem> = all_items.to_vec();
        let row_for_btn = row.clone();
        restore_btn.connect_clicked(move |_| {
            confirm_and_restore_one(
                agent.clone(),
                overlay.clone(),
                snapshot.clone(),
                target.clone(),
                row_for_btn.clone(),
            );
        });
    }

    row.add_suffix(&restore_btn);
    row
}

#[derive(Copy, Clone)]
enum BulkAction {
    Restore,
    Empty,
}

fn confirm_and_run_bulk(
    agent: &Rc<OdriveAgent>,
    overlay: &ToastOverlay,
    group: &PreferencesGroup,
    rows: &Rc<RefCell<Vec<ActionRow>>>,
    restore_all_btn: &adw::gtk::Button,
    empty_btn: &adw::gtk::Button,
    action: BulkAction,
    anchor: &adw::gtk::Button,
) {
    let window = anchor
        .root()
        .and_then(|r| r.downcast::<adw::gtk::Window>().ok());

    let (heading, body, response_id, response_label, appearance) = match action {
        BulkAction::Restore => (
            "Restore everything in trash?",
            "All trashed items will be brought back as placeholders.",
            "restore",
            "Restore All",
            ResponseAppearance::Suggested,
        ),
        BulkAction::Empty => (
            "Empty the trash?",
            "Every trashed item will be permanently deleted from your cloud storage. This cannot be undone.",
            "empty",
            "Empty Trash",
            ResponseAppearance::Destructive,
        ),
    };

    let dialog = MessageDialog::builder()
        .heading(heading)
        .body(body)
        .modal(true)
        .build();
    if let Some(w) = window.as_ref() {
        dialog.set_transient_for(Some(w));
    }
    dialog.add_response("cancel", "Cancel");
    dialog.add_response(response_id, response_label);
    dialog.set_response_appearance(response_id, appearance);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    // The 5 s poll picks up the post-action state automatically — no
    // need to drive an explicit repopulate from the worker callback.
    let _ = (group, rows, restore_all_btn, empty_btn);

    let agent = agent.clone();
    let overlay = overlay.clone();
    dialog.connect_response(None, move |dlg, response| {
        if response == response_id {
            let agent_inner = (*agent).clone();
            let overlay_w = overlay.clone();
            worker::spawn(
                move || match action {
                    BulkAction::Restore => agent_inner.restore_trash().map(|_| ()),
                    BulkAction::Empty => agent_inner.empty_trash().map(|_| ()),
                },
                move |result| {
                    let toast = match (action, result) {
                        (BulkAction::Restore, Ok(())) => {
                            Toast::new("Trash restored as placeholders")
                        }
                        (BulkAction::Empty, Ok(())) => Toast::new("Trash emptied"),
                        (_, Err(e)) => Toast::new(&format!("Trash action failed: {}", e)),
                    };
                    overlay_w.add_toast(toast);
                },
            );
        }
        dlg.close();
    });
    dialog.present();
}

fn confirm_and_restore_one(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    snapshot: Vec<TrashItem>,
    target: TrashItem,
    anchor: ActionRow,
) {
    let window = anchor
        .root()
        .and_then(|r| r.downcast::<adw::gtk::Window>().ok());

    // Single-item trash → no workaround needed; restore_trash restores
    // exactly the one item. Skip the long warning.
    let need_workaround = snapshot.len() > 1;

    let basename = Path::new(&target.local_path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| target.local_path.clone());

    let body = if need_workaround {
        format!(
            "{basename} comes back as a placeholder. The other trashed \
             items briefly disappear from the trash list and reappear \
             on the agent's next local scan (within ~30 min)."
        )
    } else {
        format!("{basename} comes back as a placeholder.")
    };

    let dialog = MessageDialog::builder()
        .heading("Restore this item?")
        .body(body)
        .modal(true)
        .build();
    if let Some(w) = window.as_ref() {
        dialog.set_transient_for(Some(w));
    }
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("restore", "Restore");
    dialog.set_response_appearance("restore", ResponseAppearance::Suggested);
    dialog.set_default_response(Some("restore"));
    dialog.set_close_response("cancel");

    dialog.connect_response(None, move |dlg, response| {
        if response == "restore" {
            run_restore_one(
                agent.clone(),
                overlay.clone(),
                snapshot.clone(),
                target.clone(),
            );
        }
        dlg.close();
    });
    dialog.present();
}

/// Workaround proper: restore-all on a worker, then delete the
/// placeholders for items we DID NOT want restored.
fn run_restore_one(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    snapshot: Vec<TrashItem>,
    target: TrashItem,
) {
    let to_redelete: Vec<String> = snapshot
        .iter()
        .filter(|i| i.local_path != target.local_path)
        .map(|i| i.local_path.clone())
        .collect();
    let total = snapshot.len();

    let agent_w = (*agent).clone();
    worker::spawn(
        move || {
            agent_w.restore_trash()?;
            // After restore, every item is on disk as a placeholder.
            // Re-delete the ones we don't want kept; the agent will
            // re-trash them on the next periodic scan.
            let mut redelete_failures: Vec<(String, String)> = Vec::new();
            for path in &to_redelete {
                if let Err(e) = remove_placeholder(path) {
                    redelete_failures.push((path.clone(), e));
                }
            }
            Ok::<_, odrive_core::OdriveError>(redelete_failures)
        },
        move |result| match result {
            Ok(redelete_failures) if redelete_failures.is_empty() => {
                let msg = if total > 1 {
                    format!(
                        "Restored 1 item. Other {} returning to trash on the next agent scan.",
                        total - 1
                    )
                } else {
                    "Restored 1 item.".to_string()
                };
                overlay.add_toast(Toast::new(&msg));
            }
            Ok(failures) => {
                eprintln!(
                    "trash: {} placeholder re-delete(s) failed during restore-one workaround:",
                    failures.len()
                );
                for (path, err) in &failures {
                    eprintln!("  {} → {}", path, err);
                }
                overlay.add_toast(Toast::new(&format!(
                    "Restored 1 item; {} placeholder(s) couldn't be cleaned up (see stderr).",
                    failures.len()
                )));
            }
            Err(e) => overlay.add_toast(Toast::new(&format!("Restore failed: {}", e))),
        },
    );
}

/// Remove the placeholder for a trashed-and-just-restored item. The
/// agent re-creates files as `<path>.cloud` and folder-placeholders as
/// `<path>.cloudf` (a single empty file, post-restore — folders only
/// become real directories after a recursive sync expansion). We try
/// both suffixes and remove whichever exists. Falls back to recursive
/// dir removal if a `.cloudf` ever happens to be a directory.
fn remove_placeholder(local_path: &str) -> Result<(), String> {
    for suffix in [".cloud", ".cloudf"] {
        let candidate = format!("{}{}", local_path, suffix);
        let p = Path::new(&candidate);
        match std::fs::symlink_metadata(p) {
            Ok(meta) if meta.is_dir() => {
                return std::fs::remove_dir_all(p)
                    .map_err(|e| format!("remove_dir_all {}: {}", candidate, e));
            }
            Ok(_) => {
                return std::fs::remove_file(p)
                    .map_err(|e| format!("remove_file {}: {}", candidate, e));
            }
            Err(_) => continue,
        }
    }
    Err(format!(
        "no placeholder at {}.cloud or {}.cloudf",
        local_path, local_path
    ))
}

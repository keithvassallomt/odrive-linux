//! Mount detail and per-folder pages. Click a mount on the dashboard
//! → `build_mount_root` pushes a page that either offers a one-time
//! "Expand placeholders" action (heuristic: top-level has no real
//! subdirectories yet) or shows the folder tree.
//!
//! Drilling deeper pushes another page for each subfolder. That page
//! shows two groups:
//!   1. `Folders` — clickable rows that push another page on the same
//!      NavigationView, so navigation is just the standard back-button
//!      stack.
//!   2. `Sync rule` — One-Time or Automatic. One-Time exposes a
//!      "Sync now" button that calls `agent.sync_recursive` without
//!      `--nodownload`. Automatic exposes a Download Threshold combo,
//!      an Apply-to-subfolders switch, and a Save / Delete button.
//!
//! "Delete" semantics work around an upstream limitation: the agent
//! has no foldersyncrule remove command, so we set the threshold to
//! `0` (= never auto-download for this folder) and drop the row from
//! our own `folder_sync_rules` table.
use crate::worker;
use libadwaita as adw;
use adw::prelude::*;
use adw::gtk as gtk;
use adw::{
    ActionRow, ComboRow, HeaderBar, MessageDialog, NavigationPage, NavigationView,
    PreferencesGroup, PreferencesPage, ResponseAppearance, StatusPage, SwitchRow,
    Toast, ToastOverlay, ToolbarView,
};
use gtk::{Align, Button, Entry, Image, Label, StringList};
use odrive_core::{FolderRule, FolderSyncThreshold, OdriveAgent, OdriveDb, OdriveError};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Top-level entry point: open the detail page for a mount root. Same
/// page shape as a regular folder page, except that on first open we
/// may need to run `sync --recursive --nodownload` to materialise the
/// directory tree before any folders can be listed.
pub fn build_mount_root(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    nav: NavigationView,
    mount_path: String,
) -> NavigationPage {
    build_page(agent, overlay, nav, mount_path, true)
}

/// Open the detail page for a non-root folder. Used by the Sync Rules
/// listing on the Mount & Sync tab to jump directly to a rule's
/// folder. Pushes onto the same NavigationView as the mount drill-in
/// flow, so the back button takes the user to wherever they were
/// before clicking the rule.
pub fn build_folder_page(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    nav: NavigationView,
    folder_path: String,
) -> NavigationPage {
    build_page(agent, overlay, nav, folder_path, false)
}

fn build_page(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    nav: NavigationView,
    folder_path: String,
    is_mount_root: bool,
) -> NavigationPage {
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&HeaderBar::new());

    let state = Rc::new(PageState {
        agent,
        overlay,
        nav,
        folder_path: folder_path.clone(),
        is_mount_root,
        toolbar: toolbar.clone(),
    });

    render_into_toolbar(&state);

    let title = if is_mount_root {
        "Mount root".to_string()
    } else {
        Path::new(&folder_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| folder_path.clone())
    };

    let nav_page = NavigationPage::builder()
        .title(&title)
        .child(&toolbar)
        .build();

    // Re-render when the page becomes visible — covers the back-nav
    // case where a child page might have just deleted/added a rule
    // that affects this page's subfolder badges or rule editor.
    // NavigationView keeps pages alive on pop, so without this hook
    // the parent stays stale.
    {
        let state = state.clone();
        nav_page.connect_shown(move |_| {
            render_into_toolbar(&state);
        });
    }

    nav_page
}

struct PageState {
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    nav: NavigationView,
    folder_path: String,
    is_mount_root: bool,
    /// The page's `ToolbarView`. We replace its content in-place when
    /// the page state changes (expansion completes, rule save/delete)
    /// rather than tracking individual children — simpler bookkeeping
    /// and the swap is visually instantaneous.
    toolbar: ToolbarView,
}

/// Build the right content widget for the current state and assign it
/// as the toolbar's content, replacing whatever was there before.
fn render_into_toolbar(state: &Rc<PageState>) {
    let path = Path::new(&state.folder_path);
    // Two distinct "needs expansion" cases: (1) the folder exists as
    // a real directory but its contents are all `.cloudf` placeholders
    // — typical mount-root first-open state; (2) the folder isn't a
    // real directory at all because there's a sibling `.cloudf` taking
    // its place — happens when a user unsyncs a folder and then
    // re-navigates back into it.
    let needs_expansion = !path.is_dir() || (state.is_mount_root && !appears_expanded(path));

    if needs_expansion {
        state.toolbar.set_content(Some(&first_time_setup_widget(state)));
    } else {
        let page = PreferencesPage::new();
        page.set_margin_top(12);

        page.add(&build_subfolders_group(state));
        if !state.is_mount_root {
            page.add(&build_rule_group(state));
        }

        state.toolbar.set_content(Some(&page));
    }
}

/// True iff a real directory has at least one child that's either a real
/// subdirectory or a non-`.cloudf` file. Used only when the path is
/// already known to be a real directory — otherwise the caller treats
/// the folder as unexpanded by definition.
fn appears_expanded(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            return true;
        }
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if !name.ends_with(".cloudf") {
                return true;
            }
        }
    }
    false
}

/// If a `.cloudf` placeholder exists at `<folder_path>.cloudf`, return
/// that placeholder path. Used to drive an `odrive sync` that
/// re-materialises an unsynced folder back into a real directory.
fn placeholder_path(folder_path: &str) -> Option<String> {
    let cloudf = format!("{}.cloudf", folder_path);
    if Path::new(&cloudf).exists() {
        Some(cloudf)
    } else {
        None
    }
}

fn first_time_setup_widget(state: &Rc<PageState>) -> StatusPage {
    let status = StatusPage::builder()
        .icon_name("folder-download-symbolic")
        .title("First-time setup")
        .description(
            "We need to expand the folder placeholders before you can set per-folder sync rules. This won't download any file content.",
        )
        .build();

    let expand_btn = Button::builder()
        .label("Expand placeholders")
        .halign(Align::Center)
        .build();
    expand_btn.add_css_class("pill");
    expand_btn.add_css_class("suggested-action");

    {
        let state = state.clone();
        expand_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            btn.set_label("Expanding…");
            // Two expansion paths:
            //   - Folder is a real dir with only .cloudf children →
            //     sync the folder itself recursively without download.
            //   - Folder is gone, replaced by a sibling .cloudf
            //     placeholder → sync that placeholder file. After
            //     it succeeds the agent recreates the directory at
            //     state.folder_path.
            let target = placeholder_path(&state.folder_path)
                .unwrap_or_else(|| state.folder_path.clone());
            let agent_for_worker = state.agent.as_ref().clone();
            let state_for_done = state.clone();
            let btn_for_done = btn.clone();
            spawn_sync(
                &state.agent,
                state.folder_path.clone(),
                move || agent_for_worker.sync_recursive(&target, true),
                move |result: Result<String, OdriveError>| {
                    btn_for_done.set_sensitive(true);
                    btn_for_done.set_label("Expand placeholders");
                    match result {
                        Ok(_) => {
                            state_for_done
                                .overlay
                                .add_toast(Toast::new("Placeholders expanded"));
                            render_into_toolbar(&state_for_done);
                        }
                        Err(e) => state_for_done
                            .overlay
                            .add_toast(Toast::new(&format!("Expansion failed: {}", e))),
                    }
                },
            );
        });
    }

    status.set_child(Some(&expand_btn));
    status
}

fn build_subfolders_group(state: &Rc<PageState>) -> PreferencesGroup {
    let subfolders = list_subfolders(Path::new(&state.folder_path));
    let group = PreferencesGroup::builder()
        .title("Folders")
        .description(if subfolders.is_empty() {
            "No subfolders here."
        } else {
            "Drill into a subfolder to set its sync rule."
        })
        .build();
    for sub in &subfolders {
        group.add(&build_subfolder_row(state, sub));
    }
    group
}


/// One subfolder candidate to render in the parent's list.
struct SubfolderEntry {
    /// Path the user navigates to on click — never includes the
    /// `.cloudf` extension. For an unexpanded folder this points at a
    /// directory that doesn't exist yet; the detail page detects that
    /// and offers an Expand button which calls sync on the placeholder.
    target_path: PathBuf,
    /// True iff the entry is an unexpanded `.cloudf` placeholder rather
    /// than a real directory.
    unexpanded: bool,
}

/// Direct subfolders to render, sorted alphabetically. Includes both
/// real directories and `.cloudf` placeholder files (treated as
/// unexpanded folders) — without the latter, a folder the user unsyncs
/// silently disappears from its parent's list, since unsync collapses
/// the directory back to a `.cloudf`.
fn list_subfolders(path: &Path) -> Vec<SubfolderEntry> {
    let mut out: Vec<SubfolderEntry> = Vec::new();
    let Ok(entries) = fs::read_dir(path) else {
        return out;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = match p.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            out.push(SubfolderEntry {
                target_path: p,
                unexpanded: false,
            });
        } else if name.ends_with(".cloudf") {
            // Strip the `.cloudf` suffix so the click handler navigates
            // to the directory path the agent will produce after sync.
            let stem = &name[..name.len() - ".cloudf".len()];
            let target = p.parent().map(|par| par.join(stem)).unwrap_or(p);
            out.push(SubfolderEntry {
                target_path: target,
                unexpanded: true,
            });
        }
    }
    out.sort_by(|a, b| a.target_path.cmp(&b.target_path));
    out
}

fn build_subfolder_row(state: &Rc<PageState>, sub: &SubfolderEntry) -> ActionRow {
    let name = sub
        .target_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| sub.target_path.to_string_lossy().into_owned());

    let row = ActionRow::builder()
        .title(&name)
        .activatable(true)
        .build();

    // Different leading icon for unexpanded vs expanded folders so the
    // user can tell at a glance which ones still need a sync.
    let icon_name = if sub.unexpanded {
        "folder-download-symbolic"
    } else {
        "folder-symbolic"
    };
    let icon = Image::from_icon_name(icon_name);
    icon.set_pixel_size(20);
    icon.set_margin_start(6);
    icon.set_margin_end(8);
    row.add_prefix(&icon);

    // "Not synced" caption for unexpanded folders sets expectation
    // that clicking will require an expand step. Otherwise show a
    // rule badge if a per-folder rule is set in our DB.
    let target_str = sub.target_path.to_string_lossy().into_owned();
    if sub.unexpanded {
        let badge = Label::new(Some("Not synced"));
        badge.add_css_class("dim-label");
        badge.add_css_class("caption");
        badge.set_margin_end(6);
        row.add_suffix(&badge);
    } else if let Some(db) = open_db(&state.agent) {
        if let Ok(Some(rule)) = db.get_folder_rule(&target_str) {
            let badge = Label::new(Some(&format_rule_badge(&rule)));
            badge.add_css_class("dim-label");
            badge.add_css_class("caption");
            badge.set_margin_end(6);
            row.add_suffix(&badge);
        }
    }

    let chevron = Image::from_icon_name("go-next-symbolic");
    chevron.set_margin_start(6);
    row.add_suffix(&chevron);

    {
        let state = state.clone();
        let target = target_str.clone();
        row.connect_activated(move |_| {
            let page = build_page(
                state.agent.clone(),
                state.overlay.clone(),
                state.nav.clone(),
                target.clone(),
                false,
            );
            state.nav.push(&page);
        });
    }

    row
}

fn format_rule_badge(rule: &FolderRule) -> String {
    match FolderSyncThreshold::from_db_value(rule.threshold_mb) {
        FolderSyncThreshold::None => "Never".to_string(),
        FolderSyncThreshold::Inf => "All".to_string(),
        FolderSyncThreshold::Mb(n) => format!("≤ {} MB", n),
    }
}

fn open_db(agent: &OdriveAgent) -> Option<OdriveDb> {
    OdriveDb::open(agent.get_db_path()).ok()
}

/// Wrap `worker::spawn` for folder-level sync operations: mark the
/// folder as in-progress in the cross-process DB before kickoff, clear
/// when done. The Nautilus extension paints `odrive-syncing` on any
/// folder whose path is in this set, so this is what drives the
/// transient syncing emblem during expand / Sync now / Save+Apply.
fn spawn_sync<T, F, G>(
    agent: &Rc<OdriveAgent>,
    folder_path: String,
    work: F,
    on_done: G,
) where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    G: FnOnce(T) + 'static,
{
    let db_path = agent.get_db_path();
    let path_for_clear = folder_path.clone();

    if let Ok(db) = OdriveDb::open(&db_path) {
        let _ = db.mark_sync_in_progress(&folder_path);
    }
    touch_mtime(&folder_path);

    worker::spawn(work, move |result: T| {
        if let Ok(db) = OdriveDb::open(&db_path) {
            let _ = db.clear_sync_in_progress(&path_for_clear);
        }
        touch_mtime(&path_for_clear);
        on_done(result);
    });
}

/// Bump a path's mtime so Nautilus's directory-listing inotify fires
/// and re-calls `update_file_info` — that's how the syncing emblem
/// appears/disappears in real time. Best-effort: silently no-ops if
/// the path doesn't exist (e.g. a `.cloudf` already replaced by its
/// expanded folder when we clear).
fn touch_mtime(path: &str) {
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

// ---------------------------------------------------------------------------
// Sync-rule editor
// ---------------------------------------------------------------------------

const OPERATION_LABELS: &[&str] = &["Automatic", "One-Time"];
const THRESHOLD_LABELS: &[&str] = &[
    "None (don't auto-download)",
    "Small (10 MB)",
    "Medium (100 MB)",
    "Large (500 MB)",
    "All",
    "Custom…",
];

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ThresholdChoice {
    None,
    Small,
    Medium,
    Large,
    All,
    Custom,
}

const THRESHOLD_VARIANTS: &[ThresholdChoice] = &[
    ThresholdChoice::None,
    ThresholdChoice::Small,
    ThresholdChoice::Medium,
    ThresholdChoice::Large,
    ThresholdChoice::All,
    ThresholdChoice::Custom,
];

impl ThresholdChoice {
    fn from_threshold(t: FolderSyncThreshold) -> (Self, Option<u32>) {
        match t {
            FolderSyncThreshold::None => (ThresholdChoice::None, None),
            FolderSyncThreshold::Inf => (ThresholdChoice::All, None),
            FolderSyncThreshold::Mb(10) => (ThresholdChoice::Small, None),
            FolderSyncThreshold::Mb(100) => (ThresholdChoice::Medium, None),
            FolderSyncThreshold::Mb(500) => (ThresholdChoice::Large, None),
            FolderSyncThreshold::Mb(n) => (ThresholdChoice::Custom, Some(n)),
        }
    }

    fn to_threshold(self, custom_mb: Option<u32>) -> FolderSyncThreshold {
        match self {
            ThresholdChoice::None => FolderSyncThreshold::None,
            ThresholdChoice::Small => FolderSyncThreshold::Mb(10),
            ThresholdChoice::Medium => FolderSyncThreshold::Mb(100),
            ThresholdChoice::Large => FolderSyncThreshold::Mb(500),
            ThresholdChoice::All => FolderSyncThreshold::Inf,
            ThresholdChoice::Custom => FolderSyncThreshold::Mb(custom_mb.unwrap_or(50)),
        }
    }
}

fn build_rule_group(state: &Rc<PageState>) -> PreferencesGroup {
    let group = PreferencesGroup::builder()
        .title("Sync rule")
        .description("How files in this folder should sync.")
        .build();

    let existing_rule = open_db(&state.agent)
        .and_then(|db| db.get_folder_rule(&state.folder_path).ok().flatten());
    let has_rule = existing_rule.is_some();

    let operation = ComboRow::builder()
        .title("Operation")
        .subtitle("Automatic applies a persistent rule. One-Time syncs once on demand.")
        .model(&StringList::new(OPERATION_LABELS))
        .build();
    operation.set_selected(0);
    group.add(&operation);

    let (initial_choice, initial_custom_mb) = match &existing_rule {
        Some(r) => {
            ThresholdChoice::from_threshold(FolderSyncThreshold::from_db_value(r.threshold_mb))
        }
        None => (ThresholdChoice::All, None),
    };

    let threshold_row = ComboRow::builder()
        .title("Download threshold")
        .subtitle("Files at or below this size auto-download")
        .model(&StringList::new(THRESHOLD_LABELS))
        .build();
    let initial_idx = THRESHOLD_VARIANTS
        .iter()
        .position(|v| *v == initial_choice)
        .unwrap_or(4);
    threshold_row.set_selected(initial_idx as u32);
    group.add(&threshold_row);

    let custom_row = ActionRow::builder().title("Custom size (MB)").build();
    let custom_entry = Entry::builder()
        .placeholder_text("e.g. 250")
        .valign(Align::Center)
        .width_chars(8)
        .build();
    if let Some(n) = initial_custom_mb {
        custom_entry.set_text(&n.to_string());
    }
    custom_row.add_suffix(&custom_entry);
    custom_row.set_visible(initial_choice == ThresholdChoice::Custom);
    group.add(&custom_row);

    let expand_row = SwitchRow::builder()
        .title("Apply to subfolders")
        .subtitle("Cascade this rule to every nested folder under this one")
        .build();
    expand_row.set_active(
        existing_rule
            .as_ref()
            .map(|r| r.expand_subfolders)
            .unwrap_or(false),
    );
    group.add(&expand_row);

    // Sync-now row (One-Time-only).
    let sync_now_row = ActionRow::builder().title("Sync now").build();
    let sync_now_btn = Button::builder()
        .label("Sync")
        .valign(Align::Center)
        .build();
    sync_now_btn.add_css_class("pill");
    sync_now_btn.add_css_class("suggested-action");
    sync_now_row.add_suffix(&sync_now_btn);
    sync_now_row.set_visible(false);
    group.add(&sync_now_row);

    // Save / Delete row (Automatic-only).
    let save_row = ActionRow::builder()
        .title(if has_rule { "Update rule" } else { "Save rule" })
        .build();
    let save_btn = Button::builder()
        .label(if has_rule { "Update" } else { "Save" })
        .valign(Align::Center)
        .build();
    save_btn.add_css_class("pill");
    save_btn.add_css_class("suggested-action");
    save_row.add_suffix(&save_btn);
    if has_rule {
        let delete_btn = Button::builder()
            .label("Delete")
            .valign(Align::Center)
            .build();
        delete_btn.add_css_class("pill");
        delete_btn.add_css_class("destructive-action");
        save_row.add_suffix(&delete_btn);

        let state_for_delete = state.clone();
        delete_btn.connect_clicked(move |btn| {
            confirm_delete_rule(btn, state_for_delete.clone());
        });
    }
    group.add(&save_row);

    // Operation toggle controls visibility of the rest of the rows.
    {
        let threshold_row = threshold_row.clone();
        let custom_row = custom_row.clone();
        let expand_row = expand_row.clone();
        let sync_now_row = sync_now_row.clone();
        let save_row = save_row.clone();
        operation.connect_selected_notify(move |op| {
            let automatic = op.selected() == 0;
            threshold_row.set_visible(automatic);
            custom_row.set_visible(
                automatic
                    && THRESHOLD_VARIANTS
                        .get(threshold_row.selected() as usize)
                        .copied()
                        == Some(ThresholdChoice::Custom),
            );
            expand_row.set_visible(automatic);
            save_row.set_visible(automatic);
            sync_now_row.set_visible(!automatic);
        });
    }

    {
        let custom_row_for_threshold = custom_row.clone();
        threshold_row.connect_selected_notify(move |t| {
            let is_custom = THRESHOLD_VARIANTS
                .get(t.selected() as usize)
                .copied()
                == Some(ThresholdChoice::Custom);
            custom_row_for_threshold.set_visible(is_custom);
        });
    }

    {
        let state = state.clone();
        sync_now_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            btn.set_label("Syncing…");
            let agent_for_worker = state.agent.as_ref().clone();
            let path = state.folder_path.clone();
            let state_for_done = state.clone();
            let btn_for_done = btn.clone();
            spawn_sync(
                &state.agent,
                state.folder_path.clone(),
                move || agent_for_worker.sync_recursive(&path, false),
                move |result: Result<String, OdriveError>| {
                    btn_for_done.set_sensitive(true);
                    btn_for_done.set_label("Sync");
                    match result {
                        Ok(_) => state_for_done.overlay.add_toast(Toast::new("Sync complete")),
                        Err(e) => state_for_done
                            .overlay
                            .add_toast(Toast::new(&format!("Sync failed: {}", e))),
                    }
                },
            );
        });
    }

    {
        let state = state.clone();
        let threshold_row = threshold_row.clone();
        let custom_entry = custom_entry.clone();
        let expand_row = expand_row.clone();
        save_btn.connect_clicked(move |btn| {
            let choice = THRESHOLD_VARIANTS
                .get(threshold_row.selected() as usize)
                .copied()
                .unwrap_or(ThresholdChoice::All);
            let custom_mb: Option<u32> = if choice == ThresholdChoice::Custom {
                let raw = custom_entry.text().to_string();
                match raw.trim().parse::<u32>() {
                    Ok(n) if n > 0 => Some(n),
                    _ => {
                        state.overlay.add_toast(Toast::new(
                            "Enter a positive integer (in MB) for the custom threshold.",
                        ));
                        return;
                    }
                }
            } else {
                None
            };
            let threshold = choice.to_threshold(custom_mb);
            let expand = expand_row.is_active();

            // foldersyncrule itself is a fast CLI call (no network IO),
            // so we keep it on the main thread. The bundled sync that
            // applies the rule to existing files moves to a worker
            // thread — that's the long-running step.
            btn.set_sensitive(false);
            btn.set_label("Saving…");
            let rule_result =
                state
                    .agent
                    .folder_sync_rule(&state.folder_path, threshold, expand);

            match rule_result {
                Ok(_) => {
                    if let Some(db) = open_db(&state.agent) {
                        if let Err(e) = db.upsert_folder_rule(
                            &state.folder_path,
                            threshold.to_db_value(),
                            expand,
                        ) {
                            state.overlay.add_toast(Toast::new(&format!(
                                "Saved upstream but DB write failed: {}",
                                e
                            )));
                        }
                    }
                    // Nudge Nautilus to re-render the folder's emblem:
                    // a folder with a rule gets the synced emblem, so
                    // adding/changing one should flip immediately
                    // rather than wait on the extension's TTL cache.
                    touch_mtime(&state.folder_path);

                    // foldersyncrule applies to *new* remote content
                    // only (per upstream docs: "Set rule for automatically
                    // syncing new remote content"). Existing local
                    // placeholders won't materialise unless we also run
                    // a sync. Bundle a sync_recursive on a worker
                    // thread so the GUI stays responsive. Skip when
                    // threshold == None since the rule means "don't
                    // auto-download anything" and the sync would be a
                    // no-op.
                    if threshold == FolderSyncThreshold::None {
                        btn.set_sensitive(true);
                        btn.set_label("Save");
                        state.overlay.add_toast(Toast::new("Folder rule saved"));
                        render_into_toolbar(&state);
                    } else {
                        // Rule itself is set instantly upstream; the
                        // background sync that materialises existing
                        // content runs on the worker. Showing "Applied"
                        // (past tense, button still disabled) signals
                        // "your save took effect" — the user doesn't
                        // need to wait. The follow-up toast on sync
                        // completion announces the bundled apply.
                        btn.set_label("Applied");
                        let agent_for_worker = state.agent.as_ref().clone();
                        let path = state.folder_path.clone();
                        let state_for_done = state.clone();
                        spawn_sync(
                            &state.agent,
                            state.folder_path.clone(),
                            move || agent_for_worker.sync_recursive(&path, false),
                            move |result: Result<String, OdriveError>| {
                                let toast = match result {
                                    Ok(_) => "Rule saved and applied to existing files".to_string(),
                                    Err(e) => format!(
                                        "Rule saved; sync of existing files failed: {}",
                                        e
                                    ),
                                };
                                state_for_done.overlay.add_toast(Toast::new(&toast));
                                render_into_toolbar(&state_for_done);
                            },
                        );
                    }
                }
                Err(e) => {
                    btn.set_sensitive(true);
                    btn.set_label("Save");
                    state
                        .overlay
                        .add_toast(Toast::new(&format!("Save failed: {}", e)));
                }
            }
        });
    }

    group
}

fn confirm_delete_rule(button: &Button, state: Rc<PageState>) {
    let dialog = MessageDialog::builder()
        .heading("Remove sync rule?")
        .body(format!(
            "This sets the auto-download threshold for {} to 0 and forgets the rule.",
            state.folder_path
        ))
        .modal(true)
        .build();
    if let Some(window) = button
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok())
    {
        dialog.set_transient_for(Some(&window));
    }

    // Optional cleanup toggle: revert every file in the folder back
    // to a `.cloud` placeholder, freeing local disk space. Off by
    // default — deleting a rule and unsyncing files are different
    // intents and we don't want to silently delete local content.
    let unsync_switch = adw::SwitchRow::builder()
        .title("Also unsync local files")
        .subtitle("Revert files in this folder to placeholders, freeing local disk space")
        .active(false)
        .build();
    let unsync_group = PreferencesGroup::new();
    unsync_group.add(&unsync_switch);
    dialog.set_extra_child(Some(&unsync_group));

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let unsync_switch_for_cb = unsync_switch.clone();
    dialog.connect_response(None, move |dlg, response| {
        if response == "delete" {
            let also_unsync = unsync_switch_for_cb.is_active();
            // Rule removal itself is fast — keep it on the main
            // thread. The optional unsync moves to a worker since it
            // can take a while on a large folder.
            let rule_result = state.agent.folder_sync_rule(
                &state.folder_path,
                FolderSyncThreshold::None,
                false,
            );
            match rule_result {
                Ok(_) => {
                    if let Some(db) = open_db(&state.agent) {
                        let _ = db.delete_folder_rule(&state.folder_path);
                    }
                    // Synced emblem disappears immediately on rule
                    // removal. The optional unsync that follows will
                    // eventually replace the folder with a `.cloudf`
                    // (Nautilus picks that up via inotify on its own).
                    touch_mtime(&state.folder_path);
                    if also_unsync {
                        state.overlay.add_toast(Toast::new(
                            "Rule removed — unsyncing local files…",
                        ));
                        let agent_for_worker = state.agent.as_ref().clone();
                        let path = state.folder_path.clone();
                        let state_for_done = state.clone();
                        worker::spawn(
                            move || agent_for_worker.unsync(&path),
                            move |result: Result<String, OdriveError>| {
                                match result {
                                    Ok(_) => state_for_done
                                        .overlay
                                        .add_toast(Toast::new("Local files unsynced")),
                                    Err(e) => state_for_done.overlay.add_toast(Toast::new(
                                        &format!("Rule removed; unsync failed: {}", e),
                                    )),
                                }
                                render_into_toolbar(&state_for_done);
                            },
                        );
                    } else {
                        state.overlay.add_toast(Toast::new("Folder rule removed"));
                        render_into_toolbar(&state);
                    }
                }
                Err(e) => state
                    .overlay
                    .add_toast(Toast::new(&format!("Delete failed: {}", e))),
            }
        }
        dlg.close();
    });
    dialog.present();
}

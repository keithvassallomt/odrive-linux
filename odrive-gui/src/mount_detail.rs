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
use libadwaita as adw;
use adw::prelude::*;
use adw::gtk as gtk;
use adw::{
    ActionRow, ComboRow, HeaderBar, MessageDialog, NavigationPage, NavigationView,
    PreferencesGroup, PreferencesPage, ResponseAppearance, StatusPage, SwitchRow,
    Toast, ToastOverlay, ToolbarView,
};
use gtk::{Align, Button, Entry, Image, Label, StringList};
use odrive_core::{FolderRule, FolderSyncThreshold, OdriveAgent, OdriveDb};
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
    let needs_expansion = state.is_mount_root && !appears_expanded(path);

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

/// Heuristic: a mount root is "expanded" iff at least one direct child
/// is a real directory, OR a non-`.cloudf` file. Before
/// `sync --recursive --nodownload` runs, the agent populates the mount
/// with `.cloudf` placeholder files only; after, those become real
/// directories.
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
            // Synchronous on the GTK thread for now (matches the
            // wizard's install-download trade-off). Mounts with a
            // few thousand entries take seconds.
            btn.set_sensitive(false);
            btn.set_label("Expanding…");
            let result = state.agent.sync_recursive(&state.folder_path, true);
            btn.set_sensitive(true);
            btn.set_label("Expand placeholders");
            match result {
                Ok(_) => {
                    state.overlay.add_toast(Toast::new("Placeholders expanded"));
                    render_into_toolbar(&state);
                }
                Err(e) => {
                    state
                        .overlay
                        .add_toast(Toast::new(&format!("Expansion failed: {}", e)));
                }
            }
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

/// Names of direct subfolders, sorted alphabetically. We deliberately
/// skip files (the per-folder rule UI applies only to folders, and the
/// agent's `foldersyncrule` likewise) and dotfiles.
fn list_subfolders(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(path) else {
        return out;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }
        out.push(p);
    }
    out.sort();
    out
}

fn build_subfolder_row(state: &Rc<PageState>, sub: &Path) -> ActionRow {
    let name = sub
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| sub.to_string_lossy().into_owned());

    let row = ActionRow::builder()
        .title(&name)
        .activatable(true)
        .build();

    // Match the dashboard's mount-row treatment so the icon sits a bit
    // off the left edge and there's a real gap before the title.
    let icon = Image::from_icon_name("folder-symbolic");
    icon.set_pixel_size(20);
    icon.set_margin_start(6);
    icon.set_margin_end(8);
    row.add_prefix(&icon);

    // Mark with a "rule set" badge if there's a folder rule for this
    // path in our DB. Caption-styled and dimmed.
    if let Some(db) = open_db(&state.agent) {
        if let Ok(Some(rule)) = db.get_folder_rule(&sub.to_string_lossy()) {
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
        let sub = sub.to_path_buf();
        row.connect_activated(move |_| {
            let page = build_page(
                state.agent.clone(),
                state.overlay.clone(),
                state.nav.clone(),
                sub.to_string_lossy().into_owned(),
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
            let result = state.agent.sync_recursive(&state.folder_path, false);
            btn.set_sensitive(true);
            btn.set_label("Sync");
            match result {
                Ok(_) => state.overlay.add_toast(Toast::new("Sync started")),
                Err(e) => state
                    .overlay
                    .add_toast(Toast::new(&format!("Sync failed: {}", e))),
            }
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

            btn.set_sensitive(false);
            let label_before = btn.label().map(|s| s.to_string()).unwrap_or_default();
            btn.set_label("Saving…");
            let agent_result =
                state
                    .agent
                    .folder_sync_rule(&state.folder_path, threshold, expand);
            btn.set_sensitive(true);
            btn.set_label(&label_before);

            match agent_result {
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
                    state.overlay.add_toast(Toast::new("Folder rule saved"));
                    render_into_toolbar(&state);
                }
                Err(e) => state
                    .overlay
                    .add_toast(Toast::new(&format!("Save failed: {}", e))),
            }
        });
    }

    group
}

fn confirm_delete_rule(button: &Button, state: Rc<PageState>) {
    let dialog = MessageDialog::builder()
        .heading("Remove sync rule?")
        .body(format!(
            "This sets the auto-download threshold for {} to 0 and forgets the rule. Existing local files stay in place.",
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
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    dialog.connect_response(None, move |dlg, response| {
        if response == "delete" {
            let agent_result =
                state
                    .agent
                    .folder_sync_rule(&state.folder_path, FolderSyncThreshold::None, false);
            match agent_result {
                Ok(_) => {
                    if let Some(db) = open_db(&state.agent) {
                        let _ = db.delete_folder_rule(&state.folder_path);
                    }
                    state.overlay.add_toast(Toast::new("Folder rule removed"));
                    render_into_toolbar(&state);
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

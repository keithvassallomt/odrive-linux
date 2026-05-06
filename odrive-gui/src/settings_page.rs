//! Preferences window. Modelled on GNOME Settings: an
//! `Adw.NavigationSplitView` with a sidebar listing categories on the
//! left and the corresponding `Adw.PreferencesPage` (or stub) on the
//! right. Categories: General, Appearance, Advanced (placeholder for
//! future settings), Status (placeholder; Phase B fills it with the
//! Agent panel + log viewer).
//!
//! Each `Adw.ComboRow` applies its change immediately on selection —
//! no Save button, same idiom as GNOME Settings. On any CLI failure we
//! surface the error verbatim as a toast and revert the row to the
//! value the agent reports back.
//!
//! Long-running operations are not expected here (each setter is a
//! single CLI invocation that exits immediately) so we run them
//! synchronously on the GTK main thread.
use crate::indicator::TrayController;
use libadwaita as adw;
use adw::prelude::*;
use adw::{
    ActionRow, ApplicationWindow, ComboRow, HeaderBar, NavigationPage, NavigationSplitView,
    PreferencesGroup, PreferencesPage, SpinRow, StatusPage, SwitchRow, Toast, ToastOverlay,
    ToolbarView, WindowTitle,
};
use adw::gtk::{
    self, glib, Adjustment, Application, Button, Label, ListBox, ListBoxRow, SelectionMode,
    Stack, StackTransitionType, StringList,
};
use odrive_core::{
    AutoTrashThreshold, AutoUnsyncThreshold, OdriveAgent, OdriveConfig, OdriveDb,
    PlaceholderThreshold, XlThreshold, DEFAULT_TRAY_ICON_COLOR, TRAY_ICON_COLORS,
};
use std::cell::RefCell;
use std::rc::Rc;

/// Sidebar categories. The order here is the on-screen order; the
/// stack name is the key passed to `Stack::set_visible_child_name`.
/// Keep in sync with `build_section_content`.
const SECTIONS: &[(&str, &str)] = &[
    ("general", "General"),
    ("appearance", "Appearance"),
    ("advanced", "Advanced"),
    ("status", "Status"),
];

/// Open the Preferences window. Creates a fresh `ApplicationWindow`
/// each time it's invoked rather than reusing a hidden one — the
/// window is cheap to build and a single-use lifecycle is easier to
/// reason about (no stale combo state from a previous open). Modeless
/// so the user can keep clicking around the dashboard while it's open.
pub fn present(
    app: &Application,
    parent: Option<&ApplicationWindow>,
    agent: Rc<OdriveAgent>,
    tray: Rc<TrayController>,
) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Preferences")
        .default_width(820)
        .default_height(560)
        .modal(false)
        .build();
    if let Some(p) = parent {
        window.set_transient_for(Some(p));
    }

    let split = NavigationSplitView::builder()
        .min_sidebar_width(200.0)
        .max_sidebar_width(280.0)
        .build();

    // Sidebar: Adwaita's `navigation-sidebar` styling on a ListBox
    // gives the standard GNOME Settings look (selected row tinted with
    // the accent colour, full-row click target).
    let sidebar_listbox = ListBox::builder()
        .selection_mode(SelectionMode::Single)
        .css_classes(vec!["navigation-sidebar".to_string()])
        .build();
    for (_name, label) in SECTIONS {
        let lbl = Label::builder()
            .label(*label)
            .halign(gtk::Align::Start)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        let row = ListBoxRow::new();
        row.set_child(Some(&lbl));
        sidebar_listbox.append(&row);
    }
    let sidebar_toolbar = ToolbarView::new();
    let sidebar_header = HeaderBar::new();
    sidebar_header.set_title_widget(Some(&WindowTitle::new("Preferences", "")));
    sidebar_toolbar.add_top_bar(&sidebar_header);
    sidebar_toolbar.set_content(Some(&sidebar_listbox));
    let sidebar_page = NavigationPage::builder()
        .title("Preferences")
        .child(&sidebar_toolbar)
        .build();
    split.set_sidebar(Some(&sidebar_page));

    // Content: a single ToastOverlay wraps the Stack so toasts surface
    // on whichever section is active. Each section is a child of the
    // Stack keyed by `SECTIONS[i].0`.
    let stack = Stack::builder()
        .transition_type(StackTransitionType::Crossfade)
        .build();
    let toast_overlay = ToastOverlay::new();
    toast_overlay.set_child(Some(&stack));

    let content_toolbar = ToolbarView::new();
    let content_header = HeaderBar::new();
    let content_title = WindowTitle::new("General", "");
    content_header.set_title_widget(Some(&content_title));
    content_toolbar.add_top_bar(&content_header);
    content_toolbar.set_content(Some(&toast_overlay));
    let content_page = NavigationPage::builder()
        .title("Preferences")
        .child(&content_toolbar)
        .build();
    split.set_content(Some(&content_page));

    // Build each section's content and add to the stack. Status needs
    // a reference to the enclosing window so its background poll can
    // be cancelled on close — passing &window through the dispatch
    // keeps the surface area uniform.
    for (name, _) in SECTIONS {
        let child = build_section_content(name, &agent, &toast_overlay, &tray, &window);
        stack.add_named(&child, Some(*name));
    }

    // Sidebar selection drives both the stack and the content header
    // title. Default to the first row so the window opens on General.
    let stack_for_select = stack.clone();
    let title_for_select = content_title.clone();
    sidebar_listbox.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        let idx = row.index() as usize;
        let Some((name, label)) = SECTIONS.get(idx) else { return };
        stack_for_select.set_visible_child_name(name);
        title_for_select.set_title(label);
    });
    if let Some(first) = sidebar_listbox.row_at_index(0) {
        sidebar_listbox.select_row(Some(&first));
    }

    window.set_content(Some(&split));
    window.present();
}

/// Construct a section's content widget. Real sections return a
/// `PreferencesPage`; placeholders return a `StatusPage`. Returned as
/// a generic `gtk::Widget` so the caller can stuff it into the stack
/// without caring which kind it got.
fn build_section_content(
    name: &str,
    agent: &Rc<OdriveAgent>,
    overlay: &ToastOverlay,
    tray: &Rc<TrayController>,
    window: &ApplicationWindow,
) -> gtk::Widget {
    match name {
        "general" => build_general_page(agent, overlay).upcast(),
        "appearance" => build_appearance_page(overlay, tray).upcast(),
        "advanced" => build_advanced_page(agent, overlay).upcast(),
        "status" => build_status_page(agent, overlay, window).upcast(),
        _ => StatusPage::new().upcast(),
    }
}

fn build_general_page(agent: &Rc<OdriveAgent>, overlay: &ToastOverlay) -> PreferencesPage {
    let page = PreferencesPage::new();
    page.set_margin_top(12);

    // Initial values — fall back to defaults if the agent isn't reachable;
    // the comboboxes will simply show the upstream defaults until the user
    // adjusts them.
    let initial = agent.get_global_settings().unwrap_or_default();

    let general = PreferencesGroup::builder()
        .title("General")
        .description("Defaults applied to all mounts. Per-folder rules can override these.")
        .build();

    let placeholder_row = build_placeholder_row(initial.placeholder);
    let xl_row = build_xl_row(initial.xl);
    let auto_unsync_row = build_auto_unsync_row(initial.auto_unsync);
    let auto_trash_row = build_auto_trash_row(initial.auto_trash);
    general.add(&placeholder_row);
    general.add(&xl_row);
    general.add(&auto_unsync_row);
    general.add(&auto_trash_row);
    page.add(&general);

    // Re-entrancy guard: applying a value may cause us to revert the
    // selection on error, which itself fires `notify::selected`. Without
    // this we'd loop or double-toast. Shared across all four handlers
    // since only one row is interactive at any given moment.
    let suppress = Rc::new(RefCell::new(false));

    wire_placeholder(&placeholder_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_xl(&xl_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_auto_unsync(&auto_unsync_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_auto_trash(&auto_trash_row, agent.clone(), overlay.clone(), suppress.clone());

    page
}

/// Advanced page exposes the agent's two `odrive_user_*_conf.txt`
/// files. The agent watches both files for mtime changes (~2 s poll
/// in `AdvancedSettingsController._configure`) and re-reads them on
/// the fly, so writes here apply without an agent restart.
///
/// Settings are split into groups by intent rather than by source
/// file (the user doesn't care which file a key lives in). Anything
/// CLI-exposed is intentionally absent — those land on the General
/// page instead. The `blackList*` lists from the premium file are
/// deferred until we have a richer text-list editor; raw
/// comma-separated entries would be a worse UX than not exposing
/// them at all.
fn build_advanced_page(agent: &Rc<OdriveAgent>, overlay: &ToastOverlay) -> PreferencesPage {
    let page = PreferencesPage::new();
    page.set_margin_top(12);

    // Read both conf files into shared, mutable state. Each widget
    // mutates the matching JSON value in-memory and writes the file
    // back; the agent picks up the change on its next poll. We
    // preserve unknown keys (round-trip via serde_json::Value) so
    // settings the user has set via other means aren't dropped.
    let general = Rc::new(RefCell::new(
        agent
            .read_general_conf()
            .unwrap_or_else(|_| serde_json::json!({})),
    ));
    let premium = Rc::new(RefCell::new(
        agent
            .read_premium_conf()
            .unwrap_or_else(|_| serde_json::json!({})),
    ));

    // Banner: tell users this is the deep end.
    let intro_group = PreferencesGroup::new();
    let intro_row = ActionRow::builder()
        .title("Advanced settings")
        .subtitle(
            "These settings live in odrive_user_general_conf.txt and \
             odrive_user_premium_conf.txt. The agent reloads both files \
             within a couple of seconds, so changes apply without a \
             restart. Most users don't need to touch anything here.",
        )
        .build();
    intro_group.add(&intro_row);
    page.add(&intro_group);

    // ----- Performance -----
    let perf_group = PreferencesGroup::builder()
        .title("Performance")
        .description("Concurrency limits and memory caps.")
        .build();
    perf_group.add(&spin_row(
        "Concurrent downloads",
        "Files downloading in parallel",
        1,
        32,
        agent.clone(),
        overlay.clone(),
        premium.clone(),
        ConfFile::Premium,
        "maxConcurrentDownloads",
        4,
    ));
    perf_group.add(&spin_row(
        "Concurrent uploads",
        "Files uploading in parallel",
        1,
        32,
        agent.clone(),
        overlay.clone(),
        premium.clone(),
        ConfFile::Premium,
        "maxConcurrentUploads",
        4,
    ));
    perf_group.add(&spin_row(
        "Concurrent jobs",
        "Background sync operations in parallel",
        1,
        32,
        agent.clone(),
        overlay.clone(),
        premium.clone(),
        ConfFile::Premium,
        "maxConcurrentJobs",
        4,
    ));
    perf_group.add(&spin_row(
        "Max transfer size (MB)",
        "Per-file chunk ceiling for uploads/downloads",
        1,
        4096,
        agent.clone(),
        overlay.clone(),
        premium.clone(),
        ConfFile::Premium,
        "maxTransferMBytes",
        256,
    ));
    perf_group.add(&spin_row(
        "Memory limit (MB)",
        "Hard cap on the agent's resident memory; clamped to ≥100 by upstream",
        100,
        65536,
        agent.clone(),
        overlay.clone(),
        general.clone(),
        ConfFile::General,
        "processMemoryLimitMBytes",
        3584,
    ));
    perf_group.add(&spin_row(
        "Download retries",
        "Number of times a failed download is retried before giving up",
        0,
        20,
        agent.clone(),
        overlay.clone(),
        general.clone(),
        ConfFile::General,
        "maxDownloadRetries",
        3,
    ));
    page.add(&perf_group);

    // ----- Schedule -----
    let sched_group = PreferencesGroup::builder()
        .title("Schedule")
        .description("How often the agent scans local and remote storage, and the backup cadence.")
        .build();
    sched_group.add(&spin_row(
        "Local scan interval (seconds)",
        "Walk of the local sync tree; clamped to ≥120 by upstream",
        120,
        86400,
        agent.clone(),
        overlay.clone(),
        general.clone(),
        ConfFile::General,
        "localScanIntervalSecs",
        1800,
    ));
    sched_group.add(&spin_row(
        "Remote scan interval (minutes)",
        "Refresh of remote-side state",
        1,
        1440,
        agent.clone(),
        overlay.clone(),
        premium.clone(),
        ConfFile::Premium,
        "remoteScanIntervalMins",
        840,
    ));
    sched_group.add(&spin_row(
        "Backup interval (minutes)",
        "Cadence between scheduled backup-job runs; clamped to ≥5 by upstream",
        5,
        10080,
        agent.clone(),
        overlay.clone(),
        premium.clone(),
        ConfFile::Premium,
        "backupIntervalMinutes",
        1440,
    ));
    page.add(&sched_group);

    // ----- Notifications -----
    let notif_group = PreferencesGroup::builder()
        .title("Notifications")
        .description("Suppress specific desktop alerts the agent would otherwise post.")
        .build();
    notif_group.add(&switch_row(
        "Suppress trash notifications",
        "Don't notify when items move to or empty from the odrive trash",
        agent.clone(),
        overlay.clone(),
        general.clone(),
        ConfFile::General,
        "suppressTrashNotifications",
        false,
    ));
    notif_group.add(&switch_row(
        "Suppress urgent notifications",
        "Don't notify on agent-state changes flagged urgent (login required, agent stopped)",
        agent.clone(),
        overlay.clone(),
        general.clone(),
        ConfFile::General,
        "suppressUrgentNotifications",
        false,
    ));
    notif_group.add(&switch_row(
        "Suppress conflict notifications",
        "Don't notify when sync conflicts are resolved",
        agent.clone(),
        overlay.clone(),
        general.clone(),
        ConfFile::General,
        "suppressConflictNotification",
        false,
    ));
    page.add(&notif_group);

    // ----- Encryption advanced -----
    let enc_group = PreferencesGroup::builder()
        .title("Encryption")
        .description("Advanced toggles for Encryptor folders.")
        .build();
    enc_group.add(&switch_row(
        "Don't encrypt names",
        "Encrypt file content but leave file/folder names readable. Affects newly-encrypted items only.",
        agent.clone(),
        overlay.clone(),
        premium.clone(),
        ConfFile::Premium,
        "disableEncryptedNames",
        false,
    ));
    enc_group.add(&switch_row(
        "Skip hash verification",
        "Don't verify the integrity hash of decrypted content. Diagnostic only.",
        agent.clone(),
        overlay.clone(),
        premium.clone(),
        ConfFile::Premium,
        "ignoreEncryptionHashCheck",
        false,
    ));
    page.add(&enc_group);

    // ----- Diagnostic flags -----
    let diag_group = PreferencesGroup::builder()
        .title("Diagnostic flags")
        .description(
            "Escape hatches the agent uses for troubleshooting. Don't change these unless you've been instructed to by odrive support — they can mask real sync errors.",
        )
        .build();
    for (key, title, subtitle) in [
        ("allowFlagged", "Allow flagged downloads", "Bypass agent's blacklist of file types it normally refuses to download"),
        ("allowOldDownload", "Allow old downloads", "Re-download files older than what's locally synced"),
        ("allowZeroByteUpdate", "Allow zero-byte updates", "Let zero-byte remote updates overwrite a local file"),
        ("ignoreSizeMismatch", "Ignore size mismatch", "Skip the size check during sync; treats mismatches as success"),
        ("disableConflictDetectionStrict", "Disable strict conflict detection", "Skip mtime-based conflict checks"),
        ("disableConflictDetectionAll", "Disable all conflict detection", "Skip both mtime and content conflict checks"),
        ("disableFSEvents", "Disable filesystem events", "Stop listening for FS-event notifications and rely on periodic scans only"),
        ("disableLocalFileUpdates", "Disable local→remote updates", "Stop pushing local edits up to remote"),
        ("disableRemoteFileUpdates", "Disable remote→local updates", "Stop pulling remote edits down to local"),
        ("disableSparse", "Disable sparse files", "Materialise placeholders as full-size files immediately"),
        ("disableAutoupdateRestart", "Disable auto-update restart", "Don't auto-restart the agent after an in-place update"),
    ] {
        diag_group.add(&switch_row(
            title,
            subtitle,
            agent.clone(),
            overlay.clone(),
            general.clone(),
            ConfFile::General,
            key,
            false,
        ));
    }
    for (key, title, subtitle) in [
        ("backupDisableMerge", "Disable backup merge", "Each backup pass creates a fresh upload, even if the file is identical to the prior version"),
        ("autoUnsyncUseAccess", "Auto-unsync uses access time", "Auto-unsync ages files by atime instead of mtime"),
        ("allowSyncToOdriveFolderNameMismatch", "Allow odrive-folder name mismatch", "Skip the safety check that the local folder's name matches its odrive identity"),
        ("disableLocalItemDeletes", "Disable local-item deletes", "Stop the agent from removing local files when their remote counterparts are deleted"),
        ("disableRemoteItemDeletes", "Disable remote-item deletes", "Stop the agent from removing remote files when their local counterparts are deleted"),
    ] {
        diag_group.add(&switch_row(
            title,
            subtitle,
            agent.clone(),
            overlay.clone(),
            premium.clone(),
            ConfFile::Premium,
            key,
            false,
        ));
    }
    page.add(&diag_group);

    page
}

/// Which on-disk conf file a setting belongs to. The Advanced page
/// edits both files; widgets carry this enum so the writer hits the
/// right one.
#[derive(Copy, Clone)]
enum ConfFile {
    General,
    Premium,
}

/// Persist a (just-mutated) JSON value to its backing file. Errors
/// surface as toasts; we don't try to roll back the in-memory state
/// because the next reload will reconcile against whatever the file
/// actually contains.
fn write_conf(
    agent: &OdriveAgent,
    file: ConfFile,
    value: &serde_json::Value,
    overlay: &ToastOverlay,
) {
    let result = match file {
        ConfFile::General => agent.write_general_conf(value),
        ConfFile::Premium => agent.write_premium_conf(value),
    };
    if let Err(e) = result {
        overlay.add_toast(Toast::new(&format!("Couldn't save setting: {}", e)));
    }
}

/// Build a `SwitchRow` bound to a JSON-Value boolean key. Initial
/// state comes from the value (or `default` if the key is absent).
/// `connect_active_notify` mutates the in-memory value and writes
/// the conf file.
#[allow(clippy::too_many_arguments)]
fn switch_row(
    title: &str,
    subtitle: &str,
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    conf: Rc<RefCell<serde_json::Value>>,
    file: ConfFile,
    key: &'static str,
    default: bool,
) -> SwitchRow {
    let initial = conf
        .borrow()
        .get(key)
        .and_then(|v| v.as_bool())
        .unwrap_or(default);
    let row = SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(initial)
        .build();
    row.connect_active_notify(move |r| {
        let new = r.is_active();
        conf.borrow_mut()[key] = serde_json::Value::Bool(new);
        write_conf(&agent, file, &conf.borrow(), &overlay);
    });
    row
}

/// Build a `SpinRow` bound to a JSON-Value integer key. Range is
/// `min..=max`; initial value uses the JSON value if present and
/// in range, otherwise `default`.
#[allow(clippy::too_many_arguments)]
fn spin_row(
    title: &str,
    subtitle: &str,
    min: i64,
    max: i64,
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    conf: Rc<RefCell<serde_json::Value>>,
    file: ConfFile,
    key: &'static str,
    default: i64,
) -> SpinRow {
    let initial = conf
        .borrow()
        .get(key)
        .and_then(|v| v.as_i64())
        .unwrap_or(default);
    let initial = initial.clamp(min, max);
    let adj = Adjustment::new(initial as f64, min as f64, max as f64, 1.0, 10.0, 0.0);
    let row = SpinRow::builder()
        .title(title)
        .subtitle(subtitle)
        .adjustment(&adj)
        .build();
    row.connect_value_notify(move |r| {
        let new = r.value() as i64;
        conf.borrow_mut()[key] = serde_json::Value::from(new);
        write_conf(&agent, file, &conf.borrow(), &overlay);
    });
    row
}

fn build_appearance_page(overlay: &ToastOverlay, tray: &Rc<TrayController>) -> PreferencesPage {
    let page = PreferencesPage::new();
    page.set_margin_top(12);

    let cfg = OdriveConfig::load();

    // ----- Panel indicator -----
    // Tray-icon colour. The icons are installed by `odrive-cli
    // install-handlers` into hicolor's `status` category as
    // `odrive-tray-<color>`. Selection persists to
    // ~/.config/odrive-linux/config.toml; the change applies live to
    // the running indicator via TrayController and is picked up at the
    // next process start when the GUI launches without an active tray.
    let panel_group = PreferencesGroup::builder()
        .title("Panel indicator")
        .description("How the tray icon renders.")
        .build();
    let tray_row = build_tray_color_row(&cfg.tray_icon_color);
    panel_group.add(&tray_row);
    page.add(&panel_group);
    wire_tray_color(&tray_row, overlay.clone(), tray.clone());

    // ----- Nautilus emblems -----
    // The Python Nautilus extension paints two emblems: `odrive-synced`
    // on files / folders covered by a sync rule, and `odrive-syncing`
    // on entries currently mid-sync (rows in the `sync_in_progress`
    // table). Both default on; the user can opt out of either
    // independently. The extension reads the live config on each
    // `update_file_info` (with its short TTL cache), so toggles take
    // effect on the next directory listing — no Nautilus restart.
    let emblems_group = PreferencesGroup::builder()
        .title("Nautilus emblems")
        .description("Emblems painted on file-manager entries.")
        .build();
    let synced_row = adw::SwitchRow::builder()
        .title("Show synced emblem")
        .subtitle("Files and folders covered by a sync rule")
        .active(cfg.nautilus_synced_emblem)
        .build();
    let syncing_row = adw::SwitchRow::builder()
        .title("Show syncing emblem")
        .subtitle("Entries currently being synced")
        .active(cfg.nautilus_syncing_emblem)
        .build();
    emblems_group.add(&synced_row);
    emblems_group.add(&syncing_row);
    page.add(&emblems_group);
    wire_emblem_switch(&synced_row, EmblemKind::Synced, overlay.clone());
    wire_emblem_switch(&syncing_row, EmblemKind::Syncing, overlay.clone());

    page
}

/// Status section: live agent state + the local placeholder index.
/// Mirrors what used to live on the dashboard's top group; lives in
/// Preferences now because the dashboard becomes a tabbed shell where
/// "agent housekeeping" doesn't earn front-page real estate.
///
/// A 5 s `glib::timeout_add_seconds_local` poll refreshes the rows.
/// The poll's `SourceId` is removed in `connect_close_request` on the
/// enclosing window so a closed Preferences window doesn't leak a
/// timer holding clones of its own widgets.
fn build_status_page(
    agent: &Rc<OdriveAgent>,
    overlay: &ToastOverlay,
    window: &ApplicationWindow,
) -> PreferencesPage {
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

    // ----- Logs group -----
    // Two leaf actions:
    //   View → opens the live-tail viewer (`log_viewer::present`).
    //   Open → xdg-opens the log directory in Files.
    // Both target the upstream agent's `~/.odrive-agent/log/main.log`
    // (and its directory). We don't ship our own logs — the GUI's
    // useful state already lives in the agent log + the systemd-user
    // journal, and the user-visible value is in the agent's record
    // of what the daemon is doing.
    let logs_group = PreferencesGroup::builder()
        .title("Logs")
        .description("Upstream agent log at ~/.odrive-agent/log/main.log.")
        .build();

    let logs_row = ActionRow::builder()
        .title("Agent log")
        .subtitle("View live, or open the folder in Files")
        .build();
    let view_btn = Button::builder()
        .label("View")
        .valign(gtk::Align::Center)
        .build();
    view_btn.add_css_class("pill");
    let open_btn = Button::builder()
        .label("Open")
        .valign(gtk::Align::Center)
        .build();
    open_btn.add_css_class("pill");
    logs_row.add_suffix(&view_btn);
    logs_row.add_suffix(&open_btn);
    logs_group.add(&logs_row);
    page.add(&logs_group);

    view_btn.connect_clicked({
        let window = window.clone();
        let overlay = overlay.clone();
        move |_| {
            let Some(app) = window
                .application()
                .and_then(|a| a.downcast::<Application>().ok())
            else {
                overlay.add_toast(Toast::new("Could not resolve application"));
                return;
            };
            crate::log_viewer::present(&app, Some(&window));
        }
    });

    open_btn.connect_clicked({
        let overlay = overlay.clone();
        move |_| {
            let dir = crate::log_viewer::log_dir();
            if let Err(e) = std::process::Command::new("xdg-open").arg(&dir).spawn() {
                overlay.add_toast(Toast::new(&format!("Could not open log folder: {}", e)));
            }
        }
    });

    // Refresh closure: pulls is_running, paints status_row + button
    // label, and re-counts the placeholder DB. Wrapped in `Rc` so the
    // start/stop and scan handlers can fire it on demand alongside
    // the periodic poll.
    let refresh: Rc<dyn Fn()> = {
        let agent = agent.clone();
        let status_row = status_row.clone();
        let start_stop_btn = start_stop_btn.clone();
        let db_row = db_row.clone();
        Rc::new(move || {
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
        })
    };
    refresh();

    start_stop_btn.connect_clicked({
        let agent = agent.clone();
        let refresh = refresh.clone();
        let overlay = overlay.clone();
        move |_| {
            if agent.is_running() {
                let _ = agent.stop();
            } else {
                let _ = agent.start();
            }
            refresh();
            overlay.add_toast(Toast::new("Status updated"));
        }
    });

    scan_btn.connect_clicked({
        let agent = agent.clone();
        let refresh = refresh.clone();
        let overlay = overlay.clone();
        move |btn| {
            btn.set_sensitive(false);
            btn.set_label("Scanning…");
            let agent_for_worker = agent.as_ref().clone();
            let mount_path = agent.default_mount_path();
            let overlay_for_done = overlay.clone();
            let refresh_for_done = refresh.clone();
            let btn_for_done = btn.clone();
            crate::worker::spawn(
                move || agent_for_worker.scan_placeholders(&mount_path),
                move |result| {
                    btn_for_done.set_sensitive(true);
                    btn_for_done.set_label("Scan now");
                    match result {
                        Ok(count) => {
                            overlay_for_done
                                .add_toast(Toast::new(&format!("Found {} placeholders", count)));
                            refresh_for_done();
                        }
                        Err(e) => overlay_for_done
                            .add_toast(Toast::new(&format!("Scan failed: {}", e))),
                    }
                },
            );
        }
    });

    // 5 s poll. Stash the SourceId so the close handler can cancel it;
    // letting the timer keep firing after the window is destroyed
    // would be a slow memory leak (every closure clone of status_row /
    // db_row / start_stop_btn would survive for the rest of the
    // process's life).
    let source = glib::timeout_add_seconds_local(5, {
        let refresh = refresh.clone();
        move || {
            refresh();
            glib::ControlFlow::Continue
        }
    });
    let source_holder: Rc<RefCell<Option<glib::SourceId>>> =
        Rc::new(RefCell::new(Some(source)));
    window.connect_close_request(move |_| {
        if let Some(s) = source_holder.borrow_mut().take() {
            s.remove();
        }
        glib::Propagation::Proceed
    });

    page
}

#[derive(Copy, Clone)]
enum EmblemKind {
    Synced,
    Syncing,
}

/// Persist a Nautilus-emblem toggle to `OdriveConfig`. Surface a toast
/// on save failure (config unwritable, etc.). The switch state itself
/// stays at the user's selection regardless — they can retry by
/// toggling again, and the Nautilus extension will read whatever's on
/// disk on its next pass.
fn wire_emblem_switch(row: &adw::SwitchRow, kind: EmblemKind, overlay: ToastOverlay) {
    row.connect_active_notify(move |r| {
        let active = r.is_active();
        let mut cfg = OdriveConfig::load();
        match kind {
            EmblemKind::Synced => cfg.nautilus_synced_emblem = active,
            EmblemKind::Syncing => cfg.nautilus_syncing_emblem = active,
        }
        if let Err(e) = cfg.save() {
            overlay.add_toast(Toast::new(&format!("Could not save preference: {}", e)));
        }
    });
}


// ---------------------------------------------------------------------------
// Row builders
// ---------------------------------------------------------------------------

const PLACEHOLDER_LABELS: &[&str] = &["0 (no auto-download)", "Small (10 MB)", "Medium (100 MB)", "Large (500 MB)", "Unlimited"];
const PLACEHOLDER_VARIANTS: &[PlaceholderThreshold] = &[
    PlaceholderThreshold::Never,
    PlaceholderThreshold::Small,
    PlaceholderThreshold::Medium,
    PlaceholderThreshold::Large,
    PlaceholderThreshold::Always,
];

const XL_LABELS: &[&str] = &["Never (don't split)", "Small (100 MB)", "Medium (500 MB)", "Large (1 GB)", "Extra Large (2 GB)"];
const XL_VARIANTS: &[XlThreshold] = &[
    XlThreshold::Never,
    XlThreshold::Small,
    XlThreshold::Medium,
    XlThreshold::Large,
    XlThreshold::Xlarge,
];

const AUTO_UNSYNC_LABELS: &[&str] = &["Never", "After a day", "After a week", "After a month"];
const AUTO_UNSYNC_VARIANTS: &[AutoUnsyncThreshold] = &[
    AutoUnsyncThreshold::Never,
    AutoUnsyncThreshold::Day,
    AutoUnsyncThreshold::Week,
    AutoUnsyncThreshold::Month,
];

const AUTO_TRASH_LABELS: &[&str] = &[
    "Never",
    "Immediately",
    "Every 15 minutes",
    "Hourly",
    "Daily",
];
const AUTO_TRASH_VARIANTS: &[AutoTrashThreshold] = &[
    AutoTrashThreshold::Never,
    AutoTrashThreshold::Immediately,
    AutoTrashThreshold::Fifteen,
    AutoTrashThreshold::Hour,
    AutoTrashThreshold::Day,
];

fn build_placeholder_row(initial: PlaceholderThreshold) -> ComboRow {
    let row = ComboRow::builder()
        .title("Sync threshold")
        .subtitle("Files at or below this size auto-download when synced")
        .model(&StringList::new(PLACEHOLDER_LABELS))
        .build();
    row.set_selected(index_of(PLACEHOLDER_VARIANTS, initial) as u32);
    row
}

fn build_xl_row(initial: XlThreshold) -> ComboRow {
    let row = ComboRow::builder()
        .title("Split threshold")
        .subtitle("Files larger than this are uploaded in chunks")
        .model(&StringList::new(XL_LABELS))
        .build();
    row.set_selected(index_of(XL_VARIANTS, initial) as u32);
    row
}

/// User-facing labels for the tray colour combo. The order must mirror
/// `odrive_core::TRAY_ICON_COLORS` exactly — selection index is the
/// canonical mapping back to a colour name on save.
const TRAY_COLOR_LABELS: &[&str] = &["Pink", "White", "Black", "Dark grey", "Grey"];

fn build_tray_color_row(initial: &str) -> ComboRow {
    let row = ComboRow::builder()
        .title("Tray icon colour")
        .subtitle("Pick the panel-indicator variant that suits your theme")
        .model(&StringList::new(TRAY_COLOR_LABELS))
        .build();
    let idx = TRAY_ICON_COLORS
        .iter()
        .position(|c| *c == initial)
        .unwrap_or_else(|| {
            TRAY_ICON_COLORS
                .iter()
                .position(|c| *c == DEFAULT_TRAY_ICON_COLOR)
                .unwrap_or(0)
        });
    row.set_selected(idx as u32);
    row
}

fn build_auto_unsync_row(initial: AutoUnsyncThreshold) -> ComboRow {
    let row = ComboRow::builder()
        .title("Unsync threshold")
        .subtitle("Files untouched for this long revert to placeholders")
        .model(&StringList::new(AUTO_UNSYNC_LABELS))
        .build();
    row.set_selected(index_of(AUTO_UNSYNC_VARIANTS, initial) as u32);
    row
}

fn build_auto_trash_row(initial: AutoTrashThreshold) -> ComboRow {
    let row = ComboRow::builder()
        .title("Empty trash")
        .subtitle("Cadence for automatically clearing the odrive trash")
        .model(&StringList::new(AUTO_TRASH_LABELS))
        .build();
    row.set_selected(index_of(AUTO_TRASH_VARIANTS, initial) as u32);
    row
}

// ---------------------------------------------------------------------------
// Apply-on-change wiring
// ---------------------------------------------------------------------------

fn wire_placeholder(
    row: &ComboRow,
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    suppress: Rc<RefCell<bool>>,
) {
    let row_clone = row.clone();
    row.connect_selected_notify(move |r| {
        if *suppress.borrow() {
            return;
        }
        let idx = r.selected() as usize;
        let Some(value) = PLACEHOLDER_VARIANTS.get(idx).copied() else {
            return;
        };
        match agent.placeholder_threshold(value) {
            Ok(_) => overlay.add_toast(Toast::new("Sync threshold updated")),
            Err(e) => {
                overlay.add_toast(Toast::new(&format!("Update failed: {}", e)));
                revert_to_agent_state(&row_clone, &agent, &suppress, GlobalSelector::Placeholder);
            }
        }
    });
}

fn wire_xl(
    row: &ComboRow,
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    suppress: Rc<RefCell<bool>>,
) {
    let row_clone = row.clone();
    row.connect_selected_notify(move |r| {
        if *suppress.borrow() {
            return;
        }
        let idx = r.selected() as usize;
        let Some(value) = XL_VARIANTS.get(idx).copied() else {
            return;
        };
        match agent.xl_threshold(value) {
            Ok(_) => overlay.add_toast(Toast::new("Split threshold updated")),
            Err(e) => {
                overlay.add_toast(Toast::new(&format!("Update failed: {}", e)));
                revert_to_agent_state(&row_clone, &agent, &suppress, GlobalSelector::Xl);
            }
        }
    });
}

fn wire_auto_unsync(
    row: &ComboRow,
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    suppress: Rc<RefCell<bool>>,
) {
    let row_clone = row.clone();
    row.connect_selected_notify(move |r| {
        if *suppress.borrow() {
            return;
        }
        let idx = r.selected() as usize;
        let Some(value) = AUTO_UNSYNC_VARIANTS.get(idx).copied() else {
            return;
        };
        match agent.auto_unsync_threshold(value) {
            Ok(_) => overlay.add_toast(Toast::new("Unsync threshold updated")),
            Err(e) => {
                overlay.add_toast(Toast::new(&format!("Update failed: {}", e)));
                revert_to_agent_state(&row_clone, &agent, &suppress, GlobalSelector::AutoUnsync);
            }
        }
    });
}

fn wire_auto_trash(
    row: &ComboRow,
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    suppress: Rc<RefCell<bool>>,
) {
    let row_clone = row.clone();
    row.connect_selected_notify(move |r| {
        if *suppress.borrow() {
            return;
        }
        let idx = r.selected() as usize;
        let Some(value) = AUTO_TRASH_VARIANTS.get(idx).copied() else {
            return;
        };
        match agent.auto_trash_threshold(value) {
            Ok(_) => overlay.add_toast(Toast::new("Empty-trash cadence updated")),
            Err(e) => {
                overlay.add_toast(Toast::new(&format!("Update failed: {}", e)));
                revert_to_agent_state(&row_clone, &agent, &suppress, GlobalSelector::AutoTrash);
            }
        }
    });
}

/// Persist the tray-colour selection and push it to the running
/// indicator. No agent setter is involved — this is a pure local
/// preference. On config-save failure we surface a toast and leave the
/// row at the new selection (the icon already updated, and re-opening
/// Settings will reflect whatever's actually on disk).
fn wire_tray_color(
    row: &ComboRow,
    overlay: ToastOverlay,
    tray: Rc<TrayController>,
) {
    row.connect_selected_notify(move |r| {
        let idx = r.selected() as usize;
        let Some(color) = TRAY_ICON_COLORS.get(idx).copied() else {
            return;
        };
        let mut cfg = OdriveConfig::load();
        cfg.tray_icon_color = color.to_string();
        if let Err(e) = cfg.save() {
            overlay.add_toast(Toast::new(&format!("Could not save preference: {}", e)));
        }
        tray.set_icon_color(color);
    });
}

#[derive(Copy, Clone)]
enum GlobalSelector {
    Placeholder,
    Xl,
    AutoUnsync,
    AutoTrash,
}

/// Re-read the agent's reported value and force the combobox back to it
/// without firing another setter (suppress flag bracket).
fn revert_to_agent_state(
    row: &ComboRow,
    agent: &OdriveAgent,
    suppress: &Rc<RefCell<bool>>,
    which: GlobalSelector,
) {
    let Ok(g) = agent.get_global_settings() else {
        return;
    };
    *suppress.borrow_mut() = true;
    match which {
        GlobalSelector::Placeholder => {
            row.set_selected(index_of(PLACEHOLDER_VARIANTS, g.placeholder) as u32);
        }
        GlobalSelector::Xl => {
            row.set_selected(index_of(XL_VARIANTS, g.xl) as u32);
        }
        GlobalSelector::AutoUnsync => {
            row.set_selected(index_of(AUTO_UNSYNC_VARIANTS, g.auto_unsync) as u32);
        }
        GlobalSelector::AutoTrash => {
            row.set_selected(index_of(AUTO_TRASH_VARIANTS, g.auto_trash) as u32);
        }
    }
    *suppress.borrow_mut() = false;
}

fn index_of<T: PartialEq>(slice: &[T], value: T) -> usize {
    slice.iter().position(|v| *v == value).unwrap_or(0)
}

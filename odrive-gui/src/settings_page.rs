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
    ApplicationWindow, ComboRow, HeaderBar, NavigationPage, NavigationSplitView,
    PreferencesGroup, PreferencesPage, StatusPage, Toast, ToastOverlay, ToolbarView,
    WindowTitle,
};
use adw::gtk::{
    self, Application, Label, ListBox, ListBoxRow, SelectionMode, Stack,
    StackTransitionType, StringList,
};
use odrive_core::{
    AutoUnsyncThreshold, OdriveAgent, OdriveConfig, PlaceholderThreshold, XlThreshold,
    DEFAULT_TRAY_ICON_COLOR, TRAY_ICON_COLORS,
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

    // Build each section's content and add to the stack.
    for (name, _) in SECTIONS {
        let child = build_section_content(name, &agent, &toast_overlay, &tray);
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
) -> gtk::Widget {
    match name {
        "general" => build_general_page(agent, overlay).upcast(),
        "appearance" => build_appearance_page(overlay, tray).upcast(),
        "advanced" => StatusPage::builder()
            .icon_name("preferences-system-symbolic")
            .title("Advanced")
            .description("Advanced settings will live here.")
            .build()
            .upcast(),
        "status" => StatusPage::builder()
            .icon_name("emblem-default-symbolic")
            .title("Status")
            .description("Agent status and logs will live here.")
            .build()
            .upcast(),
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
    general.add(&placeholder_row);
    general.add(&xl_row);
    general.add(&auto_unsync_row);
    page.add(&general);

    // Re-entrancy guard: applying a value may cause us to revert the
    // selection on error, which itself fires `notify::selected`. Without
    // this we'd loop or double-toast. Shared across all three handlers
    // since only one row is interactive at any given moment.
    let suppress = Rc::new(RefCell::new(false));

    wire_placeholder(&placeholder_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_xl(&xl_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_auto_unsync(&auto_unsync_row, agent.clone(), overlay.clone(), suppress.clone());

    page
}

fn build_appearance_page(overlay: &ToastOverlay, tray: &Rc<TrayController>) -> PreferencesPage {
    let page = PreferencesPage::new();
    page.set_margin_top(12);

    // Tray-icon colour. The icons are installed by `odrive-cli
    // install-handlers` into hicolor's `status` category as
    // `odrive-tray-<color>`. Selection persists to
    // ~/.config/odrive-linux/config.toml; the change applies live to
    // the running indicator via TrayController and is picked up at the
    // next process start when the GUI launches without an active tray.
    let appearance = PreferencesGroup::builder()
        .title("Panel indicator")
        .description("How the tray icon renders.")
        .build();

    let cfg = OdriveConfig::load();
    let tray_row = build_tray_color_row(&cfg.tray_icon_color);
    appearance.add(&tray_row);
    page.add(&appearance);

    wire_tray_color(&tray_row, overlay.clone(), tray.clone());

    page
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
    }
    *suppress.borrow_mut() = false;
}

fn index_of<T: PartialEq>(slice: &[T], value: T) -> usize {
    slice.iter().position(|v| *v == value).unwrap_or(0)
}

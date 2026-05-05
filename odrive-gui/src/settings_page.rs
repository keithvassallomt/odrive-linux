//! Global Settings page. Three `Adw.ComboRow` widgets bound to the three
//! threshold enums in `odrive_core`. Changes apply immediately on
//! selection — same idiom as GNOME Settings, no Save button. If the
//! upstream rejects a value (e.g. autounsync on a non-premium account),
//! we surface the CLI error verbatim as a toast and revert the row to
//! the value the agent reports back.
//!
//! Layout follows current libadwaita idioms: `Adw.ToolbarView` wraps a
//! `HeaderBar` + `Adw.PreferencesPage`, and the rows are split across
//! `Adw.PreferencesGroup`s ("General" / "Premium").
//!
//! Long-running operations are not expected here (each setter is a
//! single CLI invocation that exits immediately) so we run them
//! synchronously on the GTK main thread.
use crate::indicator::TrayController;
use libadwaita as adw;
use adw::prelude::*;
use adw::{
    ComboRow, HeaderBar, NavigationPage, PreferencesGroup, PreferencesPage, Toast, ToastOverlay,
    ToolbarView,
};
use adw::gtk::StringList;
use odrive_core::{
    AutoUnsyncThreshold, OdriveAgent, OdriveConfig, PlaceholderThreshold, XlThreshold,
    DEFAULT_TRAY_ICON_COLOR, TRAY_ICON_COLORS,
};
use std::cell::RefCell;
use std::rc::Rc;

pub fn build(
    agent: Rc<OdriveAgent>,
    overlay: ToastOverlay,
    tray: Rc<TrayController>,
) -> NavigationPage {
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&HeaderBar::new());

    let page = PreferencesPage::new();
    page.set_margin_top(12);

    // Initial values — fall back to defaults if the agent isn't reachable;
    // the comboboxes will simply show the upstream defaults until the user
    // adjusts them.
    let initial = agent.get_global_settings().unwrap_or_default();

    // ----- General group -----
    // odrive removed the free tier, so the prior General/Premium split
    // no longer reflects an account-state distinction — every threshold
    // is just a global default. AutoUnsyncThreshold lives here now too.
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

    // ----- Appearance group -----
    // Tray-icon colour. The icons are installed by `odrive-cli
    // install-handlers` into hicolor's `status` category as
    // `odrive-tray-<color>`. Selection persists to
    // ~/.config/odrive-linux/config.toml; the change applies live to
    // the running indicator via TrayController and is picked up at the
    // next process start when the GUI launches without an active tray.
    let appearance = PreferencesGroup::builder()
        .title("Appearance")
        .description("How the panel indicator renders.")
        .build();

    let cfg = OdriveConfig::load();
    let tray_row = build_tray_color_row(&cfg.tray_icon_color);
    appearance.add(&tray_row);
    page.add(&appearance);

    toolbar.set_content(Some(&page));

    // Re-entrancy guard: applying a value may cause us to revert the
    // selection on error, which itself fires `notify::selected`. Without
    // this we'd loop or double-toast. Shared across all three handlers
    // since only one row is interactive at any given moment.
    let suppress = Rc::new(RefCell::new(false));

    wire_placeholder(&placeholder_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_xl(&xl_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_auto_unsync(&auto_unsync_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_tray_color(&tray_row, overlay.clone(), tray.clone());

    NavigationPage::builder()
        .title("Preferences")
        .child(&toolbar)
        .build()
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

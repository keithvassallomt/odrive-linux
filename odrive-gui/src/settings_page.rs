//! Global Settings page. Three `Adw.ComboRow` widgets bound to the three
//! threshold enums in `odrive_core`. Changes apply immediately on
//! selection — same idiom as GNOME Settings, no Save button. If the
//! upstream rejects a value (e.g. autounsync on a non-premium account),
//! we surface the CLI error verbatim as a toast and revert the row to
//! the value the agent reports back.
//!
//! Long-running operations are not expected here (each setter is a
//! single CLI invocation that exits immediately) so we run them
//! synchronously on the GTK main thread.
use libadwaita as adw;
use adw::prelude::*;
use adw::gtk as gtk;
use adw::{
    ComboRow, HeaderBar, NavigationPage, PreferencesGroup, Toast, ToastOverlay,
};
use gtk::{Box as GtkBox, Orientation, StringList};
use odrive_core::{
    AutoUnsyncThreshold, OdriveAgent, PlaceholderThreshold, XlThreshold,
};
use std::cell::RefCell;
use std::rc::Rc;

pub fn build(agent: Rc<OdriveAgent>, overlay: ToastOverlay) -> NavigationPage {
    let outer = GtkBox::new(Orientation::Vertical, 0);
    outer.append(&HeaderBar::new());

    let body = GtkBox::new(Orientation::Vertical, 12);
    body.set_margin_top(24);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);

    let group = PreferencesGroup::builder()
        .title("Global Settings")
        .description("Defaults applied to all mounts. Per-folder rules can override these.")
        .build();

    // Initial values — fall back to defaults if the agent isn't reachable;
    // the comboboxes will simply show the upstream defaults until the user
    // adjusts them.
    let initial = agent.get_global_settings().unwrap_or_default();

    let placeholder_row = build_placeholder_row(initial.placeholder);
    let xl_row = build_xl_row(initial.xl);
    let auto_unsync_row = build_auto_unsync_row(initial.auto_unsync);

    group.add(&placeholder_row);
    group.add(&xl_row);
    group.add(&auto_unsync_row);

    body.append(&group);
    outer.append(&body);

    // Re-entrancy guard: applying a value may cause us to revert the
    // selection on error, which itself fires `notify::selected`. Without
    // this we'd loop or double-toast. Shared across all three handlers
    // since only one row is interactive at any given moment.
    let suppress = Rc::new(RefCell::new(false));

    wire_placeholder(&placeholder_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_xl(&xl_row, agent.clone(), overlay.clone(), suppress.clone());
    wire_auto_unsync(&auto_unsync_row, agent.clone(), overlay.clone(), suppress.clone());

    NavigationPage::builder()
        .title("Settings")
        .child(&outer)
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
        .title("Sync Threshold")
        .subtitle("Files at or below this size auto-download when synced")
        .model(&StringList::new(PLACEHOLDER_LABELS))
        .build();
    row.set_selected(index_of(PLACEHOLDER_VARIANTS, initial) as u32);
    row
}

fn build_xl_row(initial: XlThreshold) -> ComboRow {
    let row = ComboRow::builder()
        .title("Split Threshold")
        .subtitle("Files larger than this are uploaded in chunks")
        .model(&StringList::new(XL_LABELS))
        .build();
    row.set_selected(index_of(XL_VARIANTS, initial) as u32);
    row
}

fn build_auto_unsync_row(initial: AutoUnsyncThreshold) -> ComboRow {
    let row = ComboRow::builder()
        .title("Unsync Threshold (Premium)")
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
                // Most likely failure mode: non-premium account. Surface
                // the upstream message verbatim and revert the row.
                overlay.add_toast(Toast::new(&format!("Update failed: {}", e)));
                revert_to_agent_state(&row_clone, &agent, &suppress, GlobalSelector::AutoUnsync);
            }
        }
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

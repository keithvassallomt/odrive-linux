//! Encrypt tab — the dashboard's third tab.
//!
//! odrive's "Encryptor" feature lets a remote folder be paired with a
//! local zero-knowledge encrypted view, decrypted in-process by the
//! agent once a passphrase is provided. Per the upstream User Manual,
//! creating an Encryptor folder is **only** done in the odrive web
//! app; the agent's job is just to handle the passphrase locally.
//!
//! ## What we can drive from outside the agent
//!
//! Exactly one CLI verb: `odrive encpassphrase <passphrase> <id>
//! [--initialize]`. It needs the **encryption ID**, which is stored
//! only in the agent's SEE-encrypted DB (the same wall that closed
//! per-item trash restore, share-link manage URLs, and Open in Google
//! Docs). The IPC has the matching `set_encryptor_password` /
//! `test_encryptor_password` verbs, with the same ID requirement.
//!
//! ## What we can't
//!
//! - Enumerate Encryptor folders. `encryptionEntriesPropertyList` is
//!   in `get_dev_system_status_items` (a dev-only IPC variant);
//!   production status responses strip it.
//! - Discover the encryption ID from outside. SEE-encrypted DB.
//! - Receive the agent's "needs passphrase" prompt callback. On
//!   macOS/Windows the agent calls
//!   `_uiService.render_encryptor_enter_password_dialog` in-process;
//!   the Linux headless agent has no UIService receiver, so the
//!   prompt simply never appears.
//! - Create new encryptor folders (web-only).
//!
//! ## What this tab offers
//!
//! 1. A short explainer.
//! 2. A "Manage on odrive.com" button — that's where folder creation,
//!    ID lookup, and passphrase reset happen for everyone, regardless
//!    of platform.
//! 3. A "Set passphrase" form for power users who already know their
//!    encryption ID. Two `Adw.EntryRow`s + an "Initialize" toggle +
//!    Save, all on `OdriveAgent::set_encryption_passphrase`.
//! 4. A clear "Linux limitations" note covering the prompt gap.

use crate::worker;
use adw::prelude::*;
use adw::{
    ActionRow, EntryRow, PasswordEntryRow, PreferencesGroup, PreferencesPage, SwitchRow,
    ToastOverlay,
};
use libadwaita as adw;
use odrive_core::OdriveAgent;
use crate::toasts::{error_toast, toast};
use std::rc::Rc;

const MANAGE_URL: &str = "https://www.odrive.com/account/myodrive";

pub fn build_encrypt_page(agent: Rc<OdriveAgent>, overlay: ToastOverlay) -> PreferencesPage {
    let page = PreferencesPage::new();
    page.set_margin_top(12);

    let intro_group = PreferencesGroup::new();
    let intro_row = ActionRow::builder()
        .title("Encryption")
        .subtitle(
            "Encryptor folders pair a remote location with a local zero-knowledge view. \
             Files and names are encrypted client-side before upload; the agent decrypts \
             them transparently while it's running and you've supplied the passphrase.",
        )
        .build();
    let manage_btn = adw::gtk::Button::builder()
        .label("Manage on odrive.com")
        .tooltip_text(
            "Create new Encryptor folders, look up encryption IDs, or reset passphrases on the web — that's where the upstream feature lives.",
        )
        .css_classes(["suggested-action"])
        .valign(adw::gtk::Align::Center)
        .build();
    {
        let url = MANAGE_URL.to_string();
        manage_btn.connect_clicked(move |_| {
            let _ = adw::gtk::glib::spawn_command_line_async(&format!("xdg-open {}", url));
        });
    }
    intro_row.add_suffix(&manage_btn);
    intro_group.add(&intro_row);
    page.add(&intro_group);

    // --- Set passphrase form ----------------------------------------------
    let form_group = PreferencesGroup::builder()
        .title("Set passphrase")
        .description(
            "Saves the passphrase for an encryptor folder you've already created. \
             Find the encryption ID in the odrive web app under your encryption \
             folder's settings, or copy it from a macOS/Windows client where you've \
             previously paired the folder.",
        )
        .build();

    let id_row = EntryRow::builder().title("Encryption ID").build();

    let pass_row = PasswordEntryRow::builder().title("Passphrase").build();

    let init_row = SwitchRow::builder()
        .title("Initialize")
        .subtitle(
            "Turn this on the first time you set a passphrase for a brand-new encryptor folder. \
             Off otherwise — the agent will verify it against the existing one before saving.",
        )
        .build();

    let save_row = ActionRow::builder().title("Apply").build();
    let save_btn = adw::gtk::Button::builder()
        .label("Save passphrase")
        .css_classes(["suggested-action"])
        .valign(adw::gtk::Align::Center)
        .build();
    save_row.add_suffix(&save_btn);

    form_group.add(&id_row);
    form_group.add(&pass_row);
    form_group.add(&init_row);
    form_group.add(&save_row);
    page.add(&form_group);

    {
        let agent = agent.clone();
        let overlay = overlay.clone();
        let id_row = id_row.clone();
        let pass_row = pass_row.clone();
        let init_row = init_row.clone();
        let save_btn_for_cb = save_btn.clone();
        save_btn.connect_clicked(move |_| {
            let id_text = id_row.text().to_string();
            let pass_text = pass_row.text().to_string();
            let initialize = init_row.is_active();
            let id = id_text.trim().to_string();
            if id.is_empty() {
                overlay.add_toast(error_toast("Encryption ID is required."));
                return;
            }
            if pass_text.is_empty() {
                overlay.add_toast(error_toast("Passphrase is required."));
                return;
            }
            save_btn_for_cb.set_sensitive(false);
            let agent_inner = (*agent).clone();
            let overlay_w = overlay.clone();
            let pass_row_w = pass_row.clone();
            let save_reset = save_btn_for_cb.clone();
            worker::spawn(
                move || {
                    agent_inner
                        .set_encryption_passphrase(&pass_text, &id, initialize)
                        .map(|_| ())
                },
                move |result| {
                    save_reset.set_sensitive(true);
                    let t = match result {
                        Ok(()) => {
                            // Wipe the passphrase from the input so it
                            // isn't sitting on screen after success.
                            pass_row_w.set_text("");
                            toast("Passphrase saved.")
                        }
                        Err(e) => error_toast(&format!("Couldn't save passphrase: {}", e)),
                    };
                    overlay_w.add_toast(t);
                },
            );
        });
    }

    // --- Linux limitations note -------------------------------------------
    let note_group = PreferencesGroup::builder().title("Linux limitations").build();
    let note_row = ActionRow::builder()
        .title("First-time prompts don't appear on Linux")
        .subtitle(
            "When the agent encounters an Encryptor folder it doesn't have a passphrase \
             for, it normally pops a prompt — that prompt is rendered by the desktop GUI \
             on macOS/Windows. The Linux agent runs headless and silently no-ops the \
             prompt, so the encryption stays locked until you set the passphrase here \
             or via `odrive encpassphrase`.",
        )
        .build();
    note_group.add(&note_row);
    page.add(&note_group);

    page
}

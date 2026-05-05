//! Live tail viewer for the upstream agent's `main.log`.
//!
//! Polls `~/.odrive-agent/log/main.log` every 250 ms, appends any new
//! bytes to a `Gtk.TextView`, and tints lines whose level token
//! (third whitespace-separated field, per the agent's
//! `DD Mmm HH:MM:SSAM LEVEL message` format) is `WARN` / `ERROR`.
//! INFO and unknown levels render in the default colour.
//!
//! Rotation: the upstream agent rotates `main.log` by renaming + new
//! create. We detect that as an inode change between ticks and reopen
//! from byte 0; the buffer is not cleared (so the user can still read
//! the tail of the prior file), and the new content tag-wraps the
//! same way.
//!
//! Auto-scroll: scroll to the bottom on each append iff the user is
//! already at the bottom (heuristic on the `ScrolledWindow`'s
//! vadjustment). If they've scrolled up to read context, we leave the
//! view where they parked it.
use adw::prelude::*;
use adw::{ApplicationWindow, HeaderBar, ToolbarView, WindowTitle};
use libadwaita as adw;
use adw::gtk::{
    glib, Application, ScrolledWindow, TextBuffer, TextTag, TextView, WrapMode,
};
use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

const POLL_MS: u64 = 250;

pub fn log_path() -> PathBuf {
    PathBuf::from(format!(
        "{}/.odrive-agent/log/main.log",
        std::env::var("HOME").expect("HOME must be set"),
    ))
}

pub fn log_dir() -> PathBuf {
    PathBuf::from(format!(
        "{}/.odrive-agent/log",
        std::env::var("HOME").expect("HOME must be set"),
    ))
}

pub fn present(app: &Application, parent: Option<&ApplicationWindow>) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("odrive Log")
        .default_width(900)
        .default_height(560)
        .modal(false)
        .build();
    if let Some(p) = parent {
        window.set_transient_for(Some(p));
    }

    let toolbar = ToolbarView::new();
    let header = HeaderBar::new();
    header.set_title_widget(Some(&WindowTitle::new("odrive Log", "main.log")));
    toolbar.add_top_bar(&header);

    let buffer = TextBuffer::builder().build();
    // GNOME's default light theme is `#241f31`-ish on white; the warn
    // / error colours are picked to remain readable on both light and
    // dark themes (high enough contrast either way) without going
    // full saturated red — that gets distracting on a wall of log.
    let warn_tag = buffer
        .create_tag(Some("warn"), &[("foreground", &"#c25d00")])
        .expect("warn tag");
    let error_tag = buffer
        .create_tag(Some("error"), &[("foreground", &"#c01c28"), ("weight", &700i32)])
        .expect("error tag");

    let text_view = TextView::builder()
        .buffer(&buffer)
        .editable(false)
        .monospace(true)
        .wrap_mode(WrapMode::None)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(12)
        .right_margin(12)
        .build();

    let scrolled = ScrolledWindow::builder()
        .child(&text_view)
        .vexpand(true)
        .hexpand(true)
        .build();
    toolbar.set_content(Some(&scrolled));
    window.set_content(Some(&toolbar));

    // Tail state — file handle + inode + read position. Rotation
    // detection compares the on-disk inode to `inode` each tick.
    let state = Rc::new(RefCell::new(TailState::default()));

    // Initial load: drain the whole file once so the user opens onto
    // recent context, not an empty pane. Subsequent ticks only append
    // bytes added after `pos`.
    let path = log_path();
    if let Ok(mut f) = File::open(&path) {
        let mut s = String::new();
        if f.read_to_string(&mut s).is_ok() {
            append_lines(&buffer, &warn_tag, &error_tag, &s);
        }
        let pos = f.stream_position().unwrap_or(0);
        let inode = f.metadata().map(|m| m.ino()).unwrap_or(0);
        *state.borrow_mut() = TailState { file: Some(f), inode, pos };
    }

    // Scroll to bottom once the buffer has been laid out — the
    // immediate `scroll_to_iter` after insert can race against
    // GtkTextView's lazy line-height calc. `idle_add_local_once`
    // defers to the next main-loop iteration which is enough.
    glib::idle_add_local_once({
        let buffer = buffer.clone();
        let text_view = text_view.clone();
        move || {
            let mut iter = buffer.end_iter();
            text_view.scroll_to_iter(&mut iter, 0.0, false, 0.0, 0.0);
        }
    });

    let source = glib::timeout_add_local(Duration::from_millis(POLL_MS), {
        let buffer = buffer.clone();
        let text_view = text_view.clone();
        let warn_tag = warn_tag.clone();
        let error_tag = error_tag.clone();
        let state = state.clone();
        let path = path.clone();
        move || {
            tail_step(&path, &state, &buffer, &warn_tag, &error_tag, &text_view);
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

    window.present();
}

#[derive(Default)]
struct TailState {
    file: Option<File>,
    inode: u64,
    pos: u64,
}

fn tail_step(
    path: &Path,
    state: &Rc<RefCell<TailState>>,
    buffer: &TextBuffer,
    warn_tag: &TextTag,
    error_tag: &TextTag,
    text_view: &TextView,
) {
    let mut state = state.borrow_mut();

    // Rotation: if the path's current inode differs from ours, our
    // open handle still points at the rotated-out file. Drop it so
    // the reopen branch below picks up the fresh log.
    let current_inode = fs::metadata(path).map(|m| m.ino()).ok();
    if let Some(cur) = current_inode {
        if state.file.is_some() && cur != state.inode {
            state.file = None;
        }
    } else {
        // Path doesn't exist (agent never ran, or log dir wiped).
        // Drop any handle and try again on the next tick.
        state.file = None;
        return;
    }

    if state.file.is_none() {
        match File::open(path) {
            Ok(f) => {
                state.inode = f.metadata().map(|m| m.ino()).unwrap_or(0);
                state.pos = 0;
                state.file = Some(f);
            }
            Err(_) => return,
        }
    }

    // Copy out `pos` before reborrowing `state.file` mutably, so the
    // seek call doesn't trip the borrow-checker by holding a shared
    // ref into `state` alongside the exclusive ref into `state.file`.
    let pos = state.pos;
    let Some(file) = state.file.as_mut() else { return };
    if file.seek(SeekFrom::Start(pos)).is_err() {
        return;
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return;
    }
    if buf.is_empty() {
        return;
    }
    state.pos = pos + buf.len() as u64;

    let was_at_bottom = is_at_bottom(text_view);
    let text = String::from_utf8_lossy(&buf);
    append_lines(buffer, warn_tag, error_tag, &text);
    if was_at_bottom {
        let mut iter = buffer.end_iter();
        text_view.scroll_to_iter(&mut iter, 0.0, false, 0.0, 0.0);
    }
}

/// Check whether the scrollbar is at (or essentially at) the bottom.
/// Used to decide whether to follow the tail or leave the user's
/// scroll position alone. A few-pixel tolerance keeps us from
/// flipping out of follow mode due to floating-point round-off.
fn is_at_bottom(text_view: &TextView) -> bool {
    let Some(parent) = text_view.parent() else { return true };
    let Ok(scrolled) = parent.downcast::<ScrolledWindow>() else { return true };
    let adj = scrolled.vadjustment();
    adj.value() + adj.page_size() >= adj.upper() - 2.0
}

/// Append `text` to the buffer, tagging each line by its level. Level
/// is the upstream agent's third whitespace-separated token
/// (`DD Mmm HH:MM:SSAM LEVEL message`); anything we don't recognise
/// renders in the default colour. We append per-line rather than as
/// one chunk so the tag bracket is exactly the line.
fn append_lines(buffer: &TextBuffer, warn_tag: &TextTag, error_tag: &TextTag, text: &str) {
    for line in text.split_inclusive('\n') {
        let level = line_level(line);
        let tag = match level {
            "ERROR" => Some(error_tag),
            "WARN" | "WARNING" => Some(warn_tag),
            _ => None,
        };
        let mut iter = buffer.end_iter();
        match tag {
            Some(t) => buffer.insert_with_tags(&mut iter, line, &[t]),
            None => buffer.insert(&mut iter, line),
        }
    }
}

fn line_level(line: &str) -> &str {
    line.split_whitespace().nth(3).unwrap_or("")
}

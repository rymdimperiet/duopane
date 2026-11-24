//! Duopane: a keyboard-driven dual-pane file manager.
//!
//! Module map (start reading here if you want to change something):
//! - `app.rs`      – app state, the per-frame loop and keyboard handling
//! - `actions.rs`  – what each command does (copy, rename, go to, ...)
//! - `ui.rs`       – drawing the two panes and the bottom bars
//! - `dialogs.rs`  – pop-up dialogs, "Go to" and the command palette
//! - `commands.rs` – every command and its default shortcut (add your own here)
//! - `pane.rs`     – one folder listing: entries, cursor, selection, sorting, history
//! - `fs_ops.rs`   – copy / move / delete / pack / unpack, run in the background with progress
//! - `archive.rs`  – ZIP and TAR(.GZ) packing, unpacking and listing
//! - `settings.rs` – the settings file (paths, bookmarks, key bindings, custom commands)
//! - `lister.rs`   – the F3 viewer (text, pictures, PDF, music, video); loading in
//!   `lister_load.rs`, playback in `media.rs`
//! - `rename.rs`   – batch rename patterns ([N], [E], [C], ...) and clash checks
//! - `fuzzy.rs`    – the fuzzy matcher used by "Go to" and the command palette
//! - `search.rs`   – finding files (plocate + a live walk); the dialog is `search_ui.rs`
//! - `places.rs`   – the drive menu (Alt+F1 / Alt+F2): places, drives, mounting
//! - `open_with.rs` – "Open with" (Ctrl+Enter): apps for a file from .desktop files and mimeapps.list
//! - `quickview.rs` – Quick view (Ctrl+Q): details of the item under the cursor
//! - `properties.rs` – Properties (Alt+Enter): details and permissions
//! - `checksum.rs` – SHA-256 checksums, checked against SHA256SUMS and the like
//! - `native_drag.rs` – drag and drop with other apps on Wayland
//! - `theme.rs`    – colour schemes, fonts and sizes
//! - `icons.rs`    – file-type icons (a few Nerd Font icons in assets/icons.ttf)
//! - `update.rs`   – asks GitHub once a day whether a newer version is out
//! - `filetypes.rs` – which kind a file is (picture, music, archive…), for its colour

mod actions;
mod app;
mod archive;
mod checksum;
mod commands;
mod dialogs;
mod filetypes;
mod fs_ops;
mod lister;
mod lister_load;
mod media;
#[cfg(target_os = "linux")]
mod native_drag;
mod open_with;
mod fuzzy;
mod icons;
mod pane;
mod places;
mod properties;
mod quickview;
mod rename;
mod search;
mod search_ui;
mod settings;
mod theme;
mod ui;
mod update;

fn main() -> eframe::Result<()> {
    // Optional: `duopane <left folder> [right folder]`
    let args: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Duopane")
        .with_app_id("duopane")
        .with_inner_size([1200.0, 760.0])
        .with_min_inner_size([640.0, 360.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../packaging/duopane.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Duopane",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)))),
    )
}

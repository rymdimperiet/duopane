# Duopane

A keyboard-driven dual-pane file manager (Double Commander / fman hybrid) in Rust,
drawn with egui through eframe. Linux first; nothing should needlessly break
Windows or macOS.

## Commands

```sh
cargo run --release                          # run (optionally: -- <left> [right])
cargo test                                   # all tests, headless (no window needed)
cargo clippy --all-targets -- -D warnings    # CI fails on any clippy warning
sh install.sh                                # cargo install + desktop entry and icon
sh packaging/build-appimage.sh               # AppImage into target/appimage/
```

Building needs the ALSA headers (`alsa-lib` on Arch, `libasound2-dev` on Debian).
Don't run `cargo fmt` over the whole tree: the code uses longer lines than
rustfmt's default and a blanket reformat touches hundreds of places.

## Where things are

`src/main.rs` has the module map. In short: `app.rs` (state, frame loop, keys),
`actions.rs` (what commands do), `commands.rs` (command list and default keys),
`ui.rs` / `dialogs.rs` (drawing), `pane.rs` (one folder listing, or search
results shown as one), `search.rs` + `search_ui.rs` (Find files), `quickview.rs`
(Ctrl+Q), `places.rs` (Alt+F1/F2 drive menu, mounting),
`fs_ops.rs` (background copy/move/delete/pack/unpack jobs), `archive.rs`
(zip/tar), `lister*.rs` + `media.rs` (F3 viewer), `settings.rs`, `theme.rs`.

## Rules that protect user data

- A copy must never empty or delete the destination before the new data is
  complete: `copy_file` writes `.name.duopane-part` and renames it over the target.
- Before overwriting, check `fs_ops::same_file` (hard links, bind mounts, symlinks):
  "different path" does not mean "different file". Renames use `same_entry`,
  which doesn't follow links.
- Unpacking must stay inside the destination: check names with `enclosed_name`
  / `clean_name`, and refuse to write through symlinks with `no_links_on_the_way`.
- Anything that can overwrite goes through `Ctx::place`, which settles each
  existing file by the job's `ConflictPolicy`. The default `Ask` pauses the job
  and shows `dialogs::ConflictDialog` (Double Commander's choices). Copy, move,
  unpack and extract all use it, so a new job that writes files should too.
- A move across drives (or into an existing folder) deletes each original only
  after it has been copied, so skipped files stay where they were.
- Drops with the mouse open the Copy/Move dialog; they never act directly.
- Each of these has a regression test. Keep them, and add one for any new way
  files can be lost.

## How the code works

- The UI thread must not block. Folder reads go through `Pane::start_load`, which
  waits `LOAD_WAIT` (150 ms) and then hands off to `poll_loading`. Anything that
  acts on a pane's new contents must cope with them arriving a frame later (see
  `want_cursor`). File jobs run in `fs_ops` threads and report through channels.
- Programs Duopane starts (terminal, editor, custom commands) use
  `spawn_reaped()` so they don't become zombies.
- Commands are entries in `commands::builtin()`. Each one is reachable from the
  command palette, and its key can be rebound in the settings file.
- Drag and drop with other apps on Wayland is `native_drag.rs`. It shares winit's
  Wayland connection (like the clipboard crate does) and keeps its own seat and
  pointer to learn the press serial. Every file drag from a pane is handed to the
  compositor as soon as it starts, because only the compositor knows when another
  window overlaps ours. While the drag is over our window, its `wl_data_device`
  events become egui pointer events (`app::drag_events_to_input`), so the panes'
  own drop code still runs. Files dropped in from other apps come the same way.
  A protocol error there ends the whole window's connection, so be careful with
  requests (e.g. never `get_pointer` before the seat reports a pointer). Our
  drags offer copy and move, but Duopane never deletes anything when one ends:
  a file manager taking a move does it itself. Drags from other apps are only
  accepted as copies.
- The file clipboard (Ctrl+C/X/V) lives there too: `set_clipboard` /
  `read_clipboard`, with `text/uri-list` plus GNOME's and KDE's cut markers.
  egui-winit turns Ctrl+C/X/V into `Event::Copy/Cut/Paste` instead of key
  events (handled in `handle_keys`), and only reports Paste when the clipboard
  holds text, so Ctrl+V is also caught on our own `wl_keyboard`; `paste_files`
  ignores the second of the two.
- UI tests run egui frames headlessly with synthetic pointer events
  (`ui.rs` tests); use that for mouse behaviour rather than a real window.
- Tests use temporary folders named `duopane-<what>-<pid>` and remove them.

## Style

User-facing text is plain and friendly, without jargon ("Move to trash", "Couldn't
open …"). Code comments explain why rather than what. Keep dependencies few.

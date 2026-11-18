# Duopane

A fast, keyboard-driven, dual-pane file manager written in Rust. It's inspired by
the way fman works (two folders side by side, everything reachable from the
keyboard, a fuzzy "Go to" and a command palette) and was written from scratch,
so it's yours to rename, change and extend.

![Duopane: two folders side by side](docs/screenshot.png)

## Build and run

You need Rust (https://rustup.rs). On Linux you also need the ALSA development
package for sound (`libasound2-dev` on Debian/Ubuntu, `alsa-lib` on Arch,
`alsa-lib-devel` on Fedora). Then, in this folder:

```sh
cargo run --release                 # opens your current folder and your home folder
cargo run --release -- ~/Downloads ~/Documents   # choose the two folders
cargo test                          # run the tests
```

The finished program is `target/release/duopane`. Copy it anywhere on your `PATH`.

## Using it

| Keys | What it does |
| --- | --- |
| Up / Down / PgUp / PgDn / Home / End | Move the cursor (hold Shift to select while moving) |
| Space or Insert | Select / deselect the item under the cursor |
| Type letters | Jump to the first name that matches |
| Enter | Open a folder or **step into an archive** (.zip, .tar, .tar.gz), or open a file with its usual app |
| Shift+Enter | Open with the system's app (also archives) |
| Left, Alt+Left, Backspace, mouse back button | Go up to the parent folder |
| Right, Alt+Right, mouse forward button | Go down into the folder under the cursor |
| Numpad + | **Select by mask** (like Double Commander): `*.jpg;*.png`, `*.txt \| readme*` (after `\|` excludes), `src*/` (ending in `/` matches folders) |
| Numpad − | Deselect by mask |
| Numpad * | Invert the selection |
| Tab | Switch to the other pane |
| Ctrl+P | **Go to** a folder: type part of its name, or a path like `~/Doc` (Tab completes) |
| Ctrl+Shift+P | **Command palette**: every command, searchable |
| F5 / F6 | Copy / move to the other pane (you can edit the destination) |
| F2 or Shift+F6 | Rename |
| Alt+F5 | **Pack** (compress) the selection into a ZIP, TAR.GZ or TAR archive |
| Alt+F9 | **Unpack** the selected archives |
| Ctrl+M | **Batch rename** the selection (or the whole folder) |
| F7 | New folder (`a/b/c` makes nested folders) |
| Shift+F4 | New file |
| F3 | **Lister**: view the file (see below) |
| F4 | Edit the file |
| F8 or Delete | Move to the trash |
| Shift+F8 or Shift+Delete | Delete permanently |
| Ctrl+F3 / F4 / F5 / F6 | Sort by name / type / date / size (press again to reverse) |
| Ctrl+. or Ctrl+H | Show / hide hidden files |
| Ctrl+L | Type a location |
| Ctrl+B | Bookmark this folder (bookmarks rank first in Go to) |
| (command palette) | History: previous / next folder |
| Ctrl+Left / Ctrl+Right | Open the folder under the cursor in the left / right pane |
| Ctrl+Shift+M | Show this folder in the other pane |
| Ctrl+U | Swap the panes |
| Ctrl+Shift+C | Copy the selected paths to the clipboard |
| Ctrl+Shift+S | Calculate the size of the selection |
| F9 | Open a terminal here |
| Ctrl+Plus / Ctrl+Minus | Bigger / smaller text |
| Ctrl+, | Edit the settings file |
| F12 | **Appearance**: colour scheme, fonts, text size, striped rows, the “..” row |
| F1 | List every shortcut |

Hold Alt, Shift or Ctrl to see what the F-keys do with that key in the bar at
the bottom (for example Alt: Pack and Unpack; Ctrl: the sort keys).

The mouse works too: click, Ctrl+click and Shift+click select, a double-click opens,
right-click opens a menu, and clicking a column header sorts by it.

Copies and moves run in the background with a progress bar; you can cancel them
and start more (they queue up). If something already exists at the destination
you're asked whether to replace it or skip it.

## Archives as folders

Enter (or double-click, or Right) on a .zip, .tar, .tar.gz or .tgz file shows what's
inside, like a folder: move around with the usual keys, “..” or Backspace to go
back up and out. F3 views and Enter opens a file inside (it's unpacked to a
temporary folder that's removed when Duopane closes), F5 copies the selection out
to the other pane. Renaming, deleting and adding files inside an archive aren't
supported yet.

## Batch rename (Ctrl+M)

Renames the selected items (or everything in the folder) using patterns, like
Double Commander's multi-rename tool. The name and extension patterns can use
`[N]` the old name, `[N2-5]` characters 2 to 5 of it, `[E]` the extension,
`[C]` a counter (start, step and number of digits are adjustable), `[P]` the
folder's name, and `[Y] [M] [D] [h] [m] [s]` the date and time the file was
changed. Find & replace (optionally with a regular expression) and a case change
are applied after that. A live preview shows every new name and flags clashes;
"Undo the last batch rename" in the command palette puts the old names back.

## The lister (F3)

F3 opens the file under the cursor in a full-window viewer:

- **Text and code**: coloured for most programming and markup languages; UTF-8,
  UTF-16 and Latin-1; Word (.docx), OpenDocument (.odt) and RTF show their text.
  Ctrl+F finds (Enter or F3 for the next match), W wraps long lines.
- **Pictures**: PNG, JPEG, GIF, BMP, WebP, TIFF, ICO, TGA… `+`/`-` or Ctrl+wheel to zoom, `0` to fit.
- **PDF**: page by page (PgUp/PgDn), with zoom. Needs poppler (`pdftoppm`, `pdfinfo`).
- **Music**: MP3, FLAC, Ogg, WAV, M4A/AAC and more, with tags and cover art.
  Space plays/pauses, Left/Right seek, Up/Down change the volume.
- **Video**: MP4, MKV, WebM, AVI, MOV… with sound. Needs `ffmpeg` and `ffprobe`.
- **Archives** (ZIP, TAR, TAR.GZ): a list of what's inside.
- Any other file shows as hex. Keys `1` / `2` / `3` switch between preview, text and hex.

`N` / `P` show the next / previous file in the folder, Esc (or Q, or F3) closes.

### Lister plugins

Add viewers for other file types in the settings file under `lister_plugins`.
A plugin is a command that prints either text or a picture (PNG/JPEG) for a file;
`{file}` is replaced with the file's path. Plugins are tried before the built-in
viewers, and if one fails the built-in viewer is used instead. Examples:

```json
"lister_plugins": [
  { "name": "SVG picture", "extensions": ["svg"], "command": "rsvg-convert --width 1600 {file}", "output": "image" },
  { "name": "Markdown as text", "extensions": ["md"], "command": "pandoc -t plain {file}", "output": "text" },
  { "name": "7-Zip contents", "extensions": ["7z", "rar"], "command": "7z l {file}", "output": "text" },
  { "name": "ISO contents", "extensions": ["iso"], "command": "isoinfo -l -i {file}", "output": "text" }
]
```

`"syntax"` (e.g. `"json"`, `"md"`) colours text output like that kind of file.

## Appearance (F12)

Choose a colour scheme (Duopane Dark/Light, Commander Blue, Solarized Dark, Nord,
Gruvbox Dark, High Contrast) and any font installed on your computer, for the
interface and for the lister's monospace text. Changes apply at once. Single
colours can be overridden in the settings file:

```json
"custom_colors": { "folder": "#ffcc00", "cursor": "#30507a" }
```

Keys: bg, pane, header, border, accent, text, dim, folder, symlink, selected,
cursor, cursor_inactive, error, ok, stripe (every other row).

Every folder starts with a “..” row (Enter on it goes up a level); it can't be
selected, renamed, copied or deleted. Both it and the striped rows can be turned
off in Appearance, and so can the function-key bar at the bottom (also from the
command palette: “Show / hide the function-key bar”). The status line under the panes only
appears while there's a message, a file operation in progress or a name search. New schemes go in `src/theme.rs`.

## Settings

Settings live in `~/.config/duopane/settings.json` (open it with **Ctrl+,**, then
run **Reload settings file** from the command palette). Duopane remembers the
two folders, sorting, bookmarks and the folders you visit most.

Change shortcuts with `key_bindings` (command ids are in `src/commands.rs`):

```json
"key_bindings": {
  "copy": ["F5", "Ctrl+Shift+V"],
  "toggle_hidden": ["Ctrl+H"]
}
```

Add your own commands with `custom_commands`. They run in a shell and appear in
the command palette. `{path}` (item under the cursor), `{dir}` (this folder),
`{other}` (the other pane's folder) and `{selected}` (all selected items) are
replaced with safely quoted values:

```json
"custom_commands": [
  { "title": "Open in VS Code", "run": "code {dir}", "keys": ["Ctrl+Shift+E"] },
  { "title": "Zip selection", "run": "zip -r archive.zip {selected}" },
  { "title": "Compare with other pane", "run": "meld {dir} {other}" }
]
```

Set `"editor"` (for F4) and `"terminal"` (for F9) if you don't want the defaults.

## Making it your own

The code is small and split by job:

- `src/commands.rs`: every command, its title and its default shortcuts. To add a
  built-in command, add one line here and a method in `src/actions.rs`.
- `src/actions.rs`: what each command does.
- `src/pane.rs`: one folder listing (sorting, selection, history).
- `src/fs_ops.rs`: copy / move / delete on a background thread.
- `src/ui.rs` and `src/dialogs.rs`: everything that's drawn.
- `src/theme.rs`: colours and sizes.
- `src/settings.rs`: the settings file.

The window uses [egui](https://github.com/emilk/egui) through `eframe`, which runs
on Linux, Windows and macOS.

### Ideas for next steps

- Tabs per pane, or a quick-look preview of the selected file
- Batch rename with a pattern
- Archives (zip/tar) opened like folders
- Remote locations (SFTP) as extra "file systems"
- A plugin system that loads scripts at startup

## Licence

MIT; see [LICENSE](LICENSE). Duopane was written from scratch, inspired by the way
fman works; it contains no code from fman.

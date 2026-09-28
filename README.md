# Kora

Kora displays a folder as desktop icons on Niri (native Wayland) or an EWMH-compatible X11 window manager. Each monitor has its own folder, navigation history, and selection.

**0.1.0 is in pre-release verification.** See [release readiness](docs/release-readiness.md) for completed checks and remaining tests. Nothing has been published or tagged.

## Build

Use Rust 1.88 or newer, a C linker, and these native development libraries:

- GTK 4.8 or newer, including its X11 backend
- GLib/GIO and gtk4-layer-shell
- X11 development libraries
- `pkg-config`

Common package names:

| Distribution | Packages |
| --- | --- |
| Arch Linux | `base-devel rust gtk4 gtk4-layer-shell pkgconf` |
| Debian/Ubuntu | `build-essential pkg-config libgtk-4-dev libgtk4-layer-shell-dev libx11-dev libxrandr-dev` |
| Fedora | `gcc pkgconf-pkg-config gtk4-devel gtk4-layer-shell-devel libX11-devel libXrandr-devel` |

Package availability depends on the distribution release. Older releases may not package GTK4 layer-shell. Do not substitute the GTK3 `gtk-layer-shell` library. Install a current Rust toolchain separately if the distribution compiler is too old.

Check native library discovery, then build:

```sh
pkg-config --modversion gtk4 gio-2.0 gtk4-layer-shell-0
pkg-config --atleast-version=4.8 gtk4
cargo build --locked --release
./target/release/kora --help
./target/release/kora --version
```

The binary uses native shared libraries. It is not a standalone binary for arbitrary Linux distributions.

## Run and configure

```sh
./target/release/kora
# Or choose a configuration explicitly:
./target/release/kora --config /absolute/path/to/config.toml
```

The default config location is `$XDG_CONFIG_HOME/kora/config.toml`, or `$HOME/.config/kora/config.toml` when `XDG_CONFIG_HOME` is unset or empty. If that file does not exist, Kora shows your home directory over the existing wallpaper. Kora does not create a config file or folder. A missing explicit `--config` file is an error.

Use [config.example.toml](config.example.toml) as a starting point. Change its folder paths and output names to match your machine:

```toml
[defaults]
path = "~/Desktop"
wallpaper_mode = "transparent"
background_color = "#202020"
icon_size = 48

[monitors."DP-1"]
path = "~/Projects"
wallpaper_mode = "replace"
wallpaper_image = "~/Pictures/wallpaper.jpg"

[monitors."HDMI-A-1"]
path = "~/Downloads"
wallpaper_image = ""
```

Find connector names with `niri msg outputs` on Niri or `xrandr --query` on X11. Use names such as `DP-1`, not the manufacturer's display description.

Monitor settings inherit omitted fields from `[defaults]`. An empty `wallpaper_image` clears an inherited image. Unknown monitor names stay dormant until that output connects. Relative paths resolve against the config file's directory. Only leading `~/` is expanded; `$HOME`, shell syntax, and globs are literal. Restart Kora to apply config changes.

- `transparent`: preserves wallpaper drawn on a lower layer; the configured image is ignored.
- `replace`: draws the configured color and optional cover-scaled image. A missing image reports an error and uses the color.

Neither mode edits wallpaper settings or stops a wallpaper provider.

`icon_size` sets the file icons' size in logical pixels, from 24 to 128. The default is 48; try 64 or 96 for larger icons. Set it in `[defaults]` or override it per monitor. GTK applies display scaling automatically. Restart Kora after changing it.

## Desktop controls

Icons fill the monitor instead of sitting inside a file-manager panel. Click selects; double-click opens. Ctrl/Shift-click supports multiple selections. Folders open within Kora. Filenames use a single line of shadowed text. Long names shorten in the middle; hover to see the full name. Selection highlights the icon and filename separately, without filling the grid cell.

The bottom-right controls are:

- **Current folder** icon: click to enter a path; drop files onto this button to target the current folder.
- **Back**: appears when a previous folder is available.
- **Show hidden files**: also controlled by Ctrl+H.

Right-click an icon for Open, Rename, Copy, Cut, Paste, Move to Trash, Operation results, and Quit Kora. Select the intended files before using the context menu. Copy/Cut/Paste currently use Kora's internal clipboard, shared between its monitor views, not the system clipboard.

| Key | Action |
| --- | --- |
| Enter | Open the selection |
| F2 | Rename one selected entry |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste |
| Delete | Ask for confirmation, then move to Trash |
| Alt+Left / Alt+Right | Back / forward |
| Alt+Up / Alt+Home | Parent folder / home directory |
| Ctrl+L | Show the path entry |
| Escape | Dismiss the path entry |
| Ctrl+H | Toggle hidden files |
| Ctrl+Q | Confirm a safe quit of the whole application |

Scroll with the wheel or touchpad anywhere over the exposed desktop, including empty space. Kora receives pointer input across the monitor, so blank wallpaper is no longer click-through. Normal application windows and panels above Kora retain input priority. Drag from empty grid space for rubber-band selection, or use Ctrl/Shift-click. File drops still target folder icons or the current-folder button.

## File safety

- Destinations are never silently overwritten or merged. A conflict fails that item.
- Symlinks are copied as links, including dangling links. Unsupported special files are rejected by recursive copy.
- Copies preserve file contents, symlink text, and Unix permission bits. They do not promise ownership, ACL, extended-attribute, timestamp, sparse-file, or hard-link preservation.
- Cross-filesystem moves copy first, compare metadata for the source tree, then remove only checked entries. Detected source changes retain data and report failure. Cleanup can be partial; inspect both paths when an error is reported.
- Do not move a tree while another application modifies it. Metadata checks are not a filesystem snapshot or a transaction against concurrent writers.
- Trash uses the desktop Trash service only. If Trash is unavailable, Kora reports failure and keeps the item. There is no permanent-delete fallback.
- Completed operations survive view removal in memory. **Operation results** shows up to 100 recent results; they are not saved across application restarts.
- Quit Kora waits for queued operations. Forced termination, Ctrl+C, power loss, and crashes cannot provide that guarantee. Use Ctrl+Q or the context menu while operations are active.

External drag-and-drop copy/move interoperability is still a release gate. Test with disposable files; do not assume every file manager implements source deletion the same way. Copy is the default when offered; Shift requests a move. Internal cut/paste is the simpler way to test cross-monitor moves.

## Desktop compatibility

### Niri

Kora uses layer-shell's bottom layer, above background-layer wallpaper and below normal windows. It requires layer-shell protocol version 4 for on-demand keyboard focus. Other Wayland compositors are not a compatibility promise for 0.1.0. Unsupported integration fails instead of opening a normal explorer window.

Panels remain above Kora. Avoid running another desktop-icon manager over the same surface. Kora does not rearrange other desktop components.

### X11

The window manager must advertise the required EWMH desktop-window hints. Openbox plus picom is the reference environment. Transparent mode requires a compositing manager; opaque replacement mode does not. Native GTK/GDK X11 support must be installed. Competing desktop-icon windows may cover Kora.

To explicitly select native X11 in an X11 session:

```sh
GDK_BACKEND=x11 ./target/release/kora
```

Do not force XWayland in a Wayland session as a workaround for missing layer-shell support.

## Optional installation and autostart

After manual testing, install to Cargo's binary directory:

```sh
cargo install --path . --locked
```

For Niri, an optional entry in your own config is:

```kdl
spawn-at-startup "/absolute/path/to/kora" "--config" "/absolute/path/to/config.toml"
```

For X11, add a launch to the session/WM autostart mechanism you already use, after the compositor when transparent mode is enabled:

```sh
/absolute/path/to/kora --config /absolute/path/to/config.toml &
```

These are examples, not commands Kora applies to your session. Use absolute paths. Launching a second instance activates the existing process instead of creating duplicate desktop views; it does not reload configuration.

To roll back, quit Kora safely and remove the optional autostart entry you added. Existing wallpaper settings are unchanged. Completed file operations are not undone; restore trashed files using your desktop's Trash tools.

## Development checks

```sh
cargo fmt --check
cargo check --locked --all-targets
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
openspec validate add-background-file-explorer --strict
```

Display-dependent tests are explicitly ignored by the default test run. See [the manual checklist](docs/manual-verification.md); screenshot collection and automated pointer interaction are not required.

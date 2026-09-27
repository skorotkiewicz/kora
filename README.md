# Kora

Kora is a desktop-background file explorer for Niri and EWMH-compatible X11 window managers.

## Build requirements

- Rust 1.85 or newer
- GTK 4 development files
- GLib and GIO development files
- gtk4-layer-shell development files
- X11 and XRandR development files
- `pkg-config`

The package names vary by distribution. The build must make `gtk4`, `gio-2.0`, and `gtk4-layer-shell-0` visible to `pkg-config`.

Build with:

```sh
cargo build
```

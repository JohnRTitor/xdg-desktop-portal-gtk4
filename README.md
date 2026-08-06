# xdg-desktop-portal-gtk4

A GTK4-based backend for [xdg-desktop-portal](https://github.com/flatpak/xdg-desktop-portal).

Enables sandboxed applications (like Flatpaks and Snaps) to securely interact with the host system, providing native GTK4 dialogs for file picking, app choosing, and more. Use this as your primary portal backend on Wayland compositors (like Sway, Hyprland) or X11 to benefit from modern GTK4 features like updated UI elements and native file picker thumbnails.

![screenshot.png](./static/screenshot.png)

## Supported Portals

| Portal Interface  | Description                                                   |
| ----------------- | ------------------------------------------------------------- |
| `Access`          | Prompting for device/resource access                          |
| `Account`         | Providing user account information                            |
| `AppChooser`      | Selecting an application to open a file or URI                |
| `Clipboard`       | Bridging clipboard access between sandboxes and host          |
| `DynamicLauncher` | Managing dynamic desktop launchers                            |
| `Email`           | Composing emails                                              |
| `FileChooser`     | Opening and saving files (with native GTK4 UI)                |
| `Inhibit`         | Inhibiting session state (like sleep or logout)               |
| `Lockdown`        | Querying locked-down features                                 |
| `Notification`    | Displaying desktop notifications                              |
| `Print`           | Printing documents                                            |
| `Settings`        | Reading desktop settings (such as color-scheme for dark mode) |
| `USB`             | Managing USB device access                                    |

## Dependencies

**Build:** Rust >= 1.92, `pkg-config`, `make`, GTK 4, GLib 2.0  
**Runtime:** `xdg-desktop-portal`

## Installation

### Standard Build (Quick Start)

Build the binary with Cargo and install the integration files using Meson:

```bash
cargo build --release

# Install the binary, DBus services, systemd units, and portal configurations
sudo make install PREFIX=/usr
```

> [!NOTE]
> For a complete list of dependencies, advanced build configurations, and CI details, please see **[BUILD.md](./BUILD.md)**.

### Nix / NixOS

A Flake is provided for Nix users:

```bash
nix build .#xdg-desktop-portal-gtk4
nix develop # For a configured development shell
```

**NixOS Configuration:**

```nix
{ inputs, pkgs, ... }: {
  xdg.portal = {
    enable = true;
    extraPortals = [
      inputs.xdg-desktop-portal-gtk4.packages.${pkgs.stdenv.hostPlatform.system}.xdg-desktop-portal-gtk4
    ];
    config.common.default = [ "gtk4" ];
  };
}
```

## Configuration

To make your compositor use this backend, configure `xdg-desktop-portal`. Create or edit `~/.config/xdg-desktop-portal/portals.conf`:

```ini
[preferred]
default=gtk4
```

To only use it for specific portals (like the FileChooser), while keeping another backend as default:

```ini
[preferred]
default=hyprland
org.freedesktop.impl.portal.FileChooser=gtk4
```

Apply the changes:

```bash
systemctl --user restart xdg-desktop-portal
```

## Logging & Debugging

The daemon logs directly to the systemd journal using `tracing-journald`.

Enable debug logging via environment variables:

```bash
# Start manually for debugging
RUST_LOG=debug xdg-desktop-portal-gtk4
```

View background service logs:

```bash
journalctl --user -u xdg-desktop-portal-gtk4 -f
```

## Compilation Features

Portals can be selectively disabled at compile-time to reduce binary size. By default, **all portals** are enabled.

Disable default features in Cargo to build only what you need:

```bash
cargo build --release --no-default-features --features "file_chooser,settings"
```

_(See `Cargo.toml` for the full list of portal features)._

## Development & Architecture

- **GTK Main Thread:** Runs the GTK4 event loop. GTK4 objects are `!Send` and `!Sync`, so all UI operations happen here.
- **Tokio Runtime Thread:** A dedicated background thread running a single-threaded Tokio runtime (`current_thread`). This handles `zbus` D-Bus connections, ensuring blocked clients never freeze the UI. CPU-heavy or blocking tasks are offloaded to dedicated background threads via `tokio::task::spawn_blocking`.
- **Internationalization (i18n):** Uses `rust-i18n`. Locales are stored in `locales/`.

## Acknowledgements & License

Originally created by [mahkoh](https://github.com/mahkoh). Inspired by the KDE and GNOME portal implementations.

Licensed under the **GNU Lesser General Public License v2.1**.

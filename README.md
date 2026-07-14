# xdg-desktop-portal-gtk4

This is a portal backend for [xdg-desktop-portal][portal] using GTK4.

If your Wayland compositor would otherwise use the [GTK3 portal][gtk3] (Sway, Jay,
Hyprland, etc.), you can instead use this portal to get thumbnails in the file picker.

![screenshot.png](./static/screenshot.png)

[portal]: http://github.com/flatpak/xdg-desktop-portal
[gtk3]: http://github.com/flatpak/xdg-desktop-portal-gtk

## Supported Portals

This implementation provides backend support for the following XDG Desktop Portal interfaces:

- **Access** (`org.freedesktop.impl.portal.Access`): Prompting for device/resource access.
- **Account** (`org.freedesktop.impl.portal.Account`): Providing user account information.
- **AppChooser** (`org.freedesktop.impl.portal.AppChooser`): Selecting an application to open a file or URI.
- **DynamicLauncher** (`org.freedesktop.impl.portal.DynamicLauncher`): Managing dynamic desktop launchers.
- **Email** (`org.freedesktop.impl.portal.Email`): Composing emails.
- **FileChooser** (`org.freedesktop.impl.portal.FileChooser`): Opening and saving files (with GTK4 native UI/thumbnails).
- **Inhibit** (`org.freedesktop.impl.portal.Inhibit`): Inhibiting session state (like sleep or logout).
- **Lockdown** (`org.freedesktop.impl.portal.Lockdown`): Querying locked-down features (e.g. disable printing/saving).
- **Notification** (`org.freedesktop.impl.portal.Notification`): Displaying desktop notifications.
- **Print** (`org.freedesktop.impl.portal.Print`): Printing documents.
- **Settings** (`org.freedesktop.impl.portal.Settings`): Reading desktop settings (such as color-scheme for dark mode).
- **USB** (`org.freedesktop.impl.portal.Usb`): Managing USB device access.

## Dependencies

### Build Dependencies

- **Rust/Cargo** (for compiling the backend)
- **Meson** and **Ninja** (for installing data files and systemd services)
- **pkg-config**
- **GTK 4** development libraries (e.g., `libgtk-4-dev` or `gtk4-devel`)
- **GLib 2.0** development libraries (e.g., `libglib2.0-dev` or `glib2-devel`)

### Runtime Dependencies

- **xdg-desktop-portal**

## Building & Installing

You will need to build the project using Cargo, and then use Meson to install the compiled binary alongside the necessary D-Bus service files and desktop portal configurations.

```bash
# Build the binary
cargo build --release

# Setup meson build directory
meson setup build -Dprefix=/usr

# Install the portal and its service files
sudo meson install -C build
```

### Nix / NixOS

This project provides a Flake for Nix users.

To build the package:

```bash
nix build .#xdg-desktop-portal-gtk4
```

To enter a development shell with all necessary dependencies configured:

```bash
nix develop
```

## Configuring your Compositor

To make your compositor use the portal, you have to modify its configuration file in

- `/usr/share/xdg-desktop-portal/`

Add

```ini
org.freedesktop.impl.portal.FileChooser=gtk4
```

at the end to explicitly request the GTK4 portal for the file picker.

For example

```diff
--- jay-portals.conf.old    2024-09-20 14:55:49.029327860 +0200
+++ jay-portals.conf        2024-09-20 14:55:54.699731749 +0200
@@ -1,5 +1,6 @@
 [preferred]
 default=gtk
 org.freedesktop.impl.portal.ScreenCast=jay
 org.freedesktop.impl.portal.RemoteDesktop=jay
 org.freedesktop.impl.portal.Idle=none
+org.freedesktop.impl.portal.FileChooser=gtk4
```

Restart `xdg-desktop-portal` afterwards to apply the configuration:

```bash
systemctl --user restart xdg-desktop-portal
```

## License

xdg-desktop-portal-gtk4 is free software licensed under the GNU Lesser General Public
License v2.1.

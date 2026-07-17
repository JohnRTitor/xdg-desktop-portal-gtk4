# xdg-desktop-portal-gtk4

This is a modern GTK4-based portal backend for [xdg-desktop-portal][portal].

It provides native GTK4 dialogs for various desktop portals (like file picking, app choosing, and USB access). If your Wayland compositor typically defaults to the [GTK3 portal][gtk3] (such as Sway, Jay, or Hyprland), you can use this portal backend instead to benefit from modern GTK4 features, including native file picker thumbnails and updated UI elements.

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

## Building & Installation

Please see [BUILD.md](./BUILD.md) for detailed dependencies and build instructions.

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

### NixOS Flake Installation

If you use NixOS with flakes, you can install and enable this portal directly in your `flake.nix` and system configuration.

Add the input to your `flake.nix`:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    xdg-desktop-portal-gtk4.url = "github:JohnRTitor/xdg-desktop-portal-gtk4";
  };
  # ...

}
```

Then configure your portals (e.g. in `configuration.nix`):

```nix
{ inputs, ... }: {
  xdg.portal = {
    enable = true;

    extraPortals = [
      inputs.xdg-desktop-portal-gtk4.packages.${pkgs.stdenv.hostPlatform.system}.xdg-desktop-portal-gtk4
    ];

    config.hyprland = {
      default = [
        "gtk4"
        # other portals like "hyprland", "wlr"
      ];

      # Set the default portal for file choosers
      "org.freedesktop.impl.portal.FileChooser" = [ "gtk4" ];
    };
};
}
```

## Acknowledgements

This project was originally created by [mahkoh](https://github.com/mahkoh) as [mahkoh/xdg-desktop-portal-gtk4](https://github.com/mahkoh/xdg-desktop-portal-gtk4). I extend my sincere gratitude to them for laying the foundation of this work.

I would also like to thank the KDE and GNOME communities for their respective portal implementations[^1][^2], which served as valuable references and inspiration for this project.
1

## License

xdg-desktop-portal-gtk4 is free software licensed under the GNU Lesser General Public
License v2.1.

[^1]: https://github.com/KDE/xdg-desktop-portal-kde

[^2]: https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome

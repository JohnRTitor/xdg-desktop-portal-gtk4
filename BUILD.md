# Building and Installation

## Dependencies

### Build Dependencies

- **Rust/Cargo** (for compiling the backend)
- **pkg-config**
- **GTK 4** development libraries (e.g., `libgtk-4-dev` or `gtk4-devel`)
- **GLib 2.0** development libraries (e.g., `libglib2.0-dev` or `glib2-devel`)

### Runtime Dependencies

- **xdg-desktop-portal**

## Standard Build

You will need to build the project using Cargo, and then install the compiled binary alongside the necessary D-Bus service files and desktop portal configurations. 

This project uses a custom `build.rs` script and `make` or similar standard tools are not strictly required if you just run `cargo run`, but for full installation, refer to your distribution's packaging guidelines or the project's packaging scripts (e.g., `PKGBUILD`).

```bash
# Build the binary
cargo build --release

# Install the binary
# Note: Installation steps depend on your system, but typically you install it to /usr/bin.
# You can do this manually:
sudo install -Dm755 target/release/xdg-desktop-portal-gtk4 /usr/bin/xdg-desktop-portal-gtk4

# Configure and install the remaining data files (desktop, portal, systemd services) using Meson.
# Meson will also set up a symlink in /usr/libexec pointing to the binary in /usr/bin.
meson setup build -Dprefix=/usr
sudo meson install -C build
```

### Nix / NixOS

This project provides a Flake for Nix users, which handles building and environment setup seamlessly.

To build the package:

```bash
nix build .#xdg-desktop-portal-gtk4
```

To enter a development shell with all necessary dependencies configured:

```bash
nix develop
```

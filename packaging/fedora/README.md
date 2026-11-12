# Fedora packaging for xdg-desktop-portal-gtk4

Community-maintained RPM recipe for building `xdg-desktop-portal-gtk4` in a
network-isolated environment with vendored Cargo dependencies, following the
style of the `ananicy-rs` packaging recipes.

## Files

- `xdg-desktop-portal-gtk4.spec`: RPM spec file.
- `make-vendor-archive.sh`: generates `xdg-desktop-portal-gtk4-<version>-vendor.tar.xz`
  (Source1) from `Cargo.lock`.

## Local build outline

1. Create the source tarball from the release tag (or `git archive` of HEAD for
   testing).
2. Generate the vendor archive:
   ```sh
   ./packaging/fedora/make-vendor-archive.sh
   ```
3. Place `xdg-desktop-portal-gtk4-<version>.tar.gz` and the vendor archive in
   your rpmbuild `SOURCES` directory, then build:
   ```sh
   rpmbuild -ba packaging/fedora/xdg-desktop-portal-gtk4.spec
   ```

The package installs the portal binary to `%{_libexecdir}` and the D-Bus/systemd
user/desktop/portal data files under the standard XDG locations.

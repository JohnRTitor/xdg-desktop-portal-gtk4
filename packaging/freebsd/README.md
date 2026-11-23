# FreeBSD packaging for xdg-desktop-portal-gtk4

Community-maintained FreeBSD port skeleton for building the release tarball
from GitHub. It follows the style of the other recipes in `packaging/`.

## Files

- `Makefile`: port metadata, dependencies, build and install targets.
- `distinfo`: checksum and size of the release tarball.
- `pkg-descr` / `pkg-plist`: package description and installed file list.
- `README.md`: this file.

## Build

Copy the directory into a ports tree, then build it from there:

```sh
cp -r packaging/freebsd /usr/ports/x11/xdg-desktop-portal-gtk4
cd /usr/ports/x11/xdg-desktop-portal-gtk4
make install clean
```

The port builds with `cargo build --release --locked`, so crates.io must be
reachable during the build (as with the Arch recipe). Cargo's registry is kept
under `WRKDIR` rather than the build user's home directory.

The build removes `org.freedesktop.impl.portal.Print` from `data/gtk4.portal`
with `sed`, matching the Linux-only Print gate and the same edit the FreeBSD CI
job applies before running the introspection test.

The install step redirects the Makefile's systemd user unit outside the staged
tree and discards it, since systemd is not used on FreeBSD. The D-Bus service,
desktop entry and `gtk4.portal` configuration are installed normally.

D-Bus-dependent integration tests need a session bus and are exercised by the
FreeBSD CI job in `.github/workflows/ci.yml`, not during the port build.

## Releases

Keep `DISTVERSION` in the Makefile in step with `[package].version` in
`Cargo.toml`. On a new release, refresh `distinfo` with `make makesum` from
the ports tree; `TIMESTAMP` is only a freshness marker and can be updated by
hand when adding the recipe to a tree.

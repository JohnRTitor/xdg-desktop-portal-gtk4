# Debian packaging for xdg-desktop-portal-gtk4

Community-maintained Debian packaging recipe, following the style of the
`ananicy-rs` packaging recipes.

## Build outline

```sh
cp -r packaging/debian debian
debian/make-vendor.sh
dpkg-buildpackage -us -uc -b
```

The vendored dependency archive (`debian/vendor.tar.xz`) is generated locally by
`make-vendor.sh` and is not committed. Builds run with `--locked --offline`.

Tests are not run during the package build because D-Bus/session-dependent tests
require a running session environment; they are exercised in CI workflows.

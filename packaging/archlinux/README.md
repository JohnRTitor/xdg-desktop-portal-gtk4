# Arch Linux packaging for xdg-desktop-portal-gtk4

Community-maintained PKGBUILD, following the style of the `ananicy-rs` packaging
recipes.

## Build

```sh
cd packaging/archlinux
makepkg -f
```

The recipe builds from the release tag (`v$pkgver`) and runs `cargo fetch` and
`cargo build --release --frozen` before installing with `make install`.
Integration tests requiring a D-Bus session are exercised in CI rather than in
the package check phase.

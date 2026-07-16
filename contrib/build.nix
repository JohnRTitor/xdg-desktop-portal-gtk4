{
  lib,
  rustPlatform,
  pkg-config,
  gtk4,
  glib,
  wrapGAppsHook4,
  meson,
  dbus,
  ninja,
  version ? "unstable",
  withDbusTests ? false,
}:

rustPlatform.buildRustPackage {
  pname = "xdg-desktop-portal-gtk4";
  inherit version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.fileFilter ({ hasExt, ... }: !hasExt "nix") ../.;
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  strictDeps = true;
  __structuredAttrs = true;

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook4
    meson
    ninja
  ];

  buildInputs = [
    gtk4
    glib
  ];

  # Prevent Nix from treating this primarily as a Meson package
  dontUseMesonConfigure = true;
  dontUseMesonBuild = true;
  dontUseMesonInstall = true;
  dontUseNinjaConfigure = true;
  dontUseNinjaBuild = true;
  dontUseNinjaInstall = true;

  mesonFlags = [
    "--libexecdir=libexec"
    "-Dsystemd-user-unit-dir=lib/systemd/user"
  ];

  nativeCheckInputs = lib.optionals withDbusTests [ dbus ];

  preCheck = lib.optionalString withDbusTests ''
    export RUN_DBUS_TESTS=1
    REAL_CARGO=$(command -v cargo)
    mkdir -p bin
    echo '#!/bin/sh' > bin/cargo
    echo "exec dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- $REAL_CARGO \"\$@\"" >> bin/cargo
    chmod +x bin/cargo
    export PATH=$(pwd)/bin:$PATH
  '';

  postInstall = ''
    # Let meson handle substituting templates and installing all files
    # Note: mesonConfigurePhase cd's into the build directory
    mesonConfigurePhase

    # Run meson install directly to avoid mesonInstallPhase recursively triggering postInstall
    meson install --no-rebuild
  '';

  meta = {
    description = "A portal backend for xdg-desktop-portal using GTK4";
    homepage = "https://github.com/JohnRTitor/xdg-desktop-portal-gtk4";
    license = lib.licenses.lgpl21Plus;
    maintainers = [ lib.maintainers.johnrtitor ];
    platforms = lib.platforms.linux;
  };
}

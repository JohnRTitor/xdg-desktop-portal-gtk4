{
  lib,
  rustPlatform,
  pkg-config,
  gtk4,
  glib,
  wrapGAppsHook4,
  meson,
  ninja,
}:

rustPlatform.buildRustPackage rec {
  pname = "xdg-desktop-portal-gtk4";
  version = "unstable";

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

  # We don't want cargo to install the binary to $out/bin, meson will install it to libexec
  dontCargoInstall = true;

  installPhase = ''
    runHook preInstall

    # Cargo might put the binary in target/<target-triple>/release/ depending on the host.
    # meson.build strictly expects it in target/release/, so we link it there.
    mkdir -p target/release
    find target -type f -name xdg-desktop-portal-gtk4 -executable -exec ln -sf $(pwd)/{} target/release/xdg-desktop-portal-gtk4 \;

    # Let meson handle substituting templates and installing all files
    mesonConfigurePhase
    mesonInstallPhase

    runHook postInstall
  '';

  meta = {
    description = "A portal backend for xdg-desktop-portal using GTK4";
    homepage = "https://github.com/JohnRTitor/xdg-desktop-portal-gtk4";
    license = lib.licenses.lgpl21Plus;
    maintainers = [ lib.maintainers.johnrtitor ];
    platforms = lib.platforms.linux;
  };
}

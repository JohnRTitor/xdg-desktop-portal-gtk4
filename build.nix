{
  lib,
  rustPlatform,
  pkg-config,
  gtk4,
  glib,
  wrapGAppsHook4,
}:

rustPlatform.buildRustPackage rec {
  pname = "xdg-desktop-portal-gtk4";
  version = "unstable";

  src = lib.cleanSource ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook4
  ];

  buildInputs = [
    gtk4
    glib
  ];

  postInstall = ''
    mkdir -p $out/libexec
    mv $out/bin/xdg-desktop-portal-gtk4 $out/libexec/
    rmdir $out/bin || true

    mkdir -p $out/share/xdg-desktop-portal/portals
    cp data/gtk4.portal $out/share/xdg-desktop-portal/portals/

    mkdir -p $out/share/applications
    substitute data/xdg-desktop-portal-gtk4.desktop.in $out/share/applications/xdg-desktop-portal-gtk4.desktop \
      --replace-fail "@libexecdir@" "$out/libexec"

    mkdir -p $out/share/dbus-1/services
    substitute data/org.freedesktop.impl.portal.desktop.gtk4.service.in $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.gtk4.service \
      --replace-fail "@libexecdir@" "$out/libexec"

    mkdir -p $out/share/systemd/user
    substitute data/xdg-desktop-portal-gtk4.service.in $out/share/systemd/user/xdg-desktop-portal-gtk4.service \
      --replace-fail "@libexecdir@" "$out/libexec"
  '';

  meta = {
    description = "A Gtk4 backend for xdg-desktop-portal";
    homepage = "https://github.com/JohnRTitor/xdg-desktop-portal-gtk4";
    license = lib.licenses.lgpl21Plus;
    maintainers = [ lib.maintainers.johnrtitor ];
    platforms = lib.platforms.linux;
  };
}

{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  gtk4,
  glib,
  wrapGAppsHook4,
  dbus,
  version ? "unstable",
  withDbusTests ? false,
}:
# GTK 4.22.0 through 4.22.4 broke file-chooser thumbnails: _gtk_file_info_get_icon
# fed the raster thumbnails of the freedesktop cache into the SVG-only
# gdk_texture_new_from_filename_at_scale, which parses arbitrary bytes as SVG and
# therefore returns a fully transparent texture for PNG/JPEG thumbnails. Upstream
# reverted the change with:
#   https://gitlab.gnome.org/GNOME/gtk/-/merge_requests/10080
#   4.22 branch: 89a430be07730ded37fd08321dec6bd960b81a34 (GTK 4.22.5)
#   main:        b682d926e18fdaa19d05603bf6c1d8717c3724b5 (GTK 4.24.0)
# Drop this override once nixpkgs ships gtk4 >= 4.22.5.
let
  gtk4ForPortal =
    if lib.versionOlder gtk4.version "4.22.5" then
      gtk4.overrideAttrs (old: {
        patches = (old.patches or [ ]) ++ [
          ../../patches/0001-Revert-filechooserutils-Stop-using-pixbufs.patch
        ];
      })
    else
      gtk4;
  # wrapGAppsHook4 propagates gtk4 (as `gtk3` arg) into depsTargetTargetPropagated,
  # which would otherwise leak the stock (unpatched) gtk4 into build inputs and RUNPATH.
  wrapGAppsHook4ForPortal =
    if lib.versionOlder gtk4.version "4.22.5" then
      wrapGAppsHook4.override { gtk3 = gtk4ForPortal; }
    else
      wrapGAppsHook4;
in
rustPlatform.buildRustPackage {
  pname = "xdg-desktop-portal-gtk4";
  inherit version;

  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.fileFilter ({ hasExt, ... }: !hasExt "nix") ../..;
  };

  cargoLock = {
    lockFile = ../../Cargo.lock;
  };

  strictDeps = true;
  __structuredAttrs = true;

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook4ForPortal
  ];

  buildInputs = [
    gtk4ForPortal
    glib
  ];

  nativeCheckInputs = lib.optionals withDbusTests [ dbus ];

  cargoTestFlags = lib.optionals withDbusTests [
    "--"
    "--test-threads=1"
  ];

  preCheck = lib.optionalString withDbusTests ''

    # 1. Initialize a temporary directory for the D-Bus runtime socket
    export XDG_RUNTIME_DIR=$(mktemp -d)

    # 2. Launch the system's dbus-daemon in the background and capture its address
    # We use --print-address=3 to cleanly output the connection string to a file descriptor
    dbus-daemon --config-file=${dbus}/share/dbus-1/session.conf \
                --print-address=3 \
                --nofork \
                3>dbus-address.txt &

    # Save the process ID so we can cleanly terminate it during postCheck
    DBUS_PID=$!

    # 3. Wait for the background daemon to finish writing its address string
    while [ ! -s dbus-address.txt ]; do
      sleep 0.1
    done

    # 4. Export the address so Cargo's test runner picks it up natively
    export DBUS_SESSION_BUS_ADDRESS=$(cat dbus-address.txt)
  '';

  # Clean up the background process after tests finish or fail
  postCheck = lib.optionalString withDbusTests ''
    if [ -n "$DBUS_PID" ]; then
      kill "$DBUS_PID"
    fi
  '';

  postInstall = ''
    # Remove the default bin directory provided by rustPlatform
    rm -rf $out/bin

    # Run our make install to correctly place the binary in libexec and copy data files
    make install DESTDIR= PREFIX=$out CARGO_TARGET_DIR=target/${stdenv.hostPlatform.rust.cargoShortTarget}
  '';

  meta = {
    description = "A portal backend for xdg-desktop-portal using GTK4";
    homepage = "https://github.com/JohnRTitor/xdg-desktop-portal-gtk4";
    license = lib.licenses.lgpl21Plus;
    maintainers = [ lib.maintainers.johnrtitor ];
    platforms = lib.platforms.linux;
  };
}

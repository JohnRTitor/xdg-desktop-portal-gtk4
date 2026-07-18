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
  dontUseNinjaCheck = true;
  dontUseMesonCheck = true;

  mesonFlags = [
    "--libexecdir=libexec"
    "-Dsystemd-user-unit-dir=lib/systemd/user"
  ];

  nativeCheckInputs = lib.optionals withDbusTests [ dbus ];

  cargoTestFlags = lib.optionals withDbusTests [
    "--"
    "--test-threads=1"
  ];

  preCheck = lib.optionalString withDbusTests ''
    export RUN_DBUS_TESTS=1

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

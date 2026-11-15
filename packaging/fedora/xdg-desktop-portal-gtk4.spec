# Fedora packaging recipe for xdg-desktop-portal-gtk4.
#
# Community-maintained recipe, not a Fedora package. See packaging/fedora/README.md.
# Keep Version in step with Cargo.toml's [package] version.

Name:           xdg-desktop-portal-gtk4
Version:        1.6.6
Release:        1%{?dist}
Summary:        GTK4 backend implementation of xdg-desktop-portal

License:        LGPL-2.1-only
URL:            https://github.com/JohnRTitor/xdg-desktop-portal-gtk4
Source0:        %{url}/releases/download/v%{version}/%{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  rust >= 1.92
BuildRequires:  make
BuildRequires:  gcc
BuildRequires:  pkgconf-pkg-config
BuildRequires:  gtk4-devel
BuildRequires:  gdk-pixbuf2-devel
BuildRequires:  dbus-daemon

Requires:       gtk4
Requires:       gdk-pixbuf2
Requires:       xdg-desktop-portal
Provides:       xdg-desktop-portal-impl

%description
xdg-desktop-portal-gtk4 is a backend implementation for xdg-desktop-portal
using GTK4. It provides portal interfaces such as file chooser, account,
app chooser, and other desktop integration dialogs for sandboxed applications.

%prep
%autosetup -n %{name}-%{version}

if ! grep -qx 'version = "%{version}"' Cargo.toml; then
    : 'Version: does not match [package] version in Cargo.toml'
    grep -n '^version = ' Cargo.toml || :
    exit 1
fi

%{__xz} -dc %{SOURCE1} | %{__tar} -x -C %{_builddir}

install -d %{_builddir}/cargo-home
sed -e 's|@VENDOR_DIR@|%{_builddir}/vendor|' \
    %{_builddir}/vendor-config.toml > %{_builddir}/cargo-home/config.toml

%build
CARGO_HOME=%{_builddir}/cargo-home \
    cargo build --release --locked --offline --verbose

%install
make install DESTDIR=%{buildroot} PREFIX=%{_prefix} LIBEXECDIR=%{_libexecdir} \
    CARGO_TARGET_DIR=target

%check
# D-Bus-dependent tests need a session bus; skip in %check and run separately
# in CI where appropriate.
:

%files
%license LICENSE
%{_libexecdir}/xdg-desktop-portal-gtk4
%{_datadir}/dbus-1/services/org.freedesktop.impl.portal.desktop.gtk4.service
%{_prefix}/lib/systemd/user/xdg-desktop-portal-gtk4.service
%{_datadir}/applications/xdg-desktop-portal-gtk4.desktop
%{_datadir}/xdg-desktop-portal/portals/gtk4.portal

%changelog
* Sun Nov 15 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.6.6-1
- portal: log UI cancellations separately from failures

* Thu Nov 12 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.6.5-1
- Improved file chooser thumbnail handling so previews are more reliable and use fewer resources in the background.

* Thu Nov 12 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.6.4-1
- Improved image thumbnail generation in the file chooser, with better fallback behavior (including use of glycin when available) for more reliable previews.
- General responsiveness and stability improvements.

* Wed Nov 11 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.6.3-1
- **Maintenance**
  - Internal test and CI improvements, including fixes to email portal test isolation.

* Wed Nov 11 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.6.2-1
- **File chooser**
  - Improve thumbnail generation reliability by using a GLib thread pool for thumbnail work.

* Wed Nov 11 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.6.1-1
- **File chooser**
  - Reactively attach the thumbnailer when the dialog is mapped and during view transitions, improving thumbnail display reliability.

* Tue Nov 10 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.6.0-1
- **Clipboard portal support**: Added support for the Clipboard portal, enabling sandboxed applications to use clipboard functionality through this backend.
- **File chooser thumbnails**: File chooser dialogs can now show thumbnails via direct thumbnail generation following the FreeDesktop thumbnailing specification, with improved handling in headless/sandboxed environments.
- **File chooser**
  - Correctly honors `current_filter` (including when provided without a `filters` list).
  - Defaults file-open requests to writable unless read-only access is explicitly requested.
  - Rejects empty save-file names and correctly applies the provided current file path.
- **App chooser & Dynamic launcher**
  - `ChooseApplication` now returns application IDs without the `.desktop` suffix.
  - `UpdateChoices` now reports an error for unknown/invalid handles instead of silently succeeding.
  - Restores correct handling of chooser/launcher options and result codes (including launcher type, modality, and empty-name cases).
- **Print**
  - Applies caller-provided print and page setup settings in the print dialog.
  - Binds prepared-print tokens to the requesting application to prevent misuse across apps.
- **Notifications**
  - Routes notification actions correctly and improves reliability when handling notification server signals.
- **Clipboard**
  - Validates session handles and session state for clipboard operations to ensure requests are handled for the correct session.
  - Improves clipboard ownership/change signaling and session behavior.
- **Inhibit**
  - Correctly reports session state and initializes new monitors as expected.
- **Settings**
  - `ReadAll` now returns values for the namespaces that are actually loaded.
  - Improves settings handling for more accurate results across supported configuration sources.
- **Email**
  - Sends `cc` and `bcc` recipients correctly (as single parameters) when composing emails.
- **Requests, sessions, and D-Bus behavior**
  - Reports correct response codes for dialogs/requests that were closed or never opened (instead of incorrectly reporting user cancellation in some cases).
  - Exposes portal/session version information correctly over D-Bus.
  - Enforces per-app request limits correctly and cleans up requests/sessions when client applications disconnect, improving reliability and preventing resource leaks.
- **Translations**
  - Updates translations across multiple locales.

* Sun Aug 02 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.5.1-1
- Release v1.5.1.

* Sun Aug 02 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.5.0-1
- Release v1.5.0.

* Sun Jul 19 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.4.0-1
- This release brings several notable improvements for end users, specifically focusing on notifications and overall stability!
- **Better Notifications**: Completely refactored how the notification portal works! Notifications are now fully compliant with desktop specifications, making them much more reliable and ensuring they integrate seamlessly with your desktop environment.
- **Performance Boost & Reduced Memory Usage**: Made comprehensive memory optimizations. The portal now uses less RAM and runs smoother, thanks to the resolution of multiple memory leaks and streamlined UI operations.
- **Improved Session Stability**: A new, more robust Session Manager ensures that portal sessions clean up properly. No longer leaves orphaned processes running when applications close or crash unexpectedly.
- Thorough testing implemented across all portals to prevent future regressions.
- CI/CD pipelines overhauled to improve the development and delivery process.
- Documentation updates, including a new acknowledgements section.

* Thu Jul 16 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.3.1-1
- Release v1.3.1.

* Thu Jul 16 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.3.0-1
- v1.3.0 is a major release focused on compatibility and responsiveness, introducing X11/XWayland support and a fully asynchronous D-Bus backend.

* Thu Jul 16 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.2.1-1
- update cargo dependencies

* Thu Jul 16 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.2.0-1
- This release brings crucial testing infrastructure, build process enhancements, and an important fix for remote file selection in the File Chooser portal.
- 🐛 Bug Fixes
- File Chooser: Translates remote URIs (e.g. smb://) to local FUSE URIs (e.g. file://). This ensures that remote files selected via the file chooser are translated into locally accessible files via Gvfs's FUSE mount, finally allowing containerized applications (like Flatpaks) to access them. (Ported from GNOME/xdg-desktop-portal-gnome!67).

* Tue Jul 14 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.1.0-1
- This release focuses on extensive refactoring, bringing our portal implementations into full compliance with the XDG specification.
- **Portal Compliance:** Implement full XDG portal compliance for AppChooser, Inhibit, Notification, and Settings portals. (6121bda)
- **Translations:** Added new localization/translation strings from https://github.com/flatpak/xdg-desktop-portal-gtk. (b11daff)
- **D-Bus Specification Alignment:** Rigorously aligned various portal implementations with the official D-Bus API spec. (688c0bc)
- **AppChooser/Notification/Settings:** Resolved minor DBus portal incompatibilities to ensure proper spec alignment. (54dfab5, edc7c23)
- **GtkDialog:** Fixed GtkDialog transient parent warnings in portals. (bda96b7)
- **DynamicLauncher:**
  - Fixed icon parsing and lockdown properties. (d1d535b)
  - Enforced allowlist for DynamicLauncher and added missing D-Bus properties. (59c69c8)

* Tue Jul 14 2026 xdg-desktop-portal-gtk4 packagers <JohnRTitor@users.noreply.github.com> - 1.0.0-1
- xdg-desktop-portal-gtk4 is a portal backend for xdg-desktop-portal using GTK4.
- This initial release implements the following XDG Desktop Portal interfaces:
- **Access**: Prompting for device/resource access.
- **Account**: Providing user account information.
- **AppChooser**: Selecting an application to open a file or URI.
- **DynamicLauncher**: Managing dynamic desktop launchers.
- **Email**: Composing emails.
- **FileChooser**: Opening and saving files (with GTK4 native UI and thumbnails).
- **Inhibit**: Inhibiting session state (like sleep or logout).
- **Lockdown**: Querying locked-down features (e.g. disable printing/saving).
- **Notification**: Displaying desktop notifications.
- **Print**: Printing documents via native GTK4 PrintUnixDialog and CUPS dispatch.
- **Settings**: Reading desktop settings (such as color-scheme for dark mode).
- **USB**: Managing USB device access.
- Written in safe, idiomatic Rust.
- Built with `zbus` for efficient and memory-safe D-Bus interactions.
- Includes Nix Flakes and Arch Linux PKGBUILD for easy building and distribution.
- Integrates cleanly with Wayland compositors utilizing GTK4.

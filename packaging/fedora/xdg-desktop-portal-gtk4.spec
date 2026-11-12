# Fedora packaging recipe for xdg-desktop-portal-gtk4.
#
# Community-maintained recipe, not a Fedora package. See packaging/fedora/README.md.
# Keep Version in step with Cargo.toml's [package] version.

Name:           xdg-desktop-portal-gtk4
Version:        1.6.5
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
* Thu Nov 12 2026 xdg-desktop-portal-gtk4 packagers <xdg-desktop-portal-gtk4-packaging@example.invalid> - 1.6.5-1
- Initial community packaging recipe, built from the v1.6.5 release tarball with
  Cargo dependencies vendored for a network-isolated build.

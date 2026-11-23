# FreeBSD port skeleton for xdg-desktop-portal-gtk4.
# Community-maintained recipe, not an official FreeBSD port.
# See packaging/freebsd/README.md.

PORTNAME=	xdg-desktop-portal-gtk4
DISTVERSION=	1.6.7
CATEGORIES=	x11
MASTER_SITES=	https://github.com/JohnRTitor/${PORTNAME}/releases/download/v${DISTVERSION}/

MAINTAINER=	xdg-desktop-portal-gtk4-packaging@example.invalid
COMMENT=	GTK4 backend for xdg-desktop-portal
WWW=		https://github.com/JohnRTitor/xdg-desktop-portal-gtk4

LICENSE=	LGPL21
LICENSE_FILE=	${WRKSRC}/LICENSE

LIB_DEPENDS=	libgtk-4.so:x11-toolkits/gtk40 \
		libgdk_pixbuf-2.0.so:graphics/gdk-pixbuf2
RUN_DEPENDS=	xdg-desktop-portal:deskutils/xdg-desktop-portal \
		dbus-daemon:devel/dbus
BUILD_DEPENDS=	cargo:lang/rust

USES=		gmake pkgconfig

# Cargo fetches crates from crates.io during the build, like the Arch recipe.
# Keep the registry out of the build user's home directory.
CARGO_HOME=	${WRKDIR}/cargo-home

do-build:
	@${MKDIR} ${CARGO_HOME}
	# The Print portal is Linux-only in this build. Keep the advertised
	# interface list in sync or the introspection test disagrees with the
	# binary (same edit the FreeBSD CI job applies before testing).
	sed -i '' 's|org\.freedesktop\.impl\.portal\.Print;||' \
		${WRKSRC}/data/gtk4.portal
	cd ${WRKSRC} && ${SETENV} ${MAKE_ENV} CARGO_HOME=${CARGO_HOME} \
		cargo build --release --locked

do-install:
	# The systemd user unit is meaningless on FreeBSD. Redirect it outside
	# the staged tree and discard it, then let the regular data install handle
	# the D-Bus service, desktop entry and portal configuration.
	cd ${WRKSRC} && ${SETENV} ${MAKE_ENV} ${GMAKE} install \
		DESTDIR=${STAGEDIR} \
		PREFIX=${PREFIX} \
		LIBEXECDIR=${PREFIX}/libexec \
		CARGO_TARGET_DIR=target \
		SYSTEMD_USER_UNIT_DIR=/no-systemd
	${RM} -rf ${STAGEDIR}/no-systemd

.include <bsd.port.mk>

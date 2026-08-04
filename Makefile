APPID := org.freedesktop.impl.portal.desktop.gtk4
NAME := xdg-desktop-portal-gtk4

PREFIX ?= /usr
DATADIR ?= $(PREFIX)/share
LIBEXECDIR ?= $(PREFIX)/libexec
LIBDIR ?= $(PREFIX)/lib
DBUS_SERVICE_DIR ?= $(DATADIR)/dbus-1/services
SYSTEMD_USER_UNIT_DIR ?= $(LIBDIR)/systemd/user

CARGO_TARGET_DIR ?= target
DEBUG ?= 0
ifeq ($(DEBUG),0)
	TARGET := release
	PROFILE_ARGS := --release
else
	TARGET := debug
	PROFILE_ARGS :=
endif

BIN_SRC := $(CARGO_TARGET_DIR)/$(TARGET)/$(NAME)
BIN_DST := $(DESTDIR)$(LIBEXECDIR)/$(NAME)

.PHONY: all build clean install

all: build

build:
	cargo build $(PROFILE_ARGS)

clean:
	cargo clean

install:
	install -Dm0755 $(BIN_SRC) $(BIN_DST)
	
	install -dm0755 $(DESTDIR)$(DBUS_SERVICE_DIR)
	sed -e 's|@libexecdir@|$(LIBEXECDIR)|g' data/$(APPID).service.in > data/$(APPID).service
	install -Dm0644 data/$(APPID).service $(DESTDIR)$(DBUS_SERVICE_DIR)/$(APPID).service
	
	install -dm0755 $(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)
	sed -e 's|@libexecdir@|$(LIBEXECDIR)|g' data/$(NAME).service.in > data/$(NAME).service
	install -Dm0644 data/$(NAME).service $(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)/$(NAME).service
	
	install -dm0755 $(DESTDIR)$(DATADIR)/applications
	sed -e 's|@libexecdir@|$(LIBEXECDIR)|g' data/$(NAME).desktop.in > data/$(NAME).desktop
	install -Dm0644 data/$(NAME).desktop $(DESTDIR)$(DATADIR)/applications/$(NAME).desktop
	
	install -Dm0644 data/gtk4.portal $(DESTDIR)$(DATADIR)/xdg-desktop-portal/portals/gtk4.portal

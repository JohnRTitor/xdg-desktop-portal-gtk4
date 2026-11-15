.DEFAULT_GOAL := all

APPID := org.freedesktop.impl.portal.desktop.gtk4
NAME := xdg-desktop-portal-gtk4

# Installation directories.
# Override PREFIX to relocate the entire installation.
#
# Example:
#   make install PREFIX=/usr
#   make install PREFIX=/usr/local
#   make install PREFIX=/usr DESTDIR=/tmp/package

PREFIX ?= /usr
DESTDIR ?=

DATADIR ?= $(PREFIX)/share
LIBEXECDIR ?= $(PREFIX)/libexec
LIBDIR ?= $(PREFIX)/lib

DBUS_SERVICE_DIR ?= $(DATADIR)/dbus-1/services
SYSTEMD_USER_UNIT_DIR ?= $(LIBDIR)/systemd/user
PORTAL_DIR ?= $(DATADIR)/xdg-desktop-portal/portals
APPLICATIONS_DIR ?= $(DATADIR)/applications

INSTALL ?= install
SED ?= sed
CARGO ?= cargo

# Cargo build configuration.

CARGO_TARGET_DIR ?= target

DEBUG ?= 0

ifeq ($(DEBUG),0)
	TARGET ?= release
else
	TARGET ?= debug
endif

ifeq ($(TARGET),release)
	PROFILE_ARGS := --release
else ifeq ($(TARGET),debug)
	PROFILE_ARGS :=
else
	PROFILE_ARGS := --profile $(TARGET)
endif

BIN_SRC := $(CARGO_TARGET_DIR)/$(TARGET)/$(NAME)
BIN_DST := $(DESTDIR)$(LIBEXECDIR)/$(NAME)

# Generated service/desktop files are staged outside the source tree.
GEN_DIR := $(CARGO_TARGET_DIR)/generated

DBUS_SERVICE_IN := data/$(APPID).service.in
SYSTEMD_SERVICE_IN := data/$(NAME).service.in
DESKTOP_IN := data/$(NAME).desktop.in
PORTAL_DATA := data/gtk4.portal

DBUS_SERVICE_GEN := $(GEN_DIR)/$(APPID).service
SYSTEMD_SERVICE_GEN := $(GEN_DIR)/$(NAME).service
DESKTOP_GEN := $(GEN_DIR)/$(NAME).desktop


.PHONY: all build clean generate-data
.PHONY: install install-bin install-data


# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

all: build

build:
	$(CARGO) build $(PROFILE_ARGS)

clean:
	$(CARGO) clean
	rm -rf "$(GEN_DIR)"


# ---------------------------------------------------------------------------
# Data generation
# ---------------------------------------------------------------------------

# Substitute installation paths into service and desktop templates.
# DESTDIR is intentionally excluded from embedded paths: it is only a staging
# directory, not the final installation prefix.

generate-data: $(DBUS_SERVICE_GEN) $(SYSTEMD_SERVICE_GEN) $(DESKTOP_GEN)

$(GEN_DIR):
	$(INSTALL) -d "$(GEN_DIR)"

$(DBUS_SERVICE_GEN): $(DBUS_SERVICE_IN) | $(GEN_DIR)
	$(SED) -e 's|@libexecdir@|$(LIBEXECDIR)|g' "$(DBUS_SERVICE_IN)" > "$@"

$(SYSTEMD_SERVICE_GEN): $(SYSTEMD_SERVICE_IN) | $(GEN_DIR)
	$(SED) -e 's|@libexecdir@|$(LIBEXECDIR)|g' "$(SYSTEMD_SERVICE_IN)" > "$@"

$(DESKTOP_GEN): $(DESKTOP_IN) | $(GEN_DIR)
	$(SED) -e 's|@libexecdir@|$(LIBEXECDIR)|g' "$(DESKTOP_IN)" > "$@"


# ---------------------------------------------------------------------------
# Installation
# ---------------------------------------------------------------------------

install-bin:
	$(INSTALL) -Dm0755 "$(BIN_SRC)" "$(BIN_DST)"

install-data: generate-data
	$(INSTALL) -Dm0644 "$(DBUS_SERVICE_GEN)" "$(DESTDIR)$(DBUS_SERVICE_DIR)/$(APPID).service"
	$(INSTALL) -Dm0644 "$(SYSTEMD_SERVICE_GEN)" "$(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)/$(NAME).service"
	$(INSTALL) -Dm0644 "$(DESKTOP_GEN)" "$(DESTDIR)$(APPLICATIONS_DIR)/$(NAME).desktop"
	$(INSTALL) -Dm0644 "$(PORTAL_DATA)" "$(DESTDIR)$(PORTAL_DIR)/gtk4.portal"

install: install-bin install-data

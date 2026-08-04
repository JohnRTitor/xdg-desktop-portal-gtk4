//! # Lockdown Portal
//!
//! ## Portal Purpose
//!
//! The Lockdown portal provides a mechanism for system administrators or desktop environments
//! to communicate global restrictions (lockdowns) to sandboxed applications. For instance, in a
//! kiosk environment, an administrator might disable printing or saving files to disk.
//!
//! Sandboxed applications (and toolkits like GTK/Qt running inside them) query this portal to
//! determine if certain features should be disabled or hidden in their UI.
//!
//! This implementation provides a baseline stub that currently defaults to allowing everything
//! (returning `false` for all restrictions).
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Lockdown`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Properties:** Exposes boolean properties like `disable-printing`, `disable-save-to-disk`,
//!   `disable-application-handlers`, `disable-location`, `disable-camera`, `disable-microphone`,
//!   and `disable-sound-output`.
//!
//! **Expected Caller Behavior:**
//! Callers read these properties using standard D-Bus `org.freedesktop.DBus.Properties.Get` or
//! `GetAll` calls. They cannot write to these properties; the portal is strictly read-only for apps.
//!
//! **Implementation Mapping:**
//! The D-Bus interface is implemented in `dbus.rs` by the `LockdownPortal` struct. Each lockdown
//! property maps to an `async fn` property getter (and a dummy setter that returns `NotSupported`).
//!
//! ## Request Lifecycle
//!
//! 1. **Application** attempts to access a restricted feature (e.g., printing).
//! 2. **Toolkit/App** queries the `disable-printing` property via D-Bus.
//! 3. **D-Bus Daemon** routes the request to `xdg-desktop-portal-gtk4`.
//! 4. **Portal Object (`LockdownPortal`)** receives the property read request.
//! 5. **Processing:** The portal returns `false` (hardcoded).
//! 6. **Response:** The application proceeds, knowing the feature is not locked down.
//!
//! **Ownership:** `LockdownPortal` holds no state. Requests are stateless property reads.
//!
//! ## Session Management
//!
//! The Lockdown portal does not use sessions. It relies entirely on static D-Bus properties.
//!
//! ## GTK Integration
//!
//! This portal **does not** require an internal GTK UI or dialogs. It operates purely within
//! the D-Bus/Tokio async layer without dispatching to the GTK main loop.
//!
//! ## Backend Interaction
//!
//! Currently, there is no backend interaction. A future, more complete implementation might
//! interact with GSettings (e.g., `org.gnome.desktop.lockdown`) or a configuration file to
//! dynamically determine these values based on the host environment.
//!
//! ## Specification Notes
//!
//! - **Read-Only Enforcement:** The XDG Desktop Portal specification implies these properties
//!   are controlled by the host system. Therefore, the setters for all properties in `dbus.rs`
//!   explicitly return `zbus::fdo::Error::NotSupported` to enforce read-only behavior for
//!   sandboxed apps.
//!
//! ## Extension Guide
//!
//! For future contributors extending the Lockdown portal:
//! - **New Properties:** Add new `async fn` getters decorated with `#[zbus(property, name = "...")]`
//!   to `LockdownPortal` in `dbus.rs`. Remember to add a corresponding setter that returns
//!   `NotSupported`.
//! - **Implementing Real Lockdowns:** If you wish to implement actual lockdown reads (e.g., from
//!   GSettings), you would add an async initialization step or a glib signal listener (similar to the
//!   Settings portal) to update an internal state Cache, and emit `PropertiesChanged` signals when
//!   the host lockdown settings change.
//!
//! ## Cross-Portal Consistency
//!
//! - **Stateless Design:** Similar to the `Email` portal, it relies entirely on the host environment
//!   and requires no complex session tracking or UI.
//!
//! ## Maintenance Notes
//!
//! - **Why is this a stub?** Fully implementing this requires deep integration with specific desktop
//!   environments (like GNOME's lockdown GSettings schemas). Since `xdg-desktop-portal-gtk4` aims to
//!   be broadly applicable but lightweight, hardcoding to `false` is a safe, spec-compliant default
//!   until dynamic host-policy reading is required.

pub mod dbus;

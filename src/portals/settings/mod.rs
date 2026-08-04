//! # Settings Portal
//!
//! ## Portal Purpose
//!
//! The Settings portal allows sandboxed applications to securely read system-wide settings
//! and user preferences without granting them raw, unfettered access to the host's configuration
//! backend (e.g., dconf or GSettings).
//!
//! This portal is a cornerstone of the modern Linux desktop experience, as it allows flatpaks
//! and snaps to react to user preferences like Dark Mode (`color-scheme`), accessibility settings
//! (`high-contrast`), and UI animations (`reduced-motion`). It sits between the application
//! toolkit (like GTK or Qt inside the sandbox) and the host desktop environment.
//!
//! This implementation completely fulfills the `org.freedesktop.portal.Settings` specification
//! up to Version 2.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Settings`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `Read(namespace, key)`, `ReadAll(namespaces)`
//! - **Signals:** `SettingChanged(namespace, key, value)`
//!
//! **Expected Caller Behavior:**
//! Callers can read specific keys or bulk-read entire namespaces (via `ReadAll`). They must also
//! listen to the `SettingChanged` signal to update their internal state dynamically when the user
//! changes a setting on the host.
//!
//! **Implementation Mapping:**
//! Implemented in `dbus.rs` by the `SettingsPortal` struct. The methods map directly to Rust async
//! functions.
//!
//! ## Request Lifecycle
//!
//! **Read Request:**
//! 1. **Application** calls `Read(namespace, key)`.
//! 2. **Portal Object (`SettingsPortal`)** receives the request.
//! 3. **Processing:** The portal delegates to `read_setting_static`. It checks if the requested
//!    namespace/key maps to a known GTK/GNOME setting (`org.gnome.desktop.interface`). If it's a
//!    standardized Freedesktop appearance setting (`org.freedesktop.appearance`), it translates the
//!    request to the corresponding GTK setting (e.g., `color-scheme` -> GNOME `color-scheme`).
//! 4. **Backend Interaction:** It queries the underlying `gtk4::gio::Settings` object for the value.
//! 5. **Response:** It wraps the value in a `zbus::zvariant::OwnedValue` and returns it.
//!
//! **Signal Emission (Push):**
//! 1. During initialization (`SettingsPortal::new`), the portal connects a `changed` signal listener
//!    to the host's `org.gnome.desktop.interface` GSettings schema.
//! 2. When a host setting changes, the listener closure executes.
//! 3. It reads the new value, maps the key if necessary (handling both GNOME and Freedesktop namespaces),
//!    and spawns a local GLib task.
//! 4. The local task emits the `SettingChanged` D-Bus signal to all connected sandboxed apps.
//!
//! **Ownership:** `SettingsPortal` holds no complex state, but it leverages `thread_local!` storage
//! (`GNOME_SETTINGS`) to cache the `gtk4::gio::Settings` handle, preventing repeated initialization
//! overhead.
//!
//! ## Session Management
//!
//! The Settings portal does not use sessions. Setting reads are instant, and signal subscriptions
//! are handled natively by the D-Bus message bus without explicit portal-managed session objects.
//!
//! ## GTK Integration
//!
//! This portal has no UI. However, it tightly integrates with GTK/GIO to access the host's configuration:
//! - It requires the GLib `MainContext` to handle `GSettings` signal emissions.
//! - When a `GSettings` change occurs, it spawns a task onto `gtk4::glib::MainContext::default()`
//!   to safely bridge the GLib signal callback into the `zbus` async world to emit the D-Bus signal.
//!
//! ## Backend Interaction
//!
//! The backend is `gtk4::gio::Settings`.
//! - **Responsibilities:** Abstract away `dconf` or whatever configuration backend the host uses.
//! - **Failure Handling:** If a key or namespace is not found, `Read` returns a standardized D-Bus error.
//!
//! ## Specification Notes
//!
//! - **Namespace Translation:** The XDG specification defines `org.freedesktop.appearance` as a
//!   cross-desktop standard for themes. Because this portal is GTK-specific, it explicitly translates
//!   these standardized keys (`color-scheme`, `contrast`, `reduced-motion`) into their corresponding
//!   `org.gnome.desktop.interface` GSettings keys. This translation is mandatory for cross-desktop compatibility.
//! - **Version 2:** The `ReadAll` method was introduced in version 2 of the specification, which is
//!   why the `version()` property explicitly returns `2`.
//!
//! ## Extension Guide
//!
//! For future contributors extending the Settings portal:
//! - **New Namespaces:** If a new standardized namespace is added to the specification, update
//!   `read_setting_with_settings` and `read_all` to handle the new keys, mapping them to the appropriate
//!   host GSettings or config files.
//! - **Signal Mapping:** Make sure that if you add support for a new host setting, you also update the
//!   `connect_changed` closure in `SettingsPortal::new` to emit `SettingChanged` for that new key.
//!
//! ## Cross-Portal Consistency
//!
//! - **GIO Reliance:** Like the `Email` portal, it relies heavily on `gio` primitives (`gio::Settings`)
//!   rather than reinventing configuration parsing.
//!
//! ## Maintenance Notes
//!
//! - **Why is `thread_local!` used?** `gtk4::gio::Settings` is not `Send`, meaning it cannot be safely
//!   passed across Tokio worker threads. By storing it in `thread_local!`, we ensure it remains on the
//!   thread that initialized it, while still allowing asynchronous D-Bus methods to query it without
//!   complex thread-synchronization overhead.

pub mod dbus;

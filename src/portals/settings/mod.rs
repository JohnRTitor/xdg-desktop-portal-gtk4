//! # Settings Portal
//!
//! ## Portal Purpose
//!
//! The Settings portal allows sandboxed applications to securely read system-wide settings
//! and user preferences without granting them raw, unfettered access to the host's configuration
//! backend (e.g., dconf, kdeglobals, or gtk-settings.ini).
//!
//! This portal is a cornerstone of the modern Linux desktop experience, as it allows flatpaks
//! and snaps to react to user preferences like Dark Mode (`color-scheme`), accessibility settings
//! (`high-contrast`), and UI animations (`reduced-motion`). It sits between the application
//! toolkit (like GTK or Qt inside the sandbox) and the host desktop environment.
//!
//! This implementation fulfills the `org.freedesktop.portal.Settings` specification
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
//! ## Architecture: SettingsAggregator
//!
//! Unlike simple implementations that only wrap GSettings, this portal uses a unified
//! `SettingsAggregator` to provide best-effort values across multiple wlroots-based and
//! generic Wayland compositors (Hyprland, Sway, River, etc.).
//!
//! It aggregates values from:
//! 1. **GSettings:** Specifically `org.gnome.desktop.interface`.
//! 2. **GTK Settings:** `~/.config/gtk-3.0/settings.ini` and `~/.config/gtk-4.0/settings.ini`.
//! 3. **KDE Settings:** `~/.config/kdeglobals` (for fallback values like color schemes).
//!
//! **State Cache:**
//! The aggregator maintains an internal `SettingsState` protected by an `Arc<RwLock<SettingsState>>`.
//! This cache allows immediate, non-blocking reads from asynchronous D-Bus handlers without needing
//! to query configuration files or the D-Bus system bus on every request.
//!
//! ## Request Lifecycle
//!
//! **Read Request:**
//! 1. **Application** calls `Read(namespace, key)`.
//! 2. **Portal Object (`SettingsPortal`)** acquires a read lock on the aggregator's state.
//! 3. **Processing:** It queries the cache for the requested namespace and key. If the client requests
//!    the standard `org.freedesktop.appearance` namespace, it returns the pre-translated values
//!    derived from the aggregated configuration backends.
//! 4. **Response:** It returns the `zbus::zvariant::OwnedValue`.
//!
//! **Signal Emission (Push):**
//! 1. The `SettingsAggregator` uses the `notify` crate to recursively watch `~/.config/` for changes
//!    to relevant configuration files.
//! 2. When a file change is detected, a debounced reload task triggers `aggregator.reload_all()`.
//! 3. The reload computes a diff between the old and new states.
//! 4. For every changed key, it broadcasts the `SettingChanged` D-Bus signal to all connected
//!    sandboxed apps.
//!
//! ## Session Management
//!
//! The Settings portal does not use sessions. Setting reads are instant, and signal subscriptions
//! are handled natively by the D-Bus message bus without explicit portal-managed session objects.
//!
//! ## GTK Integration & Threading
//!
//! - The `notify` watcher and debouncer are spawned onto the `gtk4::glib::MainContext::default()`
//!   using `spawn_local`. This keeps file-watching logic correctly anchored within the GTK main loop.
//! - By using `Arc<RwLock<SettingsState>>`, we avoid the need for `!Send` thread-local hacks (like `thread_local!`)
//!   that were previously used. The D-Bus worker thread (Tokio) can safely acquire a read lock on the
//!   state concurrently with the GTK main thread updating it.
//!
//! ## Extension Guide
//!
//! For future contributors extending the Settings portal:
//! - **New Backends:** Add a new reading function (like `read_kdeglobals`) in `aggregator.rs` and call
//!   it inside `reload_all`.
//! - **New Namespaces:** Map the new standardized namespace inside `reload_all` to populate the `SettingsState`.
//!   The `Read` and `ReadAll` methods will automatically pick up the new cached values.

pub mod aggregator;
pub mod dbus;

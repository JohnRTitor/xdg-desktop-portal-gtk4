//! # Notification Portal
//!
//! ## Portal Purpose
//!
//! The Notification portal allows sandboxed applications to send desktop notifications
//! safely, without granting them direct access to the host's `org.freedesktop.Notifications`
//! service.
//!
//! Sandboxed apps cannot reliably use the standard Notifications D-Bus interface directly
//! for two reasons:
//! 1. Security: Apps could spam the notification daemon or spoof notifications from other apps.
//! 2. Action Routing: When a user clicks a notification button (e.g., "Reply"), the host
//!    daemon needs to activate the application. The host daemon doesn't know how to reach
//!    into the sandbox, so the portal must proxy these activation signals back to the app.
//!
//! This portal completely implements the `org.freedesktop.portal.Notification` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Notification`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `AddNotification(app_id, id, notification)`, `RemoveNotification(app_id, id)`
//! - **Signals:** `ActionInvoked(app_id, id, action, parameter)`
//!
//! **Expected Caller Behavior:**
//! Callers pass an app-specific notification ID (`id`) and a dictionary of notification properties
//! (`title`, `body`, `icon`, `buttons`, etc.). When a user interacts with a button, the caller
//! expects to receive the `ActionInvoked` signal with the corresponding action string.
//!
//! **Implementation Mapping:**
//! Implemented in `dbus.rs` by the `Notification` struct. It maps the sandboxed app's
//! `(app_id, portal_id)` composite key to a single system-wide `u32` notification ID used by
//! the host daemon.
//!
//! ## Request Lifecycle
//!
//! **Adding a Notification:**
//! 1. **Application** calls `AddNotification(app_id, id, dict)`.
//! 2. **Portal Object (`Notification`)** receives the request.
//! 3. **Validation & Translation:** It extracts strings, maps priorities (e.g., "high" to urgency 2),
//!    and processes icons and sounds.
//!    - *Icons:* If the app provides raw pixel data (a memfd "file-descriptor" or "bytes"), the
//!      portal safely loads it into a `gdk_pixbuf::Pixbuf` and extracts the pixels for the host daemon.
//!    - *Sounds:* If the app provides a sound file via FD, the portal copies it to a temporary
//!      file in `/tmp/xdg-desktop-portal-gtk4-sounds/` and passes that path to the host daemon.
//! 4. **Backend Interaction:** It calls `org.freedesktop.Notifications.Notify` on the Session Bus.
//!    If this notification is replacing an existing one (matching `app_id` + `id`), it passes the
//!    existing host `u32` ID to replace it.
//! 5. **State Tracking:** The returned system `u32` ID is saved in two thread-safe maps:
//!    - `active_notifications`: Maps `(app_id, portal_id)` -> `u32`.
//!    - `reverse_map`: Maps `u32` -> `(app_id, portal_id, action_targets, TempSoundFile)`.
//!
//! **Action Invoked (User Clicks a Button):**
//! 1. The background task (`listen_for_action_invoked`) receives `ActionInvoked(u32)` from the host daemon.
//! 2. It looks up the `u32` in the `reverse_map` to find the original `app_id` and `portal_id`.
//! 3. It formats the parameters and uses `org.freedesktop.Application.ActivateAction` (or `Activate`)
//!    on the Session Bus to wake up the sandboxed app.
//! 4. It also explicitly emits the portal's own `ActionInvoked` D-Bus signal as required by the spec.
//!
//! **Notification Closed:**
//! 1. The background task (`listen_for_notification_closed`) receives `NotificationClosed(u32)`.
//! 2. It removes the `u32` from `reverse_map`.
//! 3. This drops the `TempSoundFile` reference, triggering its `Drop` implementation which deletes
//!    the temporary sound file from disk.
//! 4. It cleans up the `active_notifications` map.
//!
//! ## Session Management
//!
//! Notifications do not use formal Portal Sessions (like ScreenCast or FileChooser). Instead,
//! state is implicitly managed via the `id` string provided by the app, mapped internally to the
//! host's `u32` ID.
//!
//! ## GTK Integration
//!
//! This portal has no UI of its own; it entirely delegates rendering to the host's notification daemon
//! (e.g., GNOME Shell or Mako).
//! However, it uses GTK/GIO to safely parse raw image data (`gdk_pixbuf::Pixbuf`) in background
//! threads (`tokio::task::spawn_blocking`).
//!
//! ## Backend Interaction
//!
//! The sole backend is `org.freedesktop.Notifications` on the Session Bus.
//! - **Failure Handling:** If the host daemon is unavailable, the `Notify` call fails silently.
//!
//! ## Specification Notes
//!
//! - **Security (Icons):** Sandboxed apps cannot pass arbitrary file paths for icons. They must pass
//!   themed icon names or raw image data. The portal handles the conversion of raw bytes to the
//!   `iiibiiay` variant structure required by the host daemon.
//! - **Application Activation:** The spec requires the portal to attempt to activate the application
//!   via `org.freedesktop.Application` when an action is invoked. This is crucial for Flatpaks, as
//!   the app might not be running in the background when the user clicks the notification.
//!
//! ## Extension Guide
//!
//! For future contributors extending the Notification portal:
//! - **New Capabilities:** The host notification spec supports many hints (e.g., inline replies).
//!   If the portal spec adds support for these, extract them from the `PortalNotification` struct
//!   and translate them into the `hints` map passed to the host daemon.
//! - **Temporary Files:** Be extremely careful with temporary file lifetimes. If you add new temporary
//!   assets (like images), use the RAII pattern demonstrated by `TempSoundFile` to ensure they are
//!   deleted when the notification is closed.
//!
//! ## Cross-Portal Consistency
//!
//! - **Background Listeners:** Like the `Inhibit` portal, this portal uses `std::sync::Once` to spawn
//!   perpetual background Tokio tasks to listen to host daemon signals (`ActionInvoked`, `NotificationClosed`).
//!
//! ## Maintenance Notes
//!
//! - **Why two maps?**
//!   - `active_notifications` is keyed by the app's ID so `RemoveNotification` can find the host ID.
//!   - `reverse_map` is keyed by the host ID so the background signal listeners can route host events
//!     back to the correct app.
//! - **Sound File Leaks:** If the portal crashes, `/tmp/xdg-desktop-portal-gtk4-sounds/` might accumulate
//!   orphaned files because `Drop` isn't called. A future enhancement could clear this directory on startup.

pub mod dbus;

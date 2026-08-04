//! # Inhibit Portal
//!
//! ## Portal Purpose
//!
//! The Inhibit portal allows sandboxed applications to temporarily prevent the host system
//! from entering power-saving states (like sleep or idle screensaver) or from logging out.
//! For example, a sandboxed video player uses this portal to keep the screen on while a
//! movie is playing, or a CD burning app uses it to prevent sleep until the burn finishes.
//!
//! It also allows applications to actively monitor these states (e.g., detecting if the
//! screensaver is currently active).
//!
//! This portal implements the `org.freedesktop.portal.Inhibit` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Inhibit`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `Inhibit(handle, app_id, parent_window, options)`, `CreateMonitor(handle, session_handle, app_id, parent_window)`
//!   *(Note: The upstream D-Bus specification names the window parameter `window` instead of the standard `parent_window` for this portal, but they are semantically identical.)*
//! - **Signals:** `StateChanged(session_handle, state)`
//!
//! **Expected Caller Behavior:**
//! Callers pass a bitmask (`reason_flags`) specifying *what* they want to inhibit (Logout = 1,
//! User Switch = 2, Suspend = 4, Idle = 8). The portal creates a Request object that holds the
//! system lock. To release the lock, the caller explicitly closes the Request object.
//!
//! **Implementation Mapping:**
//! Implemented in `dbus.rs` by the `Inhibit` struct. It holds proxies to both the system bus
//! (`logind`) and the session bus (`ScreenSaver`).
//!
//! ## Request Lifecycle (Inhibition)
//!
//! 1. **Application** calls `Inhibit` with flags (e.g., `8` for Idle).
//! 2. **Portal Object (`Inhibit`)** receives the call.
//! 3. **Validation & Session Tracking:** It registers the request with the global `SessionManager`
//!    to ensure the app hasn't exceeded its maximum allowed concurrent locks.
//! 4. **Export Request:** It creates and exports an `InhibitRequest` D-Bus object (implementing
//!    `org.freedesktop.impl.portal.Request`).
//! 5. **Backend Processing:** It spawns a background Tokio task that translates the bitmask flags
//!    into logind strings (`"sleep"`, `"idle"`, `"shutdown"`).
//!    - It acquires an inhibition lock via the system `org.freedesktop.login1.Manager` interface,
//!      which returns a File Descriptor (FD).
//!    - If `Idle` is requested, it also attempts to acquire a lock via the session
//!      `org.freedesktop.ScreenSaver` interface.
//! 6. **Waiting:** The task awaits a notification signal (`notify.notified().await`) from the
//!    `InhibitRequest` object.
//! 7. **Release (Cleanup):** When the application closes the Request (or crashes, triggering
//!    D-Bus name-loss tracking), the `InhibitRequest::close` method fires the notification.
//! 8. **Backend Cleanup:** The background task drops the logind FD (which automatically releases
//!    the system lock) and calls `UnInhibit` on the ScreenSaver cookie.
//! 9. **Final Cleanup:** The Request object is unexported and unregistered from the `SessionManager`.
//!
//! **Ownership:** The system-level lock is represented by an `OwnedFd`. This FD is owned by the
//! spawned Tokio task and dropped when the task exits.
//!
//! ## Session Management (Monitors)
//!
//! The `CreateMonitor` method uses explicit session tracking.
//! - **Session Creation:** It registers with the `SessionManager` and exports a `Session` object.
//! - **State Tracking:** The session handle is added to a thread-safe `active_monitors` map.
//! - **Signal Broadcasting:** A dedicated background task (`init_once`) listens to the
//!   `ScreenSaver::ActiveChanged` signal. When it fires, it iterates through `active_monitors`
//!   and emits a `StateChanged` signal for every active monitor session.
//! - **Cleanup:** When the session is closed, the monitor is removed from the map.
//!
//! ## GTK Integration
//!
//! This portal has no GTK UI. It acts purely as a proxy between the sandboxed app and the
//! host's power management daemons (`logind` and desktop-specific screensavers).
//!
//! ## Backend Interaction
//!
//! - **logind (`org.freedesktop.login1`):** Accessed over the System Bus. Logind provides highly
//!   robust, FD-based locks for sleep, shutdown, and idle. If the portal crashes, the kernel
//!   closes the FD, and logind lifts the lock.
//! - **ScreenSaver (`org.freedesktop.ScreenSaver`):** Accessed over the Session Bus. Used as a
//!   fallback or supplement for idle/screen-blanking locks because some desktop environments do
//!   not honor logind's idle locks for monitor power-saving.
//!
//! ## Specification Notes
//!
//! - **Request Object as Lock:** The specification deliberately uses the D-Bus Request object's
//!   lifetime to represent the duration of the lock. This is why `Inhibit` spawns a task that
//!   hangs on `notify.notified().await`.
//! - **Flags to Strings Translation:** The portal translates integer bitmasks (1, 2, 4, 8) into
//!   string descriptors mandated by the logind API (`"shutdown"`, `"sleep"`, `"idle"`).
//!
//! ## Extension Guide
//!
//! - **New Backends:** If a new desktop environment uses a non-standard inhibition API (e.g., a
//!   custom Wayland protocol), it should be integrated in the background task spawned inside the
//!   `inhibit` method. Ensure the lock is acquired before awaiting the close notification, and
//!   released immediately after.
//!
//! ## Cross-Portal Consistency
//!
//! - **Session Manager:** Like `FileChooser` and `Print`, it uses the global `SessionManager` to
//!   enforce rate limits and track application lifetimes to prevent resource leaks (dangling locks).
//!
//! ## Maintenance Notes
//!
//! - **Why use two backends?** The dual use of `logind` and `ScreenSaver` is an intentional
//!   compatibility workaround. Logind is theoretically sufficient, but practically, GNOME and
//!   other DEs require the `ScreenSaver` D-Bus call to reliably prevent screen dimming.

pub mod dbus;

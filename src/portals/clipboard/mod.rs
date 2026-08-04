//! # Clipboard Portal
//!
//! ## Portal Purpose
//!
//! The Clipboard portal provides sandboxed applications with a secure, brokered way to
//! read from and write to the host's system clipboard (copy/paste).
//!
//! Sandboxed apps (like Flatpaks) generally do not have direct access to the Wayland
//! compositor or X11 server's clipboard mechanisms. This portal acts as an intermediary,
//! transferring data securely via file descriptors (FDs) over D-Bus, without requiring
//! the portal itself to buffer large amounts of clipboard data (like images or large text).
//!
//! This portal implements the `org.freedesktop.portal.Clipboard` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Clipboard`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `RequestClipboard(session)`, `SetSelection(session, options)`, `SelectionWrite(session, serial)`, `SelectionWriteDone(session, serial, success)`, `SelectionRead(session, mime_type)`
//! - **Signals:** `SelectionOwnerChanged(session, options)`, `SelectionTransfer(session, mime_type, serial)`
//!
//! **Expected Caller Behavior:**
//! Apps request clipboard access, then passively listen to `SelectionOwnerChanged` to know
//! what formats are available to paste.
//! To paste (read), they call `SelectionRead(mime)`, which returns a file descriptor they can read from.
//! To copy (write), they call `SetSelection(mimes)`, which claims the clipboard. When the host
//! actually requests the data, the app receives `SelectionTransfer(serial)`. It then calls
//! `SelectionWrite(serial)` to get a file descriptor it can write its data into.
//!
//! **Implementation Mapping:**
//! Implemented in `dbus.rs` by the `ClipboardPortal` struct, which routes data to the GTK
//! backend in `gtk_backend.rs`.
//!
//! ## Request Lifecycle
//!
//! **Reading from the Host Clipboard (Paste):**
//! 1. **Host Change:** The host's clipboard changes. `ClipboardPortal::new` has a background
//!    Tokio task listening to `gtk_backend::subscribe_changes()`.
//! 2. **Signal:** The portal emits `SelectionOwnerChanged` to all active sessions, detailing
//!    the available mime types.
//! 3. **Application Read:** The app calls `SelectionRead(mime_type)`.
//! 4. **FD Generation:** The portal creates a pipe (`rustix::pipe::pipe()`), returning the
//!    read end (`read_fd`) immediately to the app via D-Bus.
//! 5. **GTK Delegation:** It sends a closure to the GTK main thread (via `UiProxy`) containing
//!    the `write_fd` and asks `gtk_backend::read_selection` to asynchronously dump the host
//!    clipboard's contents into that FD.
//!
//! **Writing to the Host Clipboard (Copy):**
//! 1. **Application Claim:** App calls `SetSelection(mimes)`.
//! 2. **GTK Delegation:** The portal asks `gtk_backend::claim_selection` (on the GTK thread) to
//!    tell the host compositor "we own the clipboard for these formats".
//! 3. **Host Request:** Later, the user pastes in a host application. The host compositor asks GTK
//!    for the data.
//! 4. **Transfer Signal:** GTK triggers a callback which sends a message over a channel back to the
//!    Tokio background task spawned inside `SetSelection`.
//! 5. **Notify App:** Tokio generates a unique `serial` and emits `SelectionTransfer(serial)` to the app.
//! 6. **App Writes:** The app calls `SelectionWrite(serial)`. The portal creates a pipe, sends the
//!    `read_fd` to the GTK backend, and returns the `write_fd` to the app.
//! 7. **Streaming:** The app writes to the FD, GTK reads from it and gives it to the host compositor.
//!
//! **Ownership:**
//! The portal orchestrates the pipes, but ownership of the actual bytes is strictly streamed between
//! the app and the compositor. The portal does not own the clipboard data itself.
//!
//! ## Session Management
//!
//! The Clipboard portal heavily relies on Session handles (`ObjectPath`). An application must first
//! create a generic portal session (handled externally by the `Session` portal) and pass that handle
//! to `RequestClipboard`.
//! - **Tracking:** `ClipboardPortal` tracks these active session handles in `active_sessions`.
//! - **Targeting:** Signals like `SelectionOwnerChanged` are explicitly targeted at specific session handles.
//!
//! ## GTK Integration
//!
//! - **Mandatory Main Thread:** All interactions with `gdk::Clipboard` MUST happen on the GTK main thread.
//! - **UiProxy:** `dbus.rs` makes extensive use of `UiProxy` to dispatch closures (`Box<dyn FnOnce() + Send>`)
//!   to the GTK thread.
//! - **Async Bridging:** To wait for GTK results in Tokio, oneshot channels are used (e.g., waiting for
//!   `claim_selection` to succeed).
//!
//! ## Backend Interaction
//!
//! - **`gdk::Clipboard`:** The backend uses GDK4's `Display::default().clipboard()`.
//! - **`ContentProvider`:** When the app copies data, `gtk_backend.rs` creates a custom `gio::ContentProvider`
//!   subclass to represent the sandboxed app's data to the host.
//!
//! ## Specification Notes
//!
//! - **Deferred Transfer:** The `serial` mechanism (`SelectionTransfer` -> `SelectionWrite`) is
//!   mandated by the spec to implement "lazy" or "deferred" clipboard copying. The app doesn't
//!   have to generate a huge image until the user actually pastes it somewhere.
//!
//! ## Extension Guide
//!
//! - **Primary Selection:** Currently, this implements the standard clipboard. If the XDG spec extends
//!   to support the X11/Wayland "Primary Selection" (middle-click paste), `gtk_backend.rs` would
//!   need to be updated to target `Display::default().primary_clipboard()` based on a new flag from
//!   the D-Bus API.
//!
//! ## Cross-Portal Consistency
//!
//! - **FD Streaming:** Like the `Print` portal, the Clipboard portal avoids out-of-memory errors by
//!   using UNIX pipes (`rustix::pipe::pipe()`) to stream arbitrary-sized payloads between the sandbox
//!   and the host.
//!
//! ## Maintenance Notes
//!
//! - **Memory Leaks (Pipes):** Ensure that if an application crashes or fails to call `SelectionWriteDone`,
//!   the pending transfer maps in `dbus.rs` are cleaned up. Currently, the pending transfer map entry is
//!   removed either on `SelectionWrite` or `SelectionWriteDone`.

pub mod dbus;
pub mod gtk_backend;
pub mod provider;

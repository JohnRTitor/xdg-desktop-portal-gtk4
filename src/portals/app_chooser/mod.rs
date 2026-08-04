//! # AppChooser Portal
//!
//! ## Portal Purpose
//!
//! The AppChooser portal allows a sandboxed application to ask the user to select another
//! application to open a specific file or URI.
//!
//! Because sandboxed applications (like Flatpaks) cannot see the host's installed applications
//! (e.g., they cannot read `/usr/share/applications`), they cannot present their own "Open With..."
//! dialogs. Instead, they ask the `xdg-desktop-portal` host daemon, which finds compatible
//! applications and passes that list to this portal to display to the user.
//!
//! This portal implements the `org.freedesktop.portal.AppChooser` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.AppChooser`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `ChooseApplication(handle, app_id, parent_window, choices, options)`, `UpdateChoices(handle, choices)`
//! - **Signals:** None natively.
//!
//! **Expected Caller Behavior:**
//! The caller (the host daemon) provides an initial list of `choices` (desktop file IDs like
//! `org.gnome.TextEditor.desktop`). Because discovering all apps on the host can take time, the
//! caller can subsequently call `UpdateChoices` with an expanded list while the dialog is still open.
//! The portal expects to return the single chosen desktop file ID.
//!
//! **Implementation Mapping:**
//! Implemented in `dbus.rs` by the `AppChooser` struct. The UI rendering is handled by `AppChooserUi`
//! in `gui.rs`.
//!
//! ## Request Lifecycle
//!
//! 1. **Caller** sends a `ChooseApplication` D-Bus method call.
//! 2. **Portal Object (`AppChooser`)** receives the request in `choose_application`.
//! 3. **Live Update Setup:** It creates a Tokio mpsc channel (`update_sender`, `update_receiver`).
//!    It registers the `update_sender` in the `active_dialogs` map, keyed by the D-Bus Request `handle`.
//! 4. **GUI Interaction:** The initial choices and the `update_receiver` are dispatched to the GTK main
//!    thread via `AppChooserUi::run`.
//! 5. **User Consent:** A dialog is presented showing the available apps.
//! 6. **Dynamic Updates:** If the host daemon calls `UpdateChoices` with the same `handle`, the
//!    `AppChooser` looks up the `update_sender` and pushes the new choices down the channel. The
//!    GTK thread listens to this channel and dynamically updates the UI listbox.
//! 7. **Response Generation:** When the user selects an app and clicks "Open", the GTK thread returns
//!    the selected desktop file ID, which is returned in the `ChooseApplicationResults`.
//! 8. **Cleanup:**
//!    - `run_request` handles the D-Bus Request object.
//!    - A custom `ActiveDialogGuard` (RAII) ensures that the `update_sender` is always removed from
//!      `active_dialogs` when the request finishes or is cancelled, preventing memory leaks.
//!
//! **Ownership:** `run_request` owns the D-Bus request state. The `AppChooser` struct owns the map
//! of active channels. The GTK thread owns the dialog widget.
//!
//! ## Session Management
//!
//! The AppChooser portal does not use standard Sessions. Instead, it relies on the Request object's
//! `handle` to route `UpdateChoices` calls to the correct active dialog.
//!
//! ## GTK Integration
//!
//! A UI is mandatory.
//! - **Thread Transition:** Execution moves to the GTK main thread via `UiProxy` to construct the dialog.
//! - **Async Channels in GTK:** `gui.rs` must asynchronously read from the Tokio `mpsc::Receiver`
//!   while running the GTK main loop. It uses `glib::MainContext::spawn_local` to achieve this safely.
//!
//! ## Backend Interaction
//!
//! This portal does not interact with the host's desktop file database (MIME database) directly.
//! It relies entirely on the host daemon passing it the strings (e.g., `org.gnome.Gedit.desktop`).
//!
//! ## Specification Notes
//!
//! - **`UpdateChoices`:** This method is a relatively recent addition to the spec to handle slow
//!   app discovery on the host. The portal *must* handle the choices list changing while the dialog
//!   is visible.
//!
//! ## Extension Guide
//!
//! - **UI Refinements:** If GTK adds better widgets for App Choosers, update `gui.rs`. Currently,
//!   it manually extracts icons and names from the provided `.desktop` strings.
//! - **Remembering Choices:** The spec includes options for remembering the user's choice. Currently,
//!   this portal delegates the actual saving of that choice back to the host daemon (by simply returning
//!   the choice).
//!
//! ## Cross-Portal Consistency
//!
//! - **Dynamic Updates:** This is the only portal in this crate that receives method calls (`UpdateChoices`)
//!   intended to modify an *already running* request dialog.
//! - **Request Handling:** Uses the standard `run_request` wrapper.
//!
//! ## Maintenance Notes
//!
//! - **Guard Pattern:** The `ActiveDialogGuard` inside `choose_application_impl` is critical.
//!   Without it, if the `UiProxy` Future is cancelled (e.g., the app closes the Request via D-Bus),
//!   the sender would remain in `active_dialogs` forever.

pub mod dbus;
pub mod gui;

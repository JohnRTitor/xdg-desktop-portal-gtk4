//! # Access Portal
//!
//! ## Portal Purpose
//!
//! The Access portal is a foundational security component in the XDG Desktop Portal ecosystem.
//! It provides a generic, user-facing permissions dialog.
//!
//! When a sandboxed application requests access to a sensitive resource (like the camera,
//! microphone, or location), the host system (e.g., Flatpak or a permission store daemon)
//! first checks its policy. If the policy says "ask the user", the host system invokes this
//! Access portal to display a dialog.
//!
//! Crucially, the sandboxed application **does not** call this portal directly. It is called
//! by other portals (like the Camera portal) or by the Flatpak permission infrastructure itself.
//!
//! This portal implements the `org.freedesktop.portal.Access` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Access`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `AccessDialog(handle, app_id, parent_window, title, subtitle, body, options)`
//! - **Signals:** None natively.
//!
//! **Expected Caller Behavior:**
//! Callers (usually the host system) provide the UI strings (`title`, `subtitle`, `body`)
//! and optional UI configuration (`deny_label`, `grant_label`, `icon`). The caller waits for the
//! user to click "Allow" or "Deny", and expects a `Response::success` containing user choices,
//! or `Response::cancelled`.
//!
//! **Implementation Mapping:**
//! The D-Bus interface is implemented in `dbus.rs` by the `Access` struct. The UI rendering
//! is handled by `AccessUi` in `gui.rs`.
//!
//! ## Request Lifecycle
//!
//! 1. **Caller** sends an `AccessDialog` D-Bus method call.
//! 2. **Portal Object (`Access`)** receives the request in `access_dialog`.
//! 3. **Validation & Deserialization:** The `options` dictionary is deserialized into the
//!    strongly-typed `AccessDialogOptions` struct. Complex nested types like `Choice` (which
//!    allows the dialog to present dropdowns or checkboxes) are unpacked.
//! 4. **GUI Interaction:** The portal packages the data into an `AccessUi` struct and dispatches
//!    it to the GTK main thread using `UiProxy::run`.
//! 5. **User Consent:** A modal `adw::MessageDialog` is presented to the user.
//! 6. **Response Generation:**
//!    - If the user clicks the grant button, the GTK thread gathers any selected options from the
//!      `Choice` UI elements and returns them over a oneshot channel.
//!    - If the user clicks deny or dismisses the dialog, it returns a cancellation.
//! 7. **Cleanup:** `run_request` handles the D-Bus Request object lifecycle.
//!
//! **Ownership:** `run_request` owns the D-Bus request state. The GTK thread exclusively owns
//! the dialog widget.
//!
//! ## Session Management
//!
//! The Access portal does not use sessions. Every `AccessDialog` call is a distinct, stateless
//! request mediated by a Request object.
//!
//! ## GTK Integration
//!
//! A UI is mandatory for this portal.
//! - **Thread Transition:** Execution moves to the GTK main thread via `UiProxy` to construct
//!   the `adw::MessageDialog`.
//! - **Dynamic UI:** `gui.rs` dynamically constructs UI elements based on the `choices` array
//!   provided in the D-Bus options.
//! - **Confinement:** The dialog widget and its signal handlers remain confined to the GTK thread.
//!
//! ## Backend Interaction
//!
//! This portal has no backend interaction (like `dconf` or `logind`). It is purely a UI presentation
//! layer for the host's permission management system.
//!
//! ## Specification Notes
//!
//! - **`choices` Array:** The spec defines a very complex signature for `choices`: `a(ssa(ss)s)`.
//!   This represents a list of choices, where each choice has an ID, a label, a list of variants
//!   (ID/label pairs), and a default variant. This allows the Access dialog to present complex
//!   options (e.g., "Allow for this session", "Allow permanently").
//!
//! ## Extension Guide
//!
//! - **UI Tweaks:** Modifying the appearance of the permission dialog should be done in `gui.rs`.
//! - **New Choice Types:** If the spec adds new UI elements to the `choices` array, the parsing in
//!   `dbus.rs` and the widget generation in `gui.rs` must be updated concurrently.
//!
//! ## Cross-Portal Consistency
//!
//! - **Generic Dialog:** The `Account` portal uses a very similar pattern (and often similar internal
//!   GTK widgets) to present a permission dialog, though `Account` fetches data while `Access` is purely
//!   presentation.
//! - **Request Handling:** Uses the standard `run_request` wrapper from `crate::core::request`.
//!
//! ## Maintenance Notes
//!
//! - **Why not use libportal?** As a backend implementer, this project *is* the portal. We parse the
//!   raw D-Bus signatures that `libportal` (used by the sandboxed app) or the `xdg-desktop-portal`
//!   daemon generates.

pub mod dbus;
pub mod gui;

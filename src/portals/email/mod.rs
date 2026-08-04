//! # Email Portal
//!
//! ## Portal Purpose
//!
//! The Email portal provides a safe way for sandboxed applications to request that the host system
//! composes a new email. Instead of the application interacting directly with a mail transfer agent
//! or needing permissions to launch arbitrary applications, it delegates the email composition to
//! the host system's default email client.
//!
//! This fits into the XDG Desktop Portal ecosystem by providing an isolated IPC mechanism for mailto:
//! URIs. Applications like web browsers, document viewers, or address books use this to trigger
//! email composition without breaking sandbox boundaries.
//!
//! This portal implements the `org.freedesktop.portal.Email` specification completely.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Email`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `ComposeEmail(handle, app_id, parent_window, options)`
//! - **Signals:** None natively (request responses are sent via the `Response` object).
//!
//! **Expected Caller Behavior:**
//! Callers pass dictionaries containing addresses, CCs, BCCs, subjects, bodies, and attachments.
//! They expect the portal backend to seamlessly forward these to the user's preferred mail client.
//!
//! **Implementation Mapping:**
//! The D-Bus interface is implemented in `dbus.rs` by the `Email` struct using `zbus::interface`.
//! The `compose_email` D-Bus method maps to the `compose_email_impl` Rust method.
//!
//! ## Request Lifecycle
//!
//! 1. **Application** sends a `ComposeEmail` D-Bus method call.
//! 2. **D-Bus Daemon** routes the call to the portal frontend (xdg-desktop-portal).
//! 3. **Portal Frontend** forwards it to our backend implementation (`xdg-desktop-portal-gtk4`).
//! 4. **Portal Object (`Email`)** receives the request in `compose_email`.
//! 5. **Validation & Processing:** The backend extracts the `options` (addresses, subject, body, etc.)
//!    and constructs a compliant `mailto:` URI in `build_mailto_url`.
//! 6. **Backend Interaction:** It utilizes `gtk4::gio::AppInfo::launch_default_for_uri` to instruct
//!    the host OS to launch the default mail application. It also passes `activation_token` to GIO
//!    to ensure the launched mail client is brought to the foreground.
//! 7. **Response Generation:** It returns a successful `Response` object (with an empty dictionary,
//!    per specification) or a cancelled response if launching fails.
//! 8. **Cleanup:** `run_request` handles exporting and unexporting the request object.
//!
//! **Ownership:** Request tracking is owned by the `run_request` helper. The `Email` struct itself
//! is stateless.
//!
//! ## Session Management
//!
//! The Email portal does not use sessions. Every `ComposeEmail` request is a discrete, fire-and-forget
//! operation.
//!
//! ## GTK Integration
//!
//! This portal **does not** require an internal GTK UI or a dialog. It purely uses `gtk4::gio`
//! for URI launching. Therefore, it does not mandate execution on the GTK main thread and safely
//! executes within the standard Tokio async runtime.
//!
//! ## Backend Interaction
//!
//! The backend component for this portal is the host's `gio` application launching system.
//! - **Request Flow:** The portal translates D-Bus options into a `mailto:` URI string and asks GIO to launch it.
//! - **Failure Handling:** If GIO fails to find or launch a default application, the portal logs the error
//!   and returns a cancelled response to the caller.
//!
//! ## Specification Notes
//!
//! - **URI Construction:** The `mailto:` URI construction explicitly URI-escapes the CC, BCC, subject,
//!   body, and attachment fields using `glib::uri_escape_string` to ensure the resulting URI is
//!   well-formed and secure against injection, as implicitly mandated by the specification.
//! - **Response Object:** The specification requires returning a dictionary of results. For `ComposeEmail`,
//!   this dictionary is intentionally empty (`EmailResults {}`), which is why the implementation returns an
//!   empty serialized dictionary on success.
//!
//! ## Extension Guide
//!
//! For future contributors extending the Email portal:
//! - **D-Bus Methods:** Add new methods to the `impl Email` block decorated with `#[interface(...)]` in `dbus.rs`.
//! - **Logic:** The core string manipulation for URI construction lives in `build_mailto_url`. If new options
//!   are added to the specification, update `ComposeEmailOptions` and append them to the URI in `build_mailto_url`.
//! - **UI:** Do not add UI to this portal. It is strictly a delegation portal.
//!
//! ## Cross-Portal Consistency
//!
//! - **Request Handling:** Like other stateless portals (e.g., Settings, Lockdown), it uses the standard
//!   `run_request` wrapper from `crate::core::request` to manage the lifecycle of the D-Bus Request object.
//!
//! ## Maintenance Notes
//!
//! - **Why is this implemented differently?** Unlike `file_chooser` or `print`, the Email portal does not
//!   need a GTK interface because it relies on the OS-level default application handler. It is essentially
//!   a proxy to `gio open mailto:...`.
//! - **Specification Mandate:** The translation of dictionary options to a `mailto:` URI is an implementation
//!   choice to satisfy the specification's requirement of opening an email composer, as GIO's URI launching
//!   is the most robust way to trigger the default mail client on Linux.

pub mod dbus;

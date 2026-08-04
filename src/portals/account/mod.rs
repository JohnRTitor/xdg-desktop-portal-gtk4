//! # Account Portal
//!
//! ## Portal Purpose
//!
//! The Account portal allows sandboxed applications to request basic profile information about
//! the current user, such as their real name, login ID, and profile picture (avatar).
//!
//! Because this is personally identifiable information (PII), the portal must not silently
//! return this data. Instead, it presents a GTK access dialog, requiring explicit user consent
//! before sharing the data with the requesting application.
//!
//! This fits into the XDG Desktop Portal ecosystem by providing a secure, user-mediated bridge
//! between sandboxed apps and the host's AccountsService.
//!
//! This portal implements the `org.freedesktop.portal.Account` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Account`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `GetUserInformation(handle, app_id, parent_window, options)`
//!   *(Note: The upstream D-Bus specification names the window parameter `window` instead of the standard `parent_window` for this portal, but they are semantically identical.)*
//! - **Signals:** None natively.
//!
//! **Expected Caller Behavior:**
//! Callers invoke `GetUserInformation`, providing an optional `reason` string to display in the UI.
//! They expect a dictionary containing `id`, `name`, and `image` (a `file://` URI to the avatar) if
//! the user approves the request.
//!
//! **Implementation Mapping:**
//! The D-Bus interface is implemented in `dbus.rs` by the `Account` struct.
//! The GUI interaction is implemented in `gui.rs` via `AccountUi`.
//!
//! ## Request Lifecycle
//!
//! 1. **Application** sends a `GetUserInformation` D-Bus method call.
//! 2. **Portal Object (`Account`)** receives the request in `get_user_information`.
//! 3. **Backend Processing (Pre-flight):** The portal immediately queries the host's `AccountsService`
//!    via the system D-Bus (`fetch_user_data`) to get the user's name and icon path.
//! 4. **GUI Interaction:** The portal packages this data into an `AccountUi` struct and dispatches
//!    it to the GTK main thread using the `UiProxy`.
//! 5. **Validation (User Consent):** A generic access dialog is presented to the user showing the
//!    requesting app, the reason, and what data will be shared.
//! 6. **Response Generation:**
//!    - If approved, the portal formats the avatar path as a `file://` URI and returns the
//!      `UserInformation` dictionary.
//!    - If denied or dismissed, the portal returns a cancelled `Response`.
//! 7. **Cleanup:** `run_request` handles exporting and unexporting the request object.
//!
//! **Ownership:** `run_request` owns the D-Bus request state. The GTK thread owns the dialog widget
//! during its lifecycle.
//!
//! ## Session Management
//!
//! The Account portal does not use sessions. Every `GetUserInformation` request is a distinct,
//! one-off operation mediated by a UI dialog.
//!
//! ## GTK Integration
//!
//! A UI is mandatory because sharing user profile data requires explicit consent.
//!
//! - **Thread Transition:** Execution moves to the GTK main thread via `UiProxy::send` to construct
//!   and show the `adw::MessageDialog` (or similar access dialog).
//! - **Confinement:** The dialog widget and its signal handlers remain confined to the GTK thread.
//! - **Return:** When the user clicks "Allow" or "Deny", the response is sent back over a oneshot
//!   channel, returning control to the Tokio async runtime to construct the D-Bus reply.
//!
//! ## Backend Interaction
//!
//! The backend component is `org.freedesktop.Accounts.User` (AccountsService).
//! - **Request Flow:** The portal connects to the *System Bus* to fetch data for the UID of the
//!   process running the portal (assumed to be the current user).
//! - **Failure Handling:** If AccountsService is unavailable or the user has no configured name/icon,
//!   the portal degrades gracefully by falling back to empty strings (`unwrap_or_default()`). It
//!   still prompts the user, as sharing an empty profile is still a privacy decision.
//!
//! ## Specification Notes
//!
//! - **Image URI:** The specification mandates that the `image` field in the result dictionary must
//!   be a valid `file://` URI. `AccountsService` often returns a raw absolute path (e.g.,
//!   `/var/lib/AccountsService/icons/user`). The implementation explicitly checks for a leading `/`
//!   and prepends `file://` to comply with the spec.
//!
//! ## Extension Guide
//!
//! For future contributors extending the Account portal:
//! - **New User Fields:** If the specification adds fields (like email or locale), fetch them from
//!   AccountsService in `fetch_user_data` in `dbus.rs`, update `UserInformation`, and update the
//!   `AccountUi` dialog in `gui.rs` to reflect the new data being shared.
//! - **UI Tweaks:** Modify `gui.rs` to change how the access dialog looks. Ensure the dialog clearly
//!   states *what* is being shared.
//!
//! ## Cross-Portal Consistency
//!
//! - **Generic Access Dialog:** The Account portal uses a generic permission-style dialog pattern
//!   similar to the `Access` portal (which handles camera/microphone).
//! - **Request Handling:** Uses the standard `run_request` wrapper from `crate::core::request`.
//!
//! ## Maintenance Notes
//!
//! - **System Bus Reliance:** This is one of the few portals that must talk to the System Bus
//!   (for AccountsService) rather than the Session Bus.
//! - **Assumptions:** It assumes the portal is running as the user whose profile is being requested
//!   (which is true for standard user-session systemd deployments). It uses `rustix::process::getuid()`
//!   to determine the target user ID for AccountsService.

pub mod dbus;
pub mod gui;

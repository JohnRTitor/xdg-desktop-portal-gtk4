//! # DynamicLauncher Portal
//!
//! ## Portal Purpose
//!
//! The DynamicLauncher portal enables sandboxed applications to create `.desktop` launcher
//! entries on the host system.
//!
//! Because sandboxed apps do not have write access to `~/.local/share/applications/` or
//! `/usr/share/applications/`, they cannot install shortcuts themselves. This is a crucial
//! security feature preventing rogue apps from creating malicious autostart entries or spoofing
//! system applications.
//!
//! Through this portal, a sandboxed app (like a web browser installing a WebApp, or Steam
//! installing a game) asks the host to create a launcher. The host delegates the UI to this
//! portal, which presents a dialog to the user showing the proposed name and icon.
//!
//! This portal implements the `org.freedesktop.portal.DynamicLauncher` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.DynamicLauncher`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `PrepareInstall(handle, app_id, parent_window, name, icon_v, options)`, `RequestInstallToken(app_id, options)`
//! - **Signals:** None natively.
//!
//! **Expected Caller Behavior:**
//! The host daemon calls `RequestInstallToken`. If this portal returns `0` (Allowed), the daemon
//! skips the UI. If it returns `2` (Denied), the daemon calls `PrepareInstall`, and this portal
//! must show a UI to ask the user for confirmation. If confirmed, this portal returns the chosen
//! name and icon. The host daemon then creates the `.desktop` file.
//!
//! **Implementation Mapping:**
//! Implemented in `dbus.rs` by the `DynamicLauncher` struct. The UI rendering is handled by
//! `DynamicLauncherUi` in `gui.rs`.
//!
//! ## Request Lifecycle
//!
//! **Token Request (Pre-flight check):**
//! 1. **Caller** sends `RequestInstallToken`.
//! 2. **Policy Check:** The portal checks the `app_id` against a hardcoded list of trusted
//!    software centers (`org.gnome.Software`, `org.kde.discover`, etc.).
//! 3. **Response:** If trusted, it returns `0` (Allow). The host daemon will create the launcher
//!    without showing a dialog. If not trusted, it returns `2` (Deny).
//!
//! **Prepare Install (UI Flow):**
//! 1. **Caller** sends `PrepareInstall`.
//! 2. **Validation:** The portal extracts the `icon_v` variant. This requires complex parsing
//!    (`parse_icon`) because the icon could be a string (named icon), raw bytes, or a tuple.
//! 3. **GUI Interaction:** The request is dispatched to the GTK main thread via `DynamicLauncherUi::run`.
//! 4. **User Consent:** A dialog is presented showing the proposed name and icon. If `editable_name`
//!    is true, the dialog contains a `gtk4::Entry` allowing the user to rename the launcher.
//! 5. **Response Generation:**
//!    - If the user clicks "Install", the portal returns the final name and the original `icon_v`
//!      in the `PrepareInstallResults` dictionary.
//!    - If the user cancels, it returns `Response::cancelled()`.
//! 6. **Cleanup:** `run_request` handles the D-Bus Request object lifecycle.
//!
//! **Ownership:** `run_request` owns the D-Bus request state. The GTK thread owns the dialog widget.
//!
//! ## Session Management
//!
//! The DynamicLauncher portal does not use sessions. Every `PrepareInstall` call is a discrete request.
//!
//! ## GTK Integration
//!
//! - **Thread Transition:** Execution moves to the GTK main thread via `UiProxy` to construct the dialog.
//! - **Image Handling:** In `gui.rs`, raw byte arrays representing icons are safely loaded into
//!   `gdk::Texture` objects via `gdk_pixbuf::Pixbuf` for display in the dialog.
//!
//! ## Backend Interaction
//!
//! This portal does *not* write `.desktop` files. It acts purely as the user-consent UI layer.
//! The actual file system write operations are handled by the `xdg-desktop-portal` host daemon
//! after this portal returns a successful `PrepareInstallResults`.
//!
//! ## Specification Notes
//!
//! - **Icon Variant:** The `icon_v` parameter uses a complex `v` (variant) signature to support
//!   multiple formats natively.
//! - **Launcher Types:** The `SupportedLauncherTypes` property advertises `3` (Application + Webapp)
//!   to the host daemon, indicating this portal can handle both types of installation requests.
//!
//! ## Extension Guide
//!
//! - **Trusted Apps List:** If new software centers emerge that should be allowed to bypass the
//!   UI (e.g., a new immutable OS app store), add their IDs to the `allowed_ids` array in
//!   `request_install_token`.
//! - **Editable Icons:** The spec defines an `editable_icon` option. Currently, `gui.rs` only
//!   supports editing the *name*. If GTK adds a good image-chooser widget, `gui.rs` could be
//!   updated to allow users to upload custom icons.
//!
//! ## Cross-Portal Consistency
//!
//! - **Icon Parsing:** Shares similar variant-unpacking logic (`parse_icon`) with the `Notification` portal.
//! - **Request Handling:** Uses the standard `run_request` wrapper.
//!
//! ## Maintenance Notes
//!
//! - **Policy Hardcoding:** The hardcoded list of trusted apps in `RequestInstallToken` is an
//!   intentional design choice mirroring the GNOME implementation. Software centers are inherently
//!   trusted to manage apps, so prompting the user for every install would be extremely annoying.

pub mod dbus;
pub mod gui;

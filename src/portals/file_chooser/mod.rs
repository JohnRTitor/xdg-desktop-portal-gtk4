//! # FileChooser Portal
//!
//! ## Portal Purpose
//!
//! The FileChooser portal is one of the most critical security boundaries in the XDG Desktop Portal
//! ecosystem. It allows sandboxed applications (which cannot see the host filesystem) to open or save
//! files by asking the user to interactively select them.
//!
//! When the user selects a file, the host daemon (using the `document portal`) punches a temporary
//! hole in the sandbox, granting the application a file descriptor to exactly that file and nothing else.
//!
//! This portal implements the `org.freedesktop.portal.FileChooser` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.FileChooser`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `OpenFile(handle, app_id, parent_window, title, options)`, `SaveFile(handle, app_id, parent_window, title, options)`, `SaveFiles(handle, app_id, parent_window, title, options)`
//! - **Signals:** None natively.
//!
//! **Expected Caller Behavior:**
//! The caller passes various configuration options (filters, choices, default folders, modal status).
//! The portal displays a native GTK file chooser dialog and returns the selected URIs.
//!
//! **Implementation Mapping:**
//! Implemented in `dbus.rs` by the `FileChooser` struct. The UI rendering relies on `gtk4::FileChooserNative`,
//! wrapped by `FileChooserUi` in `gui.rs`.
//!
//! ## Request Lifecycle
//!
//! 1. **Caller** sends a method call (`OpenFile`, `SaveFile`, or `SaveFiles`).
//! 2. **Portal Object (`FileChooser`)** receives the request.
//! 3. **Validation & Mapping:**
//!    - Options like file filters and custom UI choices are translated from their raw D-Bus `zvariant`
//!      representations into internal Rust structs (`Filter`, `Choice`).
//!    - For `SaveFiles`, specific security checks are performed to ensure the requested files don't
//!      contain absolute paths or `..` directory traversal attempts.
//! 4. **GUI Interaction:** The request is dispatched to the GTK main thread via `FileChooserUi::run`.
//! 5. **User Consent:** A native file chooser dialog is presented.
//! 6. **Response Generation:**
//!    - If the user selects files, GTK returns the GIO URIs. These, along with any custom choices the
//!      user made (e.g., selecting "Read Only" or a specific character encoding), are mapped back to
//!      D-Bus structures and returned.
//!    - If the user cancels, `Response::cancelled()` is returned.
//! 7. **Cleanup:** `run_request` handles the D-Bus Request object lifecycle.
//!
//! **Ownership:** `run_request` owns the D-Bus request state. The GTK thread owns the `FileChooserNative`
//! widget.
//!
//! ## Session Management
//!
//! The FileChooser portal does not use standard portal Sessions. Each method call is a discrete request.
//!
//! ## GTK Integration
//!
//! - **Thread Transition:** Execution moves to the GTK main thread via `UiProxy` to construct the dialog.
//! - **Native Dialogs:** This portal uses `gtk4::FileChooserNative` instead of a standard `gtk4::Dialog`.
//!   Native choosers integrate better with the host environment (e.g., they can use the KDE chooser on
//!   a KDE desktop even if GTK is driving the portal).
//!
//! ## Backend Interaction
//!
//! This portal does *not* punch the hole in the sandbox. It purely provides the UI and returns the
//! selected paths (URIs) to the host daemon (`xdg-desktop-portal`). The host daemon takes those URIs,
//! uses the `document portal` to mount them into the sandbox, and returns the rewritten sandbox-friendly
//! paths to the application.
//!
//! ## Specification Notes
//!
//! - **Custom Choices:** The spec allows apps to inject custom widgets into the file chooser (e.g., a
//!   dropdown for "Character Encoding"). This requires complex mapping between the D-Bus array of structs
//!   and the GTK native choice API.
//! - **File Paths as Byte Arrays:** Because Unix paths are not guaranteed to be valid UTF-8, the spec
//!   passes them as `ay` (array of bytes). We use a custom `FilePath` wrapper with a `serde::Deserialize`
//!   implementation to convert these nul-terminated byte arrays into Rust Strings, replacing invalid UTF-8
//!   with replacement characters (since GTK requires valid UTF-8 strings for display).
//!
//! ## Extension Guide
//!
//! - **SaveFiles Deduplication:** If `SaveFiles` attempts to save multiple files to a directory where
//!   some already exist, it currently appends ` (1)`, ` (2)` to the filename. If GTK introduces native
//!   batch-save collision resolution, this custom logic could be removed.
//!
//! ## Cross-Portal Consistency
//!
//! - **Request Handling:** Uses the standard `run_request` wrapper.
//! - **Complex Options:** Like the `AppChooser`, it has extensive parsing of dictionary options before
//!   showing the UI.
//!
//! ## Maintenance Notes
//!
//! - **Security in SaveFiles:** The `try_save_files_impl` method contains vital security checks. When
//!   an app asks to save an entire directory of files, it provides relative paths for each file. The
//!   user only selects the base destination folder. If the app provided a relative path like `../../etc/passwd`,
//!   and we didn't sanitize it, the portal would write over system files using the user's privileges.
//!   These checks (`is_absolute`, `..` components) MUST remain intact.

pub mod dbus;
pub mod gui;

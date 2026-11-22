//! # Print Portal
//!
//! ## Portal Purpose
//!
//! The Print portal allows sandboxed applications to securely print documents.
//!
//! Directly interacting with CUPS or other host printing subsystems from within a sandbox
//! is a major security risk, as it exposes the entire local network and host file system to
//! the application. This portal brokers the transaction by presenting a secure GTK print
//! dialog to the user, allowing them to select a printer and configure settings (paper size,
//! orientation). Once the user approves, the application streams the raw document data (usually
//! PDF) to the portal via a file descriptor, and the portal submits it to the printer.
//!
//! This portal implements the `org.freedesktop.portal.Print` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Print`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `PreparePrint(handle, app_id, parent_window, title, settings, page_setup, options)`, `Print(handle, app_id, parent_window, title, fd, options)`
//! - **Signals:** None natively.
//!
//! **Expected Caller Behavior:**
//! The normal flow is two-step:
//! 1. The caller invokes `PreparePrint`. The portal shows the UI and returns a `token` (a `u32` identifier).
//! 2. The caller generates the print data (e.g., renders a PDF) based on the settings returned by `PreparePrint`.
//! 3. The caller invokes `Print`, providing the generated `token` and a file descriptor (`fd`) containing the raw document data.
//!
//! `Gtk.PrintDialog.print()` / `print_file()` with a `NULL` setup skip `PreparePrint` and send
//! `token = 0`. `Print` then presents the dialog itself and prints with the job it produces, the
//! way the GTK3 and GNOME backends do. A token that belongs to another application is still
//! refused.
//!
//! **Implementation Mapping:**
//! The D-Bus interface is implemented in `dbus.rs` by the `Print` struct. The GTK UI and CUPS
//! submission logic reside in `gui.rs`.
//!
//! ## Request Lifecycle
//!
//! **Step 1: PreparePrint**
//! 1. **Application** calls `PreparePrint`.
//! 2. **Portal Object (`Print`)** receives the request in `prepare_print`.
//! 3. **GUI Interaction:** The request is dispatched to the GTK main thread via `PrintUi::run`.
//! 4. **User Consent:** A `gtk4::PrintUnixDialog` is presented. The user selects a printer
//!    and configures settings.
//! 5. **Token Generation:** If the user clicks "Print", GTK generates a `gtk4::PrintSettings` and
//!    `gtk4::PageSetup`. The portal assigns a unique `token` (an integer) and caches these settings
//!    in memory.
//! 6. **Response:** The portal returns the `token` and the serialized print settings to the app.
//!
//! **Step 2: Print**
//! 1. **Application** calls `Print`, passing the `token` and a file descriptor (`fd`).
//! 2. **Portal Object (`Print`)** receives the request in `print`.
//! 3. **Validation:** The portal duplicates the received descriptor with a CLOEXEC-preserving
//!    `fcntl_dupfd_cloexec`, because zbus borrows the original from the D-Bus message and zbus
//!    closes it when the method returns. If the token is absent or is not a known prepared token,
//!    `PrintUi` presents the dialog on this call first.
//! 4. **GUI/Backend Interaction:** The request is dispatched to the GTK thread via `ExecutePrintUi::run`.
//! 5. **Execution:** The GTK thread claims the cached job (checking the owning application), builds a
//!    `gtk4::PrintJob` from its settings, hands it the duplicated descriptor, and submits it with
//!    `print_job.send()`.
//! 6. **Response & Cleanup:** The portal reports the job result (0 success / 2 backend failure) from the
//!    `send()` completion callback, so backend failures reach the application. A foreign token and a failed
//!    `set_source_fd` are refused earlier with code 2; an absent or unknown token instead takes the dialog
//!    path in step 3 (and a token that becomes unclaimable before execution is refused at that point with
//!    code 2). A dialog the user dismisses maps to 1. The callback also closes the duplicated descriptor;
//!    GTK does not close descriptors it is given. The token is consumed when the job is claimed.
//!
//! **Ownership:**
//! The `token` acts as a capability ticket. Between the two calls the GTK thread keeps the selected
//! `gtk4::Printer`, `gtk4::PrintSettings` and `gtk4::PageSetup` in a `CachedPrintJob`.
//!
//! ## Session Management
//!
//! The Print portal does not use standard Portal Sessions. Instead, it uses the `token` mechanism
//! to maintain state across the two distinct D-Bus method calls.
//!
//! ## GTK Integration
//!
//! The printing portal relies on GTK's Unix printing infrastructure: `gtk4::PrintUnixDialog` for the
//! dialog and `gtk4::PrintJob` for submission.
//! - **Thread Transition:** All printing operations, including reading the file descriptor during `Print`,
//!   must happen on the GTK main thread because those types are tied to the GTK main loop and Cairo
//!   rendering contexts.
//! - **File Descriptor Handling:** The fd received from zbus is owned by the D-Bus message and is closed
//!   when the method handler returns. The portal duplicates it with `fcntl_dupfd_cloexec` and gives the
//!   copy to `GtkPrintJob`, which reads it asynchronously on the GTK thread. GTK does not close the
//!   descriptor it is given, so the portal closes its copy from the job's completion callback.
//!
//! ## Backend Interaction
//!
//! The backend is CUPS (Common UNIX Printing System), but the portal does not talk to CUPS directly.
//! It relies entirely on GTK to abstract the CUPS interaction.
//!
//! ## Specification Notes
//!
//! - **Formats:** The spec allows the app to specify supported output formats. This implementation
//!   hardcodes `["pdf", "ps", "svg"]` as supported formats in the `PreparePrintResults`, which aligns
//!   with GTK's internal Cairo surface capabilities.
//!
//! ## Extension Guide
//!
//! - **Custom Print Settings:** Print settings and page setups cross the D-Bus boundary as typed
//!   `a{sv}` dictionaries. Keys with scalar values are round-tripped automatically by the generic
//!   helpers in `gui.rs`, which accept `b/y/n/q/i/u/x/t/d/s`; no per-key code is needed. `gui.rs`
//!   only needs updating if a value type outside that set has to be carried in either direction.
//!
//! ## Cross-Portal Consistency
//!
//! - **Two-Step Operation:** This is unique. Most portals use Sessions for multi-step operations. Print
//!   uses a custom token mechanism defined before the Session spec was fully formalized.
//! - **FD Streaming:** Like the `Clipboard` portal, it streams data via FDs to prevent out-of-memory
//!   issues when printing massive PDFs.
//!
//! ## Maintenance Notes
//!
//! - **Token Security:** Tokens are random `u32` values (`fastrand::u32(1..)` with collision checks),
//!   so they are theoretically susceptible to guessing, but because the attack surface is limited to the
//!   sandbox sending print data to an already-approved print job (which the user just clicked "Print"
//!   on), the risk is minimal. The probe (run only for a nonzero token, to tell a prepared job from the
//!   token-0 single-call path) is non-consuming, and `claim_token` still checks the owning application,
//!   so a guessed token cannot be used by a different application.

pub mod dbus;
pub mod gui;

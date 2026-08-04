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
//! This is a strict two-step process:
//! 1. The caller invokes `PreparePrint`. The portal shows the UI and returns a `token` (a `u32` identifier).
//! 2. The caller generates the print data (e.g., renders a PDF) based on the settings returned by `PreparePrint`.
//! 3. The caller invokes `Print`, providing the generated `token` and a file descriptor (`fd`) containing the raw document data.
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
//! 4. **User Consent:** A `gtk4::PrintOperation` dialog is presented. The user selects a printer
//!    and configures settings.
//! 5. **Token Generation:** If the user clicks "Print", GTK generates a `gtk4::PrintSettings` and
//!    `gtk4::PageSetup`. The portal assigns a unique `token` (an integer) and caches these settings
//!    in memory.
//! 6. **Response:** The portal returns the `token` and the serialized print settings to the app.
//!
//! **Step 2: Print**
//! 1. **Application** calls `Print`, passing the `token` and a file descriptor (`fd`).
//! 2. **Portal Object (`Print`)** receives the request in `print`.
//! 3. **Validation:** The portal extracts the raw Unix file descriptor (`AsRawFd::as_raw_fd`) because
//!    zbus duplicates it automatically.
//! 4. **GUI/Backend Interaction:** The request is dispatched to the GTK thread via `ExecutePrintUi::run`.
//! 5. **Execution:** The GTK thread looks up the cached `gtk4::PrintOperation` associated with the `token`.
//!    It connects to the `draw-page` signal, reading data from the `fd` and writing it to the GTK
//!    printing context, which then submits the job to CUPS.
//! 6. **Cleanup:** The token is invalidated, and the cached print operation is destroyed.
//!
//! **Ownership:**
//! The `token` acts as a capability ticket. The GTK thread owns the heavy `gtk4::PrintOperation`
//! state between the two calls.
//!
//! ## Session Management
//!
//! The Print portal does not use standard Portal Sessions. Instead, it uses the `token` mechanism
//! to maintain state across the two distinct D-Bus method calls.
//!
//! ## GTK Integration
//!
//! The printing portal relies entirely on GTK's robust, cross-platform printing infrastructure (`GtkPrintOperation`).
//! - **Thread Transition:** All printing operations, including reading the file descriptor during `Print`,
//!   must happen on the GTK main thread because `GtkPrintOperation` is heavily tied to the GTK main loop
//!   and Cairo rendering contexts.
//! - **File Descriptor Handling:** The raw FD is passed from Tokio to the GTK thread. GTK reads this FD
//!   synchronously or asynchronously depending on the underlying implementation, but always within the
//!   GTK context.
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
//! - **Custom Print Settings:** If GTK adds new custom print settings, `gui.rs` must be updated to
//!   serialize and deserialize these specific keys into the D-Bus dictionaries passed during `PreparePrint`.
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
//! - **Token Security:** Tokens are simple `u32` counters. This is theoretically susceptible to guessing,
//!   but because the attack surface is limited to the sandbox sending print data to an already-approved
//!   print job (which the user just clicked "Print" on), the risk is minimal.

pub mod dbus;
pub mod gui;

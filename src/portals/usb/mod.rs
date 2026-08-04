//! # USB Portal
//!
//! ## Portal Purpose
//!
//! The USB portal mediates access to raw USB devices for sandboxed applications.
//!
//! Because raw USB access allows applications to completely bypass OS-level security
//! controls (e.g., sniffing keystrokes from a USB keyboard, or flashing firmware), it
//! requires explicit user consent. This portal receives a list of currently connected
//! USB devices from the host daemon (which has hardware access) and presents them in a
//! GTK device chooser dialog. If the user selects a device, the host daemon grants the
//! sandbox a file descriptor or WebUSB token to access it.
//!
//! This portal implements the `org.freedesktop.portal.Usb` specification.
//!
//! ## D-Bus Interface
//!
//! - **Interface Name:** `org.freedesktop.impl.portal.Usb`
//! - **Object Path:** `/org/freedesktop/portal/desktop`
//! - **Methods:** `AcquireDevices(handle, parent_window, app_id, devices, options)`
//! - **Signals:** None natively.
//!
//! **Expected Caller Behavior:**
//! The caller (usually the xdg-desktop-portal host daemon, not the app directly) passes an
//! array of USB devices (`devices`), where each device contains udev properties (vendor ID,
//! product ID, etc.). The portal must present a UI and return a dictionary of the devices
//! the user explicitly selected.
//!
//! **Implementation Mapping:**
//! Implemented in `dbus.rs` by the `UsbPortal` struct. The UI rendering is handled by
//! `UsbUi` in `gui.rs`.
//!
//! ## Request Lifecycle
//!
//! 1. **Caller** sends an `AcquireDevices` D-Bus method call.
//! 2. **Portal Object (`UsbPortal`)** receives the request.
//! 3. **Validation & Parsing:** The portal parses the incoming complex `UsbDeviceData` array.
//!    - Udev often escapes spaces in strings (e.g., `Logitech\x20Mouse`). `parse_udev_string`
//!      reverts these to human-readable strings.
//!    - It extracts the vendor, model, and serial number from a prioritized list of udev keys
//!      (`ID_VENDOR_FROM_DATABASE`, `ID_VENDOR_ENC`, etc.).
//! 4. **GUI Interaction:** The parsed list of `UsbDevice` structs is sent to the GTK main thread
//!    via `UsbUi::run`.
//! 5. **User Consent:** A device chooser dialog (usually a listbox with selectable rows) is presented.
//! 6. **Response Generation:**
//!    - If the user selects devices and clicks "Allow", the portal returns the original IDs and
//!      properties of *only* the selected devices in the `UsbResults` dictionary.
//!    - If the user cancels or closes the dialog, `Response::cancelled()` is returned.
//! 7. **Cleanup:** `run_request` handles the D-Bus Request object lifecycle.
//!
//! **Ownership:** `run_request` owns the D-Bus request state. The GTK thread exclusively owns
//! the chooser dialog widget.
//!
//! ## Session Management
//!
//! The USB portal does not use sessions in the GTK backend. Every `AcquireDevices` call is a
//! discrete request. (The host daemon manages the actual sandbox hole-punching session).
//!
//! ## GTK Integration
//!
//! A UI is mandatory to select devices.
//! - **Thread Transition:** Execution moves to the GTK main thread via `UiProxy` to construct
//!   the chooser dialog.
//! - **List Construction:** `gui.rs` dynamically builds a list of widgets representing the connected
//!   hardware based on the parsed udev data.
//!
//! ## Backend Interaction
//!
//! The portal itself does not interact with udev or the USB bus directly. It entirely relies on
//! the `devices` array passed to it over D-Bus by the host `xdg-desktop-portal` daemon (which
//! uses `libusb` or `udev` internally).
//!
//! ## Specification Notes
//!
//! - **Device Data Signature:** The signature for the incoming device list is extremely complex:
//!   `a(sa{sv}a{sv})` (Array of Tuples containing: String ID, Dict of properties, Dict of options).
//! - **Fallback Strings:** If udev fails to provide human-readable strings for a device, the portal
//!   falls back to generic translated strings (`unknown_device`, `unknown_vendor`) to ensure the
//!   user still sees an entry they can select or deny.
//!
//! ## Extension Guide
//!
//! - **Udev Parsing:** If udev introduces new keys for better hardware descriptions, update the
//!   string slices in `extract_property` inside `dbus.rs`.
//!
//! ## Cross-Portal Consistency
//!
//! - **Device Chooser Pattern:** Similar to the `AppChooser` portal, it takes a list of options
//!   from D-Bus, parses them, presents a graphical list to the user, and returns the selected item(s).
//! - **Request Handling:** Uses the standard `run_request` wrapper from `crate::core::request`.
//!
//! ## Maintenance Notes
//!
//! - **Hex Unescaping:** The `parse_udev_string` logic is critical. Without it, users see ugly
//!   system strings like `Generic\x20Flash\x20Drive`, which severely degrades the user experience.

pub mod dbus;
pub mod gui;

#[cfg(feature = "access")]
pub mod access;
#[cfg(feature = "account")]
pub mod account;
#[cfg(feature = "app_chooser")]
pub mod app_chooser;
#[cfg(feature = "clipboard")]
pub mod clipboard;
#[cfg(feature = "dynamic_launcher")]
pub mod dynamic_launcher;
#[cfg(feature = "email")]
pub mod email;
#[cfg(feature = "file_chooser")]
pub mod file_chooser;
#[cfg(feature = "inhibit")]
pub mod inhibit;
#[cfg(feature = "lockdown")]
pub mod lockdown;
#[cfg(feature = "notification")]
pub mod notification;
// GTK's Unix print API (GtkPrintUnixDialog/GtkPrintJob) is only bound on Linux
// in gtk4-rs, so the print portal cannot be built on other targets.
#[cfg(all(feature = "print", target_os = "linux"))]
pub mod print;
#[cfg(feature = "settings")]
pub mod settings;
#[cfg(feature = "usb")]
pub mod usb;

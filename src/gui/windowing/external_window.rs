use {
    super::{
        wayland::set_wayland_parent, window_identifier::WindowIdentifier, x11::set_x11_parent,
    },
    gdk4_wayland::WaylandDisplay,
    gdk4_x11::X11Display,
    gtk4::prelude::{Cast, GtkWindowExt, IsA, WidgetExt},
};

/// Configures the window to act as a proper dialog for the requesting application.
///
/// This method handles:
/// 1. Setting the `activation_token` to guarantee focus transfer if provided.
/// 2. Establishing a modal `transient` parent relationship via Wayland or X11 backends.
///
/// Note: This method must be called **before** the widget is realized if the window
/// display needs to be changed (e.g., for the XWayland fallback).
pub fn setup_window<W: IsA<gtk4::Window> + IsA<gtk4::Widget>>(
    window: &W,
    parent_handle: &str,
    activation_token: Option<&str>,
) {
    if let Some(token) = activation_token {
        window.set_startup_id(token);
    }

    if parent_handle.is_empty() {
        return;
    }

    let Some(identifier) = WindowIdentifier::parse(parent_handle) else {
        return;
    };

    let default_display = gtk4::gdk::Display::default().unwrap();
    let is_wayland_display = default_display.downcast_ref::<WaylandDisplay>().is_some();
    let is_x11_display = default_display.downcast_ref::<X11Display>().is_some();

    match identifier {
        WindowIdentifier::Wayland(handle) => {
            if is_wayland_display {
                window.realize();
                set_wayland_parent(window.upcast_ref::<gtk4::Widget>(), &handle);
            } else {
                log::warn!("Wayland parent handle provided but portal is not running on Wayland.");
            }
        }
        WindowIdentifier::X11(xid) => {
            if is_x11_display {
                window.realize();
                set_x11_parent(window.upcast_ref::<gtk4::Widget>(), xid);
            } else if is_wayland_display {
                // XWayland Fallback:
                // If the portal is on Wayland, but the client is XWayland (x11 handle),
                // we open an X11 display explicitly so the dialog runs as an XWayland client,
                // allowing us to properly use XSetTransientForHint.
                if let Some(x11_display) = X11Display::open(None) {
                    window.set_display(&x11_display);
                    window.realize();
                    set_x11_parent(window.upcast_ref::<gtk4::Widget>(), xid);
                } else {
                    log::warn!("Failed to open X11 display for XWayland fallback.");
                }
            } else {
                log::warn!("X11 parent handle provided but portal backend is unsupported.");
            }
        }
    }
}

use {
    gdk4_x11::{X11Display, X11Surface},
    gtk4::prelude::{Cast, IsA, NativeExt, SurfaceExt, WidgetExt},
    x11::xlib,
};

/// Sets the transient-for hint on the X11 surface of the given widget.
/// The widget must be realized on an X11Display for this to work.
pub fn set_x11_parent(widget: &impl IsA<gtk4::Widget>, parent_xid: u64) {
    if let Some(surface) = widget.native().and_then(|n| n.surface()) {
        if let Some(x11_surface) = surface.downcast_ref::<X11Surface>() {
            let display = x11_surface.display().downcast::<X11Display>().unwrap();
            // Safety: xdisplay is a valid X11 Display pointer from gdk4-x11.
            // Both surface_xid and parent_xid are just Window identifiers (integers).
            unsafe {
                let xdisplay = display.xdisplay() as *mut xlib::Display;
                let surface_xid = x11_surface.xid();
                xlib::XSetTransientForHint(xdisplay, surface_xid, parent_xid);
            }
        } else {
            log::warn!("Tried to set X11 parent, but surface is not X11Surface");
        }
    }
}

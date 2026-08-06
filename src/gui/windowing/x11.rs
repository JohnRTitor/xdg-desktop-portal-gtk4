use {
    gdk4_x11::{X11Display, X11Surface},
    gtk4::{
        Widget,
        prelude::{Cast, IsA, NativeExt, SurfaceExt, ToplevelExt, WidgetExt},
    },
};

/// Sets the transient-for hint on the X11 surface of the given widget.
/// The widget must be realized on an X11Display for this to work.
pub fn set_x11_parent(widget: &impl IsA<Widget>, parent_xid: u64) {
    let Some(surface) = widget.native().and_then(|n| n.surface()) else {
        return;
    };

    let Some(x11_surface) = surface.downcast_ref::<X11Surface>() else {
        tracing::warn!("Tried to set X11 parent, but surface is not X11Surface");
        return;
    };

    let display = x11_surface
        .display()
        .downcast::<X11Display>()
        .expect("GTK guarantees that an X11Surface is always associated with an X11Display");

    // Safely look up the GDK surface representation from the raw parent XID
    let Some(parent_surface) = X11Surface::lookup_for_display(&display, parent_xid) else {
        tracing::error!("Failed to resolve GDK surface for parent XID: {parent_xid}");
        return;
    };

    // Set the transient parent using safe GDK4 methods
    let Some(toplevel) = surface.downcast_ref::<gtk4::gdk::Toplevel>() else {
        tracing::warn!("Tried to set X11 parent, but surface is not a Toplevel");
        return;
    };

    toplevel.set_transient_for(parent_surface.upcast_ref::<gtk4::gdk::Surface>());
}

use {
    gdk4_x11::{X11Display, X11Surface},
    gtk4::{
        Widget,
        gdk::{Surface, Toplevel},
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
        tracing::debug!("Tried to set X11 parent, but surface is not X11Surface");
        return;
    };

    let Ok(display) = x11_surface.display().downcast::<X11Display>() else {
        tracing::debug!("Tried to set X11 parent, but surface display is not X11Display");
        return;
    };

    let Some(parent_surface) = X11Surface::lookup_for_display(&display, parent_xid) else {
        tracing::debug!("Failed to resolve GDK surface for parent XID: {parent_xid}");
        return;
    };

    let Some(toplevel) = surface.downcast_ref::<Toplevel>() else {
        tracing::debug!("Tried to set X11 parent, but surface is not a Toplevel");
        return;
    };

    toplevel.set_transient_for(parent_surface.upcast_ref::<Surface>());
}

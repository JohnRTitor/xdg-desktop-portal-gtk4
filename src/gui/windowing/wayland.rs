use {
    gdk4_wayland::WaylandToplevel,
    gtk4::{
        Widget,
        prelude::{Cast, IsA, NativeExt, WidgetExt},
    },
};

pub fn set_wayland_parent(widget: &impl IsA<Widget>, parent_window: &str) {
    let Some(surface) = widget.native().and_then(|n| n.surface()) else {
        return;
    };
    let Some(toplevel) = surface.downcast_ref::<WaylandToplevel>() else {
        tracing::debug!("Tried to set Wayland parent, but surface is not WaylandToplevel");
        return;
    };
    // A false return means GDK could not export the handle, leaving the dialog
    // unparented; that is worth recording instead of silently ignoring it.
    if !toplevel.set_transient_for_exported(parent_window) {
        tracing::debug!(
            parent_window,
            "Failed to set exported Wayland transient parent"
        );
    }
}

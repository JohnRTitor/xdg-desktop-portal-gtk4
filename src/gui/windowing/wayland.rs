use {
    gdk4_wayland::WaylandToplevel,
    gtk4::prelude::{Cast, IsA, NativeExt, WidgetExt},
};

pub fn set_wayland_parent(widget: &impl IsA<gtk4::Widget>, parent_window: &str) {
    let Some(surface) = widget.native().and_then(|n| n.surface()) else {
        return;
    };
    let Some(toplevel) = surface.downcast_ref::<WaylandToplevel>() else {
        tracing::warn!("Tried to set Wayland parent, but surface is not WaylandToplevel");
        return;
    };
    toplevel.set_transient_for_exported(parent_window);
}

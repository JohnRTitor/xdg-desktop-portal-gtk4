use {
    gdk4_wayland::WaylandToplevel,
    gtk4::prelude::{Cast, IsA, WidgetExt},
};

pub fn set_wayland_parent(widget: &impl IsA<gtk4::Widget>, parent_window: &str) {
    if let Some(parent) = parent_window.strip_prefix("wayland:") {
        if let Some(surface) = widget.surface() {
            if let Some(toplevel) = surface.downcast_ref::<WaylandToplevel>() {
                toplevel.set_transient_for_exported(parent);
            }
        }
    }
}

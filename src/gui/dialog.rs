use gtk4::prelude::*;

/// A reusable custom dialog widget to replace the deprecated `MessageDialog`.
/// GTK4 `AlertDialog` does not support custom child widgets, so this struct builds
/// a standard `gtk4::Window` configured to look and behave like a dialog.
pub struct CustomDialog {
    pub window: gtk4::Window,
    pub content_area: gtk4::Box,
    pub action_area: gtk4::Box,
}

impl CustomDialog {
    pub fn new(title: &str, modal: bool) -> Self {
        let window = gtk4::Window::builder()
            .title(title)
            .modal(modal)
            .hide_on_close(true)
            .default_width(420)
            .default_height(400)
            .build();

        window.add_css_class("dialog");

        let main_box = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
        main_box.set_margin_top(12);
        main_box.set_margin_bottom(12);
        main_box.set_margin_start(12);
        main_box.set_margin_end(12);

        let content_area = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        // Allow the content area (e.g. a ScrolledWindow inside it) to grow
        // and fill available vertical space so buttons never get pushed off-screen.
        content_area.set_vexpand(true);

        let action_area = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        action_area.set_halign(gtk4::Align::End);

        main_box.append(&content_area);
        main_box.append(&action_area);

        window.set_child(Some(&main_box));

        Self {
            window,
            content_area,
            action_area,
        }
    }
}

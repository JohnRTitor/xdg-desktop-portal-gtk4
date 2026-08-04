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
            .default_width(crate::gui::DEFAULT_DIALOG_WIDTH)
            .default_height(crate::gui::DEFAULT_DIALOG_HEIGHT)
            .build();

        window.add_css_class("dialog");

        let main_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(crate::gui::DEFAULT_SPACING)
            .margin_top(crate::gui::DEFAULT_MARGIN)
            .margin_bottom(crate::gui::DEFAULT_MARGIN)
            .margin_start(crate::gui::DEFAULT_MARGIN)
            .margin_end(crate::gui::DEFAULT_MARGIN)
            .build();

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

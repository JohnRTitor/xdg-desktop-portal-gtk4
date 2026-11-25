use gtk4::{Align, Box as GtkBox, Orientation, Window, prelude::*};

/// A reusable custom dialog widget.
///
/// This struct builds a standard `gtk4::Window` configured to look and behave like
/// a dialog while allowing arbitrary child widgets in its content area.
pub struct CustomDialog {
    pub window: Window,
    pub content_area: GtkBox,
    pub action_area: GtkBox,
}

impl CustomDialog {
    pub fn new(title: &str, modal: bool) -> Self {
        let window = Window::builder()
            .title(title)
            .modal(modal)
            .hide_on_close(true)
            .default_width(crate::gui::DEFAULT_DIALOG_WIDTH)
            .default_height(crate::gui::DEFAULT_DIALOG_HEIGHT)
            .build();

        window.add_css_class("dialog");

        let main_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(crate::gui::DEFAULT_SPACING)
            .margin_top(crate::gui::DEFAULT_MARGIN)
            .margin_bottom(crate::gui::DEFAULT_MARGIN)
            .margin_start(crate::gui::DEFAULT_MARGIN)
            .margin_end(crate::gui::DEFAULT_MARGIN)
            .build();

        let content_area = GtkBox::new(Orientation::Vertical, 6);
        // Allow the content area (e.g. a ScrolledWindow inside it) to grow
        // and fill available vertical space so buttons never get pushed off-screen.
        content_area.set_vexpand(true);

        let action_area = GtkBox::new(Orientation::Horizontal, 6);
        action_area.set_halign(Align::End);

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

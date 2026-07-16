pub mod dialog;
pub mod error;
pub mod ui;

pub use {
    error::UiError,
    ui::{Ui, UiProxy},
};

use {
    crate::utils::external_window::set_wayland_parent,
    async_channel::{Receiver, Sender, bounded},
    gtk4::{glib::MainContext, prelude::*},
};

/// Runs a closure on the GTK main thread and waits for its result.
/// This abstracts the `async-channel` setup and `context.invoke` logic.
pub async fn run_ui_task<T, E, F, C>(proxy: &UiProxy, f: F, on_closed: C) -> Result<T, E>
where
    T: Send + 'static,
    E: Send + 'static,
    F: FnOnce(Sender<Result<T, E>>, MainContext, Receiver<()>) + Send + 'static,
    C: FnOnce() -> E,
{
    let (send, recv) = bounded(1);
    let (_send, close_on_close) = bounded(1);
    let context = proxy.context.clone();

    proxy
        .context
        .invoke(move || f(send, context, close_on_close));

    recv.recv().await.unwrap_or_else(|_| Err(on_closed()))
}

/// Realizes the widget and assigns the Wayland parent handle.
pub fn setup_wayland<W: IsA<gtk4::Widget>>(widget: &W, parent_handle: &str) {
    widget.realize();
    set_wayland_parent(widget.upcast_ref::<gtk4::Widget>(), parent_handle);
}

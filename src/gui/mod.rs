pub mod dialog;
pub mod error;
pub mod ui;
pub mod windowing;

pub use {
    error::UiError,
    ui::{Ui, UiProxy},
};

use {
    async_channel::{Receiver, Sender, bounded},
    gtk4::{glib::MainContext, prelude::*},
};

/// Runs a closure on the GTK main thread and waits for its result.
///
/// D-Bus methods handle requests asynchronously and may execute on background threads
/// managed by `zbus`. However, GTK objects (`gtk4::Widget`, `gtk4::Window`, etc.) are
/// strictly `!Send` and `!Sync`, meaning they must be created and accessed exclusively
/// on the GTK main thread.
///
/// This function abstracts the `async-channel` setup and `context.invoke` logic. It:
/// 1. Takes a closure `f` that will run on the GTK main thread.
/// 2. Passes a `Sender` to `f` so it can send the result back.
/// 3. Passes a `Receiver` to `f` so it can be notified if the request is cancelled (`close_on_close`).
/// 4. Waits for the result on the current (background) thread.
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

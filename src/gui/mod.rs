//! GTK UI components and cross-thread synchronization primitives.
//!
//! The `gui` module bridges the asynchronous, multithreaded world of `tokio` and `zbus`
//! with the single-threaded, thread-affine world of GTK 4.
//!
//! GTK widgets can only be instantiated and mutated from the main thread. To enforce this,
//! D-Bus request handlers (which run on Tokio background threads) use [`run_ui_task`] to
//! safely dispatch work to the GTK main loop and await the result.

pub mod dialog;
pub mod error;
pub mod ui;
pub mod windowing;

pub(crate) const DEFAULT_SPACING: i32 = 12;
pub(crate) const DEFAULT_MARGIN: i32 = 12;
pub(crate) const ELEMENT_MARGIN: i32 = 10;
pub(crate) const SMALL_MARGIN: i32 = 6;
pub(crate) const LABEL_MAX_WIDTH_CHARS: i32 = 50;
pub(crate) const DEFAULT_DIALOG_WIDTH: i32 = 420;
pub(crate) const DEFAULT_DIALOG_HEIGHT: i32 = 400;

use {gtk4::glib, tokio::sync::mpsc};

pub struct GuiDispatcher {
    pub context: glib::MainContext,
    pub sender: mpsc::UnboundedSender<Box<dyn FnOnce() + Send>>,
}

pub trait PortalDispatcher<T> {
    fn dispatch(&self, data: T) -> Result<(), T>;
}

pub type UiDispatcher<T> = std::rc::Rc<std::cell::RefCell<Option<tokio::sync::oneshot::Sender<T>>>>;

impl<T> PortalDispatcher<T> for UiDispatcher<T> {
    fn dispatch(&self, data: T) -> Result<(), T> {
        if let Some(tx) = self
            .try_borrow_mut()
            .ok()
            .and_then(|mut guard| guard.take())
        {
            return tx.send(data);
        }
        Err(data)
    }
}

use gtk4::glib::MainContext;
pub use {
    error::UiError,
    ui::{Ui, UiProxy},
};

/// Runs a closure on the GTK main thread and waits for its result.
///
/// D-Bus methods handle requests asynchronously and may execute on background threads
/// managed by `zbus`. However, GTK objects (`gtk4::Widget`, `gtk4::Window`, etc.) are
/// strictly `!Send` and `!Sync`, meaning they must be created and accessed exclusively
/// on the GTK main thread.
///
/// This function abstracts the cross-thread communication between Tokio and GTK. It:
/// 1. Takes a closure `f` that will run on the GTK main thread.
/// 2. Schedules `f` to run on GTK via the `UiProxy` sender channel.
/// 3. Passes a `Sender` to `f` so it can send the result back to Tokio.
/// 4. Passes a `Receiver` to `f` so it can be notified if the request is cancelled (`close_on_close`).
/// 5. Suspends the current Tokio task until GTK replies.
pub async fn run_ui_task<T, E, F, C>(proxy: &UiProxy, f: F, on_closed: C) -> Result<T, E>
where
    T: Send + 'static,
    E: Send + 'static,
    F: FnOnce(UiDispatcher<Result<T, E>>, MainContext, tokio::sync::oneshot::Receiver<()>)
        + Send
        + 'static,
    C: FnOnce() -> E,
{
    let (send, recv) = tokio::sync::oneshot::channel();

    // Cancellation Safety Pattern:
    // We create a oneshot channel here but DO NOT store the `_close_on_close_tx` anywhere.
    // It remains held entirely within the stack frame/generator of this `run_ui_task` Future.
    // If the caller drops the Future (i.e. task cancellation), `_close_on_close_tx` will be
    // automatically dropped. This cleanly closes the channel, notifying `close_on_close`
    // (which was passed to GTK) that the operation has been aborted, allowing GTK to close
    // the window or clean up.
    let (_close_on_close_tx, close_on_close) = tokio::sync::oneshot::channel();

    let context = proxy.context.clone();

    let _ = proxy.sender.send(Box::new(move || {
        let send_rc = std::rc::Rc::new(std::cell::RefCell::new(Some(send)));
        f(send_rc, context, close_on_close)
    }));

    recv.await.unwrap_or_else(|_| Err(on_closed()))
}

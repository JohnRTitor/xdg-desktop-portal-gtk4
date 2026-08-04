use gtk4::{
    glib,
    glib::{MainContext, MainLoop},
};

pub type UiTask = Box<dyn FnOnce() + Send + 'static>;

/// Encapsulates the GTK application state and main event loop.
///
/// # Threading Assumptions
///
/// GTK strictly requires that all UI operations (widget creation, modification,
/// window presentation) happen on a single thread—the thread where `gtk4::init()`
/// was called. This struct enforces that invariant by establishing a `MainContext`
/// and providing a channel (`UiProxy`) to send closures from Tokio background threads
/// to the GTK main thread.
pub struct Ui {
    main_loop: MainLoop,
    proxy: UiProxy,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

impl Ui {
    pub fn new() -> Self {
        let main_loop = MainLoop::new(None, false);

        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<UiTask>();

        // spawn_local is crucial here: it attaches the future to the GTK MainContext
        // rather than the Tokio runtime. This ensures that the queued tasks (which
        // typically create or manipulate GTK widgets) execute exclusively on the main thread.
        main_loop.context().spawn_local(async move {
            while let Some(task) = receiver.recv().await {
                task();
            }
        });

        Self {
            proxy: UiProxy {
                context: main_loop.context().clone(),
                sender,
            },
            main_loop,
        }
    }

    pub fn init_gtk(&self) {
        if let Err(e) = gtk4::init() {
            tracing::error!("Failed to initialize GTK: {}", e);
            std::process::exit(1);
        }
        glib::set_prgname(Some("xdg-desktop-portal-gtk4"));
    }

    pub fn run(&self) {
        self.main_loop.run();
    }

    pub fn proxy(&self) -> &UiProxy {
        &self.proxy
    }
}

/// A thread-safe proxy to the GTK MainContext.
///
/// Because GTK objects are `!Send`, we cannot easily share them across D-Bus task boundaries.
/// `UiProxy` can be safely cloned and moved into `zbus` request handlers, allowing those
/// background tasks to spawn work back onto the GTK main thread using `sender`
/// (which is exactly what [`run_ui_task`](crate::gui::run_ui_task) does).
///
/// # Ownership & Lifetime
///
/// This proxy holds a sender channel to the main GTK loop. It expects the receiving end
/// in the main loop to outlive any background tasks.
#[derive(Clone)]
pub struct UiProxy {
    pub context: MainContext,
    pub sender: tokio::sync::mpsc::UnboundedSender<UiTask>,
}

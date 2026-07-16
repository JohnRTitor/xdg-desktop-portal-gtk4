use gtk4::{
    glib,
    glib::{MainContext, MainContextAcquireGuard, MainLoop},
};

/// Encapsulates the GTK application state and main event loop.
pub struct Ui {
    main_loop: MainLoop,
    proxy: UiProxy,
}

impl Ui {
    pub fn new() -> Self {
        let main_loop = MainLoop::new(None, false);
        Self {
            proxy: UiProxy {
                context: main_loop.context().clone(),
            },
            main_loop,
        }
    }

    pub fn init_gtk(&self) {
        if let Err(e) = gtk4::init() {
            log::error!("Failed to initialize GTK: {}", e);
            std::process::exit(1);
        }
        glib::set_prgname(Some("xdg-desktop-portal-gtk4"));
    }

    /// Acquires the `MainContext`, preventing other threads from running
    /// closures inline via `context.invoke()`.
    ///
    /// When the `MainContext` has no owner, any thread calling `invoke()` can
    /// acquire it and execute the closure synchronously on that thread. This is
    /// dangerous during the startup window between `Portal::create()` (which
    /// registers D-Bus interfaces) and `init_gtk()` (which initializes GTK):
    /// an incoming D-Bus request could trigger `invoke()` on the zbus executor
    /// thread, running GTK widget code before GTK is initialized.
    ///
    /// By holding this guard, `invoke()` from other threads always queues
    /// closures as idle sources. They will only execute once `main_loop.run()`
    /// starts processing the loop (which happens after `init_gtk()`).
    pub fn hold_context(&self) -> MainContextAcquireGuard<'_> {
        self.proxy
            .context
            .acquire()
            .expect("MainContext should not already be owned")
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
/// background tasks to spawn work back onto the GTK main thread using `context.invoke()`
/// (which is exactly what `run_ui_task` does).
#[derive(Clone)]
pub struct UiProxy {
    pub context: MainContext,
}

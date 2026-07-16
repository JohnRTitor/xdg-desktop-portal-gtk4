use gtk4::{
    glib,
    glib::{MainContext, MainLoop},
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

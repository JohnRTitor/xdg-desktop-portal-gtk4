use gtk4::{
    glib,
    glib::{MainContext, MainLoop},
};

pub mod access;
pub mod account;
pub mod dynamic_launcher;
pub mod print;
pub mod file_chooser;

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

#[derive(Clone)]
pub struct UiProxy {
    context: MainContext,
}

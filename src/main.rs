use {
    clap::Parser,
    xdg_desktop_portal_gtk4::{core::Portal, gui::Ui, logging},
};

/// The xdg-desktop-portal-gtk4 portal.
#[derive(Parser, Debug)]
struct Cli {
    /// Replace the portal if it is already running.
    #[clap(long)]
    pub replace: bool,
}

fn main() {
    logging::init();
    init_i18n();

    let args = Cli::parse();
    
    // We instantiate the UI state first, which sets up the GTK MainContext and channel receivers.
    // This allows us to pass a thread-safe Proxy to the D-Bus services.
    let ui = Ui::new();
    
    // Initialize the D-Bus portal objects. We block on the GTK MainContext here
    // to ensure that name acquisition and object registration on D-Bus succeed
    // *before* we start the GTK main loop. If we fail to acquire the name (e.g. 
    // another portal is running and `replace` is false), we exit immediately.
    let _portal = match ui
        .proxy()
        .context
        .block_on(async { Portal::create(ui.proxy(), args.replace).await })
    {
        Ok(p) => p,
        Err(e) => {
            log::error!("Could not create the portal: {}", anyhow::Error::new(e));
            std::process::exit(1);
        }
    };
    // Now that D-Bus is set up and we own the bus name, we initialize GTK
    // and enter its blocking event loop.
    ui.init_gtk();
    ui.run();
}

fn init_i18n() {
    let current = match current_locale::current_locale() {
        Ok(c) => c,
        Err(e) => {
            log::error!(
                "Could not retrieve current locale: {}",
                anyhow::Error::new(e)
            );
            return;
        }
    };
    let tags = match language_tags::LanguageTag::parse(&current) {
        Ok(t) => t,
        Err(e) => {
            log::error!("Could not parse current localE: {}", anyhow::Error::new(e));
            return;
        }
    };
    rust_i18n::set_locale(tags.primary_language());
}

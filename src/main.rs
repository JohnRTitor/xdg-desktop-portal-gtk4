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

    // Acquire the MainContext so that no other thread can own it during startup.
    //
    // When the MainContext has no owner, `context.invoke()` on a background
    // thread (e.g. the zbus executor) will acquire it, then execute the closure
    // **synchronously on that background thread**. If a D-Bus request arrives
    // between `Portal::create()` (which registers the bus name) and `init_gtk()`
    // (which initializes GTK), the zbus executor would try to create GTK widgets
    // on its own thread before GTK is ready — causing a panic.
    //
    // By holding this guard, `invoke()` always queues closures as idle sources
    // instead. They will only run once `main_loop.run()` starts processing the
    // loop, which is after `init_gtk()`.
    let guard = ui.hold_context();

    // Initialize the D-Bus portal objects. We block on the GTK MainContext here
    // to ensure that name acquisition and object registration on D-Bus succeed
    // *before* we start the GTK main loop. If we fail to acquire the name (e.g.
    // another portal is running and `replace` is false), we exit immediately.
    //
    // `block_on` recursively acquires the MainContext on the same thread,
    // so it coexists safely with the guard we already hold.
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

    // Now that D-Bus is set up, initialize GTK. Any closures queued by early
    // D-Bus requests will see GTK as initialized when they finally execute.
    ui.init_gtk();

    // Drop the guard before entering the main loop, because `MainLoop::run()`
    // acquires the context itself. The queued closures will start executing
    // once the loop begins iterating.
    drop(guard);
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

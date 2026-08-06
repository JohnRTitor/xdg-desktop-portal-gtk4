use xdg_desktop_portal_gtk4::{
    core::Portal,
    gui::{Ui, UiProxy},
    logging,
};

#[tokio::main(worker_threads = 2)]
async fn portal_worker(
    proxy: UiProxy,
    replace: bool,
    tx: std::sync::mpsc::Sender<Result<(), xdg_desktop_portal_gtk4::core::PortalError>>,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    name_lost_tx: tokio::sync::oneshot::Sender<()>,
) {
    let portal = match Portal::create(&proxy, replace, name_lost_tx).await {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(Err(e));
            return;
        }
    };

    let _ = tx.send(Ok(()));

    // Keep the Tokio runtime alive until the GTK main loop exits.
    // We listen for a shutdown signal sent at the very end of `main()`.
    let _ = shutdown_rx.await;

    // Explicitly drop the portal here.
    // This ensures the zbus `ObjectServer` and `Connection` are dropped on the Tokio
    // thread, unregistering our D-Bus name gracefully before the process exits.
    drop(portal);
}
fn main() {
    logging::init();
    init_i18n();

    let replace = std::env::args().any(|arg| arg == "--replace");

    // We instantiate the UI state first, which sets up the GTK MainContext and channel receivers.
    // This allows us to pass a thread-safe Proxy to the D-Bus services.
    let ui = Ui::new();

    // Initialize the D-Bus portal objects on a dedicated Tokio background thread.
    //
    // GTK 4 objects are strictly `!Send` and `!Sync`. To prevent blocking the GTK main loop
    // (which handles rendering and window events), all asynchronous D-Bus method handling
    // happens on this Tokio thread.
    //
    // We block the main thread waiting for a success signal to ensure that name
    // acquisition and object registration on D-Bus succeed *before* we start the
    // GTK main loop. This prevents a race condition where the portal could claim
    // readiness to the desktop environment before it is actually listening.
    let (tx, rx) = std::sync::mpsc::channel();
    let proxy = ui.proxy().clone();

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (name_lost_tx, name_lost_rx) = tokio::sync::oneshot::channel::<()>();

    std::thread::spawn(move || {
        portal_worker(proxy, replace, tx, shutdown_rx, name_lost_tx);
    });

    match rx.recv() {
        Ok(Err(e)) => {
            tracing::error!("Could not create the portal: {}", e);
            std::process::exit(1);
        }
        Err(e) => {
            tracing::error!(
                "Portal background thread exited unexpectedly before initialization completed: {}",
                e
            );
            std::process::exit(1);
        }
        Ok(Ok(())) => {} // initialized successfully
    }

    // Now that D-Bus is set up, initialize GTK. Any closures queued by early
    // D-Bus requests (via `run_ui_task`) will see GTK as initialized when they finally execute.
    ui.init_gtk();

    let main_loop_clone = ui.main_loop().clone();
    ui.proxy().context.spawn_local(async move {
        let _ = name_lost_rx.await;
        main_loop_clone.quit();
    });

    // Start the GTK main loop. This will consume the current thread.
    // The queued closures from `Ui::new()` will start executing once the loop begins iterating.
    ui.run();

    let _ = shutdown_tx.send(());
}

fn init_i18n() {
    let current = match current_locale::current_locale() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(
                error = %e,
                "Could not retrieve current locale"
            );
            return;
        }
    };
    let tags = match language_tags::LanguageTag::parse(&current) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "Could not parse current locale");
            return;
        }
    };
    rust_i18n::set_locale(tags.primary_language());
}

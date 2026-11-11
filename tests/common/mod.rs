use {
    gtk4::glib::MainContext, tokio::sync::mpsc::unbounded_channel,
    xdg_desktop_portal_gtk4::gui::UiProxy,
};

/// Skips the current integration test when a D-Bus session bus is unavailable.
///
/// Expands to the connected [`zbus::Connection`] on success, or prints a skip
/// message and returns `Ok(())` from the test on failure. Intended for tests
/// whose signature is `Result<(), Box<dyn std::error::Error>>`.
#[macro_export]
macro_rules! try_dbus_session {
    () => {
        match zbus::Connection::session().await {
            Ok(conn) => conn,
            Err(_) => {
                println!("Skipping dbus test because D-Bus session is unavailable");
                return Ok(());
            }
        }
    };
}

/// Creates a dummy `UiProxy` for tests. The receiver side is immediately
/// dropped, so any closures sent via the proxy are silently discarded.
/// This is fine for settings tests that only exercise Read/ReadAll,
/// and for UI tests where we just want to ensure the D-Bus method
/// deserialization succeeds before the UI phase.
#[allow(dead_code)]
pub fn dummy_proxy() -> UiProxy {
    let (sender, _receiver) = unbounded_channel();
    UiProxy {
        context: MainContext::default(),
        sender,
    }
}

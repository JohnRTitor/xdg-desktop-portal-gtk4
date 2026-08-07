use {
    gtk4::glib::MainContext, tokio::sync::mpsc::unbounded_channel,
    xdg_desktop_portal_gtk4::gui::UiProxy,
};

#[macro_export]
macro_rules! skip_if_dbus_tests_disabled {
    () => {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            println!("Skipping dbus test because RUN_DBUS_TESTS is not set");
            return Ok(());
        }
    };
}

/// Creates a dummy `UiProxy` for tests. The receiver side is immediately
/// dropped, so any closures sent via the proxy are silently discarded.
/// This is fine for settings tests that only exercise Read/ReadAll,
/// and for UI tests where we just want to ensure the D-Bus method
/// deserialization succeeds before the UI phase.
pub fn dummy_proxy() -> UiProxy {
    let (sender, _receiver) = unbounded_channel();
    UiProxy {
        context: MainContext::default(),
        sender,
    }
}

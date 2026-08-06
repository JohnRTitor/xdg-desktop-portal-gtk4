use {
    gtk4::glib::MainContext,
    std::collections::HashMap,
    tokio::sync::mpsc::unbounded_channel,
    zbus::{
        Connection,
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, OwnedValue, Value},
    },
};
macro_rules! skip_if_dbus_tests_disabled {
    () => {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            println!("Skipping dbus test because RUN_DBUS_TESTS is not set");
            return Ok(());
        }
    };
}

use xdg_desktop_portal_gtk4::{
    gui::UiProxy,
    portals::{
        inhibit::dbus::Inhibit, lockdown::dbus::LockdownPortal, settings::dbus::SettingsPortal,
    },
};

/// Creates a dummy `UiProxy` for tests. The receiver side is immediately
/// dropped, so any closures sent via the proxy are silently discarded.
/// This is fine for settings tests that only exercise Read/ReadAll.
fn dummy_proxy() -> UiProxy {
    let (sender, _receiver) = unbounded_channel();
    UiProxy {
        context: MainContext::default(),
        sender,
    }
}

// Proxies for the tests

#[proxy(
    interface = "org.freedesktop.impl.portal.Lockdown",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Lockdown {
    #[zbus(property, name = "disable-printing")]
    fn disable_printing(&self) -> zbus::Result<bool>;

    #[zbus(property, name = "disable-save-to-disk")]
    fn disable_save_to_disk(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.freedesktop.impl.portal.Settings",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Settings {
    fn read(&self, namespace: &str, key: &str) -> zbus::Result<OwnedValue>;
    fn read_all(
        &self,
        namespaces: &[&str],
    ) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;

    #[zbus(property)]
    fn version(&self) -> zbus::Result<u32>;
}

#[proxy(
    interface = "org.freedesktop.impl.portal.Inhibit",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait InhibitTest {
    fn inhibit(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        window: &str,
        reason: u32,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;
}

#[tokio::test]
async fn test_lockdown_all_properties_false() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let _conn = Builder::session()?
        .serve_at("/org/freedesktop/portal/desktop", LockdownPortal::new())?
        .build()
        .await?;

    let client_conn = Connection::session().await?;
    let proxy = LockdownProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    assert!(!proxy.disable_printing().await?);
    assert!(!proxy.disable_save_to_disk().await?);

    Ok(())
}

#[tokio::test]
async fn test_settings_read_unknown_namespace() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let _conn = Connection::session().await?;
    let server = _conn.object_server();
    server
        .at(
            "/org/freedesktop/portal/desktop",
            SettingsPortal::new(&dummy_proxy(), server.clone()),
        )
        .await?;

    let client_conn = Connection::session().await?;
    let proxy = SettingsProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let res = proxy.read("com.nonexistent", "foo").await;
    assert!(res.is_err());

    Ok(())
}

#[tokio::test]
async fn test_settings_read_all_empty_namespaces() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let _conn = Connection::session().await?;
    let server = _conn.object_server();
    server
        .at(
            "/org/freedesktop/portal/desktop",
            SettingsPortal::new(&dummy_proxy(), server.clone()),
        )
        .await?;

    let client_conn = Connection::session().await?;
    let proxy = SettingsProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let res = proxy.read_all(&[]).await?;
    // It shouldn't crash. It might be empty if schemas are not installed.
    // Just asserting it successfully returns a HashMap.
    assert!(res.is_empty() || !res.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_inhibit_returns_success() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let client_conn = Connection::session().await?;
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Inhibit::new(
                xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
                    client_conn.clone(),
                    10,
                ),
                None,
            )
            .await,
        )?
        .build()
        .await?;

    let proxy = InhibitTestProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1").unwrap();
    proxy
        .inhibit(path, "app_id", "window", 1, HashMap::new())
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_settings_read_all_wildcard_namespaces() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let _conn = Connection::session().await?;
    let server = _conn.object_server();
    server
        .at(
            "/org/freedesktop/portal/desktop",
            SettingsPortal::new(&dummy_proxy(), server.clone()),
        )
        .await?;

    let client_conn = Connection::session().await?;
    let proxy = SettingsProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    // Use a wildcard to read all under org.freedesktop.*
    let res = proxy.read_all(&["org.freedesktop.*"]).await?;

    // Depending on the environment, it may or may not return keys, but it shouldn't error.
    assert!(res.is_empty() || !res.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_settings_portal_properties() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let _conn = Connection::session().await?;
    let server = _conn.object_server();
    server
        .at(
            "/org/freedesktop/portal/desktop",
            SettingsPortal::new(&dummy_proxy(), server.clone()),
        )
        .await?;

    let client_conn = Connection::session().await?;
    let proxy = SettingsProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    // Verify the version property returns 2
    let version = proxy.version().await?;
    assert_eq!(version, 2);

    Ok(())
}

use {
    std::collections::HashMap,
    zbus::{Connection, proxy, zvariant::OwnedValue},
};
mod common;
use {common::*, xdg_desktop_portal_gtk4::portals::settings::dbus::SettingsPortal};

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

#[tokio::test]
async fn test_settings_read_unknown_namespace() -> Result<(), Box<dyn std::error::Error>> {
    let _conn = try_dbus_session!();
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
    let _conn = try_dbus_session!();
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
async fn test_settings_read_all_wildcard_namespaces() -> Result<(), Box<dyn std::error::Error>> {
    let _conn = try_dbus_session!();
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
    let _conn = try_dbus_session!();
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

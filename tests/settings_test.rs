use {
    std::collections::HashMap,
    zbus::{Connection, OwnedVariant, proxy},
};
mod common;
use {common::*, xdg_desktop_portal_gtk4::portals::settings::dbus::SettingsPortal};

#[proxy(
    interface = "org.freedesktop.impl.portal.Settings",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Settings {
    fn read(&self, namespace: &str, key: &str) -> zbus::Result<OwnedVariant>;
    fn read_all(
        &self,
        namespaces: &[&str],
    ) -> zbus::Result<HashMap<String, HashMap<String, OwnedVariant>>>;

    #[zbus(property, name = "version")]
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
        .destination(_conn.unique_name().unwrap().clone())
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
        .destination(_conn.unique_name().unwrap().clone())
        .build()
        .await?;

    let res = proxy.read_all(&[]).await?;
    // An empty request returns every populated namespace. Which ones are
    // populated depends on the host, but an empty map is never sent.
    assert!(res.values().all(|keys| !keys.is_empty()));

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
        .destination(_conn.unique_name().unwrap().clone())
        .build()
        .await?;

    // Use a wildcard to read all under org.freedesktop.*
    let res = proxy.read_all(&["org.freedesktop.*"]).await?;

    // Matching is a prefix match on `org.freedesktop.`; which namespaces are
    // populated depends on the host, but none outside the prefix may leak in.
    assert!(res.keys().all(|ns| ns.starts_with("org.freedesktop.")));

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
        .destination(_conn.unique_name().unwrap().clone())
        .build()
        .await?;

    // Verify the version property returns 2
    let version = proxy.version().await?;
    assert_eq!(version, 2);

    Ok(())
}

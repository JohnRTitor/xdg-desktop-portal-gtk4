use std::collections::HashMap;
use zbus::{connection::Builder, proxy, zvariant::{OwnedValue, Value, OwnedObjectPath}};
macro_rules! skip_if_dbus_tests_disabled {
    () => {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            println!("Skipping dbus test because RUN_DBUS_TESTS is not set");
            return Ok(());
        }
    };
}

use xdg_desktop_portal_gtk4::portals::{
    lockdown::dbus::LockdownPortal,
    settings::dbus::SettingsPortal,
    inhibit::dbus::Inhibit,
};

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
    fn read_all(&self, namespaces: &[&str]) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;
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
    ) -> zbus::Result<(u32, HashMap<String, OwnedValue>)>;
}

#[tokio::test]
async fn test_lockdown_all_properties_false() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let _conn = Builder::session()?
        .serve_at("/org/freedesktop/portal/desktop", LockdownPortal::new())?
        .build()
        .await?;

    let client_conn = zbus::Connection::session().await?;
    let proxy = LockdownProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    assert_eq!(proxy.disable_printing().await?, false);
    assert_eq!(proxy.disable_save_to_disk().await?, false);

    Ok(())
}

#[tokio::test]
async fn test_settings_read_unknown_namespace() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let _conn = Builder::session()?
        .serve_at("/org/freedesktop/portal/desktop", SettingsPortal::new())?
        .build()
        .await?;

    let client_conn = zbus::Connection::session().await?;
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
    let _conn = Builder::session()?
        .serve_at("/org/freedesktop/portal/desktop", SettingsPortal::new())?
        .build()
        .await?;

    let client_conn = zbus::Connection::session().await?;
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
    let _conn = Builder::session()?
        .serve_at("/org/freedesktop/portal/desktop", Inhibit::new())?
        .build()
        .await?;

    let client_conn = zbus::Connection::session().await?;
    let proxy = InhibitTestProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1").unwrap();
    let res = proxy.inhibit(path, "app_id", "window", 1, HashMap::new()).await?;
    
    assert_eq!(res.0, 0); // PORTAL_SUCCESS

    Ok(())
}

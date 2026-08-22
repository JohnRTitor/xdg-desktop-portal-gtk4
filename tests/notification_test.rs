mod common;
use {
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::notification::dbus::Notification,
    zbus::{connection::Builder, proxy},
};

#[proxy(
    interface = "org.freedesktop.impl.portal.Notification",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait NotificationPortal {
    fn add_notification(
        &self,
        app_id: &str,
        id: &str,
        notification: HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<()>;

    fn remove_notification(&self, app_id: &str, id: &str) -> zbus::Result<()>;
}

#[tokio::test]
async fn test_notification_add_remove() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Notification::new(Some(client_conn.clone())).await,
        )?
        .build()
        .await?;

    let proxy = NotificationPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    // add_notification does not return an error on failure typically unless deserialization fails.
    let res = proxy
        .add_notification("app_id", "notif1", HashMap::new())
        .await;

    assert!(res.is_ok());

    let res2 = proxy.remove_notification("app_id", "notif1").await;

    assert!(res2.is_ok());

    Ok(())
}

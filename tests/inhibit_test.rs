use {
    std::collections::HashMap,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, Value},
    },
};
mod common;
use xdg_desktop_portal_gtk4::portals::inhibit::dbus::Inhibit;

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
async fn test_inhibit_returns_success() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
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

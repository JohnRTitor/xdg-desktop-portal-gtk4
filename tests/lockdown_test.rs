use zbus::{connection::Builder, proxy};
mod common;
use xdg_desktop_portal_gtk4::portals::lockdown::dbus::LockdownPortal;

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

#[tokio::test]
async fn test_lockdown_all_properties_false() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let _conn = Builder::session()?
        .serve_at("/org/freedesktop/portal/desktop", LockdownPortal::new())?
        .build()
        .await?;

    let proxy = LockdownProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    assert!(!proxy.disable_printing().await?);
    assert!(!proxy.disable_save_to_disk().await?);

    Ok(())
}

mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::app_chooser::dbus::AppChooser,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.AppChooser",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait AppChooserPortal {
    #[zbus(name = "ChooseApplication")]
    fn choose_application(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        choices: Vec<String>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;

    #[zbus(name = "UpdateChoices")]
    fn update_choices(&self, handle: OwnedObjectPath, choices: Vec<String>) -> zbus::Result<()>;
}

#[tokio::test]
async fn test_choose_application_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            AppChooser::new(&proxy_ui, sm),
        )?
        .build()
        .await?;

    let proxy = AppChooserPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/4").unwrap();
    let res = proxy
        .choose_application(
            path.clone(),
            "app_id",
            "window",
            vec!["choice".to_string()],
            HashMap::new(),
        )
        .await;

    assert!(res.is_err());

    // Also test UpdateChoices which should succeed (no-op or sends to dropped channel)
    let update_res = proxy
        .update_choices(path, vec!["new_choice".to_string()])
        .await;
    assert!(update_res.is_ok());

    Ok(())
}

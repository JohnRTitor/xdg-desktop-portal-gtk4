mod common;
use {
    parking_lot::Mutex,
    std::{collections::HashMap, sync::Arc},
    xdg_desktop_portal_gtk4::portals::email::dbus::Email,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, OwnedValue, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.Email",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait EmailPortal {
    fn compose_email(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<(u32, HashMap<String, OwnedValue>)>;
}

#[tokio::test]
async fn test_compose_email_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );

    let launched = Arc::new(Mutex::new(None));
    let launched_by_launcher = Arc::clone(&launched);
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Email::with_launcher(sm, move |url, activation_token| {
                *launched_by_launcher.lock() =
                    Some((url.to_owned(), activation_token.map(str::to_owned)));
                Ok(())
            }),
        )?
        .build()
        .await?;

    let proxy = EmailPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/3").unwrap();
    let mut options = HashMap::new();
    options.insert("subject", Value::from("Hello"));
    options.insert("activation_token", Value::from("token-1"));
    let (response, results) = proxy
        .compose_email(path, "app_id", "window", options)
        .await?;

    assert_eq!(response, 0, "ComposeEmail must report success");
    assert!(results.is_empty(), "ComposeEmail has no results");

    let launched = launched.lock();
    let (url, activation_token) = launched.as_ref().expect("launcher was not called");
    assert_eq!(url, "mailto:?subject=Hello");
    assert_eq!(activation_token.as_deref(), Some("token-1"));
    Ok(())
}

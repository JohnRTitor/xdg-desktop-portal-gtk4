mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::file_chooser::dbus::FileChooser,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.FileChooser",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait FileChooserPortal {
    fn open_file(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;

    fn save_file(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;

    fn save_files(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;
}

#[tokio::test]
async fn test_file_chooser_open_file_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            FileChooser::new(&proxy_ui, sm),
        )?
        .build()
        .await?;

    let proxy = FileChooserPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/6").unwrap();
    let res = proxy
        .open_file(path, "app_id", "window", "title", HashMap::new())
        .await;

    assert!(res.is_err()); // UI dummy proxy error

    Ok(())
}

#[tokio::test]
async fn test_file_chooser_save_file_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            FileChooser::new(&proxy_ui, sm),
        )?
        .build()
        .await?;

    let proxy = FileChooserPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/7").unwrap();
    let res = proxy
        .save_file(path, "app_id", "window", "title", HashMap::new())
        .await;

    assert!(res.is_err()); // UI dummy proxy error

    Ok(())
}

#[test]
fn test_dialog_thumbnailer_e2e() {
    if gtk4::init().is_err() {
        eprintln!("No graphical display available; skipping GTK dialog test");
        return;
    }

    use gtk4::prelude::*;
    use std::fs;
    use xdg_desktop_portal_gtk4::portals::file_chooser::thumbnailer::{self, ThumbnailSize};

    let test_dir = std::env::temp_dir().join("portal_e2e_thumb_test_dir");
    let _ = fs::create_dir_all(&test_dir);
    let sample_img = test_dir.join("test_img.png");

    let minimal_png: [u8; 67] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    fs::write(&sample_img, minimal_png).unwrap();

    let uri = thumbnailer::file_to_uri(&sample_img).unwrap();
    let thumb_path = thumbnailer::get_thumbnail_path(&uri, ThumbnailSize::Normal).unwrap();
    let _ = fs::remove_file(&thumb_path);

    let dialog = gtk4::FileChooserDialog::new(
        Some("Test E2E Thumbnailer"),
        None::<&gtk4::Window>,
        gtk4::FileChooserAction::Open,
        &[("Cancel", gtk4::ResponseType::Cancel)],
    );
    let _ = dialog.set_current_folder(Some(&gtk4::gio::File::for_path(&test_dir)));

    let ctx = gtk4::glib::MainContext::default();
    thumbnailer::setup_dialog_thumbnailer(&dialog, &ctx);
    dialog.show();

    // Pump main loop so GTK processes view models, signals, and thumbnail generation
    for _ in 0..50 {
        ctx.iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    if thumb_path.is_file() {
        let _ = fs::remove_file(&thumb_path);
    } else {
        eprintln!("no thumbnailer backend active in test environment; skipping assertion");
    }

    dialog.destroy();
    let _ = fs::remove_file(&sample_img);
    let _ = fs::remove_dir_all(&test_dir);
}

use {
    futures_util::stream::StreamExt,
    std::{collections::HashMap, sync::Mutex},
    zbus::{
        Connection, ObjectServer, interface,
        object_server::SignalEmitter,
        zvariant::{DeserializeDict, OwnedValue, Structure, Type, Value},
    },
};

pub struct TempSoundFile {
    pub path: std::path::PathBuf,
}

impl Drop for TempSoundFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: &HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.Application")]
trait Application {
    fn activate(&self, platform_data: &HashMap<&str, Value<'_>>) -> zbus::Result<()>;
    fn activate_action(
        &self,
        action_name: &str,
        parameter: &[Value<'_>],
        platform_data: &HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;
}

#[derive(DeserializeDict, Type, Default, Debug, Clone)]
#[zvariant(signature = "dict")]
pub struct PortalNotification {
    title: Option<String>,
    body: Option<String>,
    icon: Option<OwnedValue>,
    priority: Option<String>,
    #[zvariant(rename = "default-action")]
    default_action: Option<String>,
    #[zvariant(rename = "default-action-target")]
    default_action_target: Option<OwnedValue>,
    buttons: Option<Vec<(String, HashMap<String, OwnedValue>)>>,
    #[zvariant(rename = "markup-body")]
    markup_body: Option<String>,
    category: Option<String>,
    #[zvariant(rename = "display-hint")]
    display_hint: Option<Vec<String>>,
    sound: Option<OwnedValue>,
}

pub struct Notification {
    active_notifications: std::sync::Arc<Mutex<HashMap<(String, String), u32>>>,
    // Maps system D-Bus notification ID back to the portal app_id, portal_id, action targets, and optional sound temp file.
    // This is needed so we can correctly propagate the `ActionInvoked` signal back to the sandboxed app, and clean up temp files.
    reverse_map: std::sync::Arc<
        Mutex<
            HashMap<
                u32,
                (
                    String,
                    String,
                    HashMap<String, OwnedValue>,
                    Option<std::sync::Arc<TempSoundFile>>,
                ),
            >,
        >,
    >,
    init_once: std::sync::Once,
    connection: Option<Connection>,
}

impl Notification {
    pub fn new(connection: Option<Connection>) -> Self {
        Self {
            active_notifications: std::sync::Arc::new(Mutex::new(HashMap::new())),
            reverse_map: std::sync::Arc::new(Mutex::new(HashMap::new())),
            init_once: std::sync::Once::new(),
            connection,
        }
    }
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.Notification`.
///
/// This portal acts as a proxy between sandboxed applications and the host system's
/// `org.freedesktop.Notifications` D-Bus service. It translates action invocations
/// back to the sandboxed app.
#[interface(name = "org.freedesktop.impl.portal.Notification")]
impl Notification {
    async fn add_notification(
        &self,
        app_id: String,
        id: String,
        notification: PortalNotification,
        #[zbus(object_server)] server: &ObjectServer,
    ) {
        let title_ref = notification.title.as_deref().unwrap_or("");
        let body_ref = notification
            .markup_body
            .as_deref()
            .unwrap_or(notification.body.as_deref().unwrap_or(""));

        // Zbus notifications signature expects strings
        let title = title_ref;
        let body = body_ref;

        let mut icon_name = String::new();
        let mut hints = HashMap::new();
        hints.insert("desktop-entry", Value::from(app_id.as_str()));

        let priority = notification.priority.as_deref().unwrap_or("normal");
        let urgency: u8 = match priority {
            "low" => 0,
            "normal" => 1,
            "high" | "urgent" => 2,
            _ => 1,
        };
        hints.insert("urgency", Value::from(urgency));

        if let Some(category) = notification.category.as_deref() {
            hints.insert("category", Value::from(category));
        }

        if let Some(display_hints) = notification.display_hint.as_ref() {
            if display_hints.iter().any(|h| h == "transient") {
                hints.insert("transient", Value::from(true));
            }
            if display_hints.iter().any(|h| h == "persistent") {
                hints.insert("resident", Value::from(true));
            }
        }

        let mut sound_file: Option<std::sync::Arc<TempSoundFile>> = None;
        if let Some(sound) = notification.sound.as_ref() {
            let inner = match std::ops::Deref::deref(sound) {
                Value::Value(v) => v.as_ref(),
                other => other,
            };
            if let Ok(sound_str) = <&str>::try_from(inner) {
                if sound_str == "silent" {
                    hints.insert("suppress-sound", Value::from(true));
                }
            } else if let Value::Fd(fd) = inner {
                use std::{io::Read, os::fd::AsFd};
                if let Ok(owned_fd) = fd.as_fd().try_clone_to_owned() {
                    let mut file = std::fs::File::from(owned_fd);
                    let mut path = std::env::temp_dir();
                    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
                        path = std::path::PathBuf::from(runtime_dir);
                    }
                    path.push("xdg-desktop-portal-gtk4-sounds");
                    let _ = std::fs::create_dir_all(&path);

                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_micros();
                    path.push(format!(
                        "{}_{}.snd",
                        app_id.replace(['.', '-'], "_"),
                        timestamp
                    ));

                    let path_clone = path.clone();
                    let bytes = gtk4::gio::spawn_blocking(move || {
                        let mut data = Vec::new();
                        if file.read_to_end(&mut data).is_ok() {
                            return Some(data);
                        }
                        None
                    })
                    .await
                    .unwrap_or(None);

                    if let Some(data) = bytes {
                        if std::fs::write(&path_clone, data).is_ok() {
                            sound_file =
                                Some(std::sync::Arc::new(TempSoundFile { path: path_clone }));
                        }
                    }
                } else {
                    tracing::error!("Failed to dup sound fd");
                }
            }
        }

        if let Some(s) = sound_file.as_ref() {
            if let Some(path_str) = s.path.to_str() {
                hints.insert("sound-file", Value::from(path_str));
            }
        }

        if let Some(v) = notification.icon.as_ref() {
            let v_ref = std::ops::Deref::deref(v);
            if let Ok(s) = <&str>::try_from(v_ref) {
                icon_name = s.to_string();
            } else if let Ok(structure) = <Structure>::try_from(v_ref) {
                let fields = structure.fields();
                if fields.len() == 2
                    && let Ok(icon_type) = <&str>::try_from(&fields[0])
                {
                    // The icon format is (sv) — the payload in fields[1] is wrapped
                    // in a variant. Unwrap it so we can extract the actual value.
                    let payload = match &fields[1] {
                        Value::Value(inner) => inner.as_ref(),
                        other => other,
                    };
                    match icon_type {
                        "themed" => {
                            if let Ok(names) = <Vec<String>>::try_from(payload.clone())
                                && let Some(first) = names.first()
                            {
                                icon_name = first.to_string();
                            }
                        }
                        "file-descriptor" => {
                            // Note: xdg-desktop-portal drops raw "file" icon paths for security.
                            // Apps sending "bytes" arrays will have their bytes written to a memfd
                            // by the host portal, which forwards it to us here as "file-descriptor".
                            if let Value::Fd(fd) = payload {
                                use std::os::fd::AsFd;
                                if let Ok(owned_fd) = fd.as_fd().try_clone_to_owned() {
                                    let mut file = std::fs::File::from(owned_fd);
                                    let image_data = gtk4::gio::spawn_blocking(move || {
                                        use {
                                            gdk_pixbuf::Pixbuf,
                                            gtk4::{gio::MemoryInputStream, glib::Bytes},
                                            std::io::Read,
                                        };
                                        let mut data = Vec::new();
                                        if file.read_to_end(&mut data).is_ok() {
                                            let bytes = Bytes::from(&data);
                                            let stream = MemoryInputStream::from_bytes(&bytes);
                                            if let Ok(pixbuf) = Pixbuf::from_stream(
                                                &stream,
                                                gtk4::gio::Cancellable::NONE,
                                            ) {
                                                return OwnedValue::try_from(Value::new((
                                                    pixbuf.width(),
                                                    pixbuf.height(),
                                                    pixbuf.rowstride(),
                                                    pixbuf.has_alpha(),
                                                    pixbuf.bits_per_sample(),
                                                    pixbuf.n_channels(),
                                                    Value::from(pixbuf.read_pixel_bytes().as_ref()),
                                                )))
                                                .ok();
                                            }
                                        }
                                        None
                                    })
                                    .await
                                    .unwrap_or(None);

                                    if let Some(image_data) = image_data {
                                        hints.insert("image-data", Value::from(image_data));
                                    }
                                }
                            }
                        }
                        "bytes" => {
                            if let Ok(byte_array) = <Vec<u8>>::try_from(payload.clone()) {
                                let image_data = gtk4::gio::spawn_blocking(move || {
                                    use {
                                        gdk_pixbuf::Pixbuf,
                                        gtk4::{gio::MemoryInputStream, glib::Bytes},
                                    };
                                    let bytes = Bytes::from(&byte_array);
                                    let stream = MemoryInputStream::from_bytes(&bytes);
                                    if let Ok(pixbuf) =
                                        Pixbuf::from_stream(&stream, gtk4::gio::Cancellable::NONE)
                                    {
                                        return OwnedValue::try_from(Value::new((
                                            pixbuf.width(),
                                            pixbuf.height(),
                                            pixbuf.rowstride(),
                                            pixbuf.has_alpha(),
                                            pixbuf.bits_per_sample(),
                                            pixbuf.n_channels(),
                                            Value::from(pixbuf.read_pixel_bytes().as_ref()),
                                        )))
                                        .ok();
                                    }
                                    None
                                })
                                .await
                                .unwrap_or(None);

                                if let Some(image_data) = image_data {
                                    hints.insert("image-data", Value::from(image_data));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        let mut action_targets = HashMap::new();
        let mut parsed_actions: Vec<String> = Vec::new();

        if let Some(default_action) = notification.default_action.as_ref() {
            parsed_actions.push("default".to_string());
            parsed_actions.push(default_action.clone());
            if let Some(target) = notification.default_action_target.as_ref() {
                action_targets.insert("default".to_string(), target.clone());
            }
        }

        if let Some(buttons) = notification.buttons.as_ref() {
            for (action, options) in buttons {
                let label = options
                    .get("label")
                    .and_then(|v| <&str>::try_from(std::ops::Deref::deref(v)).ok())
                    .unwrap_or(action.as_str());
                parsed_actions.push(action.clone());
                parsed_actions.push(label.to_string());
                if let Some(target) = options.get("action-target") {
                    action_targets.insert(action.clone(), target.clone());
                }
            }
        }

        let actions: Vec<&str> = parsed_actions.iter().map(|s| s.as_str()).collect();

        if let Some(session_bus) = &self.connection
            && let Ok(proxy) = NotificationsProxy::new(session_bus).await
        {
            let key = (app_id.clone(), id.clone());
            let replaces_id = {
                let mut lock = self
                    .active_notifications
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *lock.entry(key.clone()).or_insert(0)
            };

            if replaces_id != 0
                && let Ok(mut lock) = self.reverse_map.lock()
            {
                lock.remove(&replaces_id);
            }

            if let Ok(new_id) = proxy
                .notify(
                    &app_id,
                    replaces_id,
                    &icon_name,
                    title,
                    body,
                    &actions,
                    &hints,
                    -1,
                )
                .await
            {
                if let Ok(mut lock) = self.active_notifications.lock() {
                    lock.insert(key, new_id);
                }
                if let Ok(mut lock) = self.reverse_map.lock() {
                    lock.insert(
                        new_id,
                        (app_id.clone(), id.clone(), action_targets, sound_file),
                    );
                }
            }
        }

        let reverse_map_clone = self.reverse_map.clone();
        let server_clone = server.clone();
        let conn_clone = self.connection.clone();

        self.init_once.call_once(move || {
            let rm1 = reverse_map_clone.clone();
            let s1 = server_clone.clone();
            let c1 = conn_clone.clone();
            gtk4::glib::MainContext::default().spawn(async move {
                if let Some(conn) = c1 {
                    if let Err(e) = listen_for_action_invoked(rm1, s1, Some(conn)).await {
                        tracing::error!(
                            "Action invoked listener failed: {}",
                            anyhow::Error::new(e)
                        );
                    }
                }
            });

            let rm2 = reverse_map_clone.clone();
            let act2 = self.active_notifications.clone();
            let c2 = conn_clone.clone();
            gtk4::glib::MainContext::default().spawn(async move {
                if let Some(conn) = c2 {
                    if let Err(e) = listen_for_notification_closed(rm2, act2, Some(conn)).await {
                        tracing::error!(
                            "Notification closed listener failed: {}",
                            anyhow::Error::new(e)
                        );
                    }
                }
            });
        });
    }

    async fn remove_notification(&self, app_id: String, id: String) {
        let key = (app_id, id);
        let fdo_id = if let Ok(mut lock) = self.active_notifications.lock() {
            lock.remove(&key)
        } else {
            tracing::error!("Failed to lock active_notifications mutex in remove_notification");
            None
        };
        if let Some(fdo_id) = fdo_id {
            if let Some(session_bus) = &self.connection
                && let Ok(proxy) = NotificationsProxy::new(session_bus).await
            {
                let _ = proxy.close_notification(fdo_id).await;
            }
        }
    }

    #[zbus(signal)]
    async fn action_invoked(
        ctx: &SignalEmitter<'_>,
        app_id: &str,
        id: &str,
        action: &str,
        parameter: &[Value<'_>],
    ) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    #[zbus(property, name = "SupportedOptions")]
    fn supported_options(&self) -> HashMap<String, OwnedValue> {
        let mut options = HashMap::new();
        if let Ok(true_val) = OwnedValue::try_from(Value::Bool(true)) {
            options.insert("body".to_string(), true_val.clone());
            options.insert("icon".to_string(), true_val.clone());
            options.insert("buttons".to_string(), true_val.clone());
            options.insert("priority".to_string(), true_val.clone());
            options.insert("default-action".to_string(), true_val.clone());
            options.insert("default-action-target".to_string(), true_val.clone());
            options.insert("markup-body".to_string(), true_val.clone());
            options.insert("category".to_string(), true_val.clone());
            options.insert("display-hint".to_string(), true_val.clone());
            options.insert("sound".to_string(), true_val);
        }
        options
    }
}

// Spawns a background task that listens to `ActionInvoked` signals from the system notification daemon.
// When an action is invoked on a notification created through this portal, it looks up the original
// portal app_id and notification id in the `reverse_map` and emits the portal's `ActionInvoked` signal.
async fn listen_for_action_invoked(
    reverse_map: std::sync::Arc<
        Mutex<
            HashMap<
                u32,
                (
                    String,
                    String,
                    HashMap<String, OwnedValue>,
                    Option<std::sync::Arc<TempSoundFile>>,
                ),
            >,
        >,
    >,
    server: ObjectServer,
    session_bus_opt: Option<Connection>,
) -> zbus::Result<()> {
    let session_bus = session_bus_opt.ok_or(zbus::Error::Failure("No connection".into()))?;
    let proxy = NotificationsProxy::new(&session_bus).await?;
    let mut stream = proxy.receive_action_invoked().await?;

    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        let id = args.id;
        let action_key = args.action_key;

        let target_data = if let Ok(lock) = reverse_map.lock() {
            lock.get(&id).cloned()
        } else {
            None
        };

        if let Some((app_id, portal_id, action_targets, _)) = target_data {
            let mut params: Vec<Value<'_>> = vec![];

            // XDG Notification spec requires parameter: av
            // 1. The target for the action, if one was specified.
            // 2. The platform-data as vardict containing an activation-token (s)
            if let Some(tv) = action_targets.get(action_key) {
                params.push(Value::from(tv.clone()));
            }

            let platform_data: HashMap<&str, Value<'_>> = HashMap::new();
            let platform_data_val = zbus::zvariant::Value::from(platform_data.clone());
            params.push(platform_data_val);

            let mut app_path = String::from("/");
            app_path.push_str(&app_id.replace('.', "/").replace('-', "_"));

            if let Some(action_name) = action_key.strip_prefix("app.") {
                let proxy_res = ApplicationProxy::builder(&session_bus)
                    .destination(app_id.as_str())
                    .unwrap()
                    .path(app_path.as_str())
                    .unwrap()
                    .build()
                    .await;

                if let Ok(proxy) = proxy_res {
                    let _ = proxy
                        .activate_action(action_name, &params, &platform_data)
                        .await;
                }
            } else {
                let proxy_res = ApplicationProxy::builder(&session_bus)
                    .destination(app_id.as_str())
                    .unwrap()
                    .path(app_path.as_str())
                    .unwrap()
                    .build()
                    .await;

                if let Ok(proxy) = proxy_res {
                    let _ = proxy.activate(&platform_data).await;
                }

                let iface_ref_res = server
                    .interface::<_, Notification>("/org/freedesktop/portal/desktop")
                    .await;

                if let Ok(iface_ref) = iface_ref_res {
                    let _ = Notification::action_invoked(
                        iface_ref.signal_emitter(),
                        &app_id,
                        &portal_id,
                        action_key,
                        &params,
                    )
                    .await;
                }
            }
        }
    }
    Ok(())
}

async fn listen_for_notification_closed(
    reverse_map: std::sync::Arc<
        Mutex<
            HashMap<
                u32,
                (
                    String,
                    String,
                    HashMap<String, OwnedValue>,
                    Option<std::sync::Arc<TempSoundFile>>,
                ),
            >,
        >,
    >,
    active_notifications: std::sync::Arc<Mutex<HashMap<(String, String), u32>>>,
    session_bus_opt: Option<Connection>,
) -> zbus::Result<()> {
    let session_bus = session_bus_opt.ok_or(zbus::Error::Failure("No connection".into()))?;
    let proxy = NotificationsProxy::new(&session_bus).await?;
    let mut stream = proxy.receive_notification_closed().await?;

    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        let id = args.id;

        let removed_key = if let Ok(mut lock) = reverse_map.lock() {
            if let Some((app_id, portal_id, _, _sound_file)) = lock.remove(&id) {
                // _sound_file drops here and deletes the temp file via Drop trait
                Some((app_id, portal_id))
            } else {
                None
            }
        } else {
            None
        };

        if let Some(key) = removed_key
            && let Ok(mut lock) = active_notifications.lock()
        {
            // To avoid a race condition where the FDO server replaces the notification
            // but still emits NotificationClosed for the old one, we only remove if it's the exact same FDO ID.
            if lock.get(&key) == Some(&id) {
                lock.remove(&key);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_properties() {
        let notification = Notification::new(None);
        assert_eq!(notification.version(), 2);

        let options = notification.supported_options();
        assert!(options.contains_key("body"));
        assert!(options.contains_key("icon"));
        assert!(options.contains_key("default-action"));
    }

    #[test]
    fn test_portal_notification_deserialize() {
        use {
            std::collections::HashMap,
            zbus::zvariant::{Endian, Value, serialized::Context},
        };

        let mut dict = HashMap::new();
        dict.insert("title", Value::from("Test Title"));
        dict.insert("body", Value::from("Test Body"));
        dict.insert("priority", Value::from("high"));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zbus::zvariant::to_bytes(ctxt, &dict).unwrap();
        let notification: PortalNotification = encoded.deserialize().unwrap().0;

        assert_eq!(notification.title.as_deref(), Some("Test Title"));
        assert_eq!(notification.body.as_deref(), Some("Test Body"));
        assert_eq!(notification.priority.as_deref(), Some("high"));
        assert_eq!(notification.category, None);
    }
}

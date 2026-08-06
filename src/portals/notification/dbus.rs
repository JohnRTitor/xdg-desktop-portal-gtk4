#![allow(clippy::too_many_arguments)]

use {
    futures_util::stream::StreamExt,
    parking_lot::Mutex,
    std::collections::HashMap,
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
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let _ = std::fs::remove_file(&path);
        });
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
#[allow(clippy::too_many_arguments)]
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

pub type NotificationTargetData = (
    String,
    String,
    HashMap<String, OwnedValue>,
    Option<std::sync::Arc<TempSoundFile>>,
);
pub type ReverseMapType =
    std::sync::Arc<Mutex<HashMap<u32, std::sync::Arc<NotificationTargetData>>>>;

/// The D-Bus interface wrapper for the Notification portal.
///
/// This struct holds shared state used to map between the sandboxed application's
/// portal notification IDs and the host system's actual notification IDs.
pub struct Notification {
    /// Maps a composite key `(app_id, portal_id)` to the system notification ID (`u32`).
    /// This is used so we can replace or remove an existing notification.
    active_notifications: std::sync::Arc<Mutex<HashMap<(String, String), u32>>>,

    /// Maps the system D-Bus notification ID (`u32`) back to the portal `app_id`, `portal_id`,
    /// action targets, and optional sound temp file.
    ///
    /// # Threading & Invariants
    ///
    /// This map is populated when a notification is added, and it is consulted
    /// asynchronously by the background tasks listening to `ActionInvoked` and
    /// `NotificationClosed` signals from the host's notification daemon.
    /// When `NotificationClosed` is received, the entry is removed, which also
    /// drops the `TempSoundFile` (deleting the temporary file).
    reverse_map: ReverseMapType,

    init_once: std::sync::Once,
    connection: Option<Connection>,
    proxy: Option<std::sync::Arc<NotificationsProxy<'static>>>,
}

impl Notification {
    pub async fn new(connection: Option<Connection>) -> Self {
        let proxy = if let Some(session_bus) = &connection {
            NotificationsProxy::builder(session_bus)
                .build()
                .await
                .ok()
                .map(std::sync::Arc::new)
        } else {
            None
        };

        Self {
            active_notifications: std::sync::Arc::new(Mutex::new(HashMap::new())),
            reverse_map: std::sync::Arc::new(Mutex::new(HashMap::new())),
            init_once: std::sync::Once::new(),
            connection,
            proxy,
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
                    let _ = tokio::fs::create_dir_all(&path).await;

                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros();
                    path.push(format!(
                        "{}_{}.snd",
                        app_id.replace(['.', '-'], "_"),
                        timestamp
                    ));

                    let bytes = tokio::task::spawn_blocking(move || {
                        let mut data = Vec::new();
                        if file.read_to_end(&mut data).is_ok() {
                            return Some(data);
                        }
                        None
                    })
                    .await
                    .unwrap_or(None);

                    if let Some(data) = bytes
                        && tokio::fs::write(&path, data).await.is_ok()
                    {
                        sound_file = Some(std::sync::Arc::new(TempSoundFile { path }));
                    }
                } else {
                    tracing::error!("Failed to dup sound fd");
                }
            }
        }

        if let Some(s) = sound_file.as_ref()
            && let Some(path_str) = s.path.to_str()
        {
            hints.insert("sound-file", Value::from(path_str));
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
                                    let image_data = tokio::task::spawn_blocking(move || {
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
                                let image_data = tokio::task::spawn_blocking(move || {
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

        if let Some(proxy) = &self.proxy {
            let key = (app_id.clone(), id.clone());
            let replaces_id = {
                let lock = self.active_notifications.lock();
                *lock.get(&key).unwrap_or(&0)
            };

            if replaces_id != 0 {
                self.reverse_map.lock().remove(&replaces_id);
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
                self.active_notifications.lock().insert(key, new_id);
                self.reverse_map.lock().insert(
                    new_id,
                    std::sync::Arc::new((app_id.clone(), id.clone(), action_targets, sound_file)),
                );
            }
        }

        let server_clone = server.clone();
        let proxy_opt = self.proxy.clone();
        let conn_opt = self.connection.clone();
        let reverse_map_clone = self.reverse_map.clone();
        let active_notifications_clone = self.active_notifications.clone();

        self.init_once.call_once(move || {
            if let Some(proxy) = proxy_opt
                && let Some(session_bus) = conn_opt
            {
                let rm = reverse_map_clone.clone();
                let server_clone2 = server_clone.clone();
                let proxy_clone1 = proxy.clone();
                let session_bus_clone = session_bus.clone();
                tokio::spawn(async move {
                    if let Err(e) = listen_for_action_invoked(
                        rm,
                        server_clone2,
                        proxy_clone1,
                        session_bus_clone,
                    )
                    .await
                    {
                        tracing::error!("Action invoked stream failed: {}", e);
                    }
                });

                let rm2 = reverse_map_clone.clone();
                let an = active_notifications_clone.clone();
                let proxy_clone2 = proxy.clone();
                tokio::spawn(async move {
                    if let Err(e) = listen_for_notification_closed(rm2, an, proxy_clone2).await {
                        tracing::error!("Notification closed stream failed: {}", e);
                    }
                });
            }
        });
    }

    async fn remove_notification(&self, app_id: String, id: String) {
        let key = (app_id, id);
        let fdo_id = self.active_notifications.lock().remove(&key);
        if let Some(fdo_id) = fdo_id
            && let Some(proxy) = &self.proxy
        {
            let _ = proxy.close_notification(fdo_id).await;
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

/// Spawns a background task that listens to `ActionInvoked` signals from the system notification daemon.
///
/// When an action is invoked on a notification created through this portal, this function looks up
/// the original portal `app_id` and notification id in the `reverse_map`. It then emits the portal's
/// `ActionInvoked` signal back to the sandboxed application over D-Bus, completing the cycle.
async fn listen_for_action_invoked(
    reverse_map: ReverseMapType,
    server: ObjectServer,
    proxy: std::sync::Arc<NotificationsProxy<'static>>,
    session_bus: Connection,
) -> zbus::Result<()> {
    let mut stream = proxy.receive_action_invoked().await?;

    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        let id = args.id;
        let action_key = args.action_key;

        let target_data = reverse_map.lock().get(&id).cloned();

        let Some((app_id, portal_id, action_targets, _)) = target_data.as_deref() else {
            continue;
        };

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

        let app_id_clone = app_id.clone();
        let action_key_clone = action_key.to_string();
        let server_clone = server.clone();
        let portal_id_clone = portal_id.clone();
        let session_bus_clone = session_bus.clone();
        let app_path_clone = app_path.clone();
        let params_clone = params.clone();
        let platform_data_clone = platform_data.clone();

        tokio::spawn(async move {
            if let Some(action_name) = action_key_clone.strip_prefix("app.") {
                // This proxy is used to talk back to the specific client application that triggered the notification
                // (e.g., when a user clicks a notification action). Because the destination address
                // (the app_id or unique connection name) changes dynamically on every single request,
                // we must instantiate it on the fly.
                let Ok(builder) = ApplicationProxy::builder(&session_bus_clone)
                    .destination(app_id_clone.as_str())
                else {
                    tracing::error!("Invalid D-Bus destination: {}", app_id_clone);
                    return;
                };
                let Ok(builder) = builder.path(app_path_clone.as_str()) else {
                    tracing::error!("Invalid D-Bus path: {}", app_path_clone);
                    return;
                };
                let proxy_res = builder
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await;

                if let Ok(proxy) = proxy_res {
                    let _ = proxy
                        .activate_action(action_name, &params_clone, &platform_data_clone)
                        .await;
                }
            } else {
                let Ok(builder) = ApplicationProxy::builder(&session_bus_clone)
                    .destination(app_id_clone.as_str())
                else {
                    tracing::error!("Invalid D-Bus destination: {}", app_id_clone);
                    return;
                };
                let Ok(builder) = builder.path(app_path_clone.as_str()) else {
                    tracing::error!("Invalid D-Bus path: {}", app_path_clone);
                    return;
                };
                let proxy_res = builder
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await;

                if let Ok(proxy) = proxy_res {
                    let _ = proxy.activate(&platform_data_clone).await;
                }

                let iface_ref_res = server_clone
                    .interface::<_, Notification>(crate::core::DBUS_PATH)
                    .await;

                if let Ok(iface_ref) = iface_ref_res {
                    let _ = Notification::action_invoked(
                        iface_ref.signal_emitter(),
                        &app_id_clone,
                        &portal_id_clone,
                        &action_key_clone,
                        &params_clone,
                    )
                    .await;
                }
            }
        });
    }
    Ok(())
}

async fn listen_for_notification_closed(
    reverse_map: ReverseMapType,
    active_notifications: std::sync::Arc<parking_lot::Mutex<HashMap<(String, String), u32>>>,
    proxy: std::sync::Arc<NotificationsProxy<'static>>,
) -> zbus::Result<()> {
    let mut stream = proxy.receive_notification_closed().await?;

    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        let id = args.id;

        let Some(target_data) = reverse_map.lock().remove(&id) else {
            continue;
        };

        let key = (target_data.0.clone(), target_data.1.clone());
        let mut lock = active_notifications.lock();
        // To avoid a race condition where the FDO server replaces the notification
        // but still emits NotificationClosed for the old one, we only remove if it's the exact same FDO ID.
        if lock.get(&key) == Some(&id) {
            lock.remove(&key);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_notification_properties() {
        let notification = Notification::new(None).await;
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

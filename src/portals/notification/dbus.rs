use {
    futures_util::stream::StreamExt,
    std::{collections::HashMap, str::FromStr, sync::Mutex},
    zbus::{
        Connection, ObjectServer, interface,
        object_server::SignalEmitter,
        zvariant::{DeserializeDict, ObjectPath, OwnedValue, Type, Value},
    },
};

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

#[derive(DeserializeDict, Type, Default, Debug)]
#[zvariant(signature = "dict")]
struct PortalNotification {
    title: Option<String>,
    body: Option<String>,
    icon: Option<OwnedValue>,
    priority: Option<String>,
    #[zvariant(rename = "default-action")]
    default_action: Option<String>,
    #[zvariant(rename = "default-action-target")]
    default_action_target: Option<OwnedValue>,
}

pub struct Notification {
    active_notifications: std::sync::Arc<Mutex<HashMap<String, u32>>>,
    reverse_map: std::sync::Arc<Mutex<HashMap<u32, (String, String, HashMap<String, OwnedValue>)>>>,
    init_once: std::sync::Once,
}

impl Notification {
    pub fn new() -> Self {
        Self {
            active_notifications: std::sync::Arc::new(Mutex::new(HashMap::new())),
            reverse_map: std::sync::Arc::new(Mutex::new(HashMap::new())),
            init_once: std::sync::Once::new(),
        }
    }

    fn get_key(app_id: &str, id: &str) -> String {
        format!("{}::{}", app_id, id)
    }
}

#[interface(name = "org.freedesktop.impl.portal.Notification")]
impl Notification {
    async fn add_notification(
        &self,
        app_id: String,
        id: String,
        notification: HashMap<String, Value<'_>>,
        #[zbus(object_server)] server: &ObjectServer,
    ) {
        let title = notification
            .get("title")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("")
            .to_string();
        let body = notification
            .get("body")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("")
            .to_string();

        let icon = if let Some(v) = notification.get("icon") {
            if let Ok(s) = <&str>::try_from(v) {
                s
            } else {
                ""
            }
        } else {
            ""
        };

        let mut action_targets = HashMap::new();
        let mut parsed_actions: Vec<String> = Vec::new();
        if let Some(default_action) = notification.get("default-action") {
            if let Ok(action) = <&str>::try_from(default_action) {
                parsed_actions.push("default".to_string());
                parsed_actions.push(action.to_string());
                if let Some(target) = notification.get("default-action-target") {
                    if let Ok(owned) = OwnedValue::try_from(target.clone()) {
                        action_targets.insert("default".to_string(), owned);
                    }
                }
            }
        }

        if let Some(buttons_val) = notification.get("buttons") {
            if let Ok(buttons) =
                <Vec<(String, HashMap<String, Value<'_>>)>>::try_from(buttons_val.clone())
            {
                for (label, options) in buttons {
                    let action = options
                        .get("action")
                        .and_then(|v| <&str>::try_from(v).ok())
                        .unwrap_or("");
                    if !action.is_empty() && !label.is_empty() {
                        parsed_actions.push(action.to_string());
                        parsed_actions.push(label.clone());
                        if let Some(target) = options.get("action-target") {
                            if let Ok(owned) = OwnedValue::try_from(target.clone()) {
                                action_targets.insert(action.to_string(), owned);
                            }
                        }
                    }
                }
            }
        }

        let actions: Vec<&str> = parsed_actions.iter().map(|s| s.as_str()).collect();

        if let Ok(system_bus) = Connection::session().await {
            if let Ok(proxy) = NotificationsProxy::new(&system_bus).await {
                let key = Self::get_key(&app_id, &id);
                let replaces_id = {
                    let mut lock = self
                        .active_notifications
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *lock.entry(key.clone()).or_insert(0)
                };

                let hints = HashMap::new(); // desktop-entry could be added

                if let Ok(new_id) = proxy
                    .notify(
                        &app_id,
                        replaces_id,
                        icon,
                        &title,
                        &body,
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
                        lock.insert(new_id, (app_id.clone(), id.clone(), action_targets));
                    }
                }
            }
        }

        let reverse_map_clone = self.reverse_map.clone();
        let server_clone = server.clone();

        self.init_once.call_once(move || {
            let rm1 = reverse_map_clone.clone();
            let s1 = server_clone.clone();
            std::thread::spawn(move || {
                zbus::block_on(async move {
                    if let Err(e) = listen_for_action_invoked(rm1, s1).await {
                        log::error!("Action invoked listener failed: {}", anyhow::Error::new(e));
                    }
                });
            });

            let rm2 = reverse_map_clone.clone();
            std::thread::spawn(move || {
                zbus::block_on(async move {
                    if let Err(e) = listen_for_notification_closed(rm2).await {
                        log::error!(
                            "Notification closed listener failed: {}",
                            anyhow::Error::new(e)
                        );
                    }
                });
            });
        });
    }

    async fn remove_notification(&self, app_id: String, id: String) {
        let key = Self::get_key(&app_id, &id);
        let fdo_id = if let Ok(mut lock) = self.active_notifications.lock() {
            lock.remove(&key)
        } else {
            log::error!("Failed to lock active_notifications mutex in remove_notification");
            None
        };
        if let Some(fdo_id) = fdo_id {
            if let Ok(system_bus) = Connection::session().await {
                if let Ok(proxy) = NotificationsProxy::new(&system_bus).await {
                    let _ = proxy.close_notification(fdo_id).await;
                }
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
        1
    }

    #[zbus(property, name = "SupportedOptions")]
    fn supported_options(&self) -> HashMap<String, OwnedValue> {
        let mut options = HashMap::new();
        if let Ok(true_val) = OwnedValue::try_from(Value::Bool(true)) {
            options.insert("body".to_string(), true_val.clone());
            options.insert("icon".to_string(), true_val.clone());
            options.insert("default-action".to_string(), true_val);
        }
        options
    }
}

async fn listen_for_action_invoked(
    reverse_map: std::sync::Arc<Mutex<HashMap<u32, (String, String, HashMap<String, OwnedValue>)>>>,
    server: ObjectServer,
) -> zbus::Result<()> {
    let session_bus = Connection::session().await?;
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

        if let Some((app_id, portal_id, action_targets)) = target_data {
            let iface_ref = server
                .interface::<_, Notification>("/org/freedesktop/portal/desktop")
                .await?;
            let mut params: Vec<Value<'_>> = vec![];
            if let Some(tv) = action_targets.get(action_key) {
                params.push(Value::from(tv.clone()));
            }
            let _ = Notification::action_invoked(
                iface_ref.signal_emitter(),
                &app_id,
                &portal_id,
                &action_key,
                &params,
            )
            .await;
        }
    }
    Ok(())
}

async fn listen_for_notification_closed(
    reverse_map: std::sync::Arc<Mutex<HashMap<u32, (String, String, HashMap<String, OwnedValue>)>>>,
) -> zbus::Result<()> {
    let session_bus = Connection::session().await?;
    let proxy = NotificationsProxy::new(&session_bus).await?;
    let mut stream = proxy.receive_notification_closed().await?;

    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        let id = args.id;
        if let Ok(mut lock) = reverse_map.lock() {
            lock.remove(&id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {super::*, zbus::zvariant::Type};

    #[test]
    fn test_get_key() {
        assert_eq!(Notification::get_key("org.app", "123"), "org.app::123");
    }

    #[test]
    fn test_get_key_empty() {
        assert_eq!(Notification::get_key("", ""), "::");
    }

    #[test]
    fn test_portal_notification_signature() {
        assert_eq!(PortalNotification::SIGNATURE, "a{sv}");
    }

    #[test]
    fn test_notification_properties() {
        let notification = Notification::new();
        assert_eq!(notification.version(), 1);

        let options = notification.supported_options();
        assert!(options.contains_key("body"));
        assert!(options.contains_key("icon"));
        assert!(options.contains_key("default-action"));
    }
}

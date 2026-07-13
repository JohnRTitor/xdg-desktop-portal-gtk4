use {
    std::{collections::HashMap, sync::Mutex},
    zbus::{
        interface,
        zvariant::{DeserializeDict, Value},
        Connection,
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
}

#[derive(DeserializeDict, Default, Debug)]
#[zvariant(signature = "dict")]
struct PortalNotification<'a> {
    title: Option<String>,
    body: Option<String>,
    icon: Option<Value<'a>>,
    priority: Option<String>,
    #[zvariant(rename = "default-action")]
    default_action: Option<String>,
    #[zvariant(rename = "default-action-target")]
    default_action_target: Option<Value<'a>>,
    // buttons is a(sa{sv}) where s is label, a{sv} is action/target etc.
}

pub struct Notification {
    active_notifications: Mutex<HashMap<String, u32>>,
}

impl Notification {
    pub fn new() -> Self {
        Self {
            active_notifications: Mutex::new(HashMap::new()),
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
    ) {
        let title = notification
            .get("title")
            .and_then(|v| v.downcast_ref::<str>())
            .unwrap_or("")
            .to_string();
        let body = notification
            .get("body")
            .and_then(|v| v.downcast_ref::<str>())
            .unwrap_or("")
            .to_string();

        let icon = if let Some(v) = notification.get("icon") {
            if let Some(s) = v.downcast_ref::<str>() {
                s
            } else {
                ""
            }
        } else {
            ""
        };

        let mut actions = Vec::new();
        if let Some(default_action) = notification.get("default-action") {
            if let Some(action) = default_action.downcast_ref::<str>() {
                actions.push("default");
                actions.push(action);
            }
        }

        // Basic implementation, button mapping can be complex

        if let Ok(system_bus) = Connection::session().await {
            if let Ok(proxy) = NotificationsProxy::new(&system_bus).await {
                let key = Self::get_key(&app_id, &id);
                let replaces_id = *self
                    .active_notifications
                    .lock()
                    .unwrap()
                    .get(&key)
                    .unwrap_or(&0);

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
                    self.active_notifications.lock().unwrap().insert(key, new_id);
                }
            }
        }
    }

    async fn remove_notification(&self, app_id: String, id: String) {
        let key = Self::get_key(&app_id, &id);
        if let Some(fdo_id) = self.active_notifications.lock().unwrap().remove(&key) {
            if let Ok(system_bus) = Connection::session().await {
                if let Ok(proxy) = NotificationsProxy::new(&system_bus).await {
                    let _ = proxy.close_notification(fdo_id).await;
                }
            }
        }
    }

    #[zbus(signal)]
    async fn action_invoked(
        ctx: &zbus::SignalContext<'_>,
        app_id: &str,
        id: &str,
        action: &str,
        parameter: &[Value<'_>],
    ) -> zbus::Result<()>;
}

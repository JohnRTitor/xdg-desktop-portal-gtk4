use {
    crate::core::session::Session,
    futures_util::stream::StreamExt,
    parking_lot::Mutex,
    std::{collections::HashMap, sync::Arc},
    tokio::sync::Notify,
    zbus::{
        Connection, ObjectServer, fdo, interface,
        message::Header,
        object_server::SignalEmitter,
        zvariant::{DeserializeDict, OwnedObjectPath, Type, Value},
    },
};

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct InhibitOptions {
    reason: Option<String>,
}

#[zbus::proxy(
    interface = "org.freedesktop.ScreenSaver",
    default_service = "org.freedesktop.ScreenSaver",
    default_path = "/org/freedesktop/ScreenSaver"
)]
trait ScreenSaver {
    fn inhibit(&self, application_name: &str, reason_for_inhibit: &str) -> zbus::Result<u32>;
    fn un_inhibit(&self, cookie: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn active_changed(&self, active: bool) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> zbus::Result<zbus::zvariant::OwnedFd>;
}

struct InhibitRequest {
    notify: Arc<Notify>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl InhibitRequest {
    async fn close(&self) {
        self.notify.notify_one();
    }
}

/// D-Bus interface wrapper for the Inhibit portal.
///
/// This struct holds the connection to the system bus (for logind) and the session bus
/// (for ScreenSaver) to place inhibition locks on behalf of sandboxed apps.
pub struct Inhibit {
    /// Tracks active monitors (session handles) requesting state change notifications.
    active_monitors: Arc<Mutex<HashMap<OwnedObjectPath, OwnedObjectPath>>>,
    init_once: std::sync::Once,
    session_manager: crate::core::session_manager::SessionManager,
    logind_proxy: Option<Arc<Login1ManagerProxy<'static>>>,
    screensaver_proxy: Option<Arc<ScreenSaverProxy<'static>>>,
}

impl Inhibit {
    pub async fn new(
        session_manager: crate::core::session_manager::SessionManager,
        system_conn: Option<Connection>,
    ) -> Self {
        let logind_proxy = if let Some(system_bus) = &system_conn {
            Login1ManagerProxy::builder(system_bus)
                .build()
                .await
                .ok()
                .map(Arc::new)
        } else {
            None
        };

        let screensaver_proxy = ScreenSaverProxy::builder(session_manager.connection())
            .build()
            .await
            .ok()
            .map(Arc::new);

        Self {
            active_monitors: Arc::new(Mutex::new(HashMap::new())),
            init_once: std::sync::Once::new(),
            session_manager,
            logind_proxy,
            screensaver_proxy,
        }
    }
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.Inhibit`.
///
/// This portal allows applications to inhibit session state changes like sleep,
/// logout, or idle (screensaver) on behalf of the user. It also allows applications
/// to monitor these states.
#[interface(name = "org.freedesktop.impl.portal.Inhibit")]
impl Inhibit {
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    async fn inhibit(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        _window: String,
        reason: u32,
        options: InhibitOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> fdo::Result<()> {
        let notify = Arc::new(Notify::new());
        let request = InhibitRequest {
            notify: notify.clone(),
        };

        if let Err(e) = server.at(handle.clone(), request).await {
            tracing::error!("Failed to export Inhibit Request {}: {}", handle, e);
            return Err(fdo::Error::Failed("Failed to export Request".into()));
        }

        let sender = header
            .sender()
            .map(|s| s.as_str().to_string())
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;

        let cancel_notify = Arc::new(Notify::new()); // We don't use this one in Inhibit itself but we must pass it

        if let Err(e) =
            self.session_manager
                .register(&app_id, &sender, handle.as_str(), cancel_notify.clone())
        {
            let _ = server.remove::<InhibitRequest, _>(handle.clone()).await;
            return Err(fdo::Error::Failed(format!("Session limit exceeded: {}", e)));
        }

        let server_clone = server.clone();
        let session_manager_clone = self.session_manager.clone();
        let app_id_clone = app_id.clone();
        let handle_clone = handle.clone();
        let logind_proxy_clone = self.logind_proxy.clone();
        let screensaver_proxy_clone = self.screensaver_proxy.clone();

        tokio::spawn(async move {
            {
                let mut screen_saver_cookie = None;
                let mut logind_fd = None;

                let mut inhibit_what = Vec::new();

                // Flags:
                // 1: Logout
                // 2: User Switch
                // 4: Suspend
                // 8: Idle
                if reason & 1 != 0 {
                    inhibit_what.push("shutdown");
                }
                if reason & 4 != 0 {
                    inhibit_what.push("sleep");
                }
                if reason & 8 != 0 {
                    inhibit_what.push("idle");
                }

                let reason_str = options.reason.as_deref().unwrap_or("Portal inhibit");

                // Try logind first for sleep/shutdown/idle.
                // logind provides a robust system-level inhibition API via file descriptors.
                if !inhibit_what.is_empty()
                    && let Some(logind_proxy) = &logind_proxy_clone
                {
                    let what_str = inhibit_what.join(":");
                    match logind_proxy
                        .inhibit(&what_str, &app_id, reason_str, "block")
                        .await
                    {
                        Ok(fd) => {
                            // The lock is held as long as the FD is kept open.
                            logind_fd = Some(fd);
                            tracing::debug!("Acquired logind inhibit lock for {}", what_str);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to inhibit via logind: {}", e);
                        }
                    }
                }

                // If Idle is requested, try ScreenSaver as a fallback or in addition.
                // Some desktop environments (like GNOME) don't fully honor logind idle locks
                // for screen blanking, so using the standard D-Bus ScreenSaver API is recommended.
                if reason & 8 != 0
                    && let Some(ss_proxy) = &screensaver_proxy_clone
                {
                    match ss_proxy.inhibit(&app_id, reason_str).await {
                        Ok(cookie) => {
                            screen_saver_cookie = Some((ss_proxy, cookie));
                            tracing::debug!("Acquired ScreenSaver inhibit cookie {}", cookie);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to inhibit via ScreenSaver: {}", e);
                        }
                    }
                }
                // Wait for the Request to be closed or the app to disconnect
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = cancel_notify.notified() => {}
                }
                tracing::debug!("Inhibit Request {} closed, releasing locks", handle);

                // Release ScreenSaver cookie
                if let Some((proxy, cookie)) = screen_saver_cookie {
                    let _ = proxy.un_inhibit(cookie).await;
                }

                // logind_fd is automatically released when dropped, which closes the FD
                // and tells logind to lift the inhibition.
                drop(logind_fd);

                // Unexport the Request
                let _ = server_clone
                    .remove::<InhibitRequest, _>(handle_clone.clone())
                    .await;
                session_manager_clone.unregister(&app_id_clone, &sender, handle_clone.as_str());
            }
        });

        Ok(())
    }

    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    async fn create_monitor(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _window: String,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> fdo::Result<u32> {
        let notify = Arc::new(Notify::new());
        let cancel_notify = Arc::new(Notify::new());

        let sender = match header.sender() {
            Some(s) => s.as_str().to_string(),
            None => return Ok(2),
        };

        if let Err(e) = self.session_manager.register(
            &app_id,
            &sender,
            session_handle.as_str(),
            cancel_notify.clone(),
        ) {
            tracing::warn!("Session limit exceeded for monitor: {}", e);
            return Ok(2);
        }

        let session = Session::new(session_handle.as_str().to_string(), Some(notify.clone()));
        if let Err(e) = server.at(session_handle.clone(), session).await {
            tracing::error!("Failed to export monitor session: {}", e);
            self.session_manager
                .unregister(&app_id, &sender, session_handle.as_str());
            return Ok(2); // Returning 2 as general error for create_monitor according to xdp-gtk
        }

        self.active_monitors
            .lock()
            .insert(handle.clone(), session_handle.clone());

        let handle_clone = handle.clone();
        let session_handle_clone = session_handle.clone();
        let monitors_clone = self.active_monitors.clone();
        let session_manager_clone = self.session_manager.clone();
        let app_id_clone = app_id.clone();
        let sender_clone = sender.clone();
        let server_clone = server.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = notify.notified() => {}
                _ = cancel_notify.notified() => {}
            }

            monitors_clone.lock().remove(&handle_clone);
            session_manager_clone.unregister(
                &app_id_clone,
                &sender_clone,
                session_handle_clone.as_str(),
            );

            // Remove the exported Session object
            let _ = server_clone
                .remove::<Session, _>(&session_handle_clone)
                .await;
        });

        let ss_proxy_opt = self.screensaver_proxy.clone();
        let active_monitors_clone2 = self.active_monitors.clone();
        let server_clone = server.clone();

        self.init_once.call_once(move || {
            let active_monitors_clone = active_monitors_clone2;

            tokio::spawn(async move {
                let Some(proxy) = ss_proxy_opt else {
                    return;
                };
                let Ok(mut stream) = proxy.receive_active_changed().await else {
                    return;
                };

                while let Some(signal) = stream.next().await {
                    let Ok(args) = signal.args() else {
                        continue;
                    };
                    let active = args.active;
                    let Ok(iface_ref) = server_clone
                        .interface::<_, Inhibit>(crate::core::DBUS_PATH)
                        .await
                    else {
                        continue;
                    };

                    let mut state: HashMap<&str, Value<'_>> = HashMap::new();
                    state.insert("screensaver-active", Value::Bool(active));

                    let sessions: Vec<OwnedObjectPath> =
                        active_monitors_clone.lock().values().cloned().collect();

                    for session_h in sessions {
                        let _ = Self::state_changed(iface_ref.signal_emitter(), &session_h, &state)
                            .await;
                    }
                }
            });
        });

        Ok(0) // 0 == success
    }

    async fn query_end_response(&self, _session_handle: OwnedObjectPath) {
        tracing::debug!("query_end_response called");
    }

    #[zbus(signal)]
    async fn state_changed(
        ctx: &SignalEmitter<'_>,
        session_handle: &zbus::zvariant::ObjectPath<'_>,
        state: &HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::collections::HashMap,
        zbus::zvariant::{self, Endian, Value, serialized::Context},
    };

    #[test]
    fn test_inhibit_options_deserialize() {
        let mut dict = HashMap::new();
        dict.insert("reason", Value::from("Playing a movie"));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: InhibitOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.reason.as_deref(), Some("Playing a movie"));
    }

    #[test]
    fn test_inhibit_options_empty() {
        let dict: HashMap<&str, Value> = HashMap::new();
        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: InhibitOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.reason, None);
    }
}

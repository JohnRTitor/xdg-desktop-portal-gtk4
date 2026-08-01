use {
    crate::core::session::Session,
    futures_util::stream::StreamExt,
    std::{collections::HashMap, sync::Mutex},
    zbus::{
        Connection, ObjectServer, interface,
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
    send: async_channel::Sender<()>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl InhibitRequest {
    async fn close(&self) {
        let _ = self.send.send(()).await;
    }
}

pub struct Inhibit {
    active_monitors: std::sync::Arc<Mutex<HashMap<OwnedObjectPath, OwnedObjectPath>>>,
    init_once: std::sync::Once,
    session_manager: crate::core::session_manager::SessionManager,
    system_conn: Option<Connection>,
}

impl Inhibit {
    pub fn new(
        session_manager: crate::core::session_manager::SessionManager,
        system_conn: Option<Connection>,
    ) -> Self {
        Self {
            active_monitors: std::sync::Arc::new(Mutex::new(HashMap::new())),
            init_once: std::sync::Once::new(),
            session_manager,
            system_conn,
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
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        _window: String,
        reason: u32,
        options: InhibitOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<()> {
        let (send, recv) = async_channel::bounded(1);
        let request = InhibitRequest { send: send.clone() };

        if let Err(e) = server.at(handle.clone(), request).await {
            tracing::error!("Failed to export Inhibit Request {}: {}", handle, e);
            return Err(zbus::fdo::Error::Failed("Failed to export Request".into()));
        }

        let sender = header
            .sender()
            .map(|s| s.as_str().to_string())
            .ok_or_else(|| zbus::fdo::Error::Failed("Missing sender".into()))?;

        if let Err(e) =
            self.session_manager
                .register(&app_id, &sender, handle.as_str(), send.clone())
        {
            let _ = server.remove::<InhibitRequest, _>(handle.clone()).await;
            return Err(zbus::fdo::Error::Failed(format!(
                "Session limit exceeded: {}",
                e
            )));
        }

        let server_clone = server.clone();
        let session_manager_clone = self.session_manager.clone();
        let app_id_clone = app_id.clone();
        let handle_clone = handle.clone();
        let system_conn_clone = self.system_conn.clone();
        let session_conn_clone = self.session_manager.connection().clone();

        gtk4::glib::MainContext::default().spawn(async move {
            {
                let session_bus = session_conn_clone;
                let mut screen_saver_cookie = None;
                let mut logind_fd = None;

                let system_bus_opt = system_conn_clone;

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
                    && let Some(system_bus) = &system_bus_opt
                    && let Ok(logind_proxy) = Login1ManagerProxy::new(system_bus).await
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
                    && let Ok(ss_proxy) = ScreenSaverProxy::new(&session_bus).await
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

                // Wait for the Request to be closed
                let _ = recv.recv().await;

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
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _window: String,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<u32> {
        let (tx, rx) = async_channel::bounded(1);
        let (cancel_tx, cancel_rx) = async_channel::bounded(1);

        let sender = match header.sender() {
            Some(s) => s.as_str().to_string(),
            None => return Ok(2),
        };

        if let Err(e) =
            self.session_manager
                .register(&app_id, &sender, session_handle.as_str(), cancel_tx)
        {
            tracing::warn!("Session limit exceeded for monitor: {}", e);
            return Ok(2);
        }

        let session = Session::new(session_handle.as_str().to_string(), Some(tx));
        if let Err(e) = server.at(session_handle.clone(), session).await {
            tracing::error!("Failed to export monitor session: {}", e);
            self.session_manager
                .unregister(&app_id, &sender, session_handle.as_str());
            return Ok(2); // Returning 2 as general error for create_monitor according to xdp-gtk
        }

        if let Ok(mut lock) = self.active_monitors.lock() {
            lock.insert(handle.clone(), session_handle.clone());
        }

        let handle_clone = handle.clone();
        let session_handle_clone = session_handle.clone();
        let monitors_clone = self.active_monitors.clone();
        let session_manager_clone = self.session_manager.clone();
        let app_id_clone = app_id.clone();
        let sender_clone = sender.clone();
        let server_clone = server.clone();

        gtk4::glib::MainContext::default().spawn(async move {
            futures_util::future::select(
                std::pin::pin!(rx.recv()),
                std::pin::pin!(cancel_rx.recv()),
            )
            .await;

            if let Ok(mut lock) = monitors_clone.lock() {
                lock.remove(&handle_clone);
            }
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

        let server_clone = server.clone();
        let monitors_clone = self.active_monitors.clone();
        let session_manager_clone2 = self.session_manager.clone();

        self.init_once.call_once(move || {
            gtk4::glib::MainContext::default().spawn(async move {
                {
                    let session_bus = session_manager_clone2.connection().clone();
                    if let Ok(proxy) = ScreenSaverProxy::new(&session_bus).await
                        && let Ok(mut stream) = proxy.receive_active_changed().await
                    {
                        while let Some(signal) = stream.next().await {
                            if let Ok(args) = signal.args() {
                                let active = args.active;
                                if let Ok(iface_ref) = server_clone
                                    .interface::<_, Inhibit>("/org/freedesktop/portal/desktop")
                                    .await
                                {
                                    let mut state = HashMap::new();
                                    state.insert(
                                        "screensaver-active".to_string(),
                                        Value::Bool(active),
                                    );

                                    let sessions: Vec<OwnedObjectPath> =
                                        if let Ok(lock) = monitors_clone.lock() {
                                            lock.values().cloned().collect()
                                        } else {
                                            Vec::new()
                                        };

                                    for session_h in sessions {
                                        let _ = Self::state_changed(
                                            iface_ref.signal_emitter(),
                                            session_h,
                                            state.clone(),
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
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
        session_handle: OwnedObjectPath,
        state: HashMap<String, Value<'_>>,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::collections::HashMap,
        zbus::zvariant::{Endian, Value, serialized::Context},
    };

    #[test]
    fn test_inhibit_options_deserialize() {
        let mut dict = HashMap::new();
        dict.insert("reason", Value::from("Playing a movie"));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zbus::zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: InhibitOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.reason.as_deref(), Some("Playing a movie"));
    }

    #[test]
    fn test_inhibit_options_empty() {
        let dict: HashMap<&str, Value> = HashMap::new();
        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zbus::zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: InhibitOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.reason, None);
    }
}

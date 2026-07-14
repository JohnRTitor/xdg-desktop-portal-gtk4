use {
    crate::core::session::Session,
    futures_util::stream::StreamExt,
    std::collections::HashMap,
    std::str::FromStr,
    std::sync::Mutex,
    zbus::{
        Connection, ObjectServer, interface,
        object_server::SignalEmitter,
        zvariant::{DeserializeDict, ObjectPath, OwnedObjectPath, Type, Value},
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
    active_monitors: std::sync::Arc<Mutex<HashMap<String, OwnedObjectPath>>>,
    init_once: std::sync::Once,
}

impl Inhibit {
    pub fn new() -> Self {
        Self {
            active_monitors: std::sync::Arc::new(Mutex::new(HashMap::new())),
            init_once: std::sync::Once::new(),
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Inhibit")]
impl Inhibit {
    async fn inhibit(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        _window: String,
        reason: u32,
        options: InhibitOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<()> {
        let (send, recv) = async_channel::bounded(1);
        let request = InhibitRequest { send };

        if let Err(e) = server.at(handle.clone(), request).await {
            log::error!("Failed to export Inhibit Request {}: {}", handle, e);
            return Err(zbus::fdo::Error::Failed("Failed to export Request".into()));
        }

        let server_clone = server.clone();

        std::thread::spawn(move || {
            zbus::block_on(async move {
                let session_bus_res = Connection::session().await;
                let mut screen_saver_cookie = None;
                let mut logind_fd = None;

                let system_bus_res = Connection::system().await;

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

                // Try logind first for sleep/shutdown/idle
                if !inhibit_what.is_empty() {
                    if let Ok(system_bus) = &system_bus_res {
                        if let Ok(logind_proxy) = Login1ManagerProxy::new(system_bus).await {
                            let what_str = inhibit_what.join(":");
                            match logind_proxy
                                .inhibit(&what_str, &app_id, reason_str, "block")
                                .await
                            {
                                Ok(fd) => {
                                    logind_fd = Some(fd);
                                    log::debug!("Acquired logind inhibit lock for {}", what_str);
                                }
                                Err(e) => {
                                    log::warn!("Failed to inhibit via logind: {}", e);
                                }
                            }
                        }
                    }
                }

                // If Idle is requested, try ScreenSaver as a fallback or in addition (since logind idle is sometimes ignored by DEs)
                if reason & 8 != 0 {
                    if let Ok(session_bus) = &session_bus_res {
                        if let Ok(ss_proxy) = ScreenSaverProxy::new(session_bus).await {
                            match ss_proxy.inhibit(&app_id, reason_str).await {
                                Ok(cookie) => {
                                    screen_saver_cookie = Some((ss_proxy, cookie));
                                    log::debug!("Acquired ScreenSaver inhibit cookie {}", cookie);
                                }
                                Err(e) => {
                                    log::warn!("Failed to inhibit via ScreenSaver: {}", e);
                                }
                            }
                        }
                    }
                }

                // Wait for the Request to be closed
                let _ = recv.recv().await;

                log::debug!("Inhibit Request {} closed, releasing locks", handle);

                // Release ScreenSaver cookie
                if let Some((proxy, cookie)) = screen_saver_cookie {
                    let _ = proxy.un_inhibit(cookie).await;
                }

                // logind_fd is automatically released when dropped
                drop(logind_fd);

                // Unexport the Request
                let _ = server_clone.remove::<InhibitRequest, _>(handle).await;
            });
        });

        Ok(())
    }

    async fn create_monitor(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _window: String,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<u32> {
        let session = Session::new(session_handle.as_str().to_string());
        if let Err(e) = server.at(session_handle.clone(), session).await {
            log::error!("Failed to export monitor session: {}", e);
            return Ok(2); // Returning 2 as general error for create_monitor according to xdp-gtk
        }

        let handle_str = handle.as_str().to_string();
        if let Ok(mut lock) = self.active_monitors.lock() {
            lock.insert(handle_str, session_handle.clone());
        }

        let server_clone = server.clone();
        let monitors_clone = self.active_monitors.clone();

        self.init_once.call_once(move || {
            std::thread::spawn(move || {
                zbus::block_on(async move {
                    if let Ok(session_bus) = Connection::session().await {
                        if let Ok(proxy) = ScreenSaverProxy::new(&session_bus).await {
                            if let Ok(mut stream) = proxy.receive_active_changed().await {
                                while let Some(signal) = stream.next().await {
                                    if let Ok(args) = signal.args() {
                                        let active = args.active;
                                        if let Ok(iface_ref) = server_clone
                                            .interface::<_, Inhibit>(
                                                "/org/freedesktop/portal/desktop",
                                            )
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
                    }
                });
            });
        });

        Ok(0) // 0 == success
    }

    async fn query_end_response(&self, _session_handle: OwnedObjectPath) {
        log::debug!("query_end_response called");
    }

    #[zbus(signal)]
    async fn state_changed(
        ctx: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        state: HashMap<String, Value<'_>>,
    ) -> zbus::Result<()>;
}

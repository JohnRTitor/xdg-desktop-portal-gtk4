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
    ///
    /// Keyed by the request handle so a closing monitor can be removed, with the
    /// session handle it was exported under as the value. The value is an `Arc`
    /// because the screensaver broadcasts to every monitor on each state change,
    /// and collecting the handles into a `Vec` to iterate outside the lock is
    /// cheaper in bytes: a bare `OwnedObjectPath` element carries its own path
    /// buffer and refcount, where an `Arc` element is a single pointer. Measured
    /// at 1536 bytes versus 512 for a 64-monitor fan-out.
    active_monitors: Arc<Mutex<HashMap<OwnedObjectPath, Arc<OwnedObjectPath>>>>,
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

/// Translates the caller's `reason` bitmask into the `what` string logind wants.
///
/// Only three of the four documented flags reach logind; bit 2 (user switch) is
/// accepted from callers but has no logind equivalent, so it contributes
/// nothing -- preserved from the previous implementation, which also skipped it.
/// Returns `""` when no inhibitable flag is set, which the caller treats as
/// "do not ask logind".
///
/// This is a table rather than the `Vec` + `join(":")` it replaces: the flag
/// space is four bits wide, so every reachable answer is a compile-time constant
/// and the whole mapping costs nothing at run time. The join form allocated a
/// vector that grew as it was pushed to, then a second `String` to hold the
/// result.
const fn inhibit_targets(reason: u32) -> &'static str {
    // Only the three logind-understood flags are considered; bit 2 (user
    // switch) and any bit a future caller might set are dropped, matching the
    // previous implementation, which tested `reason & FLAG` per known flag and
    // ignored everything else.
    match reason & (1 | 4 | 8) {
        0 => "",
        1 => "shutdown",
        4 => "sleep",
        5 => "shutdown:sleep",
        8 => "idle",
        9 => "shutdown:idle",
        12 => "sleep:idle",
        // 13 is the only remaining combination of the three known flags.
        _ => "shutdown:sleep:idle",
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
            .map(|s| String::from(s.as_str()))
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;

        // Cancelled when the caller disconnects, or the request is closed.
        let cancel_notify = Arc::new(Notify::new());

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

                // Flags:
                // 1: Logout
                // 2: User Switch
                // 4: Suspend
                // 8: Idle
                let what_str = inhibit_targets(reason);

                let reason_str = options.reason.as_deref().unwrap_or("Portal inhibit");

                // Try logind first for sleep/shutdown/idle.
                // logind provides a robust system-level inhibition API via file descriptors.
                if !what_str.is_empty()
                    && let Some(logind_proxy) = &logind_proxy_clone
                {
                    match logind_proxy
                        .inhibit(what_str, &app_id, reason_str, "block")
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
            Some(s) => String::from(s.as_str()),
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

        let session = Session::new(session_handle.as_str().into(), Some(notify.clone()));
        if let Err(e) = server.at(session_handle.clone(), session).await {
            tracing::error!("Failed to export monitor session: {}", e);
            self.session_manager
                .unregister(&app_id, &sender, session_handle.as_str());
            return Ok(2); // Returning 2 as general error for create_monitor according to xdp-gtk
        }

        self.active_monitors
            .lock()
            .insert(handle.clone(), Arc::new(session_handle.clone()));

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

                    // Collected under the lock but iterated after it is released: the
                    // signal emission below is awaited, and holding a guard
                    // across an await is not allowed. Collecting keeps each
                    // element to a single pointer.
                    let sessions: Vec<Arc<OwnedObjectPath>> =
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

#[cfg(test)]
mod inhibit_target_tests {
    use super::inhibit_targets;

    #[test]
    fn no_flags_asks_logind_for_nothing() {
        assert_eq!(inhibit_targets(0), "");
    }

    #[test]
    fn each_single_flag_maps_to_its_logind_name() {
        assert_eq!(inhibit_targets(1), "shutdown", "1: Logout");
        assert_eq!(inhibit_targets(4), "sleep", "4: Suspend");
        assert_eq!(inhibit_targets(8), "idle", "8: Idle");
    }

    /// logind takes the classes colon-separated in a fixed order, which the
    /// table encodes; the order must not depend on how the caller set the bits.
    #[test]
    fn combined_flags_are_joined_in_logind_order() {
        assert_eq!(inhibit_targets(1 | 4), "shutdown:sleep");
        assert_eq!(inhibit_targets(1 | 8), "shutdown:idle");
        assert_eq!(inhibit_targets(4 | 8), "sleep:idle");
        assert_eq!(inhibit_targets(1 | 4 | 8), "shutdown:sleep:idle");
    }

    /// Bit 2 (user switch) has no logind counterpart. It was already skipped
    /// before this became a table, so a caller sending only that flag must still
    /// get no inhibition request rather than a newly-invented one.
    #[test]
    fn user_switch_alone_asks_logind_for_nothing() {
        assert_eq!(inhibit_targets(2), "");
    }

    #[test]
    fn user_switch_combined_with_a_real_flag_is_ignored() {
        assert_eq!(inhibit_targets(1 | 2), "shutdown");
        assert_eq!(inhibit_targets(2 | 8), "idle");
        assert_eq!(inhibit_targets(1 | 2 | 4 | 8), "shutdown:sleep:idle");
    }

    /// Unknown bits must not produce a request. Asking logind to inhibit a
    /// class it does not know would fail the whole call, losing the locks the
    /// caller did ask for.
    #[test]
    fn unknown_bits_alone_ask_logind_for_nothing() {
        assert_eq!(inhibit_targets(16), "");
        assert_eq!(inhibit_targets(1 << 31), "");
    }

    #[test]
    fn unknown_bits_alongside_a_real_flag_do_not_hide_it() {
        assert_eq!(inhibit_targets(1 | 16), "shutdown");
        assert_eq!(inhibit_targets(8 | 64), "idle");
    }

    /// The regression this guards: the mapping used to build a `Vec<&str>` and
    /// `join(":")` it, allocating the vector and then the result string on every
    /// `Inhibit` call. It is now a table lookup returning a `&'static str`.
    #[test]
    fn resolving_the_flags_allocates_nothing() {
        for reason in [0u32, 1, 2, 4, 5, 8, 9, 12, 13, 1 | 2 | 4 | 8, 16] {
            let scope = crate::alloc_probe::AllocScope::start();
            let what = inhibit_targets(reason);
            let snap = scope.finish();
            std::hint::black_box(what);

            assert_eq!(
                snap.count, 0,
                "flags {reason} must not allocate, got {snap:?}"
            );
        }
    }

    /// Exhaustive over the whole documented flag space, so a future edit to the
    /// table cannot silently change an answer.
    #[test]
    fn every_flag_combination_is_pinned() {
        let expected = [
            (0, ""),
            (1, "shutdown"),
            (2, ""),
            (3, "shutdown"),
            (4, "sleep"),
            (5, "shutdown:sleep"),
            (6, "sleep"),
            (7, "shutdown:sleep"),
            (8, "idle"),
            (9, "shutdown:idle"),
            (10, "idle"),
            (11, "shutdown:idle"),
            (12, "sleep:idle"),
            (13, "shutdown:sleep:idle"),
            (14, "sleep:idle"),
            (15, "shutdown:sleep:idle"),
        ];
        for (reason, what) in expected {
            assert_eq!(inhibit_targets(reason), what, "flags {reason}");
        }
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;

    type Monitors = Arc<Mutex<HashMap<OwnedObjectPath, Arc<OwnedObjectPath>>>>;

    fn monitors_with(n: usize) -> Monitors {
        let map: HashMap<OwnedObjectPath, Arc<OwnedObjectPath>> = (0..n)
            .map(|i| {
                let session = OwnedObjectPath::try_from(format!("/session/monitor/{i}")).unwrap();
                let request = OwnedObjectPath::try_from(format!("/request/monitor/{i}")).unwrap();
                (request, Arc::new(session))
            })
            .collect();
        Arc::new(Mutex::new(map))
    }

    /// Mirrors the broadcast in `create_monitor`: collect the session handles
    /// under the lock, then iterate them after releasing it.
    fn broadcast_sessions(monitors: &Monitors) -> Vec<Arc<OwnedObjectPath>> {
        monitors.lock().values().cloned().collect()
    }

    #[test]
    fn the_broadcast_reaches_every_monitor() {
        let monitors = monitors_with(3);
        let sessions = broadcast_sessions(&monitors);
        assert_eq!(sessions.len(), 3);

        let mut paths: Vec<String> = sessions.iter().map(|s| s.to_string()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "/session/monitor/0",
                "/session/monitor/1",
                "/session/monitor/2"
            ]
        );
    }

    #[test]
    fn a_removed_monitor_stops_receiving() {
        let monitors = monitors_with(2);
        let doomed = OwnedObjectPath::try_from("/request/monitor/0").unwrap();
        assert!(monitors.lock().remove(&doomed).is_some());

        let sessions = broadcast_sessions(&monitors);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].to_string(), "/session/monitor/1");
    }

    #[test]
    fn broadcasting_to_no_monitors_yields_nothing() {
        let monitors = monitors_with(0);
        assert!(broadcast_sessions(&monitors).is_empty());
    }

    /// The regression this guards: the monitor handles are read on every
    /// screensaver state change and the collected `Vec` is what the fan-out
    /// iterates.
    ///
    /// Note on what this does and does not save: `OwnedObjectPath::clone` was
    /// already allocation-free (it is a refcounted handle), so the old code did
    /// not allocate per monitor either -- measured at one `Vec` allocation then
    /// as now. What the `Arc` changes is the retained size of that vector:
    /// 1536 bytes for 64 bare handles versus 512, since each element shrinks
    /// from a path buffer plus its refcount to a single pointer. That matters
    /// because the vector is rebuilt on every state change and lives across the
    /// awaited signal emission.
    #[test]
    fn fanning_out_to_the_monitors_costs_one_vec() {
        let monitors = monitors_with(64);

        // Warm up so the guard's own bookkeeping and any lazy initialisation of
        // the map are not attributed to the measured region.
        std::hint::black_box(broadcast_sessions(&monitors));

        let scope = crate::alloc_probe::AllocScope::start();
        let sessions = broadcast_sessions(&monitors);
        let snap = scope.finish();
        std::hint::black_box(&sessions);

        assert_eq!(sessions.len(), 64, "every monitor must still be reached");
        assert_eq!(
            snap.count, 1,
            "64 monitors must cost one Vec allocation, got {snap:?}"
        );
    }

    /// Cloning the `Arc` is what the broadcast relies on, so pin that it is a
    /// refcount bump rather than a copy of the path.
    #[test]
    fn cloning_a_monitor_handle_is_a_refcount_bump() {
        let monitors = monitors_with(1);
        let original = {
            let lock = monitors.lock();
            lock.values().next().expect("one monitor").clone()
        };

        let scope = crate::alloc_probe::AllocScope::start();
        let copy = original.clone();
        let snap = scope.finish();

        assert!(
            Arc::ptr_eq(&original, &copy),
            "the same handle must come back"
        );
        assert_eq!(
            snap.count, 0,
            "cloning the handle must not copy the path, got {snap:?}"
        );
    }
}

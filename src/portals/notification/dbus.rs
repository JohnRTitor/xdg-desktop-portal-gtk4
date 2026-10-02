#![allow(clippy::too_many_arguments)]

use {
    futures_util::stream::StreamExt,
    gtk4::gio::Cancellable,
    parking_lot::Mutex,
    std::{collections::HashMap, sync::Arc},
    tokio::task::spawn_blocking,
    zbus::{
        Connection, ObjectServer, interface,
        object_server::SignalEmitter,
        zvariant::{DeserializeDict, OwnedValue, Structure, Type, Value},
    },
};

/// Subdirectory of `$XDG_RUNTIME_DIR` that staged notification sounds live in.
const TEMP_SOUND_SUBDIR: &str = "xdg-desktop-portal-gtk4-sounds";

pub struct TempSoundFile {
    pub path: std::path::PathBuf,
}

impl Drop for TempSoundFile {
    fn drop(&mut self) {
        let path = self.path.clone();
        spawn_blocking(move || {
            let _ = std::fs::remove_file(&path);
        });
    }
}

/// The directory staged notification sounds are written into.
///
/// Returns `None` when `$XDG_RUNTIME_DIR` is unset or empty. Unlike
/// `std::env::temp_dir()`, that directory is guaranteed to be private to the
/// user (mode `0700`), so it is the only place this daemon may create files on
/// an unprivileged caller's behalf. `std::env::temp_dir()` is world-writable and
/// shared with every other process on the machine.
fn temp_sound_dir() -> Option<std::path::PathBuf> {
    sound_dir_under(std::env::var_os("XDG_RUNTIME_DIR").as_deref())
}

/// [`temp_sound_dir`] with the runtime directory supplied explicitly.
///
/// Split out so the rule can be tested without mutating process-global
/// environment state, which is `unsafe` on this Rust edition.
fn sound_dir_under(runtime_dir: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    let runtime_dir = runtime_dir?;
    if runtime_dir.is_empty() {
        return None;
    }
    let mut path = std::path::PathBuf::from(runtime_dir);
    path.push(TEMP_SOUND_SUBDIR);
    Some(path)
}

/// Create `dir` with mode `0700`, tolerating an existing directory.
///
/// A plain `create_dir_all` applies the process umask to a `0777` request, which
/// under a permissive umask yields a group- or world-accessible directory. The
/// mode is requested explicitly so the result is `0700` regardless of umask.
/// An existing path is accepted: `EEXIST` on a directory is the normal case
/// after the first sound in a session, and the parent is already private.
fn ensure_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    match rustix::fs::mkdir(dir, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::EXIST) => Ok(()),
        Err(e) => Err(std::io::Error::from_raw_os_error(e.raw_os_error())),
    }
}

/// Write `data` to `path`, failing if `path` already exists.
///
/// `O_EXCL` is the whole point: it makes `open` fail with `EEXIST` when the
/// target already exists, *including when it is a symlink*, so the caller can
/// never be tricked into writing through a link planted by another process. The
/// file is created `0600` because the sound is derived from a sandboxed
/// caller's data and is passed to the notification daemon.
async fn write_exclusive(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or(std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let bytes = data.to_vec();
    let path_buf = path.to_path_buf();

    // Both the `open` and the write are syscalls. Keep them off the runtime
    // thread so a stalled filesystem cannot stall the single-threaded D-Bus
    // reactor.
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        ensure_private_dir(&parent)?;

        let file = rustix::fs::open(
            &path_buf,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOCTTY,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;

        std::fs::File::from(file).write_all(&bytes)
    })
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))?
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

/// Identifies one notification as the calling app knows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NotifKey {
    pub app_id: Arc<str>,
    pub portal_id: Arc<str>,
}

impl NotifKey {
    pub fn new(app_id: &str, portal_id: &str) -> Self {
        Self {
            app_id: Arc::from(app_id),
            portal_id: Arc::from(portal_id),
        }
    }
}

/// The per-notification state the signal listeners need, keyed by the host's
/// notification ID.
pub struct NotificationTarget {
    /// Back-reference to the same `NotifKey` used as the forward map's key.
    pub key: Arc<NotifKey>,
    pub action_targets: HashMap<String, OwnedValue>,
    pub sound_file: Option<Arc<TempSoundFile>>,
}

/// Maps a calling app's view of a notification to the host's notification ID.
///
/// Keyed as `app_id -> portal_id -> id` rather than by a single composite key so
/// a lookup can borrow both halves of the key. `Arc<NotifKey>: Borrow<NotifKey>`
/// only helps for an owned key, and building one to search with would allocate;
/// two nested maps let `RemoveNotification` find its entry in constant time with
/// no allocation. That matters because this map is neither rate-limited nor
/// evicted, so an unprivileged caller can grow it at will and a linear scan here
/// would be quadratic under the lock.
pub type ActiveNotifications = Arc<Mutex<HashMap<Arc<str>, HashMap<Arc<str>, u32>>>>;
/// Maps the host's notification ID back to the portal-side state.
pub type ReverseMapType = Arc<Mutex<HashMap<u32, Arc<NotificationTarget>>>>;

/// Looks up the host notification ID for `(app_id, portal_id)` without allocating.
fn active_id(active: &ActiveNotifications, app_id: &str, portal_id: &str) -> Option<u32> {
    active
        .lock()
        .get(app_id)
        .and_then(|by_portal| by_portal.get(portal_id))
        .copied()
}

/// Removes the forward mapping for `(app_id, portal_id)`, returning the host
/// notification ID it pointed at.
///
/// See [`ActiveNotifications`] for why the map is nested: this must stay a
/// constant-time, allocation-free lookup.
fn remove_active_notification(
    active: &ActiveNotifications,
    app_id: &str,
    portal_id: &str,
) -> Option<u32> {
    let mut lock = active.lock();
    let removed = lock.get_mut(app_id)?.remove(portal_id);
    if removed.is_some() && lock[app_id].is_empty() {
        lock.remove(app_id);
    }
    removed
}

/// Registers a notification in both maps, returning the shared key.
///
/// The same [`NotifKey`] must be used for the forward map's key and the reverse
/// map's back-reference; building it twice would reintroduce the duplicate
/// storage this type exists to remove. Factored out so a test can assert the
/// sharing rather than having to trust the call site.
fn register_notification(
    active: &ActiveNotifications,
    reverse: &ReverseMapType,
    app_id: &str,
    portal_id: &str,
    fdo_id: u32,
    action_targets: HashMap<String, OwnedValue>,
    sound_file: Option<Arc<TempSoundFile>>,
) -> Arc<NotifKey> {
    let key = Arc::new(NotifKey::new(app_id, portal_id));
    active
        .lock()
        .entry(key.app_id.clone())
        .or_default()
        .insert(key.portal_id.clone(), fdo_id);
    reverse.lock().insert(
        fdo_id,
        Arc::new(NotificationTarget {
            key: key.clone(),
            action_targets,
            sound_file,
        }),
    );
    key
}

/// Retires a notification the host daemon reported closed.
///
/// Returns whether the forward entry was also removed.
///
/// The forward entry is only removed when it still maps to *this* host ID. The
/// host can replace a notification and then emit `NotificationClosed` for the
/// old one; removing unconditionally would drop the live replacement's mapping.
fn retire_closed_notification(
    reverse: &ReverseMapType,
    active: &ActiveNotifications,
    fdo_id: u32,
) -> bool {
    let Some(target) = reverse.lock().remove(&fdo_id) else {
        return false;
    };
    let key = target.key.clone();
    let mut lock = active.lock();
    let Some(by_portal) = lock.get_mut(key.app_id.as_ref()) else {
        return false;
    };
    if by_portal.get(key.portal_id.as_ref()) != Some(&fdo_id) {
        return false;
    }
    by_portal.remove(key.portal_id.as_ref());
    if by_portal.is_empty() {
        lock.remove(key.app_id.as_ref());
    }
    true
}

/// The D-Bus interface wrapper for the Notification portal.
///
/// This struct holds shared state used to map between the sandboxed application's
/// portal notification IDs and the host system's actual notification IDs.
pub struct Notification {
    /// Maps a [`NotifKey`] to the system notification ID (`u32`).
    /// This is used so we can replace or remove an existing notification.
    active_notifications: ActiveNotifications,

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
    proxy: Option<Arc<NotificationsProxy<'static>>>,
}

impl Notification {
    pub async fn new(connection: Option<Connection>) -> Self {
        let proxy = if let Some(session_bus) = &connection {
            NotificationsProxy::builder(session_bus)
                .build()
                .await
                .ok()
                .map(Arc::new)
        } else {
            None
        };

        Self {
            active_notifications: Arc::new(Mutex::new(HashMap::new())),
            reverse_map: Arc::new(Mutex::new(HashMap::new())),
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

        let mut sound_file: Option<Arc<TempSoundFile>> = None;
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

                    // Only ever write below a directory we own, with `0700`, and
                    // create the file with `O_EXCL`.
                    //
                    // The previous code fell back to `std::env::temp_dir()`
                    // (`/tmp`) whenever `XDG_RUNTIME_DIR` was unset, created the
                    // directory with `create_dir_all` at the default umask, and
                    // wrote with a plain truncate-create at the predictable name
                    // `{app_id}_{micros}.snd`. In a shared `/tmp` that is a
                    // symlink attack: a local attacker pre-creates the directory
                    // (or the file, as a symlink to something the user owns) and
                    // the portal -- running as the user -- truncates and
                    // overwrites the target. `$XDG_RUNTIME_DIR` is 0700 by
                    // definition, so requiring it removes the shared-directory
                    // case entirely; `O_EXCL` removes the pre-created-file case
                    // even inside a correctly-permissioned directory.
                    let Some(mut path) = temp_sound_dir() else {
                        tracing::warn!(
                            "XDG_RUNTIME_DIR is unset; refusing to stage a notification sound file"
                        );
                        return;
                    };

                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros();
                    path.push(format!(
                        "{}_{}.snd",
                        app_id.replace(['.', '-'], "_"),
                        timestamp
                    ));

                    let bytes = spawn_blocking(move || {
                        let mut data = Vec::new();
                        if file.read_to_end(&mut data).is_ok() {
                            return Some(data);
                        }
                        None
                    })
                    .await
                    .unwrap_or(None);

                    if let Some(data) = bytes
                        && write_exclusive(&path, &data).await.is_ok()
                    {
                        sound_file = Some(Arc::new(TempSoundFile { path }));
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
                icon_name = s.into();
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
                                icon_name = first.clone();
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
                                    let image_data = spawn_blocking(move || {
                                        use {
                                            gdk_pixbuf::Pixbuf,
                                            gtk4::{gio::MemoryInputStream, glib::Bytes},
                                            std::io::Read,
                                        };
                                        let mut data = Vec::new();
                                        if file.read_to_end(&mut data).is_ok() {
                                            // `from_owned` hands the buffer's
                                            // contents to GLib instead of
                                            // memcpy'ing them out. `read_to_end`
                                            // over-allocates, so the retained
                                            // allocation can be larger than the
                                            // image.
                                            let bytes = Bytes::from_owned(data);
                                            let stream = MemoryInputStream::from_bytes(&bytes);
                                            if let Ok(pixbuf) =
                                                Pixbuf::from_stream(&stream, Cancellable::NONE)
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
                                let image_data = spawn_blocking(move || {
                                    use {
                                        gdk_pixbuf::Pixbuf,
                                        gtk4::{gio::MemoryInputStream, glib::Bytes},
                                    };
                                    let bytes = Bytes::from_owned(byte_array);
                                    let stream = MemoryInputStream::from_bytes(&bytes);
                                    if let Ok(pixbuf) =
                                        Pixbuf::from_stream(&stream, Cancellable::NONE)
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

        let (actions, action_targets) = build_actions(&notification);

        if let Some(proxy) = &self.proxy {
            let replaces_id = active_id(&self.active_notifications, &app_id, &id).unwrap_or(0);

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
                register_notification(
                    &self.active_notifications,
                    &self.reverse_map,
                    &app_id,
                    &id,
                    new_id,
                    action_targets,
                    sound_file,
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
        let fdo_id =
            remove_active_notification(&self.active_notifications, app_id.as_str(), id.as_str());
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
            options.insert("body".into(), true_val.clone());
            options.insert("icon".into(), true_val.clone());
            options.insert("buttons".into(), true_val.clone());
            options.insert("priority".into(), true_val.clone());
            options.insert("default-action".into(), true_val.clone());
            options.insert("default-action-target".into(), true_val.clone());
            options.insert("markup-body".into(), true_val.clone());
            options.insert("category".into(), true_val.clone());
            options.insert("display-hint".into(), true_val.clone());
            options.insert("sound".into(), true_val);
        }
        options
    }
}

/// How many `&str` slots [`build_actions`] will push, so the vector is sized
/// once. Each action contributes an id and a label.
fn actions_capacity(notification: &PortalNotification) -> usize {
    let buttons = notification.buttons.as_ref().map_or(0, Vec::len);
    2 * (usize::from(notification.default_action.is_some()) + buttons)
}

/// Builds the FDO action list and the per-action target map.
///
/// `Notify` takes the actions as a flat alternating list of `(id, label)` pairs,
/// and the reply needs each action's target keyed by its id.
///
/// The labels are borrowed straight out of `notification` rather than collected
/// into an owned `Vec<String>` first. The previous code built one `String` per
/// id and per label -- including for a label, which was already a `&str` borrowed
/// from the caller's options -- and then dropped the whole vector after taking
/// `&str` from it, so every action cost two allocations that lived only long
/// enough to be reborrowed. `notification` outlives the `Notify` call, so the
/// borrows are valid for as long as the caller needs them.
///
/// The targets are still cloned: they are `OwnedValue`s that must outlive this
/// function, since they are stored in the reverse map and read later by the
/// `ActionInvoked` listener.
fn build_actions(notification: &PortalNotification) -> (Vec<&str>, HashMap<String, OwnedValue>) {
    let mut actions = Vec::with_capacity(actions_capacity(notification));
    let mut targets = HashMap::new();

    if let Some(default_action) = notification.default_action.as_deref() {
        actions.push("default");
        actions.push(default_action);
        if let Some(target) = notification.default_action_target.as_ref() {
            targets.insert("default".to_owned(), target.clone());
        }
    }

    if let Some(buttons) = notification.buttons.as_deref() {
        for (action, options) in buttons {
            // An explicit label wins; otherwise the action id doubles as the
            // label, matching what the reference implementations send.
            let label = options
                .get("label")
                .and_then(|v| <&str>::try_from(std::ops::Deref::deref(v)).ok())
                .unwrap_or(action.as_str());
            actions.push(action.as_str());
            actions.push(label);
            if let Some(target) = options.get("action-target") {
                targets.insert(action.clone(), target.clone());
            }
        }
    }

    (actions, targets)
}

/// Spawns a background task that listens to `ActionInvoked` signals from the system notification daemon.
///
/// When an action is invoked on a notification created through this portal, this function looks up
/// the original portal `app_id` and notification id in the `reverse_map`. It then emits the portal's
/// `ActionInvoked` signal back to the sandboxed application over D-Bus, completing the cycle.
async fn listen_for_action_invoked(
    reverse_map: ReverseMapType,
    server: ObjectServer,
    proxy: Arc<NotificationsProxy<'static>>,
    session_bus: Connection,
) -> zbus::Result<()> {
    let mut stream = proxy.receive_action_invoked().await?;

    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        let id = args.id;
        let action_key = args.action_key;

        let target_data = reverse_map.lock().get(&id).cloned();

        let Some(target) = target_data.as_deref() else {
            continue;
        };
        let (app_id, portal_id) = (&target.key.app_id, &target.key.portal_id);
        let action_targets = &target.action_targets;

        let mut params: Vec<Value<'_>> = vec![];

        // XDG Notification spec requires parameter: av
        // 1. The target for the action, if one was specified.
        // 2. The platform-data as vardict containing an activation-token (s)
        if let Some(tv) = action_targets.get(action_key) {
            params.push(Value::from(tv.clone()));
        }

        let platform_data: HashMap<&str, Value<'_>> = HashMap::new();
        let platform_data_val = Value::from(platform_data.clone());
        params.push(platform_data_val);

        // Build the application object path in one pass. `replace` twice would
        // allocate an intermediate `String` for the first pass and another for
        // the second, plus the `String::from("/")` seed that `push_str` then
        // reallocates as it grows.
        let mut app_path = String::with_capacity(app_id.len() + 1);
        app_path.push('/');
        app_path.extend(app_id.chars().map(|c| match c {
            '.' => '/',
            '-' => '_',
            other => other,
        }));

        // `app_id` and `portal_id` are borrowed from `target_data`, which dies
        // at the end of this iteration, so the spawned task needs owned values.
        // They are `Arc<str>`, so each clone is a refcount bump rather than a
        // new allocation.
        let app_id_clone = app_id.clone();
        let portal_id_clone = portal_id.clone();
        // `app_path` and `params` are owned locals that are dead after the
        // spawn, so they are moved rather than cloned. `params` holds only
        // `Value<'static>` (both entries are built from owned `OwnedValue`s),
        // so it reaches the task without a deep copy of every element.
        let action_key_clone = String::from(action_key);
        let server_clone = server.clone();
        let session_bus_clone = session_bus.clone();

        tokio::spawn(async move {
            if let Some(action_name) = action_key_clone.strip_prefix("app.") {
                // This proxy is used to talk back to the specific client application that triggered the notification
                // (e.g., when a user clicks a notification action). Because the destination address
                // (the app_id or unique connection name) changes dynamically on every single request,
                // we must instantiate it on the fly.
                let Ok(builder) = ApplicationProxy::builder(&session_bus_clone)
                    .destination(app_id_clone.as_ref())
                else {
                    tracing::error!("Invalid D-Bus destination: {}", app_id_clone);
                    return;
                };
                let Ok(builder) = builder.path(app_path.as_str()) else {
                    tracing::error!("Invalid D-Bus path: {}", app_path);
                    return;
                };
                let proxy_res = builder
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await;

                if let Ok(proxy) = proxy_res {
                    let _ = proxy
                        .activate_action(action_name, &params, &platform_data)
                        .await;
                }
            } else {
                let Ok(builder) = ApplicationProxy::builder(&session_bus_clone)
                    .destination(app_id_clone.as_ref())
                else {
                    tracing::error!("Invalid D-Bus destination: {}", app_id_clone);
                    return;
                };
                let Ok(builder) = builder.path(app_path.as_str()) else {
                    tracing::error!("Invalid D-Bus path: {}", app_path);
                    return;
                };
                let proxy_res = builder
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await;

                if let Ok(proxy) = proxy_res {
                    let _ = proxy.activate(&platform_data).await;
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
                        &params,
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
    active_notifications: ActiveNotifications,
    proxy: Arc<NotificationsProxy<'static>>,
) -> zbus::Result<()> {
    let mut stream = proxy.receive_notification_closed().await?;

    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        let id = args.id;

        retire_closed_notification(&reverse_map, &active_notifications, id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The staged-sound directory must live under `$XDG_RUNTIME_DIR`, never in
    /// the world-writable system temp directory.
    ///
    /// Regression: the previous code used `std::env::temp_dir()` as the base and
    /// only preferred `$XDG_RUNTIME_DIR` when it happened to be set. When it was
    /// not, sounds landed in `/tmp` at a semi-predictable name, writable by any
    /// local user.
    #[test]
    fn there_is_no_sound_directory_without_a_runtime_dir() {
        assert_eq!(
            sound_dir_under(None),
            None,
            "without XDG_RUNTIME_DIR there is no private directory to write into"
        );
    }

    #[test]
    fn an_empty_runtime_dir_is_treated_as_unset() {
        assert_eq!(sound_dir_under(Some(std::ffi::OsStr::new(""))), None);
    }

    #[test]
    fn the_sound_directory_is_created_under_the_runtime_dir() {
        assert_eq!(
            sound_dir_under(Some(std::ffi::OsStr::new("/run/user/1000"))),
            Some(std::path::PathBuf::from("/run/user/1000").join(TEMP_SOUND_SUBDIR))
        );
    }

    /// The create must be exclusive.
    ///
    /// Regression: `tokio::fs::write` opens `O_WRONLY|O_CREAT|O_TRUNC` with no
    /// `O_EXCL` and no `O_NOFOLLOW`, so a pre-existing symlink at the target
    /// path was followed and truncated. This asserts the `O_EXCL` behaviour that
    /// replaces it.
    #[tokio::test]
    async fn write_exclusive_refuses_an_existing_target() {
        let dir = std::env::temp_dir().join(format!("xdp-sound-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("taken.snd");

        write_exclusive(&target, b"first")
            .await
            .expect("the first create must succeed");

        let err = write_exclusive(&target, b"second")
            .await
            .expect_err("an existing target must not be overwritten");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::AlreadyExists,
            "O_EXCL must surface as EEXIST, got {err:?}"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"first",
            "the original contents must survive the refused write"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A symlink at the target must not be followed.
    ///
    /// This is the attack the `O_EXCL` flag exists to stop: a local attacker
    /// plants `attack.snd -> ~/.bashrc` in the sound directory and waits for the
    /// portal to write through it.
    #[tokio::test]
    async fn write_exclusive_does_not_follow_a_symlink() {
        let dir = std::env::temp_dir().join(format!("xdp-sound-link-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let victim = dir.join("victim");
        std::fs::write(&victim, b"intact").unwrap();
        let link = dir.join("attack.snd");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        let result = write_exclusive(&link, b"overwritten").await;

        assert!(
            result.is_err(),
            "O_EXCL must refuse to open through an existing symlink"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"intact",
            "the symlink target must be untouched"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

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
            zbus::zvariant::{self, Endian, Value, serialized::Context},
        };

        let mut dict = HashMap::new();
        dict.insert("title", Value::from("Test Title"));
        dict.insert("body", Value::from("Test Body"));
        dict.insert("priority", Value::from("high"));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &dict).unwrap();
        let notification: PortalNotification = encoded.deserialize().unwrap().0;

        assert_eq!(notification.title.as_deref(), Some("Test Title"));
        assert_eq!(notification.body.as_deref(), Some("Test Body"));
        assert_eq!(notification.priority.as_deref(), Some("high"));
        assert_eq!(notification.category, None);
    }
}

#[cfg(test)]
mod notification_map_tests {
    use super::*;

    fn empty_maps() -> (ActiveNotifications, ReverseMapType) {
        (
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
        )
    }

    #[test]
    fn key_fields_round_trip() {
        let key = NotifKey::new("org.gnome.TextEditor", "42");
        assert_eq!(&*key.app_id, "org.gnome.TextEditor");
        assert_eq!(&*key.portal_id, "42");
    }

    #[test]
    fn keys_compare_by_value_not_identity() {
        let a = NotifKey::new("app", "1");
        assert_eq!(a, NotifKey::new("app", "1"), "equal contents compare equal");
        assert_ne!(a, NotifKey::new("app", "2"));
        assert_ne!(a, NotifKey::new("other", "1"));
    }

    /// The point of the refactor: one `NotifKey`, shared by both maps.
    #[test]
    fn registration_shares_one_key_between_both_maps() {
        let (active, reverse) = empty_maps();
        let key = register_notification(
            &active,
            &reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            None,
        );

        assert_eq!(active_id(&active, &key.app_id, &key.portal_id), Some(42));
        let reverse_lock = reverse.lock();
        let target = reverse_lock.get(&42).expect("reverse entry must exist");
        assert!(
            Arc::ptr_eq(&key, &target.key),
            "the reverse map must hold the very same key, not a copy"
        );
    }

    #[test]
    fn retiring_a_closed_notification_clears_both_maps() {
        let (active, reverse) = empty_maps();
        register_notification(
            &active,
            &reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            None,
        );

        assert!(retire_closed_notification(&reverse, &active, 42));
        assert!(active.lock().is_empty(), "forward entry must be gone");
        assert!(reverse.lock().is_empty(), "reverse entry must be gone");
    }

    /// The race the guard exists for: the host replaces a notification and then
    /// reports the *old* one closed. The live replacement's mapping must survive.
    #[test]
    fn retiring_a_superseded_id_keeps_the_live_mapping() {
        let (active, reverse) = empty_maps();
        let key = register_notification(
            &active,
            &reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            None,
        );
        // The host replaced it: same portal key, new host ID.
        register_notification(
            &active,
            &reverse,
            "org.example.App",
            "7",
            99,
            HashMap::new(),
            None,
        );

        // A late NotificationClosed for the *old* host ID arrives.
        assert!(
            !retire_closed_notification(&reverse, &active, 42),
            "a superseded id must not retire the replacement"
        );
        assert_eq!(
            active_id(&active, &key.app_id, &key.portal_id),
            Some(99),
            "the live mapping must survive"
        );
        assert!(reverse.lock().contains_key(&99));
        assert!(!reverse.lock().contains_key(&42));
    }

    #[test]
    fn retiring_an_unknown_id_is_a_no_op() {
        let (active, reverse) = empty_maps();
        register_notification(
            &active,
            &reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            None,
        );
        assert!(!retire_closed_notification(&reverse, &active, 1234));
        assert_eq!(active.lock().len(), 1);
        assert_eq!(reverse.lock().len(), 1);
    }

    #[tokio::test]
    async fn retiring_releases_the_sound_file_arc() {
        let dir = std::env::temp_dir().join("xdpg-notif-sound-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sound.snd");
        std::fs::write(&path, b"x").unwrap();

        let (active, reverse) = empty_maps();
        let sound = Arc::new(TempSoundFile { path });
        assert_eq!(Arc::strong_count(&sound), 1);
        register_notification(
            &active,
            &reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            Some(sound.clone()),
        );
        assert_eq!(Arc::strong_count(&sound), 2);

        assert!(retire_closed_notification(&reverse, &active, 42));
        assert!(
            !reverse.lock().contains_key(&42),
            "retiring must drop the target, releasing its Arc<TempSoundFile>"
        );
        assert_eq!(
            Arc::strong_count(&sound),
            1,
            "the last portal-held reference must be gone, so TempSoundFile::drop runs"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_notification_finds_the_entry_without_allocating() {
        let (active, _reverse) = empty_maps();
        register_notification(
            &active,
            &_reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            None,
        );

        let scope = crate::alloc_probe::AllocScope::start();
        let removed = remove_active_notification(&active, "org.example.App", "7");
        let snap = scope.finish();
        assert_eq!(removed, Some(42));
        assert_eq!(
            snap.count, 0,
            "removal must borrow the key rather than build one, got {snap:?}",
        );
    }

    #[test]
    fn remove_notification_only_matches_the_exact_pair() {
        let (active, _reverse) = empty_maps();
        register_notification(
            &active,
            &_reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            None,
        );

        for (app_id, portal_id) in [
            ("org.example.App", "8"),
            ("org.example.Ap", "7"),
            ("org.example", "7"),
            ("", ""),
        ] {
            assert_eq!(
                remove_active_notification(&active, app_id, portal_id),
                None,
                "{app_id:?}/{portal_id:?} must not match",
            );
        }
        assert_eq!(active.lock().len(), 1, "failed lookups must not remove");
    }

    #[test]
    fn remove_notification_reports_a_miss() {
        let (active, _reverse) = empty_maps();
        assert_eq!(remove_active_notification(&active, "nope", "nope"), None);
    }

    /// `active_notifications` is neither rate-limited nor evicted, so an
    /// unprivileged caller can grow it. A lookup that scans it would make
    /// `RemoveNotification` quadratic while holding the lock that
    /// `add_notification` and the signal listeners also need.
    #[test]
    fn lookup_stays_hashed_as_the_map_grows() {
        let (active, _reverse) = empty_maps();
        for i in 0..2000 {
            register_notification(
                &active,
                &_reverse,
                "org.example.App",
                &format!("n{i}"),
                i as u32 + 1,
                HashMap::new(),
                None,
            );
        }
        assert_eq!(
            active_id(&active, "org.example.App", "n1999"),
            Some(2000),
            "a deep entry must still be found"
        );
        assert_eq!(
            remove_active_notification(&active, "org.example.App", "n1999"),
            Some(2000)
        );
        assert_eq!(active_id(&active, "org.example.App", "n1999"), None);
    }

    #[test]
    fn an_app_with_no_notifications_left_is_forgotten() {
        let (active, reverse) = empty_maps();
        register_notification(
            &active,
            &reverse,
            "org.example.A",
            "1",
            5,
            HashMap::new(),
            None,
        );
        register_notification(
            &active,
            &reverse,
            "org.example.B",
            "1",
            6,
            HashMap::new(),
            None,
        );

        assert!(remove_active_notification(&active, "org.example.A", "1").is_some());
        {
            let lock = active.lock();
            assert!(
                !lock.contains_key("org.example.A"),
                "an emptied app must not keep an empty inner map"
            );
            assert!(
                lock.contains_key("org.example.B"),
                "other apps are untouched"
            );
        }
        // The inner map survives while the app still has live notifications.
        register_notification(
            &active,
            &reverse,
            "org.example.B",
            "2",
            7,
            HashMap::new(),
            None,
        );
        assert!(remove_active_notification(&active, "org.example.B", "1").is_some());
        assert_eq!(active_id(&active, "org.example.B", "2"), Some(7));
    }
}

#[cfg(test)]
mod bytes_tests {
    /// `Bytes::from_owned` replaces a copy in the pixbuf decode path. The failure
    /// mode of getting the length wrong is silent (a truncated image), so pin that
    /// it carries exactly the same bytes as the copying constructor.
    #[test]
    fn from_owned_matches_a_copying_from_slice() {
        use gtk4::glib::Bytes;

        for payload in [
            vec![],
            vec![0u8],
            vec![0, 1, 2, 253, 254, 255],
            vec![7u8; 5000],
        ] {
            let owned = Bytes::from_owned(payload.clone());
            let copied = Bytes::from(payload.as_slice());
            assert_eq!(owned.len(), copied.len(), "length must match");
            assert!(
                owned == copied,
                "content must match for {} bytes",
                payload.len()
            );
        }
    }
}

#[cfg(test)]
mod action_list_tests {
    use super::*;

    fn owned(s: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(s)).expect("str to OwnedValue is infallible")
    }

    fn notification_with_default_action() -> PortalNotification {
        PortalNotification {
            default_action: Some("open".into()),
            default_action_target: Some(owned("t")),
            ..Default::default()
        }
    }

    #[test]
    fn no_actions_yields_an_empty_list() {
        let n = PortalNotification::default();
        let (actions, targets) = build_actions(&n);
        assert!(actions.is_empty());
        assert!(targets.is_empty());
    }

    #[test]
    fn the_default_action_is_reported_under_the_reserved_id() {
        let n = notification_with_default_action();
        let (actions, targets) = build_actions(&n);
        assert_eq!(actions, vec!["default", "open"]);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets["default"], owned("t"));
    }

    #[test]
    fn a_default_action_without_a_target_still_reports_the_action() {
        let n = PortalNotification {
            default_action: Some("open".into()),
            ..Default::default()
        };
        let (actions, targets) = build_actions(&n);
        assert_eq!(actions, vec!["default", "open"]);
        assert!(
            targets.is_empty(),
            "an action with no target must not get an empty entry"
        );
    }

    /// The wire format is a flat alternating list, so an odd length would
    /// silently pair a label with the next action's id.
    #[test]
    fn the_list_is_always_id_label_pairs() {
        let n = PortalNotification {
            default_action: Some("open".into()),
            default_action_target: Some(owned("t")),
            buttons: Some(vec![
                ("reply".into(), HashMap::new()),
                (
                    "archive".into(),
                    HashMap::from([("label".into(), owned("Archive"))]),
                ),
            ]),
            ..Default::default()
        };
        let (actions, _) = build_actions(&n);
        assert_eq!(actions.len() % 2, 0, "ids and labels must pair up");
        assert_eq!(
            actions,
            vec!["default", "open", "reply", "reply", "archive", "Archive"]
        );
    }

    #[test]
    fn an_explicit_label_overrides_the_action_id() {
        let n = PortalNotification {
            buttons: Some(vec![(
                "app.reply".into(),
                HashMap::from([("label".into(), owned("Reply"))]),
            )]),
            ..Default::default()
        };
        let (actions, _) = build_actions(&n);
        assert_eq!(actions, vec!["app.reply", "Reply"]);
    }

    /// An absent label falls back to the id, which is what the reference
    /// implementations do and what keeps the list correctly paired.
    #[test]
    fn a_missing_label_falls_back_to_the_action_id() {
        let n = PortalNotification {
            buttons: Some(vec![("app.reply".into(), HashMap::new())]),
            ..Default::default()
        };
        let (actions, _) = build_actions(&n);
        assert_eq!(actions, vec!["app.reply", "app.reply"]);
    }

    #[test]
    fn targets_are_kept_per_action() {
        let n = PortalNotification {
            default_action: Some("open".into()),
            default_action_target: Some(owned("default-target")),
            buttons: Some(vec![
                (
                    "reply".into(),
                    HashMap::from([("action-target".into(), owned("reply-target"))]),
                ),
                ("archive".into(), HashMap::new()),
            ]),
            ..Default::default()
        };
        let (_, targets) = build_actions(&n);
        assert_eq!(targets["default"], owned("default-target"));
        assert_eq!(targets["reply"], owned("reply-target"));
        assert!(
            !targets.contains_key("archive"),
            "a button with no target must not be recorded"
        );
    }

    /// The regression this guards: the list used to be built as a
    /// `Vec<String>`, allocating one `String` per id and per label -- including
    /// for labels, which were already `&str` -- and the whole vector was then
    /// dropped after reborrowing it as `Vec<&str>`. Two buttons with no labels
    /// and no targets must now cost only the `Vec` itself.
    #[test]
    fn building_the_list_does_not_allocate_a_string_per_action() {
        let n = PortalNotification {
            default_action: Some("open".into()),
            buttons: Some(vec![
                ("reply".into(), HashMap::new()),
                ("archive".into(), HashMap::new()),
            ]),
            ..Default::default()
        };

        let scope = crate::alloc_probe::AllocScope::start();
        let (actions, targets) = build_actions(&n);
        let snap = scope.finish();
        std::hint::black_box(&actions);
        std::hint::black_box(&targets);

        assert_eq!(actions.len(), 6, "three actions, id and label each");
        assert!(
            targets.is_empty(),
            "no targets were requested, so none may be cloned"
        );
        assert_eq!(
            snap.count, 1,
            "only the result Vec may allocate, got {snap:?}"
        );
    }
}

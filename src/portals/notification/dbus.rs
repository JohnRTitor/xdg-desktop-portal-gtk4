#![allow(clippy::too_many_arguments)]

use {
    futures_util::stream::StreamExt,
    gtk4::gio::Cancellable,
    parking_lot::Mutex,
    smallvec::SmallVec,
    std::{
        collections::HashMap,
        sync::{Arc, LazyLock, Weak},
    },
    tokio::task::spawn_blocking,
    zbus::{
        Connection, DeserializeDict, ObjectServer, OwnedVariant, Structure, Type, Variant,
        interface, object_server::SignalEmitter,
    },
};

/// Subdirectory of `$XDG_RUNTIME_DIR` that staged notification sounds live in.
const TEMP_SOUND_SUBDIR: &str = "xdg-desktop-portal-gtk4-sounds";

/// The fixed set of notification features this backend advertises.
///
/// Built once. Property reads are frequent and the value never changes, so
/// rebuilding ten owned keys and ten owned variants on every read was pure
/// waste. zbus still serialises the returned reference on each read, but the
/// map and its values are now shared.
static SUPPORTED_OPTIONS: LazyLock<HashMap<String, OwnedVariant>> = LazyLock::new(|| {
    let mut options = HashMap::with_capacity(10);
    if let Ok(true_val) = OwnedVariant::try_from(Variant::Bool(true)) {
        for key in [
            "body",
            "icon",
            "buttons",
            "priority",
            "default-action",
            "default-action-target",
            "markup-body",
            "category",
            "display-hint",
            "sound",
        ] {
            options.insert(key.to_owned(), true_val.clone());
        }
    }
    options
});

pub struct TempSoundFile {
    pub path: std::path::PathBuf,
}

impl Drop for TempSoundFile {
    fn drop(&mut self) {
        // The last `Arc` can be dropped on a thread with no Tokio runtime (a
        // shutdown path, a synchronous caller, a plain unit test). `spawn_blocking`
        // panics in that case, and this daemon builds with `panic = "abort"`, so
        // a stray drop would take down the whole process. Unlinking one file is
        // cheap: do it inline when there is no runtime to hand it to.
        if tokio::runtime::Handle::try_current().is_ok() {
            let path = self.path.clone();
            spawn_blocking(move || {
                let _ = std::fs::remove_file(&path);
            });
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
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
        hints: &HashMap<&str, Variant<'_>>,
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
    fn activate(&self, platform_data: &HashMap<&str, Variant<'_>>) -> zbus::Result<()>;
    fn activate_action(
        &self,
        action_name: &str,
        parameter: &[Variant<'_>],
        platform_data: &HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<()>;
}

#[derive(DeserializeDict, Type, Default, Debug, Clone)]
#[zbus(signature = "dict")]
pub struct PortalNotification {
    title: Option<String>,
    body: Option<String>,
    icon: Option<OwnedVariant>,
    priority: Option<String>,
    #[zbus(rename = "default-action")]
    default_action: Option<String>,
    #[zbus(rename = "default-action-target")]
    default_action_target: Option<OwnedVariant>,
    buttons: Option<Vec<PortalButton>>,
    #[zbus(rename = "markup-body")]
    markup_body: Option<String>,
    category: Option<String>,
    #[zbus(rename = "display-hint")]
    display_hint: Option<Vec<String>>,
    sound: Option<OwnedVariant>,
}

/// One entry of the `buttons` option.
///
/// The portal specification serializes buttons as `aa{sv}`: an array of
/// vardicts with `label`, `action`, `target` and `purpose` keys. `purpose` is a
/// hint for the notification server's own UI and has no fdo equivalent, so it
/// is not read.
#[derive(DeserializeDict, Type, Default, Debug, Clone)]
#[zbus(signature = "dict")]
pub struct PortalButton {
    label: Option<String>,
    action: Option<String>,
    target: Option<OwnedVariant>,
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
    pub action_targets: HashMap<String, OwnedVariant>,
    /// The portal action behind the fdo `default` action key, if one was set.
    pub default_action: Option<String>,
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
/// The forward map is keyed by the key's two `Arc<str>` fields and the reverse
/// entry holds an `Arc<NotifKey>` that shares those allocations. Building the
/// strings twice would reintroduce the duplicate storage this type exists to
/// remove. Factored out so a test can assert the sharing rather than having to
/// trust the call site.
fn register_notification(
    active: &ActiveNotifications,
    reverse: &ReverseMapType,
    app_id: &str,
    portal_id: &str,
    fdo_id: u32,
    action_targets: HashMap<String, OwnedVariant>,
    default_action: Option<String>,
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
            default_action,
            sound_file,
        }),
    );
    key
}

/// Applies the outcome of a `Notify` call to the two routing maps.
///
/// Retiring the previous state *before* the call meant a failed `Notify` left
/// the still-live host notification with no reverse entry (so its actions could
/// no longer be routed) and dropped its staged sound file early, while the
/// forward map kept pointing at the old host ID. Nothing is changed on error.
///
/// A server may reuse `replaces_id` as the new ID. In that case
/// [`register_notification`] overwrites the old reverse entry itself, retiring
/// the old sound file; removing the old entry as well would delete the one we
/// just registered. Only a genuinely different ID needs an explicit removal.
fn apply_notify_result<E>(
    active: &ActiveNotifications,
    reverse: &ReverseMapType,
    replaces_id: u32,
    app_id: &str,
    portal_id: &str,
    result: Result<u32, E>,
    action_targets: HashMap<String, OwnedVariant>,
    default_action: Option<String>,
    sound_file: Option<Arc<TempSoundFile>>,
) {
    let Ok(new_id) = result else {
        return;
    };
    if replaces_id != 0 && replaces_id != new_id {
        // The host may have closed the old notification and recycled its id for
        // a different one while `Notify` was in flight. Only retire the entry
        // if it still belongs to this caller; otherwise we would delete the new
        // owner's routing state.
        let mut reverse_lock = reverse.lock();
        let still_ours = reverse_lock.get(&replaces_id).is_some_and(|target| {
            target.key.app_id.as_ref() == app_id && target.key.portal_id.as_ref() == portal_id
        });
        if still_ours {
            reverse_lock.remove(&replaces_id);
        }
    }
    register_notification(
        active,
        reverse,
        app_id,
        portal_id,
        new_id,
        action_targets,
        default_action,
        sound_file,
    );
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
    /// Maps `app_id -> portal_id -> ` the system notification ID (`u32`).
    /// This is used so we can replace or remove an existing notification.
    active_notifications: ActiveNotifications,

    /// Maps the system D-Bus notification ID (`u32`) back to the portal `app_id`, `portal_id`,
    /// action targets, and optional sound temp file.
    ///
    /// # Threading & Invariants
    ///
    /// This map is populated when a notification is added, and it is consulted
    /// asynchronously by the background task listening to `ActivationToken`,
    /// `ActionInvoked` and `NotificationClosed` signals from the host's notification daemon.
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
        // At most eight hints are ever inserted: desktop-entry, urgency,
        // category, transient, resident, suppress-sound, sound-file and
        // image-data. Size once instead of growing on the common path.
        let mut hints = HashMap::with_capacity(8);
        hints.insert("desktop-entry", Variant::from(app_id.as_str()));

        let priority = notification.priority.as_deref().unwrap_or("normal");
        let urgency: u8 = match priority {
            "low" => 0,
            "normal" => 1,
            "high" | "urgent" => 2,
            _ => 1,
        };
        hints.insert("urgency", Variant::from(urgency));

        if let Some(category) = notification.category.as_deref() {
            hints.insert("category", Variant::from(category));
        }

        if let Some(display_hints) = notification.display_hint.as_ref() {
            if display_hints.iter().any(|h| h == "transient") {
                hints.insert("transient", Variant::from(true));
            }
            if display_hints.iter().any(|h| h == "persistent") {
                hints.insert("resident", Variant::from(true));
            }
        }

        let mut sound_file: Option<Arc<TempSoundFile>> = None;
        if let Some(sound) = notification.sound.as_ref() {
            let inner = match std::ops::Deref::deref(sound) {
                Variant::Variant(v) => v.as_ref(),
                other => other,
            };
            if let Ok(sound_str) = <&str>::try_from(inner) {
                if sound_str == "silent" {
                    hints.insert("suppress-sound", Variant::from(true));
                }
            } else if let Variant::Fd(fd) = inner {
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
            hints.insert("sound-file", Variant::from(path_str));
        }

        if let Some(v) = notification.icon.as_ref() {
            let v_ref = std::ops::Deref::deref(v);
            if let Ok(s) = <&str>::try_from(v_ref) {
                icon_name = s.into();
            } else if let Ok(structure) = <&Structure>::try_from(v_ref) {
                let fields = structure.fields();
                if fields.len() == 2
                    && let Ok(icon_type) = <&str>::try_from(&fields[0])
                {
                    // The icon format is (sv) — the payload in fields[1] is wrapped
                    // in a variant. Unwrap it so we can extract the actual value.
                    let payload = match &fields[1] {
                        Variant::Variant(inner) => inner.as_ref(),
                        other => other,
                    };
                    match icon_type {
                        "themed" => {
                            if let Some(name) = themed_icon_name(payload) {
                                icon_name = name;
                            }
                        }
                        "file-descriptor" => {
                            // Note: xdg-desktop-portal drops raw "file" icon paths for security.
                            // Apps sending "bytes" arrays will have their bytes written to a memfd
                            // by the host portal, which forwards it to us here as "file-descriptor".
                            if let Variant::Fd(fd) = payload {
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
                                                return OwnedVariant::try_from(Variant::new((
                                                    pixbuf.width(),
                                                    pixbuf.height(),
                                                    pixbuf.rowstride(),
                                                    pixbuf.has_alpha(),
                                                    pixbuf.bits_per_sample(),
                                                    pixbuf.n_channels(),
                                                    Variant::from(
                                                        pixbuf.read_pixel_bytes().as_ref(),
                                                    ),
                                                )))
                                                .ok();
                                            }
                                        }
                                        None
                                    })
                                    .await
                                    .unwrap_or(None);

                                    if let Some(image_data) = image_data {
                                        hints.insert("image-data", Variant::from(image_data));
                                    }
                                }
                            }
                        }
                        "bytes" => {
                            // The payload is an `ay`. Copy its bytes out of the
                            // borrowed array into an exactly-sized buffer; the
                            // `Bytes::from_owned` below then hands that single
                            // allocation to GLib without a further copy.
                            let byte_array: Option<Vec<u8>> = (|| {
                                let array = <&zbus::wire::Array>::try_from(payload).ok()?;
                                let mut bytes = Vec::with_capacity(array.len());
                                for byte in array.inner() {
                                    bytes.push(u8::try_from(byte).ok()?);
                                }
                                Some(bytes)
                            })();
                            if let Some(byte_array) = byte_array {
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
                                        return OwnedVariant::try_from(Variant::new((
                                            pixbuf.width(),
                                            pixbuf.height(),
                                            pixbuf.rowstride(),
                                            pixbuf.has_alpha(),
                                            pixbuf.bits_per_sample(),
                                            pixbuf.n_channels(),
                                            Variant::from(pixbuf.read_pixel_bytes().as_ref()),
                                        )))
                                        .ok();
                                    }
                                    None
                                })
                                .await
                                .unwrap_or(None);

                                if let Some(image_data) = image_data {
                                    hints.insert("image-data", Variant::from(image_data));
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

            // `apply_notify_result` is a no-op on error: the host is still
            // showing the old notification, so its routing state and sound file
            // must stay intact. `AddNotification` has no reply value, so there
            // is no `Response::other()` to return here.
            apply_notify_result(
                &self.active_notifications,
                &self.reverse_map,
                replaces_id,
                &app_id,
                &id,
                proxy
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
                    .await,
                action_targets,
                notification.default_action.clone(),
                sound_file,
            );
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
                tokio::spawn(async move {
                    if let Err(e) = listen_for_server_signals(
                        reverse_map_clone,
                        active_notifications_clone,
                        server_clone,
                        proxy,
                        session_bus,
                    )
                    .await
                    {
                        tracing::error!("Notification server signal stream failed: {}", e);
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
        parameter: &[Variant<'_>],
    ) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    #[zbus(property, name = "SupportedOptions")]
    fn supported_options(&self) -> &'static HashMap<String, OwnedVariant> {
        &SUPPORTED_OPTIONS
    }
}

/// Read the first name from a themed-icon `as` payload.
///
/// Borrowing the array avoids materialising a `Vec<String>` just to keep one
/// name, but the old conversion validated every element, so a malformed
/// trailing name must still reject the whole icon. `Array -> Vec<T>` unwraps
/// exactly one level of `Variant::Variant` per element.
fn themed_icon_name(payload: &Variant<'_>) -> Option<String> {
    let array = <&zbus::wire::Array>::try_from(payload).ok()?;
    let mut first: Option<&str> = None;
    for value in array.inner() {
        let value = match value {
            Variant::Variant(inner) => inner.as_ref(),
            other => other,
        };
        let Ok(name) = <&str>::try_from(value) else {
            return None;
        };
        if first.is_none() {
            first = Some(name);
        }
    }
    first.map(str::to_owned)
}

/// How many `&str` slots [`build_actions`] will push, so the list is sized
/// once. Each action contributes an id and a label.
fn actions_capacity(notification: &PortalNotification) -> usize {
    // Buttons without an `action` are skipped by `build_actions`, so counting
    // them would over-size the SmallVec and can force a needless heap spill
    // even though the real slots still fit inline.
    let buttons = notification.buttons.as_ref().map_or(0, |buttons| {
        buttons
            .iter()
            .filter(|button| button.action.is_some())
            .count()
    });
    2 * (usize::from(notification.default_action.is_some()) + buttons)
}

/// Exact number of targets [`build_actions`] will insert.
///
/// Counting first keeps the common "no targets" case allocation-free: sizing
/// the map by the number of actions would allocate even when nothing is ever
/// inserted. A button only contributes when it has both an id and a target,
/// and the default action only contributes when it is present and targeted.
fn targets_capacity(notification: &PortalNotification) -> usize {
    let default = usize::from(
        notification.default_action.is_some() && notification.default_action_target.is_some(),
    );
    let buttons = notification.buttons.as_ref().map_or(0, |buttons| {
        buttons
            .iter()
            .filter(|button| button.action.is_some() && button.target.is_some())
            .count()
    });
    default + buttons
}

/// Builds the FDO action list and the per-action target map.
///
/// `Notify` takes the actions as a flat alternating list of `(id, label)` pairs,
/// and the reply needs each action's target keyed by its id.
///
/// The default action is reported under fdo's reserved `default` id with an
/// empty label, as the reference implementation does: servers activate it on a
/// click on the notification body rather than drawing it as a button, and its
/// portal action name is recorded separately in [`NotificationTarget`]. Buttons
/// are reported under their portal action name, so the id routes straight back
/// to the action. A button without an `action` cannot be routed and is skipped.
///
/// The labels are borrowed straight out of `notification` rather than collected
/// into an owned `Vec<String>` first. The previous code built one `String` per
/// id and per label -- including for a label, which was already a `&str` borrowed
/// from the caller's options -- and then dropped the whole vector after taking
/// `&str` from it, so every action cost two allocations that lived only long
/// enough to be reborrowed. `notification` outlives the `Notify` call, so the
/// borrows are valid for as long as the caller needs them. A [`SmallVec`] keeps
/// the common case (a default action plus a few buttons) entirely on the stack.
///
/// The targets are still cloned: they are `OwnedVariant`s that must outlive this
/// function, since they are stored in the reverse map and read later by the
/// `ActionInvoked` listener.
fn build_actions(
    notification: &PortalNotification,
) -> (SmallVec<[&str; 8]>, HashMap<String, OwnedVariant>) {
    let mut actions: SmallVec<[&str; 8]> = SmallVec::with_capacity(actions_capacity(notification));
    let mut targets = HashMap::with_capacity(targets_capacity(notification));

    if notification.default_action.is_some() {
        actions.push("default");
        actions.push("");
        if let Some(target) = notification.default_action_target.as_ref() {
            targets.insert("default".to_owned(), target.clone());
        }
    }

    if let Some(buttons) = notification.buttons.as_deref() {
        for button in buttons {
            let Some(action) = button.action.as_deref() else {
                tracing::debug!("Skipping a notification button with no action");
                continue;
            };
            // An explicit label wins; otherwise the action id doubles as the
            // label, so the list stays correctly paired.
            let label = button.label.as_deref().unwrap_or(action);
            actions.push(action);
            actions.push(label);
            if let Some(target) = button.target.as_ref() {
                targets.insert(action.to_owned(), target.clone());
            }
        }
    }

    (actions, targets)
}

/// The portal action behind an fdo action id, and its target.
///
/// Buttons are reported under their own action name, so the id is the action.
/// The reserved `default` id stands for the notification's `default-action`; a
/// notification with no `default-action` only reports `default` when one of its
/// buttons is literally named that, in which case the id is the action again.
fn action_route<'a>(
    target: &'a NotificationTarget,
    action_key: &'a str,
) -> (&'a str, Option<&'a OwnedVariant>) {
    let name = if action_key == "default" {
        target.default_action.as_deref().unwrap_or(action_key)
    } else {
        action_key
    };
    (name, target.action_targets.get(action_key))
}

/// The `platform-data` vardict handed to the application on activation.
///
/// The token comes from the server's `ActivationToken` signal, which the fdo
/// specification sends just before `ActionInvoked`. It is passed under both
/// keys the reference implementation uses: `activation-token` for current GTK
/// and Qt, `desktop-startup-id` for GTK before 4.10.
fn platform_data(activation_token: Option<String>) -> HashMap<&'static str, Variant<'static>> {
    let mut data = HashMap::with_capacity(2);
    if let Some(token) = activation_token {
        // `Variant::from(String)` owns its text through a refcounted `Str`, so
        // cloning the variant shares one allocation instead of copying the
        // token into a second `String`. Both keys need the same value.
        let token = Variant::from(token);
        data.insert("desktop-startup-id", token.clone());
        data.insert("activation-token", token);
    }
    data
}

/// Whether a stored activation token still belongs to the notification that
/// currently owns the host ID.
///
/// Host IDs can be reused: a replacement may keep the same ID while swapping
/// the reverse-map target. A token received before that swap was issued for the
/// old notification and must not be passed to the new one's action. Each token
/// is stored together with a weak reference to the target it was issued for,
/// and identity (rather than value) comparison is used because two
/// registrations with identical fields still represent different notifications.
/// A `Weak` keeps the control block (and therefore pointer identity) alive
/// without keeping the target's action map and staged sound file alive after
/// the notification has been retired.
fn token_belongs_to(
    issued_for: &Weak<NotificationTarget>,
    current: Option<&Arc<NotificationTarget>>,
) -> bool {
    current.is_some_and(|current| {
        issued_for
            .upgrade()
            .is_some_and(|issued_for| Arc::ptr_eq(&issued_for, current))
    })
}

/// Listens to the notification server's `ActivationToken`, `ActionInvoked` and
/// `NotificationClosed` signals on one stream, in the order the server sent
/// them.
///
/// One stream rather than one per signal is what makes the routing correct:
/// the server sends `ActivationToken` before the `ActionInvoked` it belongs to,
/// and usually `NotificationClosed` right after it. With separate streams on
/// separate tasks, the closed handler could retire the notification before the
/// action handler looked it up, and the token could arrive after the action
/// was already dispatched.
async fn listen_for_server_signals(
    reverse_map: ReverseMapType,
    active_notifications: ActiveNotifications,
    server: ObjectServer,
    proxy: Arc<NotificationsProxy<'static>>,
    session_bus: Connection,
) -> zbus::Result<()> {
    let mut stream = proxy.inner().receive_all_signals().await?;
    // Tokens are single-use, so each is consumed by the action it precedes.
    // The target the token was issued for travels with it as a Weak reference:
    // a replacement or removal invalidates the token even when the host reuses
    // the same ID, but a pending token does not keep the retired target (and
    // its staged sound file) alive.
    let mut activation_tokens: HashMap<u32, (Weak<NotificationTarget>, String)> = HashMap::new();

    while let Some(message) = stream.next().await {
        let header = message.header();
        match header.member().map(|m| m.as_str()) {
            Some("ActivationToken") => {
                let Ok((id, token)) = message.body().deserialize::<(u32, String)>() else {
                    tracing::warn!("Failed to deserialize ActivationToken signal body");
                    continue;
                };
                if let Some(target) = reverse_map.lock().get(&id).cloned() {
                    activation_tokens.insert(id, (Arc::downgrade(&target), token));
                } else {
                    // No live notification owns this ID (it was replaced with a
                    // different ID, or removed). Clear any token left over from
                    // the old one rather than letting it linger.
                    activation_tokens.remove(&id);
                }
            }
            Some("ActionInvoked") => {
                let Ok((id, action_key)) = message.body().deserialize::<(u32, String)>() else {
                    tracing::warn!("Failed to deserialize ActionInvoked signal body");
                    continue;
                };
                let target = reverse_map.lock().get(&id).cloned();
                let token = match activation_tokens.remove(&id) {
                    Some((issued_for, token)) if token_belongs_to(&issued_for, target.as_ref()) => {
                        Some(token)
                    }
                    Some((_, stale)) => {
                        tracing::debug!(
                            "Discarding activation token {stale} issued for a replaced notification"
                        );
                        None
                    }
                    None => None,
                };
                if let Some(target) = target {
                    dispatch_action(&server, &session_bus, &target, &action_key, token);
                }
            }
            Some("NotificationClosed") => {
                let Ok((id, _reason)) = message.body().deserialize::<(u32, u32)>() else {
                    tracing::warn!("Failed to deserialize NotificationClosed signal body");
                    continue;
                };
                // A replacement or removal is reported by the host as a close;
                // drop the token with the routing state it belongs to.
                activation_tokens.remove(&id);
                retire_closed_notification(&reverse_map, &active_notifications, id);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Delivers an invoked fdo action to the application that sent the notification.
///
/// This follows the portal specification and the reference implementation:
///
/// - An `app.`-prefixed action is exported by the application, so it is called
///   directly with `org.freedesktop.Application.ActivateAction`. Its parameter
///   array carries only the action's target; GApplication rejects an
///   `ActivateAction` whose parameter does not match the action's type.
/// - Any other action is not exported. The application is activated with
///   `org.freedesktop.Application.Activate`, and the portal's `ActionInvoked`
///   signal carries the action name with the target and `platform-data`.
fn dispatch_action(
    server: &ObjectServer,
    session_bus: &Connection,
    target: &Arc<NotificationTarget>,
    action_key: &str,
    activation_token: Option<String>,
) {
    let (name, action_target) = action_route(target, action_key);
    let name = String::from(name);
    // At most one target is sent. Push conditionally rather than routing a
    // 0/1-element iterator through `collect`.
    let mut parameter: Vec<Variant<'static>> =
        Vec::with_capacity(usize::from(action_target.is_some()));
    if let Some(t) = action_target {
        parameter.push(Variant::from(t.clone()));
    }
    let platform_data = platform_data(activation_token);

    // `app_id` and `portal_id` are `Arc<str>`, so the task gets refcount bumps
    // rather than copies.
    let app_id = target.key.app_id.clone();
    let portal_id = target.key.portal_id.clone();
    let server = server.clone();
    let session_bus = session_bus.clone();

    tokio::spawn(async move {
        let app = application_proxy(&session_bus, &app_id).await;

        if let Some(exported) = name.strip_prefix("app.") {
            if let Some(app) = app
                && let Err(e) = app
                    .activate_action(exported, &parameter, &platform_data)
                    .await
            {
                tracing::warn!("ActivateAction {exported} on {app_id} failed: {e}");
            }
            return;
        }

        if let Some(app) = app
            && let Err(e) = app.activate(&platform_data).await
        {
            tracing::warn!("Activate on {app_id} failed: {e}");
        }

        let mut signal_parameter = parameter;
        signal_parameter.push(Variant::from(platform_data));
        if let Ok(iface_ref) = server
            .interface::<_, Notification>(crate::core::DBUS_PATH)
            .await
        {
            let _ = Notification::action_invoked(
                iface_ref.signal_emitter(),
                &app_id,
                &portal_id,
                &name,
                &signal_parameter,
            )
            .await;
        }
    });
}

/// A proxy for the application's `org.freedesktop.Application` interface.
///
/// The object path follows the D-Bus convention for an app ID (`.` becomes
/// `/`, `-` becomes `_`). Returns `None` when the app ID is not a valid bus
/// name, which includes the empty app ID of an unsandboxed caller.
async fn application_proxy(
    session_bus: &Connection,
    app_id: &str,
) -> Option<ApplicationProxy<'static>> {
    let mut app_path = String::with_capacity(app_id.len() + 1);
    app_path.push('/');
    app_path.extend(app_id.chars().map(|c| match c {
        '.' => '/',
        '-' => '_',
        other => other,
    }));

    let builder = ApplicationProxy::builder(session_bus)
        .destination(app_id.to_owned())
        .path(app_path);
    builder
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .inspect_err(|e| tracing::error!("Could not build an application proxy: {e}"))
        .ok()
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

    /// The last `Arc<TempSoundFile>` can be dropped on a thread with no Tokio
    /// runtime. The old unconditional `spawn_blocking` panicked there, and with
    /// `panic = "abort"` that took down the whole daemon instead of just
    /// leaking a staged sound file. This must stay a plain `#[test]`.
    #[test]
    fn dropping_a_sound_file_outside_a_runtime_removes_it() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);

        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "this regression test must run outside a Tokio runtime"
        );

        let dir = std::env::temp_dir().join(format!(
            "xdp-sound-drop-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sound.snd");
        std::fs::write(&path, b"x").unwrap();

        {
            let sound = Arc::new(TempSoundFile { path: path.clone() });
            assert_eq!(Arc::strong_count(&sound), 1);
            drop(sound); // last reference: Drop runs on this non-Tokio thread
        }

        assert!(
            !path.exists(),
            "TempSoundFile::drop must unlink inline when there is no runtime"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_notification_properties() {
        let notification = Notification::new(None).await;
        assert_eq!(notification.version(), 2);

        // The property used to rebuild a ten-entry HashMap (and ten variants)
        // on every read. Warm the LazyLock, then assert a warm read allocates
        // nothing, so reverting to per-read construction fails this test.
        let _ = notification.supported_options();
        let scope = crate::alloc_probe::AllocScope::start();
        let options = notification.supported_options();
        let snap = scope.finish();
        std::hint::black_box(options);

        assert_eq!(
            snap.count, 0,
            "a cached property read must not allocate: {snap:?}"
        );
        assert!(options.contains_key("body"));
        assert!(options.contains_key("icon"));
        assert!(options.contains_key("default-action"));
    }

    #[test]
    fn test_portal_notification_deserialize() {
        use {
            std::collections::HashMap,
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let mut dict = HashMap::new();
        dict.insert("title", Variant::from("Test Title"));
        dict.insert("body", Variant::from("Test Body"));
        dict.insert("priority", Variant::from("high"));

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &dict).unwrap();
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
            None,
        );
        assert!(!retire_closed_notification(&reverse, &active, 1234));
        assert_eq!(active.lock().len(), 1);
        assert_eq!(reverse.lock().len(), 1);
    }

    fn staged_sound() -> Arc<TempSoundFile> {
        Arc::new(TempSoundFile {
            path: std::env::temp_dir().join("xdpg-apply-notify-nonexistent.snd"),
        })
    }

    /// A host can recycle a notification id for a different caller while a
    /// Notify is in flight. The stale-id guard must not delete the new owner's
    /// routing state or drop its staged sound file.
    #[test]
    fn a_recycled_id_owned_by_another_caller_is_not_retired() {
        let (active, reverse) = empty_maps();
        let sound = staged_sound();
        register_notification(
            &active,
            &reverse,
            "org.example.A",
            "1",
            42,
            HashMap::new(),
            None,
            Some(sound.clone()),
        );

        apply_notify_result(
            &active,
            &reverse,
            42,
            "org.example.B",
            "2",
            Ok::<u32, ()>(99),
            HashMap::new(),
            None,
            None,
        );

        assert_eq!(active_id(&active, "org.example.A", "1"), Some(42));
        assert_eq!(active_id(&active, "org.example.B", "2"), Some(99));
        let lock = reverse.lock();
        assert_eq!(lock.len(), 2, "the recycled id's new owner must survive");
        assert_eq!(lock[&42].key.app_id.as_ref(), "org.example.A");
        assert_eq!(
            Arc::strong_count(&sound),
            2,
            "the other caller's staged sound must not be dropped"
        );

        // Same app, different portal id: both halves of the key are load-bearing.
        let (active, reverse) = empty_maps();
        register_notification(
            &active,
            &reverse,
            "org.example.A",
            "1",
            42,
            HashMap::new(),
            None,
            None,
        );
        apply_notify_result(
            &active,
            &reverse,
            42,
            "org.example.A",
            "2",
            Ok::<u32, ()>(99),
            HashMap::new(),
            None,
            None,
        );
        assert_eq!(active_id(&active, "org.example.A", "1"), Some(42));
        assert_eq!(active_id(&active, "org.example.A", "2"), Some(99));
        assert_eq!(reverse.lock().len(), 2);
    }

    /// A failed `Notify` leaves the old host notification live, so both maps
    /// and its sound file must be left exactly as they were.
    #[test]
    fn a_failed_notify_keeps_the_old_routing_state() {
        let (active, reverse) = empty_maps();
        let sound = staged_sound();
        register_notification(
            &active,
            &reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            Some("old-action".into()),
            Some(sound.clone()),
        );

        apply_notify_result(
            &active,
            &reverse,
            42,
            "org.example.App",
            "7",
            Err::<u32, ()>(()),
            HashMap::new(),
            Some("new-action".into()),
            None,
        );

        assert_eq!(active_id(&active, "org.example.App", "7"), Some(42));
        let lock = reverse.lock();
        assert_eq!(lock.len(), 1);
        assert_eq!(lock[&42].default_action.as_deref(), Some("old-action"));
        assert_eq!(
            Arc::strong_count(&sound),
            2,
            "the sound file must not be dropped"
        );
    }

    /// A successful replacement retires the old reverse entry (and its sound
    /// file) and points the forward map at the new host ID.
    #[test]
    fn a_successful_notify_retires_the_old_state() {
        let (active, reverse) = empty_maps();
        let sound = staged_sound();
        register_notification(
            &active,
            &reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            Some("old-action".into()),
            Some(sound.clone()),
        );

        apply_notify_result(
            &active,
            &reverse,
            42,
            "org.example.App",
            "7",
            Ok::<u32, ()>(99),
            HashMap::new(),
            Some("new-action".into()),
            None,
        );

        assert_eq!(active_id(&active, "org.example.App", "7"), Some(99));
        let lock = reverse.lock();
        assert!(!lock.contains_key(&42), "the superseded entry must be gone");
        assert_eq!(lock.len(), 1);
        assert_eq!(lock[&99].default_action.as_deref(), Some("new-action"));
        drop(lock);
        assert_eq!(
            Arc::strong_count(&sound),
            1,
            "retiring the old entry must release its sound file"
        );
    }

    /// Some hosts reuse the replaced ID. Overwriting the reverse entry is then
    /// the retirement; removing the old entry too would delete the new one.
    #[test]
    fn a_successful_notify_reusing_the_id_keeps_the_new_entry() {
        let (active, reverse) = empty_maps();
        let sound = staged_sound();
        register_notification(
            &active,
            &reverse,
            "org.example.App",
            "7",
            42,
            HashMap::new(),
            Some("old-action".into()),
            Some(sound.clone()),
        );

        apply_notify_result(
            &active,
            &reverse,
            42,
            "org.example.App",
            "7",
            Ok::<u32, ()>(42),
            HashMap::new(),
            Some("new-action".into()),
            None,
        );

        assert_eq!(active_id(&active, "org.example.App", "7"), Some(42));
        let lock = reverse.lock();
        assert_eq!(lock.len(), 1);
        assert_eq!(
            lock[&42].default_action.as_deref(),
            Some("new-action"),
            "the new target must survive the reuse of its ID"
        );
        drop(lock);
        assert_eq!(
            Arc::strong_count(&sound),
            1,
            "the old sound file must be retired"
        );
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
            None,
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

    fn owned(s: &str) -> OwnedVariant {
        OwnedVariant::try_from(Variant::from(s)).expect("str to OwnedVariant is infallible")
    }

    fn button(action: &str, label: Option<&str>, target: Option<&str>) -> PortalButton {
        PortalButton {
            label: label.map(Into::into),
            action: Some(action.into()),
            target: target.map(owned),
        }
    }

    fn notification_with_default_action() -> PortalNotification {
        PortalNotification {
            default_action: Some("open".into()),
            default_action_target: Some(owned("t")),
            ..Default::default()
        }
    }

    fn target_for(notification: &PortalNotification) -> NotificationTarget {
        let (_, action_targets) = build_actions(notification);
        NotificationTarget {
            key: Arc::new(NotifKey::new("org.example.App", "1")),
            action_targets,
            default_action: notification.default_action.clone(),
            sound_file: None,
        }
    }

    #[test]
    fn no_actions_yields_an_empty_list() {
        let n = PortalNotification::default();
        let (actions, targets) = build_actions(&n);
        assert!(actions.is_empty());
        assert!(targets.is_empty());
    }

    /// The default action is activated by clicking the notification, not drawn
    /// as a button, so its fdo label is empty, as in the reference backend.
    /// Sending the action name as the label used to show `app.open` to users of
    /// servers that list every action.
    #[test]
    fn the_default_action_is_reported_under_the_reserved_id() {
        let n = notification_with_default_action();
        let (actions, targets) = build_actions(&n);
        assert_eq!(&actions[..], &["default", ""]);
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
        assert_eq!(&actions[..], &["default", ""]);
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
                button("reply", None, None),
                button("archive", Some("Archive"), None),
            ]),
            ..Default::default()
        };
        let (actions, _) = build_actions(&n);
        assert_eq!(actions.len() % 2, 0, "ids and labels must pair up");
        assert_eq!(
            &actions[..],
            &["default", "", "reply", "reply", "archive", "Archive"]
        );
    }

    #[test]
    fn an_explicit_label_overrides_the_action_id() {
        let n = PortalNotification {
            buttons: Some(vec![button("app.reply", Some("Reply"), None)]),
            ..Default::default()
        };
        let (actions, _) = build_actions(&n);
        assert_eq!(&actions[..], &["app.reply", "Reply"]);
    }

    /// An absent label falls back to the id, which keeps the list correctly
    /// paired.
    #[test]
    fn a_missing_label_falls_back_to_the_action_id() {
        let n = PortalNotification {
            buttons: Some(vec![button("app.reply", None, None)]),
            ..Default::default()
        };
        let (actions, _) = build_actions(&n);
        assert_eq!(&actions[..], &["app.reply", "app.reply"]);
    }

    /// A button the server reports back by id must have an id to report.
    #[test]
    fn a_button_without_an_action_is_skipped() {
        let n = PortalNotification {
            buttons: Some(vec![
                PortalButton {
                    label: Some("Orphan".into()),
                    ..Default::default()
                },
                button("app.reply", Some("Reply"), None),
            ]),
            ..Default::default()
        };
        let (actions, _) = build_actions(&n);
        assert_eq!(&actions[..], &["app.reply", "Reply"]);
    }

    /// The specification names the key `target`. The previous code read
    /// `action-target`, so a button's target was never delivered.
    #[test]
    fn targets_are_kept_per_action() {
        let n = PortalNotification {
            default_action: Some("open".into()),
            default_action_target: Some(owned("default-target")),
            buttons: Some(vec![
                button("reply", None, Some("reply-target")),
                button("archive", None, None),
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

    /// A click on the notification arrives as fdo's `default` id and must be
    /// delivered as the app's own `default-action`, with its target. The
    /// previous code delivered the literal name `default`.
    #[test]
    fn the_default_id_routes_to_the_default_action() {
        let target = target_for(&PortalNotification {
            default_action: Some("app.open".into()),
            default_action_target: Some(owned("thread-7")),
            ..Default::default()
        });
        let (name, action_target) = action_route(&target, "default");
        assert_eq!(name, "app.open");
        assert_eq!(action_target, Some(&owned("thread-7")));
    }

    #[test]
    fn a_button_id_routes_to_its_own_action() {
        let target = target_for(&PortalNotification {
            default_action: Some("app.open".into()),
            buttons: Some(vec![button("app.reply", None, Some("thread-7"))]),
            ..Default::default()
        });
        let (name, action_target) = action_route(&target, "app.reply");
        assert_eq!(name, "app.reply");
        assert_eq!(action_target, Some(&owned("thread-7")));
    }

    /// With no `default-action`, `default` can only be a button named that.
    #[test]
    fn a_button_named_default_keeps_its_name() {
        let target = target_for(&PortalNotification {
            buttons: Some(vec![button("default", Some("OK"), None)]),
            ..Default::default()
        });
        assert_eq!(action_route(&target, "default"), ("default", None));
    }

    #[test]
    fn platform_data_carries_the_token_under_both_keys() {
        let data = platform_data(Some("token-1".into()));
        assert!(
            data.capacity() >= 2,
            "both keys are known up front, so the map must be pre-sized"
        );
        assert_eq!(data["activation-token"], Variant::from("token-1"));
        assert_eq!(data["desktop-startup-id"], Variant::from("token-1"));
        assert!(platform_data(None).is_empty());
    }

    /// The old `Vec<String>` conversion rejected an `as` icon if any element
    /// was not a string. The borrowed replacement must keep that invariant,
    /// including when the bad element is not the first one.
    #[test]
    fn themed_icon_rejects_a_malformed_trailing_name() {
        use zbus::wire::Array;

        let good: Array<'_> = vec![Variant::from("first"), Variant::from("second")].into();
        assert_eq!(
            themed_icon_name(&Variant::Array(good)).as_deref(),
            Some("first")
        );

        let bad: Array<'_> = vec![Variant::from("first"), Variant::from(42u32)].into();
        assert_eq!(
            themed_icon_name(&Variant::Array(bad)),
            None,
            "a malformed trailing name must reject the whole icon"
        );

        let empty: Array<'_> = Vec::<Variant<'_>>::new().into();
        assert_eq!(themed_icon_name(&Variant::Array(empty)), None);
    }

    /// A token received before a replacement that reuses the host ID belongs
    /// to the old notification and must not activate the new one's action.
    #[test]
    fn a_token_from_a_replaced_notification_is_rejected() {
        let sound = Arc::new(TempSoundFile {
            path: std::env::temp_dir().join("xdpg-token-pending-sound.snd"),
        });
        let old = Arc::new(NotificationTarget {
            key: Arc::new(NotifKey::new("org.example.App", "1")),
            action_targets: HashMap::new(),
            default_action: Some("app.old".into()),
            sound_file: Some(sound.clone()),
        });
        let new = Arc::new(target_for(&PortalNotification {
            default_action: Some("app.new".into()),
            ..Default::default()
        }));

        let issued_for = Arc::downgrade(&old);
        assert!(token_belongs_to(&issued_for, Some(&old)));
        assert!(!token_belongs_to(&issued_for, Some(&new)));
        assert!(!token_belongs_to(&issued_for, None));

        // Once the old target is gone the weak identity is stale, even if a
        // new target happens to be registered at the same host id.
        drop(old);
        assert!(issued_for.upgrade().is_none());
        assert!(!token_belongs_to(&issued_for, Some(&new)));
        assert_eq!(
            Arc::strong_count(&sound),
            1,
            "a pending Weak token must not pin the retired notification's sound file"
        );
    }

    /// The regression this guards: the list used to be built as a
    /// `Vec<String>`, allocating one `String` per id and per label -- including
    /// for labels, which were already `&str` -- and the whole vector was then
    /// dropped after reborrowing it as `Vec<&str>`. Two buttons with no labels
    /// and no targets now fit entirely in the `SmallVec`'s inline buffer, so
    /// building the list costs zero heap allocations.
    #[test]
    fn building_the_list_does_not_allocate_a_string_per_action() {
        let n = PortalNotification {
            default_action: Some("open".into()),
            buttons: Some(vec![
                button("reply", None, None),
                button("archive", None, None),
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
            !actions.spilled(),
            "six &str slots fit inline in [&str; 8], so no heap spill is allowed"
        );
        assert!(
            targets.is_empty(),
            "no targets were requested, so none may be cloned"
        );
        assert_eq!(
            snap.count, 0,
            "inline SmallVec and an empty target map must not allocate, got {snap:?}"
        );
    }

    /// Buttons without an `action` are skipped by `build_actions`. Counting
    /// them when sizing the SmallVec used to force a heap spill even though no
    /// slots were actually pushed.
    #[test]
    fn actionless_buttons_do_not_size_the_action_list() {
        let n = PortalNotification {
            buttons: Some(
                (0..5)
                    .map(|_| PortalButton {
                        action: None,
                        label: None,
                        target: None,
                    })
                    .collect(),
            ),
            ..Default::default()
        };

        let scope = crate::alloc_probe::AllocScope::start();
        let (actions, targets) = build_actions(&n);
        let snap = scope.finish();
        std::hint::black_box(&actions);
        std::hint::black_box(&targets);

        assert!(actions.is_empty());
        assert!(targets.is_empty());
        assert!(
            !actions.spilled(),
            "five skipped buttons must not spill the SmallVec"
        );
        assert_eq!(
            snap.count, 0,
            "no slots were pushed, so nothing may allocate: {snap:?}"
        );
    }
}

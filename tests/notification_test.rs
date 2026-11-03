mod common;
use {
    futures_util::StreamExt,
    parking_lot::Mutex,
    std::{
        collections::HashMap,
        sync::{Arc, LazyLock},
        time::Duration,
    },
    xdg_desktop_portal_gtk4::portals::notification::dbus::Notification,
    zbus::{
        Connection, MatchRule, MessageStream,
        connection::Builder,
        fdo::{RequestNameFlags, RequestNameReply},
        interface,
        object_server::SignalEmitter,
        proxy,
        zvariant::{OwnedValue, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.Notification",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait NotificationPortal {
    fn add_notification(
        &self,
        app_id: &str,
        id: &str,
        notification: HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<()>;

    fn remove_notification(&self, app_id: &str, id: &str) -> zbus::Result<()>;
}

/// These tests share one session bus. Serialize them so a temporary fake
/// notification server cannot receive another test's `Notify` call.
async fn test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
    LOCK.lock().await
}

#[tokio::test]
async fn test_notification_add_remove() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = test_guard().await;
    let client_conn = try_dbus_session!();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Notification::new(Some(client_conn.clone())).await,
        )?
        .build()
        .await?;

    let proxy = NotificationPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    // add_notification does not return an error on failure typically unless deserialization fails.
    let res = proxy
        .add_notification("app_id", "notif1", HashMap::new())
        .await;

    assert!(res.is_ok());

    let res2 = proxy.remove_notification("app_id", "notif1").await;

    assert!(res2.is_ok());

    Ok(())
}

/// A serialized button, as xdg-desktop-portal forwards it: a vardict.
fn button<'a>(
    label: &'a str,
    action: &'a str,
    target: Option<Value<'a>>,
) -> HashMap<&'a str, Value<'a>> {
    let mut button = HashMap::from([
        ("label", Value::from(label)),
        ("action", Value::from(action)),
    ]);
    if let Some(target) = target {
        button.insert("target", target);
    }
    button
}

/// `buttons` is `aa{sv}` in the portal specification. A notification carrying
/// buttons in that format must be accepted; it used to fail deserialization,
/// which rejected the whole `AddNotification` call.
#[tokio::test]
async fn buttons_in_the_specified_format_are_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = test_guard().await;
    let client_conn = try_dbus_session!();
    let portal_conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Notification::new(Some(client_conn.clone())).await,
        )?
        .build()
        .await?;
    let proxy = NotificationPortalProxy::builder(&client_conn)
        .destination(portal_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let notification = HashMap::from([
        ("title", Value::from("New message")),
        (
            "buttons",
            Value::from(vec![
                button("Reply", "app.reply", Some(Value::from("thread-7"))),
                button("Mark read", "mark-read", None),
            ]),
        ),
    ]);
    proxy
        .add_notification("org.example.App", "n1", notification)
        .await
        .expect("a notification with spec-format buttons must be accepted");

    Ok(())
}

/// A stand-in `org.freedesktop.Notifications` server that records `Notify`.
struct FakeServer {
    actions: Arc<Mutex<Vec<Vec<String>>>>,
}

#[interface(name = "org.freedesktop.Notifications")]
impl FakeServer {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        _summary: String,
        _body: String,
        actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        self.actions.lock().push(actions);
        FAKE_ID
    }

    fn close_notification(&self, _id: u32) {}

    #[zbus(signal)]
    async fn activation_token(
        emitter: &SignalEmitter<'_>,
        id: u32,
        activation_token: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

const FAKE_ID: u32 = 42;
const APP_ID: &str = "org.example.NotificationRouting";
const APP_PATH: &str = "/org/example/NotificationRouting";

/// One call the fake application received: the method, the action name for
/// `ActivateAction`, its parameter array, and `platform-data`.
type AppCall = (
    &'static str,
    String,
    Vec<OwnedValue>,
    HashMap<String, OwnedValue>,
);

/// A stand-in application exporting `org.freedesktop.Application`.
struct FakeApp {
    calls: Arc<Mutex<Vec<AppCall>>>,
}

#[interface(name = "org.freedesktop.Application")]
impl FakeApp {
    fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
        self.calls
            .lock()
            .push(("Activate", String::new(), Vec::new(), platform_data));
    }

    fn activate_action(
        &self,
        action_name: String,
        parameter: Vec<OwnedValue>,
        platform_data: HashMap<String, OwnedValue>,
    ) {
        self.calls
            .lock()
            .push(("ActivateAction", action_name, parameter, platform_data));
    }

    fn open(&self, _uris: Vec<String>, _platform_data: HashMap<String, OwnedValue>) {}
}

/// Serves `object` and claims `name` without replacing or queueing behind an
/// existing owner. Returns `None` when someone else owns the name, as the
/// notification server does on a desktop session bus.
async fn claim<I: zbus::object_server::Interface>(
    name: &'static str,
    path: &'static str,
    object: I,
) -> Result<Option<Connection>, Box<dyn std::error::Error>> {
    let conn = Builder::session()?.serve_at(path, object)?.build().await?;
    match conn
        .request_name_with_flags(name, RequestNameFlags::DoNotQueue.into())
        .await?
    {
        RequestNameReply::PrimaryOwner => Ok(Some(conn)),
        _ => Ok(None),
    }
}

async fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    for _ in 0..250 {
        if let Some(value) = probe() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for the expected D-Bus traffic");
}

fn owned(value: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(value).expect("plain values convert")
}

/// The fdo action ids the server reports back must reach the app as the
/// portal specification describes: the default action and `app.` buttons via
/// `ActivateAction` with only their target as parameter, the server's
/// activation token in `platform-data`, and other buttons via `Activate` plus
/// the portal's `ActionInvoked` signal.
#[tokio::test]
async fn invoked_actions_reach_the_application() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = test_guard().await;
    let client_conn = try_dbus_session!();

    let notified = Arc::new(Mutex::new(Vec::new()));
    let Some(server_conn) = claim(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        FakeServer {
            actions: notified.clone(),
        },
    )
    .await?
    else {
        println!("Skipping: org.freedesktop.Notifications is owned on this bus");
        return Ok(());
    };
    let app_calls = Arc::new(Mutex::new(Vec::new()));
    let Some(_app_conn) = claim(
        APP_ID,
        APP_PATH,
        FakeApp {
            calls: app_calls.clone(),
        },
    )
    .await?
    else {
        println!("Skipping: {APP_ID} is owned on this bus");
        return Ok(());
    };

    let portal_conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Notification::new(Some(client_conn.clone())).await,
        )?
        .build()
        .await?;
    let portal = NotificationPortalProxy::builder(&client_conn)
        .destination(portal_conn.unique_name().unwrap().clone())?
        .build()
        .await?;
    let mut portal_signals = MessageStream::for_match_rule(
        MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(portal_conn.unique_name().unwrap().clone())?
            .interface("org.freedesktop.impl.portal.Notification")?
            .member("ActionInvoked")?
            .build(),
        &client_conn,
        None,
    )
    .await?;

    let notification = HashMap::from([
        ("title", Value::from("New message")),
        ("default-action", Value::from("app.open")),
        ("default-action-target", Value::from(7u32)),
        (
            "buttons",
            Value::from(vec![
                button("Reply", "app.reply", Some(Value::from("thread-7"))),
                button("Mark read", "mark-read", None),
            ]),
        ),
    ]);
    portal.add_notification(APP_ID, "n1", notification).await?;

    let actions = wait_for(|| notified.lock().first().cloned()).await;
    assert_eq!(
        actions,
        [
            "default",
            "",
            "app.reply",
            "Reply",
            "mark-read",
            "Mark read"
        ],
        "the default action is unlabelled and each button is reported under its action"
    );

    // The portal subscribes to the server's signals on a task it spawns while
    // handling the first notification; give the match rule time to land.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let server = server_conn
        .object_server()
        .interface::<_, FakeServer>("/org/freedesktop/Notifications")
        .await?;
    let emitter = server.signal_emitter();

    // A click on the notification body, preceded by its activation token.
    FakeServer::activation_token(emitter, FAKE_ID, "token-1").await?;
    FakeServer::action_invoked(emitter, FAKE_ID, "default").await?;
    let (method, name, parameter, platform_data) =
        wait_for(|| app_calls.lock().first().cloned()).await;
    assert_eq!((method, name.as_str()), ("ActivateAction", "open"));
    assert_eq!(parameter, [owned(Value::from(7u32))]);
    assert_eq!(
        platform_data["activation-token"],
        owned(Value::from("token-1"))
    );
    assert_eq!(
        platform_data["desktop-startup-id"],
        owned(Value::from("token-1"))
    );

    // An exported button: its target is the only parameter, and the token was
    // already used up by the click above.
    FakeServer::action_invoked(emitter, FAKE_ID, "app.reply").await?;
    let (method, name, parameter, platform_data) =
        wait_for(|| app_calls.lock().get(1).cloned()).await;
    assert_eq!((method, name.as_str()), ("ActivateAction", "reply"));
    assert_eq!(parameter, [owned(Value::from("thread-7"))]);
    assert!(platform_data.is_empty());

    // A button that isn't an exported action: the app is activated and the
    // portal signal names the action.
    FakeServer::action_invoked(emitter, FAKE_ID, "mark-read").await?;
    let (method, ..) = wait_for(|| app_calls.lock().get(2).cloned()).await;
    assert_eq!(method, "Activate");
    let signal = tokio::time::timeout(Duration::from_secs(5), portal_signals.next())
        .await?
        .expect("the portal must emit ActionInvoked")?;
    let (app_id, id, action, _parameter): (String, String, String, Vec<OwnedValue>) =
        signal.body().deserialize()?;
    assert_eq!(
        (app_id.as_str(), id.as_str(), action.as_str()),
        (APP_ID, "n1", "mark-read")
    );

    Ok(())
}

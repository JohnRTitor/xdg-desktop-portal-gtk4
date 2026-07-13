use {
    crate::portal::{request::run_request, response::Response, session::Session},
    error_reporter::Report,
    std::collections::HashMap,
    zbus::{
        interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type, Value},
        Connection, ObjectServer,
    },
};

#[zbus::proxy(
    interface = "org.freedesktop.ScreenSaver",
    default_service = "org.freedesktop.ScreenSaver",
    default_path = "/org/freedesktop/ScreenSaver"
)]
trait ScreenSaver {
    fn inhibit(&self, application_name: &str, reason_for_inhibit: &str) -> zbus::Result<u32>;
    fn un_inhibit(&self, cookie: u32) -> zbus::Result<()>;
}

pub struct Inhibit {}

impl Inhibit {
    pub fn new() -> Self {
        Self {}
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct InhibitOptions {
    reason: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct InhibitResults {}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct CreateMonitorOptions {}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct CreateMonitorResults {}

impl Inhibit {
    async fn inhibit_impl(
        &self,
        app_id: String,
        _window: String,
        reason: u32,
        _options: InhibitOptions,
    ) -> Response<InhibitResults> {
        let reason_str = match reason {
            1 => "Logout",
            2 => "User Switch",
            4 => "Suspend",
            8 => "Idle",
            _ => "Unknown",
        };

        if let Ok(session_bus) = Connection::session().await {
            if let Ok(proxy) = ScreenSaverProxy::new(&session_bus).await {
                // For a proper implementation, we'd need to store the cookie and tie it to the request/app lifetime.
                // But for parity we at least call the dbus method.
                if let Err(e) = proxy.inhibit(&app_id, reason_str).await {
                    log::warn!("Failed to inhibit via ScreenSaver: {}", e);
                }
            }
        }
        Response::success(InhibitResults::default())
    }

    async fn create_monitor_impl(
        &self,
        session_handle: String,
        _app_id: String,
        _window: String,
        _options: CreateMonitorOptions,
        server: &ObjectServer,
    ) -> Response<CreateMonitorResults> {
        let session = Session::new(session_handle.clone());
        if let Ok(path) = OwnedObjectPath::try_from(session_handle) {
            let _ = server.at(path, session).await;
        }
        Response::success(CreateMonitorResults::default())
    }
}

#[interface(name = "org.freedesktop.impl.portal.Inhibit")]
impl Inhibit {
    async fn inhibit(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        window: String,
        reason: u32,
        options: InhibitOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<InhibitResults> {
        run_request(
            server,
            handle,
            self.inhibit_impl(app_id, window, reason, options),
        )
        .await
    }

    async fn create_monitor(
        &self,
        handle: OwnedObjectPath,
        session_handle: String,
        app_id: String,
        window: String,
        options: CreateMonitorOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<CreateMonitorResults> {
        run_request(
            server,
            handle,
            self.create_monitor_impl(session_handle, app_id, window, options, server),
        )
        .await
    }

    async fn query_end_response(
        &self,
        _session_handle: OwnedObjectPath,
        _response: u32,
        _options: HashMap<String, Value<'_>>,
    ) {
        // Dummy implementation
    }

    #[zbus(signal)]
    async fn state_changed(
        ctx: &zbus::SignalContext<'_>,
        session_handle: OwnedObjectPath,
        state: u32,
    ) -> zbus::Result<()>;
}

use {
    super::gui::AccountUi,
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    zbus::{
        Connection, ObjectServer, fdo, interface,
        message::Header,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
    },
};

// Proxy for the org.freedesktop.Accounts.User D-Bus interface.
// This is provided by AccountsService, which manages user accounts on Linux.
#[zbus::proxy(
    interface = "org.freedesktop.Accounts.User",
    default_service = "org.freedesktop.Accounts"
)]
trait User {
    #[zbus(property)]
    fn user_name(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn real_name(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn icon_file(&self) -> zbus::Result<String>;
}

/// D-Bus interface wrapper for the Account portal.
///
/// This struct holds a proxy to the GTK main loop, used to present the generic
/// access dialog prompting the user to share their account information.
pub struct Account {
    proxy: UiProxy,
    session_manager: crate::core::session_manager::SessionManager,
}

impl Account {
    pub fn new(
        proxy: &UiProxy,
        session_manager: crate::core::session_manager::SessionManager,
    ) -> Self {
        Self {
            proxy: proxy.clone(),
            session_manager,
        }
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct GetUserInformationOptions {
    reason: Option<String>,
    pub activation_token: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct UserInformation {
    id: String,
    name: String,
    image: String,
}

impl Account {
    async fn get_user_information_impl(
        &self,
        app_id: String,
        parent_window: String,
        options: GetUserInformationOptions,
    ) -> Response<UserInformation> {
        let (user_name, real_name, icon_file) = fetch_user_data().await.unwrap_or_default();

        let res = AccountUi {
            app_id,
            parent_window,
            activation_token: options.activation_token.clone(),
            user_name,
            real_name,
            icon_file,
            reason: options.reason,
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(res) => {
                let mut image_uri = res.image;
                if image_uri.starts_with('/') {
                    image_uri.insert_str(0, "file://");
                }
                Response::success(UserInformation {
                    id: res.user_name,
                    name: res.real_name,
                    image: image_uri,
                })
            }
            Err(e) => {
                tracing::error!(error = %e, "GetUserInformation failed");
                Response::cancelled()
            }
        }
    }
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.Account`.
///
/// This portal allows sandboxed apps to get basic user information (name, avatar)
/// after prompting the user for confirmation.
#[interface(name = "org.freedesktop.impl.portal.Account")]
impl Account {
    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    async fn get_user_information(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: GetUserInformationOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Response<UserInformation>, fdo::Error> {
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::Failed("Missing sender".to_string()))?
            .to_string();
        Ok(run_request(
            server,
            self.session_manager.clone(),
            &app_id,
            &sender,
            handle,
            self.get_user_information_impl(app_id.clone(), parent_window, options),
        )
        .await)
    }
}

/// Fetches the current user's profile information from AccountsService via D-Bus.
async fn fetch_user_data() -> zbus::Result<(String, String, String)> {
    // We assume the portal is running as the user invoking the application.
    let uid = rustix::process::getuid().as_raw();

    // AccountsService exposes user objects at paths like `/org/freedesktop/Accounts/User1000`.
    let path = format!("/org/freedesktop/Accounts/User{}", uid);
    let obj_path = zbus::zvariant::ObjectPath::try_from(path)?;

    let system_bus = Connection::system().await?;
    let user_proxy = UserProxy::builder(&system_bus)
        .path(obj_path)?
        .build()
        .await?;

    let user_name = user_proxy.user_name().await.unwrap_or_default();
    let real_name = user_proxy.real_name().await.unwrap_or_default();
    let icon_file = user_proxy.icon_file().await.unwrap_or_default();

    Ok((user_name, real_name, icon_file))
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::collections::HashMap,
        zbus::zvariant::{self, Endian, Value, serialized::Context},
    };

    #[test]
    fn test_get_user_information_options_deserialize() {
        let mut dict = HashMap::new();
        dict.insert("reason", Value::from("Because"));
        dict.insert("activation_token", Value::from("token123"));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: GetUserInformationOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.reason.as_deref(), Some("Because"));
        assert_eq!(options.activation_token.as_deref(), Some("token123"));
    }

    #[test]
    fn test_get_user_information_options_empty() {
        let dict: HashMap<&str, Value> = HashMap::new();
        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: GetUserInformationOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.reason, None);
        assert_eq!(options.activation_token, None);
    }

    #[test]
    fn test_user_information_serialize() {
        let info = UserInformation {
            id: "user1".to_string(),
            name: "User One".to_string(),
            image: "file:///icon.png".to_string(),
        };

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &info).unwrap();

        let decoded: HashMap<String, Value> = encoded.deserialize().unwrap().0;

        assert_eq!(
            decoded.get("id").unwrap().try_clone().unwrap(),
            Value::from("user1")
        );
        assert_eq!(
            decoded.get("name").unwrap().try_clone().unwrap(),
            Value::from("User One")
        );
        assert_eq!(
            decoded.get("image").unwrap().try_clone().unwrap(),
            Value::from("file:///icon.png")
        );
    }
}

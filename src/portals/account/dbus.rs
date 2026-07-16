use {
    super::gui::AccountUi,
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    zbus::{
        Connection, ObjectServer, interface,
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

pub struct Account {
    proxy: UiProxy,
}

impl Account {
    pub fn new(proxy: &UiProxy) -> Self {
        Self {
            proxy: proxy.clone(),
        }
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct GetUserInformationOptions {
    reason: Option<String>,
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
                    image_uri = format!("file://{}", image_uri);
                }
                Response::success(UserInformation {
                    id: res.user_name,
                    name: res.real_name,
                    image: image_uri,
                })
            }
            Err(e) => {
                log::error!("GetUserInformation failed: {}", anyhow::Error::new(e));
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
    async fn get_user_information(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: GetUserInformationOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<UserInformation> {
        run_request(
            server,
            handle,
            self.get_user_information_impl(app_id, parent_window, options),
        )
        .await
    }
}

/// Fetches the current user's profile information from AccountsService via D-Bus.
async fn fetch_user_data() -> zbus::Result<(String, String, String)> {
    // We assume the portal is running as the user invoking the application.
    let uid = unsafe { libc::getuid() };
    
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

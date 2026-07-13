use {
    crate::{
        gui::{account::AccountUi, UiProxy},
        portal::{request::run_request, response::Response},
    },
    error_reporter::Report,
    zbus::{
        interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
        Connection, ObjectServer,
    },
};

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
        let (mut user_name, mut real_name, mut icon_file) = (String::new(), String::new(), String::new());

        let uid = unsafe { libc::getuid() };
        let path = format!("/org/freedesktop/Accounts/User{}", uid);
        let obj_path = match zbus::zvariant::ObjectPath::try_from(path) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to parse object path in account portal: {}", e);
                return Response::cancelled();
            }
        };
        
        if let Ok(system_bus) = Connection::system().await {
            if let Ok(user_proxy) = UserProxy::builder(&system_bus).path(obj_path).unwrap_or_else(|_| unreachable!("Valid path was provided")).build().await {
                if let Ok(u) = user_proxy.user_name().await { user_name = u; }
                if let Ok(r) = user_proxy.real_name().await { real_name = r; }
                if let Ok(i) = user_proxy.icon_file().await { icon_file = i; }
            }
        }

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
            Ok(res) => Response::success(UserInformation {
                id: res.user_name,
                name: res.real_name,
                image: res.image,
            }),
            Err(e) => {
                log::error!("GetUserInformation failed: {}", Report::new(e));
                Response::cancelled()
            }
        }
    }
}

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

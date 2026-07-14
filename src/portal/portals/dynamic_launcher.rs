use {
    crate::{
        gui::{dynamic_launcher::DynamicLauncherUi, UiProxy},
        portal::{request::run_request, response::Response},
    },
    uuid::Uuid,
    zbus::{
        interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type, Value, OwnedValue},
        ObjectServer,
    },
};

pub struct DynamicLauncher {
    proxy: UiProxy,
}

impl DynamicLauncher {
    pub fn new(proxy: &UiProxy) -> Self {
        Self {
            proxy: proxy.clone(),
        }
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct PrepareInstallOptions {
    modal: Option<bool>,
    launcher_type: Option<u32>,
    target: Option<String>,
    editable_name: Option<bool>,
    editable_icon: Option<bool>,
}

#[derive(SerializeDict, Type, Debug)]
#[zvariant(signature = "dict")]
struct PrepareInstallResults {
    name: String,
    icon_v: OwnedValue,
    token: String,
}

impl Default for PrepareInstallResults {
    fn default() -> Self {
        Self {
            name: String::new(),
            icon_v: OwnedValue::try_from(Value::Str("".into())).unwrap_or_else(|_| unreachable!("OOM")),
            token: String::new(),
        }
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct RequestInstallTokenOptions {
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct RequestInstallTokenResults {
    token: String,
}

impl DynamicLauncher {
    async fn prepare_install_impl(
        &self,
        app_id: String,
        parent_window: String,
        name: String,
        icon_v: OwnedValue,
        options: PrepareInstallOptions,
    ) -> Response<PrepareInstallResults> {
        let icon_name = if let Ok(s) = <&str>::try_from(&*icon_v) {
            Some(s.to_string())
        } else {
            None
        };

        let res = DynamicLauncherUi {
            app_id,
            parent_window,
            name,
            editable_name: options.editable_name.unwrap_or(false),
            icon_name,
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(res) => {
                let token = Uuid::new_v4().to_string();
                Response::success(PrepareInstallResults {
                    name: res.name,
                    icon_v, // pass through
                    token,
                })
            }
            Err(e) => {
                log::error!("PrepareInstall failed: {}", anyhow::Error::new(e));
                Response::cancelled()
            }
        }
    }

    async fn request_install_token_impl(
        &self,
        _app_id: String,
        _options: RequestInstallTokenOptions,
    ) -> Response<RequestInstallTokenResults> {
        let token = Uuid::new_v4().to_string();
        Response::success(RequestInstallTokenResults { token })
    }
}

#[interface(name = "org.freedesktop.impl.portal.DynamicLauncher")]
impl DynamicLauncher {
    async fn prepare_install(
        &self,
        _handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        name: String,
        icon_v: Value<'_>,
        options: PrepareInstallOptions,
        #[zbus(object_server)] _server: &ObjectServer,
    ) -> Response<PrepareInstallResults> {
        let icon_owned = match OwnedValue::try_from(icon_v) {
            Ok(v) => v,
            Err(e) => {
                log::error!("Failed to allocate OwnedValue: {}", e);
                return Response::cancelled();
            }
        };
        self.prepare_install_impl(app_id, parent_window, name, icon_owned, options).await
    }

    async fn request_install_token(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        options: RequestInstallTokenOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<RequestInstallTokenResults> {
        run_request(
            server,
            handle,
            self.request_install_token_impl(app_id, options),
        )
        .await
    }
}

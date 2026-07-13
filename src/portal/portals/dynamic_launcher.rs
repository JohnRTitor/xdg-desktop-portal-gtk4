use {
    crate::{
        gui::{dynamic_launcher::DynamicLauncherUi, UiProxy},
        portal::{request::run_request, response::Response},
    },
    error_reporter::Report,
    uuid::Uuid,
    zbus::{
        interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type, Value},
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

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct PrepareInstallResults<'a> {
    name: String,
    icon_v: Value<'a>,
    token: String,
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
    async fn prepare_install_impl<'a>(
        &self,
        app_id: String,
        parent_window: String,
        name: String,
        icon_v: Value<'a>,
        options: PrepareInstallOptions,
    ) -> Response<PrepareInstallResults<'a>> {
        let icon_name = if let Ok(s) = icon_v.downcast_ref::<str>() {
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
                log::error!("PrepareInstall failed: {}", Report::new(e));
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
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        name: String,
        icon_v: Value<'_>,
        options: PrepareInstallOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<PrepareInstallResults<'static>> {
        // zbus currently requires static lifetime for return types from async functions if they own data
        // For simplicity we will just clone the icon_v into a static or return a static empty value if complex.
        // Actually, Response<T> where T has lifetime 'a is tricky.
        // Let's just create an owned Value.
        let icon_owned = icon_v.to_owned();
        
        let res = self.prepare_install_impl(app_id, parent_window, name, icon_owned.clone(), options).await;
        
        match res {
            Response(code, Some(results)) => {
                Response(code, Some(PrepareInstallResults {
                    name: results.name,
                    icon_v: icon_owned,
                    token: results.token,
                }))
            },
            Response(code, None) => Response(code, None),
        }
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

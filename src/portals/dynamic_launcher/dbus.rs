use {
    crate::{
        gui::UiProxy,
        core::{request::run_request, response::Response},
    },
    super::gui::DynamicLauncherUi,
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
    #[zvariant(rename = "icon")]
    icon_v: OwnedValue,
}

impl Default for PrepareInstallResults {
    fn default() -> Self {
        Self {
            name: String::new(),
            icon_v: OwnedValue::try_from(Value::Str("".into())).unwrap_or_else(|_| unreachable!("OOM")),
        }
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct RequestInstallTokenOptions {
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
        let (icon_name, icon_data) = parse_icon(&icon_v);

        let res = DynamicLauncherUi {
            app_id,
            parent_window,
            name,
            editable_name: options.editable_name.unwrap_or(false),
            icon_name,
            icon_data,
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(res) => {
                Response::success(PrepareInstallResults {
                    name: res.name,
                    icon_v, // pass through
                })
            }
            Err(e) => {
                log::error!("PrepareInstall failed: {}", anyhow::Error::new(e));
                Response::cancelled()
            }
        }
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
    ) -> Response<PrepareInstallResults> {
        let icon_owned = match OwnedValue::try_from(icon_v) {
            Ok(v) => v,
            Err(e) => {
                log::error!("Failed to allocate OwnedValue: {}", e);
                return Response::cancelled();
            }
        };
        run_request(
            server,
            handle,
            self.prepare_install_impl(app_id, parent_window, name, icon_owned, options)
        )
        .await
    }

    async fn request_install_token(
        &self,
        app_id: String,
        _options: RequestInstallTokenOptions,
    ) -> u32 {
        // Blanket allow certain apps to create app entries. Ported from GTK portal.
        let allowed_ids = [
            "org.gnome.Software",
            "org.gnome.SoftwareDevel",
            "io.elementary.appcenter",
            "org.kde.discover",
        ];

        if allowed_ids.contains(&app_id.as_str()) {
            0 // Allowed
        } else {
            2 // Access denied
        }
    }

    #[zbus(property, name = "SupportedLauncherTypes")]
    fn supported_launcher_types(&self) -> u32 {
        3 // 1 (Application) | 2 (Webapp)
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

fn parse_icon(icon_v: &OwnedValue) -> (Option<String>, Option<Vec<u8>>) {
    if let Ok(s) = <&str>::try_from(&**icon_v) {
        return (Some(s.to_string()), None);
    }

    let Ok(structure) = zbus::zvariant::Structure::try_from(&**icon_v) else {
        return (None, None);
    };

    let fields = structure.fields();
    if fields.len() != 2 {
        return (None, None);
    }

    let Ok(type_str) = <&str>::try_from(&fields[0]) else {
        return (None, None);
    };

    match type_str {
        "bytes" => {
            let Ok(v) = <zbus::zvariant::Value>::try_from(&fields[1]) else { return (None, None) };
            let Ok(bytes) = <Vec<u8>>::try_from(v) else { return (None, None) };
            (None, Some(bytes))
        }
        "themed" => {
            let Ok(v) = <zbus::zvariant::Value>::try_from(&fields[1]) else { return (None, None) };
            let Ok(names) = <Vec<String>>::try_from(v) else { return (None, None) };
            if !names.is_empty() {
                (Some(names[0].clone()), None)
            } else {
                (None, None)
            }
        }
        _ => (None, None),
    }
}

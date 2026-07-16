use {
    super::gui::DynamicLauncherUi,
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    zbus::{
        ObjectServer, interface,
        zvariant::{DeserializeDict, OwnedObjectPath, OwnedValue, SerializeDict, Type, Value},
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
    pub activation_token: Option<String>,
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
            icon_v: OwnedValue::try_from(Value::Str("".into()))
                .unwrap_or_else(|_| unreachable!("OOM")),
        }
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct RequestInstallTokenOptions {}

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
            activation_token: options.activation_token.clone(),
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

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.DynamicLauncher`.
///
/// This portal allows applications to create desktop entries (launchers) dynamically,
/// for example, for web apps or installed games.
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
            self.prepare_install_impl(app_id, parent_window, name, icon_owned, options),
        )
        .await
    }

    async fn request_install_token(
        &self,
        app_id: String,
        _options: RequestInstallTokenOptions,
    ) -> u32 {
        // Blanket allow certain trusted software centers to create app entries without
        // prompting the user. This matches the behavior of the GTK and GNOME portals.
        let allowed_ids = [
            "org.gnome.Software",
            "org.gnome.SoftwareDevel",
            "io.elementary.appcenter",
            "org.kde.discover",
        ];

        if allowed_ids.contains(&app_id.as_str()) {
            0 // Allowed
        } else {
            2 // Access denied (forces a fallback to PrepareInstall UI flow)
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

/// Parses the `icon_v` variant passed by the portal frontend.
///
/// The portal specification allows the icon to be passed as a string (icon name),
/// a byte array (serialized image data), or a themed icon struct.
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
            let Ok(v) = <zbus::zvariant::Value>::try_from(&fields[1]) else {
                return (None, None);
            };
            let Ok(bytes) = <Vec<u8>>::try_from(v) else {
                return (None, None);
            };
            (None, Some(bytes))
        }
        "themed" => {
            let Ok(v) = <zbus::zvariant::Value>::try_from(&fields[1]) else {
                return (None, None);
            };
            let Ok(names) = <Vec<String>>::try_from(v) else {
                return (None, None);
            };
            if !names.is_empty() {
                (Some(names[0].clone()), None)
            } else {
                (None, None)
            }
        }
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use {super::*, zbus::zvariant::Value};

    #[test]
    fn test_parse_icon_string() {
        let v = OwnedValue::try_from(Value::Str("my-icon".into())).unwrap();
        let (name, data) = parse_icon(&v);
        assert_eq!(name, Some("my-icon".to_string()));
        assert_eq!(data, None);
    }

    #[test]
    fn test_parse_icon_bytes() {
        let v = OwnedValue::try_from(Value::from(("bytes", vec![1u8, 2, 3, 4]))).unwrap();
        let (name, data) = parse_icon(&v);
        assert_eq!(name, None);
        assert_eq!(data, Some(vec![1, 2, 3, 4]));
    }

    #[test]
    fn test_parse_icon_themed() {
        let v = OwnedValue::try_from(Value::from(("themed", vec!["icon1", "icon2"]))).unwrap();
        let (name, data) = parse_icon(&v);
        assert_eq!(name, Some("icon1".to_string()));
        assert_eq!(data, None);
    }

    #[test]
    fn test_parse_icon_themed_empty() {
        let empty_vec: Vec<String> = vec![];
        let v = OwnedValue::try_from(Value::from(("themed", empty_vec))).unwrap();
        let (name, data) = parse_icon(&v);
        assert_eq!(name, None);
        assert_eq!(data, None);
    }

    #[test]
    fn test_parse_icon_invalid_structure() {
        let type_val = Value::Str("unknown".into());
        let dummy_val = Value::from(123);
        let variant_val = Value::from(dummy_val);
        let v = OwnedValue::try_from(Value::from((type_val, variant_val))).unwrap();
        let (name, data) = parse_icon(&v);
        assert_eq!(name, None);
        assert_eq!(data, None);
    }

    #[test]
    fn test_parse_icon_wrong_field_count() {
        let type_val = Value::Str("bytes".into());
        let v = OwnedValue::try_from(Value::from((type_val,))).unwrap();
        let (name, data) = parse_icon(&v);
        assert_eq!(name, None);
        assert_eq!(data, None);
    }

    #[tokio::test]
    async fn test_request_install_token_allowed() {
        // We just create an empty MainContext for the UiProxy so we don't start GTK.
        let proxy = UiProxy {
            context: gtk4::glib::MainContext::default(),
        };
        let launcher = DynamicLauncher::new(&proxy);

        assert_eq!(
            launcher
                .request_install_token(
                    "org.gnome.Software".to_string(),
                    RequestInstallTokenOptions::default()
                )
                .await,
            0
        );
        assert_eq!(
            launcher
                .request_install_token(
                    "org.kde.discover".to_string(),
                    RequestInstallTokenOptions::default()
                )
                .await,
            0
        );
    }

    #[tokio::test]
    async fn test_request_install_token_denied() {
        let proxy = UiProxy {
            context: gtk4::glib::MainContext::default(),
        };
        let launcher = DynamicLauncher::new(&proxy);

        assert_eq!(
            launcher
                .request_install_token(
                    "com.example.App".to_string(),
                    RequestInstallTokenOptions::default()
                )
                .await,
            2
        );
    }

    #[test]
    fn test_dynamic_launcher_properties() {
        let proxy = UiProxy {
            context: gtk4::glib::MainContext::default(),
        };
        let launcher = DynamicLauncher::new(&proxy);
        assert_eq!(launcher.supported_launcher_types(), 3);
        assert_eq!(launcher.version(), 1);
    }
}

use {
    super::gui::{AccessUi, Choice as GuiChoice, ChoiceVariant},
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    serde::Deserialize,
    zbus::{
        ObjectServer, interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
    },
};

pub struct Access {
    // Keep a cloned UI proxy to dispatch GTK tasks.
    proxy: UiProxy,
}

impl Access {
    pub fn new(proxy: &UiProxy) -> Self {
        Self {
            proxy: proxy.clone(),
        }
    }
}

#[derive(Type, Debug, Deserialize)]
#[zvariant(signature = "(ssa(ss)s)")]
struct Choice(String, String, Vec<(String, String)>, String);

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct AccessDialogOptions {
    modal: Option<bool>,
    deny_label: Option<String>,
    grant_label: Option<String>,
    icon: Option<String>,
    choices: Option<Vec<Choice>>,
    pub activation_token: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct AccessResults {
    choices: Option<Vec<(String, String)>>,
}

impl Access {
    /// Internal implementation for the AccessDialog request.
    ///
    /// This converts the D-Bus dictionary parameters into a strongly-typed `AccessUi` struct
    /// and dispatches the dialog creation to the GTK main thread.
    async fn access_dialog_impl(
        &self,
        app_id: String,
        parent_window: String,
        title: String,
        subtitle: String,
        body: String,
        options: AccessDialogOptions,
    ) -> Response<AccessResults> {
        let choices = options.choices.map(|c| {
            c.into_iter()
                .map(|ch| GuiChoice {
                    id: ch.0,
                    label: ch.1,
                    variants: ch
                        .2
                        .into_iter()
                        .map(|v| ChoiceVariant {
                            id: v.0,
                            label: v.1,
                        })
                        .collect(),
                    default: ch.3,
                })
                .collect()
        });

        let res = AccessUi {
            app_id,
            parent_window,
            activation_token: options.activation_token.clone(),
            title,
            subtitle,
            body,
            modal: options.modal.unwrap_or(true),
            deny_label: options.deny_label,
            grant_label: options.grant_label,
            icon: options.icon,
            choices,
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(res) => Response::success(AccessResults {
                choices: res
                    .final_choices
                    .map(|c| c.into_iter().map(|fc| (fc.id, fc.variant_id)).collect()),
            }),
            Err(e) => {
                log::error!("AccessDialog failed: {}", anyhow::Error::new(e));
                Response::cancelled()
            }
        }
    }
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.Access`.
///
/// This portal is used by flatpak and other systems to request permissions from the user,
/// such as accessing the camera, microphone, or location.
#[interface(name = "org.freedesktop.impl.portal.Access")]
impl Access {
    async fn access_dialog(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        subtitle: String,
        body: String,
        options: AccessDialogOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<AccessResults> {
        // Run the request concurrently with a cancellation listener.
        // If the frontend calls `Close()` on the request object path, `run_request`
        // will return `Response::cancelled()` and drop the future.
        run_request(
            server,
            handle,
            self.access_dialog_impl(app_id, parent_window, title, subtitle, body, options),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::collections::HashMap,
        zbus::zvariant::{Endian, Value, serialized::Context},
    };

    #[test]
    fn test_choice_signature() {
        assert_eq!(Choice::SIGNATURE, "(ssa(ss)s)");
    }

    #[test]
    fn test_access_dialog_options_deserialize() {
        let mut dict = HashMap::new();
        dict.insert("modal", Value::from(false));
        dict.insert("deny_label", Value::from("No"));
        dict.insert("grant_label", Value::from("Yes"));
        dict.insert("icon", Value::from("icon-name"));
        dict.insert("activation_token", Value::from("token123"));

        let choice = (
            "choice_id",
            "choice_label",
            vec![("variant_id", "variant_label")],
            "default_variant",
        );

        dict.insert("choices", Value::from(vec![choice]));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zbus::zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: AccessDialogOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.modal, Some(false));
        assert_eq!(options.deny_label.as_deref(), Some("No"));
        assert_eq!(options.grant_label.as_deref(), Some("Yes"));
        assert_eq!(options.icon.as_deref(), Some("icon-name"));
        assert_eq!(options.activation_token.as_deref(), Some("token123"));
        assert!(options.choices.is_some());
    }

    #[test]
    fn test_access_dialog_options_empty() {
        let dict: HashMap<&str, Value> = HashMap::new();
        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zbus::zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: AccessDialogOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.modal, None);
        assert_eq!(options.deny_label, None);
        assert!(options.choices.is_none());
    }

    #[test]
    fn test_access_results_serialize() {
        let results = AccessResults {
            choices: Some(vec![("choice_id".to_string(), "variant_id".to_string())]),
        };

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zbus::zvariant::to_bytes(ctxt, &results).unwrap();

        let decoded: HashMap<String, Value> = encoded.deserialize().unwrap().0;
        let choices = decoded.get("choices").unwrap();
        assert_eq!(choices.value_signature(), "a(ss)");
    }
}

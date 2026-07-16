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

use {
    crate::{
        gui::{
            access::{AccessUi, Choice as GuiChoice, ChoiceVariant},
            UiProxy,
        },
        portal::{request::run_request, response::Response},
    },
    error_reporter::Report,
    serde::Deserialize,
    zbus::{
        interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
        ObjectServer,
    },
};

pub struct Access {
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
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct AccessResults {
    choices: Option<Vec<(String, String)>>,
}

impl Access {
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
                        .map(|v| ChoiceVariant { id: v.0, label: v.1 })
                        .collect(),
                    default: ch.3,
                })
                .collect()
        });

        let res = AccessUi {
            app_id,
            parent_window,
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
                choices: res.final_choices.map(|c| {
                    c.into_iter().map(|fc| (fc.id, fc.variant_id)).collect()
                }),
            }),
            Err(e) => {
                log::error!("AccessDialog failed: {}", Report::new(e));
                Response::cancelled()
            }
        }
    }
}

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
        run_request(
            server,
            handle,
            self.access_dialog_impl(app_id, parent_window, title, subtitle, body, options),
        )
        .await
    }
}

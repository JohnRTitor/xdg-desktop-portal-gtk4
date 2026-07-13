use zbus::zvariant::{DeserializeDict, SerializeDict, Type, OwnedObjectPath};
use zbus::interface;
use crate::{
    gui::{UiProxy, app_chooser::{AppChooserUi, AppChooserError}},
    portal::{request::run_request, response::Response},
};

#[derive(DeserializeDict, Type, Debug)]
#[zvariant(signature = "dict")]
pub struct ChooseApplicationOptions {
    last_choice: Option<String>,
    modal: Option<bool>,
    content_type: Option<String>,
    uri: Option<String>,
    filename: Option<String>,
    activation_token: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
pub struct ChooseApplicationResults {
    choice: Option<String>,
    activation_token: Option<String>,
}

pub struct AppChooser {
    proxy: UiProxy,
}

impl AppChooser {
    pub fn new(proxy: &UiProxy) -> Self {
        Self { proxy: proxy.clone() }
    }

    async fn choose_application_impl(
        &self,
        app_id: String,
        parent_window: String,
        choices: Vec<String>,
        options: ChooseApplicationOptions,
    ) -> Response<ChooseApplicationResults> {
        let ui = AppChooserUi {
            app_id,
            parent_window,
            title: rust_i18n::t!("Choose an application").to_string(),
            choices,
            filename: options.filename,
            content_type: options.content_type,
        };
        
        match ui.run(&self.proxy).await {
            Ok(result) => {
                let res = ChooseApplicationResults {
                    choice: Some(result.choice),
                    activation_token: options.activation_token,
                };
                Response::success(res)
            }
            Err(AppChooserError::Closed) | Err(AppChooserError::Rejected) => {
                Response::cancelled()
            }
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.AppChooser")]
impl AppChooser {
    #[zbus(name = "ChooseApplication")]
    async fn choose_application(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        choices: Vec<String>,
        options: ChooseApplicationOptions,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> Response<ChooseApplicationResults> {
        run_request(
            server,
            handle,
            self.choose_application_impl(app_id, parent_window, choices, options)
        )
        .await
    }

    #[zbus(name = "UpdateChoices")]
    async fn update_choices(
        &self,
        _handle: OwnedObjectPath,
        _choices: Vec<String>,
    ) -> zbus::fdo::Result<()> {
        log::info!("UpdateChoices called");
        Ok(())
    }
}

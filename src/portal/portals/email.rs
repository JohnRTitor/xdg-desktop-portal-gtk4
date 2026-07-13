use {
    crate::portal::{request::run_request, response::Response},
    error_reporter::Report,
    gtk4::{glib, gio::AppInfo},
    zbus::{
        interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
        ObjectServer,
    },
};

pub struct Email;

impl Email {
    pub fn new() -> Self {
        Self
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct ComposeEmailOptions {
    addresses: Option<Vec<String>>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    subject: Option<String>,
    body: Option<String>,
    attachments: Option<Vec<String>>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct EmailResults {}

impl Email {
    async fn compose_email_impl(
        &self,
        _app_id: String,
        _parent_window: String,
        options: ComposeEmailOptions,
    ) -> Response<EmailResults> {
        let mut url = String::from("mailto:");
        
        if let Some(addresses) = options.addresses {
            url.push_str(&addresses.join(","));
        }
        
        url.push('?');
        
        if let Some(cc) = options.cc {
            for addr in cc {
                url.push_str(&format!("cc={}&", glib::uri_escape_string(&addr, None::<&str>, true)));
            }
        }
        if let Some(bcc) = options.bcc {
            for addr in bcc {
                url.push_str(&format!("bcc={}&", glib::uri_escape_string(&addr, None::<&str>, true)));
            }
        }
        if let Some(subject) = options.subject {
            url.push_str(&format!("subject={}&", glib::uri_escape_string(&subject, None::<&str>, true)));
        }
        if let Some(body) = options.body {
            url.push_str(&format!("body={}&", glib::uri_escape_string(&body, None::<&str>, true)));
        }
        if let Some(attachments) = options.attachments {
            for att in attachments {
                url.push_str(&format!("attachment={}&", glib::uri_escape_string(&att, None::<&str>, true)));
            }
        }
        
        // Remove trailing '?' or '&'
        url.pop();
        
        match AppInfo::launch_default_for_uri(&url, None::<&gtk4::gio::AppLaunchContext>) {
            Ok(_) => Response::success(EmailResults::default()),
            Err(e) => {
                log::error!("ComposeEmail failed: {}", Report::new(e));
                Response::cancelled()
            }
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Email")]
impl Email {
    async fn compose_email(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: ComposeEmailOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<EmailResults> {
        run_request(
            server,
            handle,
            self.compose_email_impl(app_id, parent_window, options),
        )
        .await
    }
}

use {
    crate::gui::{UiError, UiProxy},
    async_channel::{Receiver, Sender},
    gtk4::{
        Button, CheckButton, Image, Label,
        glib::MainContext,
        prelude::{BoxExt, ButtonExt, CheckButtonExt, GtkWindowExt, WidgetExt},
    },
    rust_i18n::t,
};

pub struct Choice {
    pub id: String,
    pub label: String,
    pub default: String,
    pub variants: Vec<ChoiceVariant>,
}

pub struct ChoiceVariant {
    pub id: String,
    pub label: String,
}

pub struct FinalChoice {
    pub id: String,
    pub variant_id: String,
}

pub struct AccessUi {
    pub app_id: String,
    pub parent_window: String,
    pub activation_token: Option<String>,
    pub title: String,
    pub subtitle: String,
    pub body: String,
    pub modal: bool,
    pub deny_label: Option<String>,
    pub grant_label: Option<String>,
    pub icon: Option<String>,
    pub choices: Option<Vec<Choice>>,
}

pub struct AccessResult {
    pub final_choices: Option<Vec<FinalChoice>>,
}

impl AccessUi {
    pub async fn run(self, proxy: &UiProxy) -> Result<AccessResult, UiError> {
        crate::gui::run_ui_task(
            proxy,
            |send, context, close_on_close| self.run_impl(send, context, close_on_close),
            || UiError::Closed,
        )
        .await
    }

    fn run_impl(
        self,
        send: Sender<Result<AccessResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        // We use our CustomDialog which wraps a standard GtkWindow instead of a GtkDialog.
        // GtkDialog is deprecated in GTK4.
        let dialog = crate::gui::dialog::CustomDialog::new(&self.title, self.modal);

        let deny_label = self
            .deny_label
            .unwrap_or_else(|| t!("deny_access_action").to_string());
        let grant_label = self
            .grant_label
            .unwrap_or_else(|| t!("grant_access_action").to_string());

        let deny_btn = Button::with_label(&deny_label);
        let grant_btn = Button::with_label(&grant_label);
        grant_btn.add_css_class("suggested-action");

        dialog.action_area.append(&deny_btn);
        dialog.action_area.append(&grant_btn);

        if !self.subtitle.is_empty() {
            let subtitle_lbl = Label::new(Some(&self.subtitle));
            subtitle_lbl.add_css_class("title-2");
            subtitle_lbl.set_halign(gtk4::Align::Start);
            dialog.content_area.append(&subtitle_lbl);
        }

        if !self.body.is_empty() {
            let body_label = Label::new(Some(&self.body));
            body_label.set_halign(gtk4::Align::Start);
            body_label.set_margin_top(10);
            body_label.set_wrap(true);
            body_label.set_max_width_chars(50);
            dialog.content_area.append(&body_label);
        }

        let mut boolean_choices = Vec::new();
        let mut radio_choices = Vec::new();

        if let Some(choices) = &self.choices {
            for choice in choices {
                if choice.variants.is_empty() {
                    let button = CheckButton::with_label(&choice.label);
                    button.set_margin_top(10);
                    if choice.default == "true" {
                        button.set_active(true);
                    }
                    dialog.content_area.append(&button);
                    boolean_choices.push((choice.id.clone(), button));
                } else {
                    let label = Label::new(Some(&choice.label));
                    label.set_halign(gtk4::Align::Start);
                    label.set_margin_top(10);
                    label.add_css_class("dim-label");
                    dialog.content_area.append(&label);

                    let mut group = None::<CheckButton>;
                    let mut variants_for_choice = Vec::new();

                    for variant in &choice.variants {
                        let radio = if let Some(ref g) = group {
                            CheckButton::builder()
                                .label(&variant.label)
                                .group(g)
                                .build()
                        } else {
                            CheckButton::builder().label(&variant.label).build()
                        };

                        if group.is_none() {
                            group = Some(radio.clone());
                        }

                        if choice.default == variant.id {
                            radio.set_active(true);
                        }

                        dialog.content_area.append(&radio);
                        variants_for_choice.push((variant.id.clone(), radio));
                    }
                    radio_choices.push((choice.id.clone(), variants_for_choice));
                }
            }
        }

        if let Some(icon) = &self.icon {
            let image = Image::from_icon_name(icon);
            image.set_pixel_size(48);
            dialog.content_area.prepend(&image);
        }

        let choices_cfg = self.choices.is_some();
        let window = dialog.window.clone();

        // Handle the user clicking the "X" button or pressing Escape.
        let send_close = send.clone();
        window.connect_close_request(move |_| {
            let _ = send_close.send_blocking(Err(UiError::Rejected));
            // Let GTK handle the actual window destruction.
            gtk4::glib::Propagation::Proceed
        });

        let send_deny = send.clone();
        let w_deny = window.clone();
        deny_btn.connect_clicked(move |_| {
            let _ = send_deny.send_blocking(Err(UiError::Rejected));
            w_deny.close();
        });

        let send_grant = send.clone();
        let w_grant = window.clone();
        grant_btn.connect_clicked(move |_| {
            let mut final_choices = None;
            if choices_cfg {
                let mut fc = Vec::new();
                for (id, button) in &boolean_choices {
                    fc.push(FinalChoice {
                        id: id.clone(),
                        variant_id: if button.is_active() {
                            "true".to_string()
                        } else {
                            "false".to_string()
                        },
                    });
                }
                for (id, variants) in &radio_choices {
                    if let Some((v_id, _)) = variants.iter().find(|(_, r)| r.is_active()) {
                        fc.push(FinalChoice {
                            id: id.clone(),
                            variant_id: v_id.clone(),
                        });
                    }
                }
                final_choices = Some(fc);
            }
            let _ = send_grant.send_blocking(Ok(AccessResult { final_choices }));
            w_grant.close();
        });

        // Bind the dialog to the calling application's window if running under Wayland.
        crate::gui::windowing::external_window::setup_window(
            &window,
            &self.parent_window,
            self.activation_token.as_deref(),
        );

        window.show();

        // Spawn a background task to close the window if the D-Bus request is cancelled.
        // This task runs on the GTK MainContext, so it can safely manipulate the `window`.
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            window.close();
        });
    }
}

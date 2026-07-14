use {
    crate::{gui::UiProxy, utils::external_window::set_wayland_parent},
    async_channel::{Receiver, Sender},
    gtk4::{
        CheckButton, DialogFlags, Image, Label, MessageDialog, MessageType, ResponseType, Widget,
        glib::MainContext,
        prelude::{BoxExt, Cast, CheckButtonExt, DialogExt, GtkWindowExt, WidgetExt},
    },
    rust_i18n::t,
    thiserror::Error,
};

#[derive(Debug, Error)]
pub enum AccessError {
    #[error("Operation could not be started")]
    Closed,
    #[error("Operation was rejected")]
    Rejected,
}

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
    pub async fn run(self, proxy: &UiProxy) -> Result<AccessResult, AccessError> {
        let (send, recv) = async_channel::bounded(1);
        let (_send, close_on_close) = async_channel::bounded(1);
        let context = proxy.context.clone();
        proxy
            .context
            .invoke(move || self.run_impl(send, context, close_on_close));
        recv.recv().await.map_err(|_| AccessError::Closed)?
    }

    fn run_impl(
        self,
        send: Sender<Result<AccessResult, AccessError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let mut flags = DialogFlags::empty();
        if self.modal {
            flags |= DialogFlags::MODAL;
        }

        let dialog = MessageDialog::new(
            None::<&gtk4::Window>,
            flags,
            MessageType::Question,
            gtk4::ButtonsType::None,
            &*self.title,
        );

        dialog.format_secondary_text(Some(&self.subtitle));

        let deny_label = self
            .deny_label
            .unwrap_or_else(|| t!("_Deny Access").to_string());
        let grant_label = self
            .grant_label
            .unwrap_or_else(|| t!("_Grant Access").to_string());

        dialog.add_button(&deny_label, ResponseType::Cancel);
        dialog.add_button(&grant_label, ResponseType::Ok);

        if let Ok(area) = dialog.message_area().downcast::<gtk4::Box>() {
            if !self.body.is_empty() {
                let body_label = Label::new(Some(&self.body));
                body_label.set_halign(gtk4::Align::Start);
                body_label.set_margin_top(10);
                body_label.set_wrap(true);
                body_label.set_max_width_chars(50);
                area.append(&body_label);
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
                        area.append(&button);
                        boolean_choices.push((choice.id.clone(), button));
                    } else {
                        let label = Label::new(Some(&choice.label));
                        label.set_halign(gtk4::Align::Start);
                        label.set_margin_top(10);
                        label.add_css_class("dim-label");
                        area.append(&label);

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

                            area.append(&radio);
                            variants_for_choice.push((variant.id.clone(), radio));
                        }
                        radio_choices.push((choice.id.clone(), variants_for_choice));
                    }
                }
            }

            if let Some(icon) = &self.icon {
                let image = Image::from_icon_name(icon);
                area.prepend(&image);
            }

            let choices_cfg = self.choices.is_some();

            dialog.connect_response(move |d, r| {
                let res = match r {
                    ResponseType::Ok => {
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
                                if let Some((v_id, _)) =
                                    variants.iter().find(|(_, r)| r.is_active())
                                {
                                    fc.push(FinalChoice {
                                        id: id.clone(),
                                        variant_id: v_id.clone(),
                                    });
                                }
                            }
                            final_choices = Some(fc);
                        }
                        Ok(AccessResult { final_choices })
                    }
                    _ => Err(AccessError::Rejected),
                };
                let _ = send.send_blocking(res);
                d.close();
            });
        } else {
            log::error!("Failed to downcast message_area to Box in AccessDialog");
            let _ = send.send_blocking(Err(AccessError::Rejected));
        }

        dialog.upcast_ref::<Widget>().realize();
        set_wayland_parent(dialog.upcast_ref::<Widget>(), &self.parent_window);

        dialog.show();
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            dialog.close();
        });
    }
}

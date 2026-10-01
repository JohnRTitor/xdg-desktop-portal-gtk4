use {
    crate::gui::{PortalDispatcher, UiError, UiProxy},
    gtk4::{
        Align, Button, CheckButton, Image, Label,
        glib::{self, MainContext},
        prelude::{BoxExt, ButtonExt, CheckButtonExt, GtkWindowExt, WidgetExt},
    },
    rust_i18n::t,
    tokio::sync::oneshot::Receiver,
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Builds the reply to `AccessDialog` from the dialog's widget state.
///
/// Returns `None` when the caller sent no `choices` at all -- the contract only
/// carries a `choices` key back when it asked for one. `choices_requested` must
/// therefore be sampled *before* the choice list is consumed.
///
/// Takes borrowed iterators rather than collected vectors, so each id is cloned
/// exactly once -- into its `FinalChoice` -- and the variants of an unselected
/// radio group are never even inspected.
fn build_final_choices<'a, B, R, V>(
    choices_requested: bool,
    booleans: B,
    radios: R,
) -> Option<Vec<FinalChoice>>
where
    B: IntoIterator<Item = (&'a str, bool)>,
    R: IntoIterator<Item = (&'a str, V)>,
    V: IntoIterator<Item = (&'a str, bool)>,
{
    if !choices_requested {
        return None;
    }
    let mut out = Vec::new();
    for (id, active) in booleans {
        out.push(FinalChoice {
            id: id.to_owned(),
            variant_id: if active {
                "true".into()
            } else {
                "false".into()
            },
        });
    }
    for (id, variants) in radios {
        // A radio group with nothing selected contributes nothing, matching the
        // previous `find(..is_active())` behaviour.
        if let Some((variant_id, _)) = variants.into_iter().find(|(_, active)| *active) {
            out.push(FinalChoice {
                id: id.to_owned(),
                variant_id: variant_id.to_owned(),
            });
        }
    }
    Some(out)
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
        mut self,
        send: crate::gui::UiDispatcher<Result<AccessResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        // We use our CustomDialog which wraps a standard GtkWindow instead of a GtkDialog.
        // GtkDialog is deprecated in GTK4.
        let dialog = crate::gui::dialog::CustomDialog::new(&self.title, self.modal);

        let deny_label = self
            .deny_label
            .unwrap_or_else(|| t!("deny_access_action").into());
        let grant_label = self
            .grant_label
            .unwrap_or_else(|| t!("grant_access_action").into());

        let deny_btn = Button::with_label(&deny_label);
        let grant_btn = Button::with_label(&grant_label);
        grant_btn.add_css_class("suggested-action");

        dialog.action_area.append(&deny_btn);
        dialog.action_area.append(&grant_btn);

        if !self.subtitle.is_empty() {
            let subtitle_lbl = Label::new(Some(&self.subtitle));
            subtitle_lbl.add_css_class("title-2");
            subtitle_lbl.set_halign(Align::Start);
            dialog.content_area.append(&subtitle_lbl);
        }

        if !self.body.is_empty() {
            let body_label = Label::new(Some(&self.body));
            body_label.set_halign(Align::Start);
            body_label.set_margin_top(crate::gui::ELEMENT_MARGIN);
            body_label.set_wrap(true);
            body_label.set_max_width_chars(crate::gui::LABEL_MAX_WIDTH_CHARS);
            dialog.content_area.append(&body_label);
        }

        let mut boolean_choices = Vec::new();
        let mut radio_choices = Vec::new();

        // Whether the caller supplied choices at all, captured before the list
        // is consumed below.
        let choices_cfg = self.choices.is_some();

        // `run_impl` owns `self`, and the choice IDs are needed again inside
        // `'static` GTK signal closures. Taking the list consumes the IDs instead
        // of cloning each one while the originals are still alive.
        if let Some(choices) = std::mem::take(&mut self.choices) {
            for choice in choices {
                if choice.variants.is_empty() {
                    let button = CheckButton::with_label(&choice.label);
                    button.set_margin_top(crate::gui::ELEMENT_MARGIN);
                    if choice.default == "true" {
                        button.set_active(true);
                    }
                    dialog.content_area.append(&button);
                    boolean_choices.push((choice.id, button));
                } else {
                    let label = Label::new(Some(&choice.label));
                    label.set_halign(Align::Start);
                    label.set_margin_top(crate::gui::ELEMENT_MARGIN);
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
                    radio_choices.push((choice.id, variants_for_choice));
                }
            }
        }

        if let Some(icon) = &self.icon {
            let image = Image::from_icon_name(icon);
            image.set_pixel_size(48);
            dialog.content_area.prepend(&image);
        }

        let window = dialog.window.clone();

        // Handle the user clicking the "X" button or pressing Escape.
        let send_close = send.clone();
        window.connect_close_request(move |_| {
            let _ = send_close.dispatch(Err(UiError::Rejected));
            // Let GTK handle the actual window destruction.
            glib::Propagation::Proceed
        });

        let send_deny = send.clone();
        deny_btn.connect_clicked(glib::clone!(
            #[weak]
            window,
            move |_| {
                let _ = send_deny.dispatch(Err(UiError::Rejected));
                window.close();
            }
        ));

        let send_grant = send.clone();
        grant_btn.connect_clicked(glib::clone!(
            #[weak]
            window,
            move |_| {
                let final_choices = build_final_choices(
                    choices_cfg,
                    boolean_choices
                        .iter()
                        .map(|(id, button)| (id.as_str(), button.is_active())),
                    radio_choices.iter().map(|(id, variants)| {
                        (
                            id.as_str(),
                            variants
                                .iter()
                                .map(|(vid, button)| (vid.as_str(), button.is_active())),
                        )
                    }),
                );
                let _ = send_grant.dispatch(Ok(AccessResult { final_choices }));
                window.close();
            }
        ));

        // Bind the dialog to the calling application's window if running under Wayland.
        crate::gui::windowing::external_window::setup_window(
            &window,
            &self.parent_window,
            self.activation_token.as_deref(),
        );

        window.show();

        // Spawn a background task to close the window if the D-Bus request is cancelled.
        // This task runs on the GTK MainContext, so it can safely manipulate the `window`.
        context.spawn_local(glib::clone!(
            #[weak]
            window,
            async move {
                let _ = close_on_close.await;
                window.close();
            }
        ));
    }
}

#[cfg(test)]
mod final_choice_tests {
    use super::*;

    fn pairs(out: Option<Vec<FinalChoice>>) -> Option<Vec<(String, String)>> {
        out.map(|choices| choices.into_iter().map(|c| (c.id, c.variant_id)).collect())
    }

    #[test]
    fn no_choices_requested_yields_none_even_with_widgets() {
        // Checkboxes exist for the dialog's own layout, but the caller must not
        // receive a `choices` key it never asked for.
        let out = build_final_choices(false, [("a", true), ("b", false)], [("c", [("c1", true)])]);
        assert!(out.is_none());
    }

    /// The invariant the `mem::take` reordering depends on: a caller that sent
    /// `choices: []` must still get an empty list back, not `None`.
    #[test]
    fn an_empty_choices_list_yields_some_empty_vec() {
        assert_eq!(
            build_final_choices(
                true,
                std::iter::empty::<(&str, bool)>(),
                std::iter::empty::<(&str, [(&str, bool); 0])>(),
            ),
            Some(Vec::new()),
        );
    }

    #[test]
    fn boolean_choices_report_their_state() {
        let out = build_final_choices(
            true,
            [("a", true), ("b", false)],
            std::iter::empty::<(&str, [(&str, bool); 0])>(),
        );
        assert_eq!(
            pairs(out),
            Some(vec![
                ("a".to_owned(), "true".to_owned()),
                ("b".to_owned(), "false".to_owned()),
            ])
        );
    }

    #[test]
    fn the_active_radio_variant_is_reported() {
        let out = build_final_choices(
            true,
            std::iter::empty::<(&str, bool)>(),
            [("m", [("m1", false), ("m2", true), ("m3", false)])],
        );
        assert_eq!(pairs(out), Some(vec![("m".to_owned(), "m2".to_owned())]));
    }

    #[test]
    fn a_radio_group_with_nothing_selected_is_omitted() {
        let out = build_final_choices(
            true,
            std::iter::empty::<(&str, bool)>(),
            [("m", [("m1", false)])],
        );
        assert_eq!(pairs(out), Some(vec![]));
    }

    #[test]
    fn booleans_come_before_radios() {
        let out = build_final_choices(true, [("z", true)], [("a", [("a1", true)])]);
        assert_eq!(
            pairs(out),
            Some(vec![
                ("z".to_owned(), "true".to_owned()),
                ("a".to_owned(), "a1".to_owned()),
            ])
        );
    }

    #[test]
    fn an_empty_radio_group_contributes_nothing() {
        let out = build_final_choices(true, std::iter::empty::<(&str, bool)>(), [("m", [])]);
        assert_eq!(pairs(out), Some(vec![]));
    }
}

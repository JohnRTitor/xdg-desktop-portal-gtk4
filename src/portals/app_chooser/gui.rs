use {
    crate::gui::{UiError, UiProxy},
    async_channel::{Receiver, Sender},
    gtk4::{
        Box as GtkBox, Button, Image, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow,
        gio::AppInfo, glib::MainContext, prelude::*,
    },
    rust_i18n::t,
};

pub struct AppChooserUi {
    pub app_id: String,
    pub parent_window: String,
    pub activation_token: Option<String>,
    pub title: String,
    pub choices: Vec<String>,
    pub filename: Option<String>,
    pub content_type: Option<String>,
}

pub struct AppChooserResult {
    pub choice: String,
    pub activation_token: Option<String>,
}

impl AppChooserUi {
    pub async fn run(
        self,
        proxy: &UiProxy,
        update_receiver: Receiver<Vec<String>>,
    ) -> Result<AppChooserResult, UiError> {
        crate::gui::run_ui_task(
            proxy,
            move |send, context, close_on_close| {
                self.run_impl(send, context, close_on_close, update_receiver)
            },
            || UiError::Closed,
        )
        .await
    }

    fn run_impl(
        self,
        send: Sender<Result<AppChooserResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
        update_receiver: Receiver<Vec<String>>,
    ) {
        let dialog = crate::gui::dialog::CustomDialog::new(&self.title, true);

        let cancel_button = Button::with_label(&t!("cancel_action"));
        let ok_button = Button::with_label(&t!("open_action"));
        ok_button.set_sensitive(false);
        ok_button.add_css_class("suggested-action");

        dialog.action_area.append(&cancel_button);
        dialog.action_area.append(&ok_button);

        let label_text = if let Some(ref filename) = self.filename {
            format!("{} {}", t!("select_application_to_open"), filename)
        } else {
            t!("select_application_to_open_file").to_string()
        };
        let label = Label::new(Some(&label_text));
        dialog.content_area.append(&label);

        let scrolled_window = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .vexpand(true)
            .build();
        dialog.content_area.append(&scrolled_window);

        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk4::SelectionMode::Single);
        scrolled_window.set_child(Some(&list_box));

        let all_apps = AppInfo::all();
        let recommended_apps = if let Some(ct) = self.content_type.as_deref() {
            let recommended = AppInfo::recommended_for_type(ct);
            if recommended.is_empty() {
                all_apps.clone()
            } else {
                recommended
            }
        } else {
            all_apps.clone()
        };

        populate_list_box(&list_box, &self.choices, &all_apps, &recommended_apps);

        // Spawn a task to listen for `UpdateChoices` D-Bus calls.
        // It runs on the main thread, so it can safely call `populate_list_box` to update GTK widgets.
        let list_box_clone2 = list_box.clone();
        context.spawn_local(async move {
            while let Ok(new_choices) = update_receiver.recv().await {
                populate_list_box(&list_box_clone2, &new_choices, &all_apps, &recommended_apps);
            }
        });

        let ok_button_weak = ok_button.downgrade();
        list_box.connect_row_selected(move |_, row| {
            if let Some(btn) = ok_button_weak.upgrade() {
                btn.set_sensitive(row.is_some());
            }
        });

        let list_box_clone = list_box.clone();

        let window = dialog.window.clone();

        let send_close = send.clone();
        window.connect_close_request(move |_| {
            let _ = send_close.send_blocking(Err(UiError::Rejected));
            gtk4::glib::Propagation::Proceed
        });

        let send_cancel = send.clone();
        let w_cancel = window.downgrade();
        cancel_button.connect_clicked(move |_| {
            let _ = send_cancel.send_blocking(Err(UiError::Rejected));
            if let Some(w) = w_cancel.upgrade() {
                w.close();
            }
        });

        let send_ok = send.clone();
        let w_ok = window.downgrade();
        ok_button.connect_clicked(move |_| {
            let res = if let Some(row) = list_box_clone.selected_row() {
                // If the user selected an app, generate a startup notification token.
                // This allows the desktop environment to show a "starting" animation
                // or focus the new window once it appears.
                let launch_context = gtk4::gio::AppLaunchContext::new();
                let token = launch_context
                    .startup_notify_id(None::<&gtk4::gio::AppInfo>, &[])
                    .map(|s| s.to_string());
                Ok(AppChooserResult {
                    choice: row.widget_name().to_string(),
                    activation_token: token,
                })
            } else {
                Err(UiError::Rejected)
            };
            let _ = send_ok.send_blocking(res);
            if let Some(w) = w_ok.upgrade() {
                w.close();
            }
        });

        crate::gui::windowing::external_window::setup_window(
            &window,
            &self.parent_window,
            self.activation_token.as_deref(),
        );

        window.show();
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            window.close();
        });
    }
}

fn populate_list_box(
    list_box: &ListBox,
    choices: &[String],
    all_apps: &[AppInfo],
    recommended_apps: &[AppInfo],
) {
    // Clear existing children
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let mut apps_to_show = Vec::new();

    if !choices.is_empty() {
        // If the frontend provided specific choices (e.g., from its own history or cache),
        // we only show those.
        for app in all_apps {
            if let Some(id) = app.id() {
                if choices.contains(&id.to_string()) {
                    apps_to_show.push(app.clone());
                }
            }
        }
    } else {
        apps_to_show = recommended_apps.to_vec();
    }

    apps_to_show.sort_by(|a, b| a.name().cmp(&b.name()));
    apps_to_show.dedup_by(|a, b| a.id() == b.id());

    for app in apps_to_show {
        let row = ListBoxRow::new();
        let hbox = GtkBox::new(Orientation::Horizontal, 12);
        hbox.set_margin_top(6);
        hbox.set_margin_bottom(6);
        hbox.set_margin_start(6);
        hbox.set_margin_end(6);

        if let Some(icon) = app.icon() {
            let image = Image::from_gicon(&icon);
            image.set_pixel_size(32);
            hbox.append(&image);
        }

        let name_label = Label::new(Some(&app.name()));
        name_label.set_halign(gtk4::Align::Start);
        hbox.append(&name_label);

        row.set_child(Some(&hbox));

        if let Some(id) = app.id() {
            row.set_widget_name(&id.to_string());
            list_box.append(&row);
        }
    }
}

use crate::{gui::UiProxy, utils::external_window::set_wayland_parent};
use async_channel::{Receiver, Sender};
use gtk4::{
    glib::MainContext,
    prelude::*,
    ResponseType, Widget,
    gio::AppInfo,
    ListBox, ListBoxRow, Label, Image, Box as GtkBox, Orientation, ScrolledWindow
};
use rust_i18n::t;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppChooserError {
    #[error("Operation could not be started")]
    Closed,
    #[error("Operation was rejected")]
    Rejected,
}

pub struct AppChooserUi {
    pub app_id: String,
    pub parent_window: String,
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
    pub async fn run(self, proxy: &UiProxy, update_receiver: Receiver<Vec<String>>) -> Result<AppChooserResult, AppChooserError> {
        let (send, recv) = async_channel::bounded(1);
        let (_send, close_on_close) = async_channel::bounded(1);
        let context = proxy.context.clone();
        proxy
            .context
            .invoke(move || self.run_impl(send, context, close_on_close, update_receiver));
        recv.recv().await.map_err(|_| AppChooserError::Closed)?
    }

    fn run_impl(
        self,
        send: Sender<Result<AppChooserResult, AppChooserError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
        update_receiver: Receiver<Vec<String>>,
    ) {
        let dummy_parent = gtk4::Window::new();
        let dialog = gtk4::Dialog::builder()
            .title(&self.title)
            .modal(true)
            .default_width(400)
            .default_height(500)
            .transient_for(&dummy_parent)
            .build();

        dialog.add_button(&t!("_Cancel"), ResponseType::Cancel);
        let ok_button = dialog.add_button(&t!("_Open"), ResponseType::Ok);
        ok_button.set_sensitive(false);

        let content_area = dialog.content_area();
        content_area.set_margin_top(12);
        content_area.set_margin_bottom(12);
        content_area.set_margin_start(12);
        content_area.set_margin_end(12);
        content_area.set_spacing(12);
        
        let label_text = if let Some(ref filename) = self.filename {
            format!("{} {}", t!("Select an application to open"), filename)
        } else {
            t!("Select an application to open the file").to_string()
        };
        let label = Label::new(Some(&label_text));
        content_area.append(&label);

        let scrolled_window = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .vexpand(true)
            .build();
        content_area.append(&scrolled_window);

        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk4::SelectionMode::Single);
        scrolled_window.set_child(Some(&list_box));
        
        populate_list_box(&list_box, &self.choices, self.content_type.as_deref());

        let list_box_clone2 = list_box.clone();
        let content_type = self.content_type.clone();
        context.spawn_local(async move {
            while let Ok(new_choices) = update_receiver.recv().await {
                populate_list_box(&list_box_clone2, &new_choices, content_type.as_deref());
            }
        });
        
        let ok_button_clone = ok_button.clone();
        list_box.connect_row_selected(move |_, row| {
            ok_button_clone.set_sensitive(row.is_some());
        });

        let dummy_parent_clone = dummy_parent.clone();
        let list_box_clone = list_box.clone();
        dialog.connect_response(move |d, r| {
            let res = match r {
                ResponseType::Ok => {
                    if let Some(row) = list_box_clone.selected_row() {
                        let launch_context = gtk4::gio::AppLaunchContext::new();
                        let token = launch_context.startup_notify_id(None::<&gtk4::gio::AppInfo>, &[]).map(|s| s.to_string());
                        Ok(AppChooserResult {
                            choice: row.widget_name().to_string(),
                            activation_token: token,
                        })
                    } else {
                        Err(AppChooserError::Rejected)
                    }
                }
                _ => Err(AppChooserError::Rejected),
            };
            let _ = send.send_blocking(res);
            d.close();
            dummy_parent_clone.destroy();
        });

        if let Some(w) = dialog.upcast_ref::<Widget>().downcast_ref::<gtk4::Window>() {
            set_wayland_parent(w.upcast_ref::<Widget>(), &self.parent_window);
        }

        dialog.show();
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            dialog.close();
        });
    }
}

fn populate_list_box(list_box: &ListBox, choices: &[String], content_type: Option<&str>) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    
    let all_apps = AppInfo::all();
    let mut apps_to_show = Vec::new();

    if !choices.is_empty() {
        for app in all_apps {
            if let Some(id) = app.id() {
                if choices.contains(&id.to_string()) {
                    apps_to_show.push(app);
                }
            }
        }
    } else if let Some(ct) = content_type {
        apps_to_show = AppInfo::recommended_for_type(ct);
        if apps_to_show.is_empty() {
            apps_to_show = AppInfo::all();
        }
    } else {
        apps_to_show = all_apps;
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

use {
    crate::{
        gui::{UiError, UiProxy},
        utils::file_chooser_ext::FileChooserExtManualFixed,
    },
    async_channel::{Receiver, Sender},
    gtk4::{
        FileChooserAction, FileChooserDialog, FileFilter, RecentData, RecentManager, ResponseType,
        gio::File,
        glib::MainContext,
        prelude::{
            Cast, DialogExt, FileChooserExt, FileChooserExtManual, FileExt, GtkWindowExt,
            RecentManagerExt, WidgetExt,
        },
    },
    rust_i18n::t,
    std::{
        cell::Cell,
        collections::{HashMap, HashSet},
        rc::Rc,
    },
};

#[derive(Eq, PartialEq, Clone)]
pub struct Filter {
    pub name: String,
    pub elements: Vec<FilterKind>,
}

#[derive(Eq, PartialEq, Clone)]
pub enum FilterKind {
    Glob(String),
    Mime(String),
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

pub struct FileChooserUi {
    pub title: String,
    pub multiple: bool,
    pub accept_label: Option<String>,
    pub modal: bool,
    pub directory: bool,
    pub filters: Option<Vec<Filter>>,
    pub current_filter: Option<Filter>,
    pub current_name: Option<String>,
    pub current_folder: Option<String>,
    pub current_filename: Option<String>,
    pub choices: Option<Vec<Choice>>,
    pub save: bool,
    pub parent_window: String,
    pub app_id: String,
}

pub struct FileChooserResult {
    pub uris: Vec<String>,
    pub current_filter: Option<Filter>,
    pub final_choices: Option<Vec<FinalChoice>>,
    pub writeable: bool,
}

struct DialogData {
    dialog: FileChooserDialog,
    read_only_choice: String,
    filters: HashMap<FileFilter, Filter>,
    dummy_parent: gtk4::Window,
}

impl FileChooserUi {
    pub async fn run(self, proxy: &UiProxy) -> Result<FileChooserResult, UiError> {
        crate::gui::run_ui_task(
            proxy,
            |send, context, close_on_close| self.run_impl(send, context, close_on_close),
            || UiError::Closed,
        )
        .await
    }

    fn run_impl(
        self,
        send: Sender<Result<FileChooserResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let DialogData {
            dialog,
            read_only_choice,
            filters,
            dummy_parent,
        } = self.build_dialog();
        let current_filter = Rc::new(Cell::new(dialog.filter()));
        let cf = current_filter.clone();
        dialog.connect_filter_notify(move |f| cf.set(f.filter()));
        let cf = current_filter.clone();
        dialog.connect_response(move |dialog, r| {
            let res = match r {
                ResponseType::Ok => {
                    let files: Vec<_> = dialog
                        .files()
                        .into_iter()
                        .filter_map(|f| f.ok().and_then(|f| f.downcast::<gtk4::gio::File>().ok()))
                        .map(|f| {
                            let uri = f.uri();
                            if !uri.starts_with("file://") {
                                if let Some(path) = f.path() {
                                    return gtk4::gio::File::for_path(path).uri().into();
                                }
                            }
                            uri.into()
                        })
                        .collect();
                    add_recent(&self.app_id, &files);
                    let filter = cf.take().and_then(|f| filters.get(&f).cloned());
                    let choices: Vec<_> = self
                        .choices
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .flat_map(|c| {
                            dialog.choice(&c.id).map(|v| FinalChoice {
                                id: c.id.to_string(),
                                variant_id: v.to_string(),
                            })
                        })
                        .collect();
                    let writeable = dialog
                        .choice(&read_only_choice)
                        .map(|v| v == "false")
                        .unwrap_or(false);
                    Ok(FileChooserResult {
                        uris: files,
                        current_filter: filter,
                        final_choices: self.choices.is_some().then_some(choices),
                        writeable,
                    })
                }
                _ => Err(UiError::Rejected),
            };
            let _ = send.send_blocking(res);
            dialog.close();
            dummy_parent.destroy();
        });
        dialog.show();
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            dialog.close();
        });
    }

    fn build_dialog(&self) -> DialogData {
        let action = match (self.directory, self.save) {
            (true, _) => FileChooserAction::SelectFolder,
            (_, true) => FileChooserAction::Save,
            (false, _) => FileChooserAction::Open,
        };
        let accept_label = match self.save {
            true => t!("save_action"),
            false => t!("open_action"),
        };
        let buttons = [
            (
                self.accept_label.as_deref().unwrap_or(&accept_label),
                ResponseType::Ok,
            ),
            (&t!("cancel_action"), ResponseType::Cancel),
        ];
        let dummy_parent = gtk4::Window::new();
        let dialog = FileChooserDialog::new(
            Some(self.title.clone()),
            Some(&dummy_parent),
            action,
            &buttons,
        );
        dialog.set_select_multiple(self.multiple);
        dialog.set_modal(self.modal);
        dialog.set_default_response(ResponseType::Ok);
        let mut filters_map = HashMap::new();
        if let Some(f) = &self.filters {
            for filter in f {
                let is_current = self.current_filter.as_ref() == Some(filter);
                let f = map_filter(filter);
                dialog.add_filter(&f);
                if is_current {
                    dialog.set_filter(&f);
                }
                filters_map.insert(f, filter.clone());
            }
        }
        if let Some(f) = &self.current_name {
            dialog.set_current_name(f);
        }
        if let Some(f) = &self.current_folder {
            let _ = dialog.set_current_folder(Some(&File::for_path(f)));
        }
        if let Some(f) = &self.current_filename {
            let _ = dialog.set_file(&File::for_uri(f));
        }
        let mut read_only_id = String::new();
        if action == FileChooserAction::Open {
            let choice_ids: HashSet<_> = self
                .choices
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|c| c.id.as_str())
                .collect();
            read_only_id = "_read_only".to_string();
            while choice_ids.contains(read_only_id.as_str()) {
                read_only_id.push('_');
            }
            dialog.add_choice_fixed(&read_only_id, t!("open_files_read_only").as_ref(), &[]);
            dialog.set_choice(&read_only_id, "true");
        }
        if let Some(choices) = &self.choices {
            for choice in choices {
                let mut variants = vec![];
                for variant in &choice.variants {
                    variants.push((variant.id.as_str(), variant.label.as_str()));
                }
                dialog.add_choice_fixed(&choice.id, &choice.label, &variants);
                dialog.set_choice(&choice.id, &choice.default);
            }
        }
        crate::gui::setup_wayland(&dialog, &self.parent_window);
        DialogData {
            dialog,
            read_only_choice: read_only_id,
            filters: filters_map,
            dummy_parent,
        }
    }
}

fn map_filter(f: &Filter) -> FileFilter {
    let gf = FileFilter::new();
    gf.set_name(Some(&f.name));
    for kind in &f.elements {
        match kind {
            FilterKind::Glob(g) => gf.add_pattern(g),
            FilterKind::Mime(m) => gf.add_mime_type(m),
        }
    }
    gf
}

fn add_recent(app_id: &str, uris: &[String]) {
    let manager = RecentManager::default();
    for uri in uris {
        manager.add_full(
            uri,
            &RecentData::new(
                None,
                None,
                "application/octet-stream",
                app_id,
                "false",
                &[],
                false,
            ),
        );
    }
}

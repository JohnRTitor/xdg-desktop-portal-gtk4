use {
    crate::gui::{PortalDispatcher, UiError, UiProxy},
    gtk4::{
        FileChooserAction, FileChooserDialog, FileFilter, RecentData, RecentManager, ResponseType,
        gio::{self, File, prelude::ListModelExt},
        glib::{self, MainContext},
        prelude::{
            Cast, DialogExt, FileChooserExt, FileChooserExtManual, FileExt, GtkWindowExt,
            ObjectExt, RecentManagerExt, WidgetExt,
        },
    },
    rust_i18n::t,
    std::{borrow::Cow, cell::Cell, collections::HashMap, rc::Rc, time::Duration},
    tokio::sync::{mpsc::channel, oneshot::Receiver},
};

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Filter {
    pub name: String,
    pub elements: Vec<FilterKind>,
}

#[derive(Debug, Eq, PartialEq, Clone)]
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
    pub activation_token: Option<String>,
    pub app_id: String,
}

pub struct FileChooserResult {
    pub uris: Vec<String>,
    pub current_filter: Option<Filter>,
    pub final_choices: Option<Vec<FinalChoice>>,
    pub writeable: bool,
}

/// Work out which filters the dialog should offer and which one to preselect.
///
/// The spec allows `current_filter` to be sent either alongside a non-empty
/// `filters` list, in which case it selects one of them, or on its own, in which
/// case it is applied unconditionally. Matching within a list is done by name,
/// which is what GTK's own chooser does: two filters sharing a name are the same
/// filter, and an application that sends a slightly different `current_filter`
/// should still get its intent honoured.
///
/// Returns a [`Cow`] so the common case -- a caller that sent `filters` -- hands
/// back the caller's own slice instead of deep-cloning every [`Filter`] (its
/// `name` plus every element in its `Vec<FilterKind>`). Only the `current_filter`
/// -alone case has to build a new list, and only when there is one.
///
/// Kept free of GTK so the decision can be unit tested.
pub fn effective_filters<'a>(
    filters: Option<&'a [Filter]>,
    current_filter: Option<&'a Filter>,
) -> (Cow<'a, [Filter]>, Option<usize>) {
    match filters {
        Some(list) if !list.is_empty() => {
            let selected =
                current_filter.and_then(|cur| list.iter().position(|f| f.name == cur.name));
            (Cow::Borrowed(list), selected)
        }
        _ => match current_filter {
            Some(cur) => (Cow::Owned(vec![cur.clone()]), Some(0)),
            None => (Cow::Owned(Vec::new()), None),
        },
    }
}

struct DialogData {
    dialog: FileChooserDialog,
    read_only_choice: String,
    /// Maps a GTK filter to its index in [`Self::offered`].
    ///
    /// Storing the index rather than a cloned [`Filter`] matters because GTK
    /// hands back whichever filter the user picked from the list the dialog was
    /// given. Cloning here meant every offered filter was duplicated -- name plus
    /// every element -- purely to answer that one lookup.
    filters: HashMap<FileFilter, usize>,
    offered: Vec<Filter>,
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
        mut self,
        send: crate::gui::UiDispatcher<Result<FileChooserResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let DialogData {
            dialog,
            read_only_choice,
            filters,
            offered,
        } = self.build_dialog();
        let current_filter = Rc::new(Cell::new(dialog.filter()));
        let cf = current_filter.clone();
        let filter_handler = dialog.connect_filter_notify(move |f| cf.set(f.filter()));

        // Channel to coordinate: the response handler signals this when the user
        // closes the dialog, so the spawn_local task knows to start the delayed destroy.
        let (done_tx, mut done_rx) = channel::<()>(1);

        let cf = current_filter.clone();
        let handler_id = Rc::new(Cell::new(None));
        let handler_id_clone = handler_id.clone();
        let filter_handler_id = Rc::new(Cell::new(Some(filter_handler)));
        let filter_handler_clone = filter_handler_id.clone();
        let send_clone = send.clone();
        let response_handler = dialog.connect_response(move |dialog, r| {
            let res = match r {
                ResponseType::Ok => {
                    let selected = dialog.files();
                    // `filter_map` can drop entries, so the selection count is an
                    // upper bound: reserve it once instead of growing per file.
                    let mut files: Vec<String> = Vec::with_capacity(selected.n_items() as usize);
                    files.extend(
                        selected
                            .into_iter()
                            .filter_map(|f| f.ok().and_then(|f| f.downcast::<gio::File>().ok()))
                            .map(|f| {
                                let uri = f.uri();
                                if !uri.starts_with("file://")
                                    && let Some(path) = f.path()
                                {
                                    return gio::File::for_path(path).uri().into();
                                }
                                uri.into()
                            }),
                    );
                    add_recent(&self.app_id, &files);
                    let filter = cf
                        .take()
                        .and_then(|f| filters.get(&f))
                        .and_then(|&i| offered.get(i))
                        .cloned();
                    let configured = self.choices.as_deref().unwrap_or_default();
                    // At most one final choice per configured choice.
                    let mut choices = Vec::with_capacity(configured.len());
                    choices.extend(configured.iter().flat_map(|c| {
                        dialog.choice(&c.id).map(|v| FinalChoice {
                            id: c.id.clone(),
                            variant_id: v.into(),
                        })
                    }));
                    // `read_only_choice` is only injected for Open actions, so
                    // for any other action (and for `directory: true`) the
                    // lookup misses. Default to writable, not read-only, so a
                    // directory selection is not reported as a read-only grant.
                    let writeable = if read_only_choice.is_empty() {
                        true
                    } else {
                        dialog
                            .choice(&read_only_choice)
                            .map(|v| v == "false")
                            .unwrap_or(true)
                    };
                    Ok(FileChooserResult {
                        uris: files,
                        current_filter: filter,
                        final_choices: self.choices.is_some().then_some(choices),
                        writeable,
                    })
                }
                _ => Err(UiError::Rejected),
            };
            let _ = send_clone.dispatch(res);

            // Disconnect signal handlers to break reference cycles
            if let Some(id) = handler_id_clone.take() {
                dialog.disconnect(id);
            }
            if let Some(id) = filter_handler_clone.take() {
                dialog.disconnect(id);
            }

            dialog.close();
            let _ = done_tx.try_send(());
        });
        handler_id.set(Some(response_handler));

        let thumbnail_cancel = super::thumbnailer::setup_dialog_thumbnailer(&dialog, &context);

        dialog.show();
        context.spawn_local(async move {
            tokio::select! {
                _ = done_rx.recv() => {}
                _ = close_on_close => {}
            }
            // Cancel thumbnail work as soon as the dialog is closing, on both the
            // response and D-Bus cancellation paths. Relying only on `hide` can
            // leave queued jobs running through the delayed destroy below.
            thumbnail_cancel.cancel();
            // Ensure the window is gone for the cancellation path; normal responses
            // already closed it above. This also emits `hide`, which cancels again.
            if dialog.is_visible() {
                dialog.close();
            }
            // Delay destruction to work around GTK4 FileChooserWidget bugs where
            // background GIO tasks (like directory loading) can complete after
            // the dialog is disposed, causing use-after-free SEGVs (thaw_updates).
            glib::timeout_future(Duration::from_secs(5)).await;
            dialog.destroy();
        });
    }

    fn build_dialog(&mut self) -> DialogData {
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

        // GTK copies the title into the widget, so move the caller's String out
        // and lend a borrow rather than cloning the whole buffer.
        let title = std::mem::take(&mut self.title);
        let dialog = FileChooserDialog::new(
            Some(title.as_str()),
            None::<&gtk4::Window>,
            action,
            &buttons,
        );
        dialog.set_select_multiple(self.multiple);
        dialog.set_modal(self.modal);
        dialog.set_default_response(ResponseType::Ok);
        let mut filters_map = HashMap::new();
        // Take the caller's list rather than borrowing from `self`, so the
        // `Cow::Borrowed` arm of `effective_filters` can hand the original
        // storage straight through instead of deep-cloning every `Filter`.
        let taken = self.filters.take();
        let (offered, preselected) =
            effective_filters(taken.as_deref(), self.current_filter.as_ref());
        let offered = match offered {
            Cow::Owned(list) => list,
            Cow::Borrowed(_) => taken.unwrap_or_default(),
        };
        for (i, filter) in offered.iter().enumerate() {
            let mapped = map_filter(filter);
            dialog.add_filter(&mapped);
            if preselected == Some(i) {
                dialog.set_filter(&mapped);
            }
            filters_map.insert(mapped, i);
        }
        if let Some(f) = &self.current_name {
            dialog.set_current_name(f);
        }
        if let Some(f) = &self.current_folder {
            let _ = dialog.set_current_folder(Some(&File::for_path(f)));
        }
        if let Some(f) = &self.current_filename {
            // `current_file` is a filesystem path in the caller's encoding, not
            // a URI (the spec documents it as `ay`, like `current_folder`).
            // Building a GFile with for_uri produced a relative URI that GTK
            // silently ignored; for_path is what GTK's own chooser expects.
            let _ = dialog.set_file(&File::for_path(f));
        }
        let mut read_only_id = String::new();
        if action == FileChooserAction::Open {
            // The portal spec specifies that an 'Open' dialog should let the user
            // choose whether the file is opened read-only. We inject this choice
            // dynamically into the GTK dialog if it's an Open action.
            read_only_id = "_read_only".into();
            // Ensure our injected choice ID doesn't collide with one provided by
            // the frontend. `any` scans the handful of choices directly, so no
            // throwaway `HashSet` of every id has to be built.
            while self
                .choices
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|c| c.id.as_str() == read_only_id.as_str())
            {
                read_only_id.push('_');
            }
            dialog.add_choice(&read_only_id, t!("open_files_read_only").as_ref(), &[]);
            // Default to *writable*, which is what every other backend reports:
            // the injected choice is "open read-only", so it starts unchecked.
            // Defaulting it to checked flipped the `writable` result to false
            // for every OpenFile request, silently downgrading the grant.
            dialog.set_choice(&read_only_id, "false");
        }
        if let Some(choices) = &self.choices {
            for choice in choices {
                // `add_choice` takes `&[(&str, &str)]` by value-pair, so this
                // adapter `Vec` is required by the GTK signature and cannot be
                // avoided by handing over the caller's own storage.
                let variants: Vec<(&str, &str)> = choice
                    .variants
                    .iter()
                    .map(|variant| (variant.id.as_str(), variant.label.as_str()))
                    .collect();
                dialog.add_choice(&choice.id, &choice.label, &variants);
                dialog.set_choice(&choice.id, &choice.default);
            }
        }
        crate::gui::windowing::external_window::setup_window(
            &dialog,
            &self.parent_window,
            self.activation_token.as_deref(),
        );
        DialogData {
            dialog,
            read_only_choice: read_only_id,
            filters: filters_map,
            offered,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `FilterKind` has two data-carrying `String` variants. Rust can only use
    /// a variant's niche for the tag when at most one variant carries data, so
    /// the 24-byte `String` payload is joined by an 8-byte tag: 32 bytes on
    /// 64-bit. Pin it so the enum does not silently grow. There is no
    /// `#[repr(u8)]` to apply here -- the variants carry fields.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn filter_kind_is_two_words_plus_tag() {
        assert_eq!(std::mem::size_of::<FilterKind>(), 32);
    }

    #[test]
    fn effective_filters_borrows_the_caller_list_and_preselects_by_name() {
        let filters = vec![
            Filter {
                name: "Images".into(),
                elements: vec![FilterKind::Glob("*.png".into())],
            },
            Filter {
                name: "Audio".into(),
                elements: vec![FilterKind::Mime("audio/*".into())],
            },
        ];
        // A different `current_filter` must still select by name, as GTK does.
        let current = Filter {
            name: "Audio".into(),
            elements: vec![FilterKind::Glob("*".into())],
        };

        let (offered, selected) = effective_filters(Some(&filters), Some(&current));

        assert!(matches!(offered, Cow::Borrowed(_)));
        assert_eq!(offered.len(), 2);
        assert_eq!(selected, Some(1));
        assert!(std::ptr::eq(offered.as_ptr(), filters.as_ptr()));
    }

    #[test]
    fn effective_filters_with_filters_and_no_current_selects_none() {
        let filters = vec![Filter {
            name: "Images".into(),
            elements: vec![],
        }];

        let (offered, selected) = effective_filters(Some(&filters), None);

        assert!(matches!(offered, Cow::Borrowed(_)));
        assert_eq!(offered.len(), 1);
        assert_eq!(selected, None);
    }

    #[test]
    fn effective_filters_builds_a_single_filter_when_only_current_is_set() {
        let current = Filter {
            name: "Only".into(),
            elements: vec![FilterKind::Glob("*".into())],
        };

        let (offered, selected) = effective_filters(None, Some(&current));

        assert!(matches!(offered, Cow::Owned(_)));
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0], current);
        assert_eq!(selected, Some(0));
    }

    #[test]
    fn effective_filters_with_empty_list_falls_back_to_current() {
        let empty: Vec<Filter> = Vec::new();
        let current = Filter {
            name: "Only".into(),
            elements: vec![],
        };

        let (offered, selected) = effective_filters(Some(&empty), Some(&current));

        assert!(matches!(offered, Cow::Owned(_)));
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0], current);
        assert_eq!(selected, Some(0));
    }

    #[test]
    fn effective_filters_with_no_input_is_empty() {
        let (offered, selected) = effective_filters(None, None);

        assert!(matches!(offered, Cow::Owned(_)));
        assert!(offered.is_empty());
        assert_eq!(selected, None);
    }

    /// The common path -- the caller sent `filters` -- must hand back the
    /// caller's slice untouched: borrowing each `Filter` (its name plus every
    /// `FilterKind`) must allocate nothing.
    #[test]
    fn effective_filters_borrowed_arm_allocates_nothing() {
        use crate::alloc_probe::AllocScope;

        let filters = vec![
            Filter {
                name: "Images".into(),
                elements: vec![
                    FilterKind::Glob("*.png".into()),
                    FilterKind::Glob("*.jpg".into()),
                ],
            },
            Filter {
                name: "Audio".into(),
                elements: vec![FilterKind::Mime("audio/*".into())],
            },
        ];
        let current = Filter {
            name: "Audio".into(),
            elements: vec![],
        };

        let scope = AllocScope::start();
        let (offered, selected) = effective_filters(Some(&filters), Some(&current));
        let snap = scope.finish();
        std::hint::black_box(&offered);

        assert_eq!(selected, Some(1));
        assert_eq!(
            snap.count, 0,
            "borrowing the caller's filters must not allocate, got {snap:?}"
        );
    }

    /// Positive control for the allocation probe: the `current_filter`-alone
    /// arm genuinely has to build a new list, so it must allocate. Without this
    /// the zero-allocation assertion above could pass vacuously.
    #[test]
    fn effective_filters_owned_arm_allocates() {
        use crate::alloc_probe::AllocScope;

        let current = Filter {
            name: "Only".into(),
            elements: vec![FilterKind::Glob("*".into())],
        };

        let scope = AllocScope::start();
        let (offered, _) = effective_filters(None, Some(&current));
        let snap = scope.finish();
        std::hint::black_box(&offered);

        assert!(matches!(offered, Cow::Owned(_)));
        assert!(
            snap.count > 0,
            "cloning current_filter into a new list must allocate, got {snap:?}"
        );
    }
}

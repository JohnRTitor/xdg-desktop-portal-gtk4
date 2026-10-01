use {
    crate::gui::{PortalDispatcher, UiError, UiProxy},
    gtk4::{
        Align, Box as GtkBox, Button, Image, Label, ListBox, ListBoxRow, Orientation,
        ScrolledWindow,
        gio::{self, AppInfo},
        glib::{self, GString, MainContext},
        prelude::*,
    },
    rust_i18n::t,
    std::collections::HashSet,
    tokio::sync::{mpsc::Receiver as MpscReceiver, oneshot::Receiver as OneshotReceiver},
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
        update_receiver: MpscReceiver<Vec<String>>,
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
        send: crate::gui::UiDispatcher<Result<AppChooserResult, UiError>>,
        context: MainContext,
        close_on_close: OneshotReceiver<()>,
        mut update_receiver: MpscReceiver<Vec<String>>,
    ) {
        let dialog = crate::gui::dialog::CustomDialog::new(&self.title, true);

        let cancel_button = Button::with_label(&t!("cancel_action"));
        let ok_button = Button::with_label(&t!("open_action"));
        ok_button.set_sensitive(false);
        ok_button.add_css_class("suggested-action");

        dialog.action_area.append(&cancel_button);
        dialog.action_area.append(&ok_button);

        let label_text = if let Some(ref filename) = self.filename {
            t!("select_application_to_open_file", filename = filename)
        } else {
            t!("select_application_to_open")
        };
        let label = Label::new(Some(&*label_text));
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
            AppInfo::recommended_for_type(ct)
        } else {
            Vec::new()
        };

        populate_list_box(&list_box, &self.choices, &all_apps, &recommended_apps);

        // Spawn a task to listen for `UpdateChoices` D-Bus calls.
        // It runs on the main thread, so it can safely call `populate_list_box` to update GTK widgets.
        let list_box_clone2 = list_box.clone();
        context.spawn_local(async move {
            while let Some(new_choices) = update_receiver.recv().await {
                populate_list_box(&list_box_clone2, &new_choices, &all_apps, &recommended_apps);
            }
        });

        list_box.connect_row_selected(glib::clone!(
            #[weak]
            ok_button,
            move |_, row| {
                ok_button.set_sensitive(row.is_some());
            }
        ));

        let list_box_clone = list_box.clone();

        let window = dialog.window.clone();

        let send_close = send.clone();
        window.connect_close_request(move |_| {
            let _ = send_close.dispatch(Err(UiError::Rejected));
            glib::Propagation::Proceed
        });

        let send_cancel = send.clone();
        cancel_button.connect_clicked(glib::clone!(
            #[weak]
            window,
            move |_| {
                let _ = send_cancel.dispatch(Err(UiError::Rejected));
                window.close();
            }
        ));

        let send_ok = send.clone();
        ok_button.connect_clicked(glib::clone!(
            #[weak]
            window,
            #[weak]
            list_box_clone,
            move |_| {
                let res = if let Some(row) = list_box_clone.selected_row() {
                    let launch_context = gio::AppLaunchContext::new();
                    let token = launch_context
                        .startup_notify_id(None::<&gio::AppInfo>, &[])
                        .map(|s| s.into());
                    // The contract asks for the desktop file id *without* the
                    // `.desktop` suffix, so callers can use it directly as an
                    // application id. GAppInfo::id() includes the suffix.
                    let name = row.widget_name();
                    let choice = name.strip_suffix(".desktop").unwrap_or(&name).to_owned();
                    Ok(AppChooserResult {
                        choice,
                        activation_token: token,
                    })
                } else {
                    Err(UiError::Rejected)
                };
                let _ = send_ok.dispatch(res);
                window.close();
            }
        ));

        crate::gui::windowing::external_window::setup_window(
            &window,
            &self.parent_window,
            self.activation_token.as_deref(),
        );

        window.show();
        context.spawn_local(async move {
            let _ = close_on_close.await;
            window.close();
        });
    }
}

/// The identity and display name for one row of the chooser.
///
/// Holds glib's `GString` rather than a `String`: `AppInfo::id()` and
/// `AppInfo::name()` already hand back an owned `GString`, so calling
/// `.to_string()` allocated a second copy of each, per row, on every dialog
/// open and every `UpdateChoices`.
///
/// Decoupled from GTK so the selection rules can be unit tested without an
/// application database.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AppRow {
    id: GString,
    name: GString,
}

/// Chooses which entries to show, in display order, without duplicates.
///
/// `T` is carried along untouched so the caller can pair each [`AppRow`] with
/// the `AppInfo` it came from (for the icon) without cloning the row.
///
/// * `choices` non-empty: keep only entries whose id the frontend asked for.
///   Membership goes through a `HashSet` rather than a linear scan, because this
///   runs over every installed application and is re-run on every
///   `UpdateChoices`.
/// * `choices` empty: keep everything.
///
/// Entries sharing an id are collapsed, because `GAppInfo` can report the same
/// `.desktop` file more than once (for example from two `XDG_DATA_DIRS`), and
/// the caller returns one choice.
fn select_rows<T>(entries: &[(T, AppRow)], choices: &[String]) -> Vec<usize> {
    let wanted: HashSet<&str> = choices.iter().map(String::as_str).collect();
    let mut seen: HashSet<&str> = HashSet::with_capacity(entries.len());

    // One pass: filter by the frontend's choices and drop repeated ids,
    // keeping the first instance `AppInfo::all()` reported.

    let mut selected: Vec<usize> = (0..entries.len())
        .filter(|&i| {
            let id = entries[i].1.id.as_str();
            (choices.is_empty() || wanted.contains(id)) && seen.insert(id)
        })
        .collect();

    selected.sort_unstable_by(|&a, &b| {
        entries[a]
            .1
            .name
            .cmp(&entries[b].1.name)
            .then_with(|| a.cmp(&b))
    });
    selected
}

/// Which application list to populate the dialog from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppSource {
    All,
    Recommended,
}

/// Picks the application list, following the precedence the contract implies.
///
/// The frontend's named choices win over GIO's recommendation: when it named
/// some, only those may be offered. Otherwise prefer the recommendation, and fall
/// back to every installed application.
fn choose_source(choices: &[String], has_recommendation: bool) -> AppSource {
    if !choices.is_empty() {
        AppSource::All
    } else if has_recommendation {
        AppSource::Recommended
    } else {
        AppSource::All
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

    let source: &[AppInfo] = match choose_source(choices, !recommended_apps.is_empty()) {
        AppSource::All => all_apps,
        AppSource::Recommended => recommended_apps,
    };

    // Fetch each identity and name exactly once, keeping glib's `GString`
    // rather than copying it again into a `String`.
    let entries: Vec<(&AppInfo, AppRow)> = source
        .iter()
        .filter_map(|info| {
            // Without a desktop-file id the row could never be matched back to
            // a choice, so it is dropped rather than rendered and skipped.
            let id = info.id()?;
            Some((
                info,
                AppRow {
                    id,
                    name: info.name(),
                },
            ))
        })
        .collect();

    for i in select_rows(&entries, choices) {
        let (app, row) = &entries[i];
        let list_row = ListBoxRow::new();
        let hbox = GtkBox::new(Orientation::Horizontal, crate::gui::DEFAULT_SPACING);
        hbox.set_margin_top(crate::gui::SMALL_MARGIN);
        hbox.set_margin_bottom(crate::gui::SMALL_MARGIN);
        hbox.set_margin_start(crate::gui::SMALL_MARGIN);
        hbox.set_margin_end(crate::gui::SMALL_MARGIN);

        if let Some(icon) = app.icon() {
            let image = Image::from_gicon(&icon);
            image.set_pixel_size(32);
            hbox.append(&image);
        }

        let name_label = Label::new(Some(row.name.as_str()));
        name_label.set_halign(Align::Start);
        hbox.append(&name_label);

        list_row.set_child(Some(&hbox));
        list_row.set_widget_name(row.id.as_str());
        list_box.append(&list_row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a payload-free entry list so the selection rules can be exercised
    /// without GIO, whose contents depend on which applications the host has
    /// installed.
    fn entries(rows: &[(&str, &str)]) -> Vec<((), AppRow)> {
        rows.iter()
            .map(|&(id, name)| {
                (
                    (),
                    AppRow {
                        id: id.into(),
                        name: name.into(),
                    },
                )
            })
            .collect()
    }

    fn choices(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    fn ids<'a>(rows: &'a [((), AppRow)], selected: &[usize]) -> Vec<&'a str> {
        selected.iter().map(|&i| rows[i].1.id.as_str()).collect()
    }

    #[test]
    fn empty_choices_keep_everything_in_name_order() {
        let rows = entries(&[
            ("zeta.desktop", "Alpha"),
            ("alpha.desktop", "Zulu"),
            ("mid.desktop", "Mike"),
        ]);
        assert_eq!(
            ids(&rows, &select_rows(&rows, &[])),
            vec!["zeta.desktop", "mid.desktop", "alpha.desktop"],
        );
    }

    #[test]
    fn empty_entry_list_selects_nothing() {
        let rows = entries(&[]);
        assert!(select_rows(&rows, &[]).is_empty());
        assert!(select_rows(&rows, &choices(&["a.desktop"])).is_empty());
    }

    #[test]
    fn choices_filter_the_list() {
        let rows = entries(&[
            ("a.desktop", "Alpha"),
            ("b.desktop", "Bravo"),
            ("c.desktop", "Charlie"),
        ]);
        assert_eq!(
            ids(
                &rows,
                &select_rows(&rows, &choices(&["a.desktop", "c.desktop"]))
            ),
            vec!["a.desktop", "c.desktop"],
        );
    }

    #[test]
    fn order_follows_display_name_not_the_choices_order() {
        let rows = entries(&[("c.desktop", "Charlie"), ("a.desktop", "Alpha")]);
        assert_eq!(
            ids(
                &rows,
                &select_rows(&rows, &choices(&["c.desktop", "a.desktop"]))
            ),
            vec!["a.desktop", "c.desktop"],
        );
    }

    #[test]
    fn unknown_choices_are_ignored() {
        let rows = entries(&[("a.desktop", "Alpha")]);
        assert!(select_rows(&rows, &choices(&["nope.desktop"])).is_empty());
        assert_eq!(
            ids(
                &rows,
                &select_rows(&rows, &choices(&["a.desktop", "nope.desktop"]))
            ),
            vec!["a.desktop"],
        );
    }

    #[test]
    fn choice_matching_is_exact() {
        let rows = entries(&[("org.example.App.desktop", "App")]);
        // A prefix must not match, and neither must a superstring.
        assert!(select_rows(&rows, &choices(&["org.example"])).is_empty());
        assert!(select_rows(&rows, &choices(&["org.example.App.desktop.x"])).is_empty());
        assert_eq!(
            ids(
                &rows,
                &select_rows(&rows, &choices(&["org.example.App.desktop"]))
            ),
            vec!["org.example.App.desktop"],
        );
    }

    #[test]
    fn duplicate_ids_are_collapsed() {
        // GIO can report the same desktop file twice, from two data dirs.
        let rows = entries(&[("a.desktop", "Alpha"), ("a.desktop", "Alpha")]);
        assert_eq!(ids(&rows, &select_rows(&rows, &[])), vec!["a.desktop"]);
    }

    #[test]
    fn duplicates_are_collapsed_even_when_names_interleave() {
        // Two rows share a desktop-file id but carry different names, and a third
        // row sorts strictly between them. A name-first sort leaves the duplicates
        // non-adjacent, so `dedup_by` never compares them and keeps both.
        //
        // GIO does not currently produce this (the same `.desktop` file reports the
        // same display name, which sorts the duplicates together), so this is a
        // robustness property of the ordering rather than a live bug.
        let rows = entries(&[
            ("a.desktop", "Mike"),
            ("b.desktop", "Omega"),
            ("a.desktop", "Zulu"),
        ]);
        let selected = select_rows(&rows, &[]);
        assert_eq!(
            selected.len(),
            2,
            "the duplicate id must collapse even when a name-sort separates it",
        );
        let kept = ids(&rows, &selected);
        assert!(kept.contains(&"a.desktop"));
        assert!(kept.contains(&"b.desktop"));
    }

    #[test]
    fn distinct_ids_with_the_same_name_are_both_kept_in_enumeration_order() {
        // `sort_by` is stable and the tie-break is the original index, so equal
        // display names keep `AppInfo::all()`'s order -- which is what the
        // previous single `sort_by_key` produced.
        let rows = entries(&[("a.desktop", "Terminal"), ("b.desktop", "Terminal")]);
        assert_eq!(
            ids(&rows, &select_rows(&rows, &[])),
            vec!["a.desktop", "b.desktop"],
        );
    }

    #[test]
    fn selected_indices_are_valid_and_unique() {
        let rows = entries(&[
            ("a.desktop", "Alpha"),
            ("a.desktop", "Alpha"),
            ("b.desktop", "Bravo"),
        ]);
        let selected = select_rows(&rows, &[]);
        for &i in &selected {
            assert!(i < rows.len());
        }
        let mut sorted = selected.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), selected.len(), "no index may repeat");
    }

    #[test]
    fn dedup_keeps_the_first_enumerated_instance() {
        // GIO reporting the same desktop file twice must leave the entry the
        // enumeration saw first, so its icon does not change.
        let rows = entries(&[("a.desktop", "Alpha"), ("a.desktop", "Alpha")]);
        let selected = select_rows(&rows, &[]);
        assert_eq!(selected, vec![0]);
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn frontend_choices_win_over_a_recommendation() {
        assert_eq!(
            choose_source(&["a.desktop".to_owned()], true),
            AppSource::All
        );
    }

    #[test]
    fn a_recommendation_is_used_when_no_choices_are_given() {
        assert_eq!(choose_source(&[], true), AppSource::Recommended);
    }

    #[test]
    fn everything_is_shown_when_there_is_nothing_to_prefer() {
        assert_eq!(choose_source(&[], false), AppSource::All);
    }

    #[test]
    fn an_empty_choice_list_is_still_a_choice_list() {
        // An empty `Vec` and an empty `String` are different inputs: the caller
        // sends a `Vec`, and an empty one means "no preference", not "the
        // frontend named nothing so use its recommendation".
        assert_eq!(choose_source(&[], true), AppSource::Recommended);
        assert_eq!(choose_source(&["".to_owned()], true), AppSource::All);
    }
}

#[cfg(test)]
mod row_tests {
    use super::*;

    /// The regression this guards: `AppInfo::id()` and `AppInfo::name()` return
    /// an owned `GString`, and calling `.to_string()` on it allocated a second
    /// copy of the same text -- once per field, per row, on every dialog open
    /// and every `UpdateChoices`.
    ///
    /// Measured on an already-owned `GString`, which is what GIO hands back, so
    /// the number cannot be an artefact of constructing one from a literal:
    /// copying costs one allocation per field, moving costs none.
    #[test]
    fn copying_an_owned_gstring_costs_an_allocation_per_field() {
        let owned: GString = "org.gnome.TextEditor.desktop".into();

        let copy_scope = crate::alloc_probe::AllocScope::start();
        let copied: String = owned.to_string();
        let copy_snap = copy_scope.finish();
        std::hint::black_box(&copied);

        let move_scope = crate::alloc_probe::AllocScope::start();
        let moved: GString = owned;
        let move_snap = move_scope.finish();
        std::hint::black_box(&moved);

        assert_eq!(copied, "org.gnome.TextEditor.desktop");
        assert_eq!(
            copy_snap.count, 1,
            "`.to_string()` on an owned GString copies the text, got {copy_snap:?}"
        );
        assert_eq!(
            move_snap.count, 0,
            "moving the GString through must not copy, got {move_snap:?}"
        );
    }

    /// `AppRow` must not reintroduce the copy through one of its own accessors.
    #[test]
    fn a_row_exposes_its_fields_without_copying() {
        let row = AppRow {
            id: "a.desktop".into(),
            name: "Alpha".into(),
        };

        let scope = crate::alloc_probe::AllocScope::start();
        let id = row.id.as_str();
        let name = row.name.as_str();
        let snap = scope.finish();
        std::hint::black_box(id);
        std::hint::black_box(name);

        assert_eq!(id, "a.desktop");
        assert_eq!(name, "Alpha");
        assert_eq!(
            snap.count, 0,
            "reading a row must not allocate, got {snap:?}"
        );
    }
}

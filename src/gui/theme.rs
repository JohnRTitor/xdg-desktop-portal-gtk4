//! Live reload of the user GTK stylesheet.
//!
//! GTK reads `~/.config/gtk-4.0/gtk.css` only when it starts, so a theme switch
//! leaves the portal's own dialogs on the previous palette until the service is
//! restarted. Mirror that file in a [`CssProvider`] and reload it when it
//! changes: the provider repaints the dialogs that are already open.

use {
    gtk4::{CssProvider, gdk::Display, gio, glib, prelude::*},
    std::{cell::RefCell, fs, mem, path::Path, rc::Rc, sync::OnceLock, time::Duration},
};

/// Sink for stylesheet contents, so the load/clear decision is testable
/// without constructing a real (display-bound) `CssProvider`.
trait CssTarget {
    fn set_from_path(&self, path: &Path);
    fn set_empty(&self);
}

impl CssTarget for CssProvider {
    fn set_from_path(&self, path: &Path) {
        self.load_from_path(path);
    }

    fn set_empty(&self) {
        self.load_from_data("");
    }
}

fn load_user_css(target: &impl CssTarget, path: &Path) {
    if path.is_file() {
        // Replace the CSS inside this provider: a v2 stylesheet goes into the
        // same slot at USER + 1, it does not create a second provider.
        target.set_from_path(path);
    } else {
        // Clear our mirror. GTK's own startup provider may still hold the old
        // stylesheet, so this does not fully retract its declarations.
        target.set_empty();
    }
}

/// Does this monitor event concern the user stylesheet? Both sides of a move
/// are checked, since GIO reports the destination in `other_file` on some
/// backends and atomic theme switches rely on that event.
fn is_css_event(file: &gio::File, other_file: Option<&gio::File>) -> bool {
    let is_css = |f: &gio::File| {
        f.basename()
            .is_some_and(|name| name == Path::new("gtk.css"))
    };
    is_css(file) || other_file.is_some_and(is_css)
}

/// Mirror `~/.config/gtk-4.0/gtk.css` in a display-level provider that follows
/// rewrites and symlink swaps, so open dialogs repaint on a theme switch.
pub fn watch_user_theme() {
    // Installing a second provider would mask the first and leak its monitor.
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }

    let Some(display) = Display::default() else {
        tracing::debug!("No default display; live user CSS reload is disabled");
        return;
    };

    let dir = glib::user_config_dir().join("gtk-4.0");
    if let Err(err) = fs::create_dir_all(&dir) {
        tracing::debug!(path = %dir.display(), %err, "Could not create gtk-4.0 config directory");
        return;
    }

    let css = dir.join("gtk.css");
    let provider = CssProvider::new();
    provider.connect_parsing_error(|_, _, err| {
        tracing::warn!(%err, "Invalid user gtk.css");
    });

    load_user_css(&provider, &css);
    // GTK loads gtk.css at startup into its own provider at USER (800) and
    // never reloads it — that layer is frozen with the startup stylesheet.
    // Register one provider one step above it, so our current CSS wins where
    // both declare a property. This happens once; reloading replaces this
    // provider's contents rather than adding another provider at USER + 1.
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_USER + 1,
    );

    let Ok(monitor) = gio::File::for_path(&dir)
        .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
    else {
        tracing::debug!(path = %dir.display(), "Could not monitor gtk-4.0 for CSS changes");
        return;
    };
    monitor.set_rate_limit(100);

    // A single rewrite produces several events; coalesce them into one reload
    // instead of parsing a partially written stylesheet on the main thread.
    let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
    monitor.connect_changed({
        let provider = provider.clone();
        let css = css.clone();
        let pending = pending.clone();
        move |_, file, other_file, _| {
            if !is_css_event(file, other_file) {
                return;
            }
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            let id = glib::timeout_add_local_once(Duration::from_millis(50), {
                let provider = provider.clone();
                let css = css.clone();
                let pending = pending.clone();
                move || {
                    *pending.borrow_mut() = None;
                    load_user_css(&provider, &css);
                }
            });
            *pending.borrow_mut() = Some(id);
        }
    });
    // Keep the monitor (and its callback) alive for the daemon's lifetime.
    mem::forget(monitor);
}

#[cfg(test)]
mod tests {
    use {super::*, std::path::PathBuf};

    #[derive(Debug, PartialEq)]
    enum Loaded {
        Path(PathBuf),
        Empty,
    }

    #[derive(Default)]
    struct Recorder(RefCell<Vec<Loaded>>);

    impl CssTarget for Recorder {
        fn set_from_path(&self, path: &Path) {
            self.0.borrow_mut().push(Loaded::Path(path.to_path_buf()));
        }

        fn set_empty(&self) {
            self.0.borrow_mut().push(Loaded::Empty);
        }
    }

    fn config_file(name: &str) -> gio::File {
        gio::File::for_path(Path::new("/config/gtk-4.0").join(name))
    }

    #[test]
    fn a_stylesheet_event_is_recognized() {
        assert!(is_css_event(&config_file("gtk.css"), None));
    }

    #[test]
    fn the_destination_of_a_move_is_recognized() {
        assert!(is_css_event(
            &config_file("gtk.css.new"),
            Some(&config_file("gtk.css"))
        ));
    }

    #[test]
    fn unrelated_config_files_are_ignored() {
        assert!(!is_css_event(&config_file("settings.ini"), None));
        assert!(!is_css_event(&config_file("gtk.css~"), None));
        assert!(!is_css_event(&config_file("assets"), None));
        assert!(!is_css_event(
            &config_file("gtk.css.new"),
            Some(&config_file("gtk.css~"))
        ));
    }

    #[test]
    fn an_existing_stylesheet_is_loaded_from_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let css = dir.path().join("gtk.css");
        fs::write(&css, "button {}").unwrap();
        let recorder = Recorder::default();
        load_user_css(&recorder, &css);
        assert_eq!(recorder.0.borrow().as_slice(), &[Loaded::Path(css)]);
    }

    #[test]
    fn a_missing_stylesheet_clears_the_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let css = dir.path().join("gtk.css");
        let recorder = Recorder::default();
        load_user_css(&recorder, &css);
        assert_eq!(recorder.0.borrow().as_slice(), &[Loaded::Empty]);
    }

    #[test]
    fn a_directory_named_gtk_css_does_not_load_as_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let css = dir.path().join("gtk.css");
        fs::create_dir(&css).unwrap();
        let recorder = Recorder::default();
        load_user_css(&recorder, &css);
        assert_eq!(recorder.0.borrow().as_slice(), &[Loaded::Empty]);
    }
}

//! Live reload of the user GTK stylesheet.
//!
//! GTK reads `~/.config/gtk-4.0/gtk.css` only when it starts, so a theme switch
//! leaves the portal's own dialogs on the previous palette until the service is
//! restarted. Mirror that file in a [`CssProvider`] and reload it when it
//! changes: the provider repaints the dialogs that are already open.

use {
    gtk4::{CssProvider, gdk, gio, glib, prelude::*},
    std::{cell::RefCell, path::Path},
};

thread_local! {
    // The monitor is the only owner of the watch; keep it for the process lifetime.
    static USER_CSS_MONITOR: RefCell<Option<gio::FileMonitor>> = const { RefCell::new(None) };
}

fn load_user_css(provider: &CssProvider, path: &Path) {
    if path.is_file() {
        provider.load_from_path(path);
    } else {
        // No user stylesheet: drop the previous one instead of keeping it stale.
        provider.load_from_data("");
    }
}

/// Mirror `~/.config/gtk-4.0/gtk.css` in a display provider that follows
/// rewrites and symlink swaps, so open dialogs repaint on a theme switch.
pub fn watch_user_theme() {
    let dir = glib::user_config_dir().join("gtk-4.0");
    let css = dir.join("gtk.css");
    let provider = CssProvider::new();
    load_user_css(&provider, &css);
    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
    }

    let monitor = gio::File::for_path(&dir)
        .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        .ok();
    if let Some(monitor) = &monitor {
        monitor.connect_changed(move |_, file, _, _| {
            let changed = file
                .basename()
                .is_some_and(|name| name == Path::new("gtk.css") || name == Path::new("assets"));
            if changed {
                load_user_css(&provider, &css);
            }
        });
    }
    USER_CSS_MONITOR.with(|slot| *slot.borrow_mut() = monitor);
}

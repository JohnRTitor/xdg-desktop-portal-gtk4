use {
    super::{
        gui as file_chooser,
        gui::{ChoiceVariant, FileChooserUi, Filter, FilterKind, FinalChoice},
    },
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    gtk4::{gio, prelude::FileExt},
    serde::{Deserialize, Deserializer, de::Error as DeError},
    std::{ffi::CString, path::Path},
    thiserror::Error,
    zbus::{
        DeserializeDict, ObjectServer, OwnedObjectPath, SerializeDict, Type, fdo, interface,
        message::Header,
    },
};

#[cfg(test)]
use std::{hint::black_box, mem};

/// D-Bus interface wrapper for the FileChooser portal.
///
/// This struct holds a proxy to the GTK main loop. It does not store active session
/// state itself; instead, each D-Bus method call runs as an independent async task
/// and uses the proxy to spawn a GTK `FileChooserDialog`.
pub struct FileChooser {
    proxy: UiProxy,
    session_manager: crate::core::session_manager::SessionManager,
}

impl FileChooser {
    pub fn new(
        proxy: &UiProxy,
        session_manager: crate::core::session_manager::SessionManager,
    ) -> Self {
        Self {
            proxy: proxy.clone(),
            session_manager,
        }
    }
}

type Choice = (String, String, Vec<(String, String)>, String);

type FileFilter = (String, Vec<(u32, String)>);

#[derive(Type, Debug, Default, PartialEq)]
#[zbus(signature = "ay")]
struct FilePath(String);

impl<'de> Deserialize<'de> for FilePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = <Vec<u8>>::deserialize(deserializer)?;
        let c_string = CString::from_vec_with_nul(bytes)
            .map_err(|_| DeError::custom("Bytes are not nul-terminated"))?;
        // Valid UTF-8 is the common case: `String::from_utf8` reuses the byte
        // buffer with no second allocation. Only pay for the lossy replacement
        // path when the bytes genuinely are not valid UTF-8.
        Ok(Self(match String::from_utf8(c_string.into_bytes()) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        }))
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
struct OpenFileOptions {
    accept_label: Option<String>,
    modal: Option<bool>,
    multiple: Option<bool>,
    directory: Option<bool>,
    filters: Option<Vec<FileFilter>>,
    current_filter: Option<FileFilter>,
    choices: Option<Vec<Choice>>,
    current_folder: Option<FilePath>,
    pub activation_token: Option<String>,
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
struct SaveFileOptions {
    accept_label: Option<String>,
    modal: Option<bool>,
    filters: Option<Vec<FileFilter>>,
    current_filter: Option<FileFilter>,
    choices: Option<Vec<Choice>>,
    current_name: Option<String>,
    current_folder: Option<FilePath>,
    #[zbus(rename = "current_file")]
    current_file: Option<FilePath>,
    pub activation_token: Option<String>,
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
struct SaveFilesOptions {
    accept_label: Option<String>,
    modal: Option<bool>,
    choices: Option<Vec<Choice>>,
    current_folder: Option<FilePath>,
    files: Option<Vec<FilePath>>,
    pub activation_token: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
struct OpenFileResults {
    uris: Option<Vec<String>>,
    choices: Option<Vec<(String, String)>>,
    current_filter: Option<FileFilter>,
    writable: Option<bool>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
struct SaveFileResults {
    uris: Option<Vec<String>>,
    choices: Option<Vec<(String, String)>>,
    current_filter: Option<FileFilter>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
struct SaveFilesResults {
    uris: Option<Vec<String>>,
    choices: Option<Vec<(String, String)>>,
}

#[derive(Debug, Error)]
enum SaveFilesError {
    #[error("User did not select exactly one path")]
    NotExactlyOnePath,
    #[error("Client tried to save a file with an empty name")]
    EmptyName,
    #[error("Client tried to save an absolute path")]
    AbsolutePath,
    #[error("Client tried to save a path with multiple components")]
    MultipleComponents,
    #[error("Client tried to save `.` or `..`")]
    SpecialPath,
    #[error("The selected path is not a path")]
    SelectedNotValidPath,
    #[error(transparent)]
    Ui(crate::gui::UiError),
}

impl FileChooser {
    async fn open_file_impl(
        &self,
        app_id: String,
        parent_window: String,
        title: String,
        options: OpenFileOptions,
    ) -> Response<OpenFileResults> {
        let res = FileChooserUi {
            title,
            multiple: options.multiple.unwrap_or(false),
            accept_label: options.accept_label,
            modal: options.modal.unwrap_or(true),
            directory: options.directory.unwrap_or(false),
            filters: options.filters.map(map_filters),
            current_filter: options.current_filter.map(map_filter),
            current_name: None,
            current_folder: options.current_folder.map(map_cstr),
            current_filename: None,
            choices: options.choices.map(map_choices),
            save: false,
            parent_window,
            activation_token: options.activation_token,
            app_id,
        }
        .run(&self.proxy)
        .await;
        match res {
            Ok(res) => Response::success(OpenFileResults {
                uris: Some(res.uris),
                choices: res.final_choices.map(map_final_choices),
                current_filter: res.current_filter.map(unmap_filter),
                writable: Some(res.writeable),
            }),
            Err(e) => {
                if e.is_user_cancellation() {
                    tracing::debug!(error = %e, "OpenFile cancelled by user");
                } else {
                    tracing::error!(error = %e, "OpenFile failed");
                }
                Response::from_ui_error(e)
            }
        }
    }

    async fn save_file_impl(
        &self,
        app_id: String,
        parent_window: String,
        title: String,
        options: SaveFileOptions,
    ) -> Response<SaveFileResults> {
        let res = FileChooserUi {
            title,
            multiple: false,
            accept_label: options.accept_label,
            modal: options.modal.unwrap_or(true),
            directory: false,
            filters: options.filters.map(map_filters),
            current_filter: options.current_filter.map(map_filter),
            current_name: options.current_name,
            current_folder: options.current_folder.map(map_cstr),
            current_filename: options.current_file.map(map_cstr),
            choices: options.choices.map(map_choices),
            save: true,
            parent_window,
            activation_token: options.activation_token,
            app_id,
        }
        .run(&self.proxy)
        .await;
        match res {
            Ok(res) => Response::success(SaveFileResults {
                uris: Some(res.uris),
                choices: res.final_choices.map(map_final_choices),
                current_filter: res.current_filter.map(unmap_filter),
            }),
            Err(e) => {
                if e.is_user_cancellation() {
                    tracing::debug!(error = %e, "SaveFile cancelled by user");
                } else {
                    tracing::error!(error = %e, "SaveFile failed");
                }
                Response::from_ui_error(e)
            }
        }
    }

    async fn try_save_files_impl(
        &self,
        app_id: String,
        parent_window: String,
        title: String,
        options: SaveFilesOptions,
    ) -> Result<SaveFilesResults, SaveFilesError> {
        let files = options.files.as_deref().unwrap_or(&[]);

        // Security checks: The client provides paths to save, but we must ensure
        // they don't contain absolute paths or directory traversal attacks. We are
        // going to combine these provided filenames with a directory that the user
        // selects. If a malicious client passed `../../etc/passwd`, they could
        // trick the portal into overwriting critical files.
        for file in files {
            let file = Path::new(&file.0);
            if file.as_os_str().is_empty() {
                // An empty name is not a path at all. It has no components, so
                // it slips past the checks below, and `base.join("")` would
                // resolve to the selected directory itself, which then gets
                // renamed to "<dirname> (1)" and returned as a save target.
                return Err(SaveFilesError::EmptyName);
            }
            if file.is_absolute() {
                return Err(SaveFilesError::AbsolutePath);
            }
            if file.components().nth(1).is_some() {
                return Err(SaveFilesError::MultipleComponents);
            }
            if file == Path::new(".") || file == Path::new("..") {
                return Err(SaveFilesError::SpecialPath);
            }
        }
        let mut res = FileChooserUi {
            title,
            multiple: false,
            accept_label: options.accept_label,
            modal: options.modal.unwrap_or(true),
            directory: true,
            filters: None,
            current_filter: None,
            current_name: None,
            current_folder: options.current_folder.map(map_cstr),
            current_filename: None,
            choices: options.choices.map(map_choices),
            save: true,
            parent_window,
            activation_token: options.activation_token,
            app_id,
        }
        .run(&self.proxy)
        .await
        .map_err(SaveFilesError::Ui)?;
        if res.uris.len() != 1 {
            return Err(SaveFilesError::NotExactlyOnePath);
        }
        let Some(uri) = res.uris.pop() else {
            return Err(SaveFilesError::NotExactlyOnePath);
        };
        let base = gio::File::for_uri(&uri)
            .path()
            .ok_or(SaveFilesError::SelectedNotValidPath)?;
        let mut uris = Vec::with_capacity(files.len());
        for file in files {
            let mut path = base.join(&file.0);
            if path.exists() {
                // These must stay owned: `set_file_name` below mutates `path`
                // while `stem`/`ext` are still live, so a `Cow` borrowing from
                // `path` (what `to_string_lossy` would give) cannot be held here.
                let stem = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
                for i in 1u64.. {
                    let new_name = match &ext {
                        Some(e) => format!("{stem} ({i}).{e}"),
                        None => format!("{stem} ({i})"),
                    };
                    path.set_file_name(new_name);
                    if !path.exists() {
                        break;
                    }
                }
            }
            uris.push(String::from(gio::File::for_path(&path).uri().as_str()));
        }
        Ok(SaveFilesResults {
            uris: Some(uris),
            choices: res.final_choices.map(map_final_choices),
        })
    }

    async fn save_files_impl(
        &self,
        app_id: String,
        parent_window: String,
        title: String,
        options: SaveFilesOptions,
    ) -> Response<SaveFilesResults> {
        match self
            .try_save_files_impl(app_id, parent_window, title, options)
            .await
        {
            Ok(res) => Response::success(res),
            Err(SaveFilesError::Ui(e)) => {
                if e.is_user_cancellation() {
                    tracing::debug!(error = %e, "SaveFiles cancelled by user");
                } else {
                    tracing::error!(error = %e, "SaveFiles failed");
                }
                Response::from_ui_error(e)
            }
            Err(e) => {
                tracing::error!(error = %e, "SaveFiles failed");
                // A rejected request never reached a dialog the user answered --
                // the validation ran first -- so this is a backend failure and
                // not a cancellation.
                Response::other()
            }
        }
    }
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.FileChooser`.
///
/// This portal handles `OpenFile`, `SaveFile`, and `SaveFiles` requests.
/// It wraps GTK's native `FileChooserDialog`.
#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    #[expect(
        clippy::too_many_arguments,
        reason = "OpenFile in https://github.com/flatpak/xdg-desktop-portal/tree/main/data/org.freedesktop.impl.portal.FileChooser.xml fixes five wire in-args (o, s, s, s, a{sv}); the handler also takes #[zbus(header)] and #[zbus(object_server)] arguments, so this Rust function exceeds clippy's 7-argument limit."
    )]
    async fn open_file(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: OpenFileOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Response<OpenFileResults>, fdo::Error> {
        // `sender()` borrows from `header`, which lives for the whole method.
        // `run_request` copies it into owned storage before spawning work, so
        // pass the borrow through instead of allocating an owned copy.
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;
        Ok(run_request(
            server,
            self.session_manager.clone(),
            &app_id,
            sender.as_str(),
            handle,
            self.open_file_impl(app_id.clone(), parent_window, title, options),
        )
        .await)
    }

    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    #[expect(
        clippy::too_many_arguments,
        reason = "SaveFile in https://github.com/flatpak/xdg-desktop-portal/tree/main/data/org.freedesktop.impl.portal.FileChooser.xml fixes five wire in-args (o, s, s, s, a{sv}); the handler also takes #[zbus(header)] and #[zbus(object_server)] arguments, so this Rust function exceeds clippy's 7-argument limit."
    )]
    async fn save_file(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: SaveFileOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Response<SaveFileResults>, fdo::Error> {
        // As in `open_file`, pass the borrowed sender through: `run_request`
        // copies it into owned storage before spawning work.
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;
        Ok(run_request(
            server,
            self.session_manager.clone(),
            &app_id,
            sender.as_str(),
            handle,
            self.save_file_impl(app_id.clone(), parent_window, title, options),
        )
        .await)
    }

    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    #[expect(
        clippy::too_many_arguments,
        reason = "SaveFiles in https://github.com/flatpak/xdg-desktop-portal/tree/main/data/org.freedesktop.impl.portal.FileChooser.xml fixes five wire in-args (o, s, s, s, a{sv}); the handler also takes #[zbus(header)] and #[zbus(object_server)] arguments, so this Rust function exceeds clippy's 7-argument limit."
    )]
    async fn save_files(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: SaveFilesOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Response<SaveFilesResults>, fdo::Error> {
        // As in `open_file`, pass the borrowed sender through: `run_request`
        // copies it into owned storage before spawning work.
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;
        Ok(run_request(
            server,
            self.session_manager.clone(),
            &app_id,
            sender.as_str(),
            handle,
            self.save_files_impl(app_id.clone(), parent_window, title, options),
        )
        .await)
    }
}

fn map_filters(f: Vec<FileFilter>) -> Vec<Filter> {
    let mut out = Vec::with_capacity(f.len());
    out.extend(f.into_iter().map(map_filter));
    out
}

fn map_filter(f: FileFilter) -> Filter {
    // `flat_map` drops unknown kinds, so the input length is an upper bound;
    // reserve it once instead of letting the `Vec` grow on each appended element.
    let mut elements = Vec::with_capacity(f.1.len());
    elements.extend(f.1.into_iter().flat_map(|(kind, value)| match kind {
        0 => Some(FilterKind::Glob(value)),
        1 => Some(FilterKind::Mime(value)),
        _ => None,
    }));
    Filter {
        name: f.0,
        elements,
    }
}

fn unmap_filter(f: Filter) -> FileFilter {
    let mut elements = Vec::with_capacity(f.elements.len());
    elements.extend(f.elements.into_iter().map(|f| match f {
        FilterKind::Glob(v) => (0, v),
        FilterKind::Mime(v) => (1, v),
    }));
    (f.name, elements)
}

fn map_cstr(f: FilePath) -> String {
    f.0
}

fn map_choices(c: Vec<Choice>) -> Vec<file_chooser::Choice> {
    let mut out = Vec::with_capacity(c.len());
    out.extend(c.into_iter().map(map_choice));
    out
}

fn map_choice(c: Choice) -> file_chooser::Choice {
    let mut variants = Vec::with_capacity(c.2.len());
    variants.extend(c.2.into_iter().map(|c| ChoiceVariant {
        id: c.0,
        label: c.1,
    }));
    file_chooser::Choice {
        id: c.0,
        label: c.1,
        default: c.3,
        variants,
    }
}

fn map_final_choices(c: Vec<FinalChoice>) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(c.len());
    out.extend(c.into_iter().map(map_final_choice));
    out
}

fn map_final_choice(c: FinalChoice) -> (String, String) {
    (c.id, c.variant_id)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        gtk4::glib::MainContext,
        tokio::sync::mpsc::unbounded_channel,
        zbus::{Connection, Type},
    };

    /// Six fieldless variants plus `Ui(UiError)`. `UiError` is a 1-byte enum
    /// with spare discriminant values, which the compiler uses as a niche to
    /// tag the outer enum, so the whole thing packs into a single byte. Pin it
    /// so an accidental widening (a pointer-bearing variant, a `#[repr]` change)
    /// is caught rather than silently growing every `Response` path.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn save_files_error_is_one_byte() {
        assert_eq!(mem::size_of::<SaveFilesError>(), 1);
    }

    #[test]
    fn test_map_filter_glob() {
        let f = map_filter(("Images".into(), vec![(0, "*.png".into())]));
        assert_eq!(f.name, "Images");
        assert_eq!(f.elements.len(), 1);
        assert!(matches!(f.elements[0], FilterKind::Glob(ref v) if v == "*.png"));
    }

    #[test]
    fn test_map_filter_mime() {
        let f = map_filter(("Audio".into(), vec![(1, "audio/*".into())]));
        assert_eq!(f.name, "Audio");
        assert_eq!(f.elements.len(), 1);
        assert!(matches!(f.elements[0], FilterKind::Mime(ref v) if v == "audio/*"));
    }

    #[test]
    fn test_map_filter_unknown_kind_skipped() {
        let f = map_filter((
            "Mixed".into(),
            vec![(0, "*.png".into()), (99, "unknown".into())],
        ));
        assert_eq!(f.elements.len(), 1);
        assert!(matches!(f.elements[0], FilterKind::Glob(ref v) if v == "*.png"));
    }

    #[test]
    fn test_unmap_filter_roundtrip() {
        let original: FileFilter = (
            "Images".into(),
            vec![(0, "*.png".into()), (1, "image/png".into())],
        );
        let mapped = map_filter(original.clone());
        let unmapped = unmap_filter(mapped);
        assert_eq!(original, unmapped);
    }

    #[test]
    fn test_map_choices() {
        let choice = (
            "encoding".into(),
            "Encoding".into(),
            vec![("utf8".into(), "UTF-8".into())],
            "utf8".into(),
        );
        let mapped = map_choices(vec![choice]);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].id, "encoding");
        assert_eq!(mapped[0].label, "Encoding");
        assert_eq!(mapped[0].default, "utf8");
        assert_eq!(mapped[0].variants.len(), 1);
        assert_eq!(mapped[0].variants[0].id, "utf8");
        assert_eq!(mapped[0].variants[0].label, "UTF-8");
    }

    #[test]
    fn test_map_cstr() {
        let path = FilePath("hello".into());
        assert_eq!(map_cstr(path), "hello");
    }

    /// The fast path must reuse the decoded byte buffer rather than copy it
    /// into a second `String`. Compare against the lossy conversion on the same
    /// bytes: it pays for one extra `String` allocation. (`CString::into_bytes`
    /// itself shrinks the decoded `Vec` to a boxed slice, which the counting
    /// shim records once in both paths, so comparing against a bare `Vec<u8>`
    /// deserialization would be off by one.)
    #[test]
    fn file_path_deserialize_reuses_the_byte_buffer() {
        use {
            crate::alloc_probe::AllocScope,
            std::ffi::CString,
            zbus::wire::{self, Endian, serialized::Context},
        };

        let path_bytes: Vec<u8> = b"/home/user/document.txt\0".to_vec();
        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &path_bytes).unwrap();

        // Warm up zbus's lazy state before measuring.
        let _: (FilePath, _) = encoded.deserialize().unwrap();

        let path_scope = AllocScope::start();
        let (path, _): (FilePath, _) = encoded.deserialize().unwrap();
        let path_snap = path_scope.finish();

        let lossy_scope = AllocScope::start();
        let (lossy_bytes, _): (Vec<u8>, _) = encoded.deserialize().unwrap();
        let lossy = String::from_utf8_lossy(
            &CString::from_vec_with_nul(lossy_bytes)
                .unwrap()
                .into_bytes(),
        )
        .into_owned();
        let lossy_snap = lossy_scope.finish();

        black_box(&path);
        black_box(&lossy);

        assert_eq!(path.0, "/home/user/document.txt");
        assert_eq!(path.0, lossy);
        assert_eq!(
            lossy_snap.count,
            path_snap.count + 1,
            "the old lossy path copied the buffer into a second String: \
             got {path_snap:?} vs lossy {lossy_snap:?}"
        );
    }

    /// The `from_utf8` fast path must fall back to lossy decoding for bytes
    /// that are not valid UTF-8, preserving the old behavior.
    #[test]
    fn file_path_deserialize_falls_back_for_invalid_utf8() {
        use zbus::wire::{self, Endian, serialized::Context};

        let bytes = [0xffu8, 0xfe, b'/', b'x', 0];
        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &bytes.to_vec()).unwrap();
        let (path, _): (FilePath, _) = encoded.deserialize().unwrap();
        assert_eq!(path.0, String::from_utf8_lossy(&bytes[..4]).into_owned());
    }

    #[test]
    fn test_map_final_choices() {
        let c = FinalChoice {
            id: "encoding".into(),
            variant_id: "utf8".into(),
        };
        let mapped = map_final_choices(vec![c]);
        assert_eq!(mapped, vec![("encoding".into(), "utf8".into())]);
    }

    #[test]
    fn test_open_file_options_signature() {
        assert_eq!(OpenFileOptions::SIGNATURE, "a{sv}");
    }

    #[test]
    fn test_open_file_results_signature() {
        assert_eq!(OpenFileResults::SIGNATURE, "a{sv}");
    }

    #[tokio::test]
    async fn test_save_files_validation_absolute_path() {
        let proxy = crate::gui::UiProxy {
            context: MainContext::default(),
            sender: unbounded_channel().0,
        };
        let Ok(conn) = Connection::session().await else {
            return;
        };
        let chooser = FileChooser::new(
            &proxy,
            crate::core::session_manager::SessionManager::new(conn, 10),
        );
        let options = SaveFilesOptions {
            files: Some(vec![FilePath("/absolute/path".into())]),
            ..Default::default()
        };
        let res = chooser
            .try_save_files_impl("app_id".into(), "".into(), "".into(), options)
            .await;
        assert!(matches!(res, Err(SaveFilesError::AbsolutePath)));
    }

    #[tokio::test]
    async fn test_save_files_validation_multiple_components() {
        let proxy = crate::gui::UiProxy {
            context: MainContext::default(),
            sender: unbounded_channel().0,
        };
        let Ok(conn) = Connection::session().await else {
            return;
        };
        let chooser = FileChooser::new(
            &proxy,
            crate::core::session_manager::SessionManager::new(conn, 10),
        );
        let options = SaveFilesOptions {
            files: Some(vec![FilePath("relative/path".into())]),
            ..Default::default()
        };
        let res = chooser
            .try_save_files_impl("app_id".into(), "".into(), "".into(), options)
            .await;
        assert!(matches!(res, Err(SaveFilesError::MultipleComponents)));
    }

    #[tokio::test]
    async fn test_save_files_validation_special_path_dot() {
        let proxy = crate::gui::UiProxy {
            context: MainContext::default(),
            sender: unbounded_channel().0,
        };
        let Ok(conn) = Connection::session().await else {
            return;
        };
        let chooser = FileChooser::new(
            &proxy,
            crate::core::session_manager::SessionManager::new(conn, 10),
        );
        let options = SaveFilesOptions {
            files: Some(vec![FilePath(".".into())]),
            ..Default::default()
        };
        let res = chooser
            .try_save_files_impl("app_id".into(), "".into(), "".into(), options)
            .await;
        assert!(matches!(res, Err(SaveFilesError::SpecialPath)));
    }

    #[tokio::test]
    async fn test_save_files_validation_special_path_dot_dot() {
        let proxy = crate::gui::UiProxy {
            context: MainContext::default(),
            sender: unbounded_channel().0,
        };
        let Ok(conn) = Connection::session().await else {
            return;
        };
        let chooser = FileChooser::new(
            &proxy,
            crate::core::session_manager::SessionManager::new(conn, 10),
        );
        let options = SaveFilesOptions {
            files: Some(vec![FilePath("..".into())]),
            ..Default::default()
        };
        let res = chooser
            .try_save_files_impl("app_id".into(), "".into(), "".into(), options)
            .await;
        assert!(matches!(res, Err(SaveFilesError::SpecialPath)));
    }

    #[test]
    fn test_open_file_options_deserialize() {
        use {
            std::collections::HashMap,
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let mut dict = HashMap::new();
        dict.insert("modal", Variant::from(true));
        dict.insert("multiple", Variant::from(false));
        dict.insert("directory", Variant::from(true));

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &dict).unwrap();
        let options: OpenFileOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.modal, Some(true));
        assert_eq!(options.multiple, Some(false));
        assert_eq!(options.directory, Some(true));
    }

    #[test]
    fn test_save_file_options_deserialize() {
        use {
            std::collections::HashMap,
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let mut dict = HashMap::new();
        dict.insert("modal", Variant::from(true));
        dict.insert("current_name", Variant::from("new_file.txt"));

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &dict).unwrap();
        let options: SaveFileOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.modal, Some(true));
        assert_eq!(options.current_name.as_deref(), Some("new_file.txt"));
    }

    #[test]
    fn test_save_files_options_deserialize() {
        use {
            std::collections::HashMap,
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let mut dict = HashMap::new();
        dict.insert("modal", Variant::from(true));
        dict.insert("accept_label", Variant::from("Save Here"));

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &dict).unwrap();
        let options: SaveFilesOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.modal, Some(true));
        assert_eq!(options.accept_label.as_deref(), Some("Save Here"));
    }

    #[test]
    fn test_open_file_results_serialize() {
        use {
            std::collections::HashMap,
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let results = OpenFileResults {
            uris: Some(vec!["file:///tmp/test.txt".into()]),
            choices: Some(vec![("encoding".into(), "utf8".into())]),
            current_filter: None,
            writable: Some(true),
        };

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &results).unwrap();
        let decoded: HashMap<String, Variant> = encoded.deserialize().unwrap().0;

        let uris_val = decoded.get("uris").unwrap();
        let uris: Vec<String> = uris_val.try_clone().unwrap().try_into().unwrap();
        assert_eq!(uris, vec!["file:///tmp/test.txt".to_owned()]);
        assert_eq!(
            decoded.get("writable").unwrap().try_clone().unwrap(),
            Variant::from(true)
        );
    }

    #[test]
    fn test_save_file_results_serialize() {
        use {
            std::collections::HashMap,
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let results = SaveFileResults {
            uris: Some(vec!["file:///tmp/test.txt".into()]),
            choices: None,
            current_filter: None,
        };

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &results).unwrap();
        let decoded: HashMap<String, Variant> = encoded.deserialize().unwrap().0;

        let uris_val = decoded.get("uris").unwrap();
        let uris: Vec<String> = uris_val.try_clone().unwrap().try_into().unwrap();
        assert_eq!(uris, vec!["file:///tmp/test.txt".to_owned()]);
    }

    #[test]
    fn test_save_files_results_serialize() {
        use {
            std::collections::HashMap,
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let results = SaveFilesResults {
            uris: Some(vec![
                "file:///tmp/test1.txt".into(),
                "file:///tmp/test2.txt".into(),
            ]),
            choices: None,
        };

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &results).unwrap();
        let decoded: HashMap<String, Variant> = encoded.deserialize().unwrap().0;

        let uris_val = decoded.get("uris").unwrap();
        let uris: Vec<String> = uris_val.try_clone().unwrap().try_into().unwrap();
        assert_eq!(
            uris,
            vec![
                "file:///tmp/test1.txt".to_owned(),
                "file:///tmp/test2.txt".to_owned()
            ]
        );
    }
}

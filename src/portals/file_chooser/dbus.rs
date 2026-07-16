use {
    super::{
        gui as file_chooser,
        gui::{ChoiceVariant, FileChooserUi, Filter, FilterKind, FinalChoice},
    },
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    bstr::{ByteSlice, ByteVec},
    serde::{Deserialize, Deserializer},
    std::{ffi::CString, path::Path, str::FromStr},
    thiserror::Error,
    url::Url,
    zbus::{
        ObjectServer, interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
    },
};

pub struct FileChooser {
    proxy: UiProxy,
}

impl FileChooser {
    pub fn new(proxy: &UiProxy) -> Self {
        Self {
            proxy: proxy.clone(),
        }
    }
}

type Choice = (String, String, Vec<(String, String)>, String);

type FileFilter = (String, Vec<(u32, String)>);

#[derive(Type, Debug, Default, PartialEq)]
#[zvariant(signature = "ay")]
struct FilePath(String);

impl<'de> Deserialize<'de> for FilePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = <Vec<u8>>::deserialize(deserializer)?;
        let c_string = CString::from_vec_with_nul(bytes)
            .map_err(|_| serde::de::Error::custom("Bytes are not nul-terminated"))?;
        Ok(Self(c_string.into_bytes().into_string_lossy()))
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
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
#[zvariant(signature = "dict")]
struct SaveFileOptions {
    accept_label: Option<String>,
    modal: Option<bool>,
    filters: Option<Vec<FileFilter>>,
    current_filter: Option<FileFilter>,
    choices: Option<Vec<Choice>>,
    current_name: Option<String>,
    current_folder: Option<FilePath>,
    #[zvariant(rename = "current_file")]
    current_file: Option<FilePath>,
    pub activation_token: Option<String>,
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct SaveFilesOptions {
    accept_label: Option<String>,
    modal: Option<bool>,
    choices: Option<Vec<Choice>>,
    current_folder: Option<FilePath>,
    files: Option<Vec<FilePath>>,
    pub activation_token: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct OpenFileResults {
    uris: Option<Vec<String>>,
    choices: Option<Vec<(String, String)>>,
    current_filter: Option<FileFilter>,
    writable: Option<bool>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct SaveFileResults {
    uris: Option<Vec<String>>,
    choices: Option<Vec<(String, String)>>,
    current_filter: Option<FileFilter>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct SaveFilesResults {
    uris: Option<Vec<String>>,
    choices: Option<Vec<(String, String)>>,
}

#[derive(Debug, Error)]
enum SaveFilesError {
    #[error("User did not select exactly one path")]
    NotExactlyOnePath,
    #[error("Client tried to save an absolute path")]
    AbsolutePath,
    #[error("Client tried to save a path with multiple components")]
    MultipleComponents,
    #[error("Client tried to save `.` or `..`")]
    SpecialPath,
    #[error("The selected path is not a valid URI")]
    SelectedNotValidUrl(#[source] url::ParseError),
    #[error("The selected path is not a path")]
    SelectedNotValidPath,
    #[error("The computed unique path is not a valid URI")]
    UniqueNotValidUrl,
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
            activation_token: options.activation_token.clone(),
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
                log::error!("OpenFile failed: {}", anyhow::Error::new(e));
                Response::cancelled()
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
            activation_token: options.activation_token.clone(),
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
                log::error!("SaveFile failed: {}", anyhow::Error::new(e));
                Response::cancelled()
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
        let files = options.files.as_ref().map(|v| v.as_slice()).unwrap_or(&[]);

        // Security checks: The client provides paths to save, but we must ensure
        // they don't contain absolute paths or directory traversal attacks, because
        // we will combine these with a user-selected directory.
        for file in files {
            let file = Path::new(&file.0);
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
            activation_token: options.activation_token.clone(),
            app_id,
        }
        .run(&self.proxy)
        .await
        .map_err(SaveFilesError::Ui)?;
        if res.uris.len() != 1 {
            return Err(SaveFilesError::NotExactlyOnePath);
        }
        let uri = match res.uris.pop() {
            Some(u) => u,
            None => return Err(SaveFilesError::NotExactlyOnePath),
        };
        let base = Url::from_str(&uri)
            .map_err(SaveFilesError::SelectedNotValidUrl)?
            .to_file_path()
            .map_err(|_| SaveFilesError::SelectedNotValidPath)?;
        let mut uris = vec![];
        for file in files {
            let mut path = base.join(&file.0);
            if path.exists() {
                let (prefix, dot, suffix) = match file.0.split_once('.') {
                    Some((prefix, suffix)) => (prefix, ".", suffix),
                    _ => (file.0.as_str(), "", ""),
                };
                for i in 1u64.. {
                    path.set_file_name(format!("{prefix} ({i}){dot}{suffix}"));
                    if !path.exists() {
                        break;
                    }
                }
            }
            uris.push(
                Url::from_file_path(&path)
                    .map_err(|_| SaveFilesError::UniqueNotValidUrl)?
                    .to_string(),
            );
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
            Err(e) => {
                log::error!("SaveFiles failed: {}", anyhow::Error::new(e));
                Response::cancelled()
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
    async fn open_file(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: OpenFileOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<OpenFileResults> {
        run_request(
            server,
            handle,
            self.open_file_impl(app_id, parent_window, title, options),
        )
        .await
    }

    async fn save_file(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: SaveFileOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<SaveFileResults> {
        run_request(
            server,
            handle,
            self.save_file_impl(app_id, parent_window, title, options),
        )
        .await
    }

    async fn save_files(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: SaveFilesOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<SaveFilesResults> {
        run_request(
            server,
            handle,
            self.save_files_impl(app_id, parent_window, title, options),
        )
        .await
    }
}

fn map_filters(f: Vec<FileFilter>) -> Vec<Filter> {
    f.into_iter().map(map_filter).collect()
}

fn map_filter(f: FileFilter) -> Filter {
    Filter {
        name: f.0,
        elements: f
            .1
            .into_iter()
            .flat_map(|(kind, value)| match kind {
                0 => Some(FilterKind::Glob(value)),
                1 => Some(FilterKind::Mime(value)),
                _ => None,
            })
            .collect(),
    }
}

fn unmap_filter(f: Filter) -> FileFilter {
    (
        f.name,
        f.elements
            .into_iter()
            .map(|f| match f {
                FilterKind::Glob(v) => (0, v),
                FilterKind::Mime(v) => (1, v),
            })
            .collect(),
    )
}

fn map_cstr(f: FilePath) -> String {
    f.0.as_bytes().to_str_lossy().into_owned()
}

fn map_choices(c: Vec<Choice>) -> Vec<file_chooser::Choice> {
    c.into_iter().map(map_choice).collect()
}

fn map_choice(c: Choice) -> file_chooser::Choice {
    file_chooser::Choice {
        id: c.0,
        label: c.1,
        default: c.3,
        variants: c
            .2
            .into_iter()
            .map(|c| ChoiceVariant {
                id: c.0,
                label: c.1,
            })
            .collect(),
    }
}

fn map_final_choices(c: Vec<FinalChoice>) -> Vec<(String, String)> {
    c.into_iter().map(map_final_choice).collect()
}

fn map_final_choice(c: FinalChoice) -> (String, String) {
    (c.id, c.variant_id)
}

#[cfg(test)]
mod tests {
    use {super::*, zbus::zvariant::Type};

    #[test]
    fn test_map_filter_glob() {
        let f = map_filter(("Images".to_string(), vec![(0, "*.png".to_string())]));
        assert_eq!(f.name, "Images");
        assert_eq!(f.elements.len(), 1);
        assert!(matches!(f.elements[0], FilterKind::Glob(ref v) if v == "*.png"));
    }

    #[test]
    fn test_map_filter_mime() {
        let f = map_filter(("Audio".to_string(), vec![(1, "audio/*".to_string())]));
        assert_eq!(f.name, "Audio");
        assert_eq!(f.elements.len(), 1);
        assert!(matches!(f.elements[0], FilterKind::Mime(ref v) if v == "audio/*"));
    }

    #[test]
    fn test_map_filter_unknown_kind_skipped() {
        let f = map_filter((
            "Mixed".to_string(),
            vec![(0, "*.png".to_string()), (99, "unknown".to_string())],
        ));
        assert_eq!(f.elements.len(), 1);
        assert!(matches!(f.elements[0], FilterKind::Glob(ref v) if v == "*.png"));
    }

    #[test]
    fn test_unmap_filter_roundtrip() {
        let original: FileFilter = (
            "Images".to_string(),
            vec![(0, "*.png".to_string()), (1, "image/png".to_string())],
        );
        let mapped = map_filter(original.clone());
        let unmapped = unmap_filter(mapped);
        assert_eq!(original, unmapped);
    }

    #[test]
    fn test_map_choices() {
        let choice = (
            "encoding".to_string(),
            "Encoding".to_string(),
            vec![("utf8".to_string(), "UTF-8".to_string())],
            "utf8".to_string(),
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
        let path = FilePath("hello".to_string());
        assert_eq!(map_cstr(path), "hello");
    }

    #[test]
    fn test_map_final_choices() {
        let c = FinalChoice {
            id: "encoding".to_string(),
            variant_id: "utf8".to_string(),
        };
        let mapped = map_final_choices(vec![c]);
        assert_eq!(mapped, vec![("encoding".to_string(), "utf8".to_string())]);
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
            context: gtk4::glib::MainContext::default(),
        };
        let chooser = FileChooser::new(&proxy);
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
            context: gtk4::glib::MainContext::default(),
        };
        let chooser = FileChooser::new(&proxy);
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
            context: gtk4::glib::MainContext::default(),
        };
        let chooser = FileChooser::new(&proxy);
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
            context: gtk4::glib::MainContext::default(),
        };
        let chooser = FileChooser::new(&proxy);
        let options = SaveFilesOptions {
            files: Some(vec![FilePath("..".into())]),
            ..Default::default()
        };
        let res = chooser
            .try_save_files_impl("app_id".into(), "".into(), "".into(), options)
            .await;
        assert!(matches!(res, Err(SaveFilesError::SpecialPath)));
    }
}

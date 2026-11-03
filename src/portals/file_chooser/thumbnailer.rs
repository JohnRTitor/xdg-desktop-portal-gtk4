use {
    gtk4::{
        FileChooserDialog, Widget,
        gio::{self, prelude::FileExt},
        glib::{self, MainContext},
        prelude::{Cast, IsA, ListModelExt, ObjectExt, WidgetExt},
    },
    parking_lot::Mutex,
    std::{
        collections::{HashMap, HashSet},
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Arc, LazyLock,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, UNIX_EPOCH},
    },
};

static COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailSize {
    Normal = 128,
    Large = 256,
}

impl ThumbnailSize {
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Large => "large",
        }
    }

    pub const fn pixel_size(self) -> u32 {
        self as u32
    }
}

/// Convert a local filesystem path to a canonical file:// URI per FreeDesktop specification.
pub fn file_to_uri(path: &Path) -> Option<String> {
    let file = gio::File::for_path(path);
    let uri = file.uri().to_string();
    if !uri.starts_with("file://") {
        return None;
    }
    // Spec & GNOME/Cosmic convention: square brackets in file URIs are percent-encoded
    Some(uri.replace('[', "%5B").replace(']', "%5D"))
}

/// Compute MD5 hash as 32 lowercase hex characters using GLib's built-in Checksum.
pub fn md5_hex(data: &[u8]) -> String {
    let mut checksum = glib::Checksum::new(glib::ChecksumType::Md5)
        .expect("GLib MD5 checksum should always be supported");
    checksum.update(data);
    checksum.string().expect("Checksum string representation")
}

/// Get the FreeDesktop thumbnail cache directory for the specified size.
pub fn get_thumbnail_cache_dir(size: ThumbnailSize) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("thumbnails").join(size.dir_name());
    let _ = fs::create_dir_all(&dir);
    Some(dir)
}

/// Get the expected thumbnail file path for a URI and size.
pub fn get_thumbnail_path(uri: &str, size: ThumbnailSize) -> Option<PathBuf> {
    let dir = get_thumbnail_cache_dir(size)?;
    let hash = md5_hex(uri.as_bytes());
    Some(dir.join(format!("{hash}.png")))
}

/// Check if a cached thumbnail exists and is not older than the source file.
pub fn is_thumbnail_valid(thumb_path: &Path, file_path: &Path) -> bool {
    let Ok(thumb_meta) = fs::metadata(thumb_path) else {
        return false;
    };
    if !thumb_meta.is_file() || thumb_meta.len() == 0 {
        return false;
    }
    let Ok(file_meta) = fs::metadata(file_path) else {
        return false;
    };
    let Ok(thumb_mtime) = thumb_meta.modified() else {
        return false;
    };
    let Ok(file_mtime) = file_meta.modified() else {
        return false;
    };
    thumb_mtime >= file_mtime
}

/// IEEE 802.3 CRC32 calculation for PNG chunk checksums.
fn png_crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xedb8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Build a standard PNG tEXt chunk: length (4 bytes) + "tEXt" + keyword + null + text + crc (4 bytes).
fn make_text_chunk(keyword: &str, text: &str) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(4 + 4 + keyword.len() + 1 + text.len() + 4);
    let data_len = (keyword.len() + 1 + text.len()) as u32;
    chunk.extend_from_slice(&data_len.to_be_bytes());

    let crc_start = chunk.len();
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(keyword.as_bytes());
    chunk.push(0);
    chunk.extend_from_slice(text.as_bytes());
    let crc = png_crc32(&chunk[crc_start..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    chunk
}

/// Inject FreeDesktop thumbnail metadata (tEXt chunks) into a PNG byte stream right after the IHDR chunk.
pub fn inject_png_metadata(
    png_data: &[u8],
    uri: &str,
    mtime_secs: u64,
    size_bytes: u64,
) -> Option<Vec<u8>> {
    const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if png_data.len() < 8 || &png_data[..8] != PNG_SIG {
        return None;
    }
    // IHDR chunk: 4 bytes len (13) + 4 bytes 'IHDR' + 13 bytes data + 4 bytes CRC = 25 bytes.
    if png_data.len() < 8 + 25 || &png_data[12..16] != b"IHDR" {
        return None;
    }
    let ihdr_end = 8 + 25;

    let mut result = Vec::with_capacity(png_data.len() + 256);
    result.extend_from_slice(&png_data[..ihdr_end]);
    result.extend(make_text_chunk("Thumb::URI", uri));
    result.extend(make_text_chunk("Thumb::MTime", &mtime_secs.to_string()));
    result.extend(make_text_chunk("Thumb::Size", &size_bytes.to_string()));
    result.extend(make_text_chunk("Software", "xdg-desktop-portal-gtk4"));
    result.extend_from_slice(&png_data[ihdr_end..]);
    Some(result)
}

/// Atomically write a thumbnail image to the target path with 0600 permissions and FreeDesktop metadata.
pub fn save_thumbnail_file(
    raw_png: &[u8],
    uri: &str,
    file_path: &Path,
    target_path: &Path,
) -> std::io::Result<()> {
    let meta = fs::metadata(file_path)?;
    let mtime = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let size = meta.len();

    let final_png =
        inject_png_metadata(raw_png, uri, mtime, size).unwrap_or_else(|| raw_png.to_vec());

    let parent = target_path
        .parent()
        .ok_or_else(|| std::io::Error::other("no target parent directory"))?;
    let _ = fs::create_dir_all(parent);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent.join(format!(".thumb-{}-{}.tmp", std::process::id(), id));

    fs::write(&tmp_path, final_png)?;
    let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));

    if let Err(e) = fs::rename(&tmp_path, target_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

/// Parsed FreeDesktop .thumbnailer entry.
#[derive(Debug, Clone)]
pub struct ThumbnailerSpec {
    pub name: String,
    pub exec: String,
    pub try_exec: Option<String>,
    pub mime_types: Vec<String>,
}

impl ThumbnailerSpec {
    pub fn is_available(&self) -> bool {
        if let Some(try_exec) = &self.try_exec {
            return is_executable_or_in_path(try_exec);
        }
        if let Some(cmd) = self.exec.split_whitespace().next() {
            return is_executable_or_in_path(cmd);
        }
        false
    }
}

fn is_executable_or_in_path(cmd: &str) -> bool {
    let p = Path::new(cmd);
    if p.is_absolute() || cmd.contains('/') {
        return p.is_file();
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(cmd).is_file() {
                return true;
            }
        }
    }
    false
}

/// Registry of discovered FreeDesktop thumbnailers.
#[derive(Debug, Default)]
pub struct ThumbnailerRegistry {
    by_mime: HashMap<String, Vec<ThumbnailerSpec>>,
}

impl ThumbnailerRegistry {
    pub fn load() -> Self {
        let mut reg = Self::default();
        let mut search_dirs = Vec::new();

        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
            search_dirs.push(data_home.join("thumbnailers"));
        } else if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            search_dirs.push(home.join(".local/share/thumbnailers"));
        }

        if let Some(data_dirs) = std::env::var_os("XDG_DATA_DIRS") {
            for dir in std::env::split_paths(&data_dirs) {
                search_dirs.push(dir.join("thumbnailers"));
            }
        }
        search_dirs.push(PathBuf::from("/run/current-system/sw/share/thumbnailers"));
        search_dirs.push(PathBuf::from("/usr/share/thumbnailers"));

        for dir in search_dirs {
            if !dir.is_dir() {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("thumbnailer") {
                    reg.load_thumbnailer_file(&path);
                }
            }
        }
        reg
    }

    fn load_thumbnailer_file(&mut self, path: &Path) {
        let Ok(content) = fs::read_to_string(path) else {
            return;
        };
        let mut in_section = false;
        let mut exec = None;
        let mut try_exec = None;
        let mut mime_types = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_section = trimmed.eq_ignore_ascii_case("[Thumbnailer Entry]");
                continue;
            }
            if !in_section {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=') {
                let key = k.trim();
                let val = v.trim();
                if key.eq_ignore_ascii_case("Exec") {
                    exec = Some(val.to_string());
                } else if key.eq_ignore_ascii_case("TryExec") {
                    try_exec = Some(val.to_string());
                } else if key.eq_ignore_ascii_case("MimeType") {
                    mime_types = val
                        .split(';')
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect();
                }
            }
        }

        if let Some(exec) = exec
            && !mime_types.is_empty()
        {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            let spec = ThumbnailerSpec {
                name,
                exec,
                try_exec,
                mime_types: mime_types.clone(),
            };
            if spec.is_available() {
                for mime in mime_types {
                    self.by_mime.entry(mime).or_default().push(spec.clone());
                }
            }
        }
    }

    pub fn find_thumbnailer(&self, mime: &str) -> Option<&ThumbnailerSpec> {
        if let Some(specs) = self.by_mime.get(mime) {
            return specs.first();
        }
        // Wildcard check, e.g. "image/*"
        if let Some((category, _)) = mime.split_once('/') {
            let wildcard = format!("{category}/*");
            if let Some(specs) = self.by_mime.get(&wildcard) {
                return specs.first();
            }
        }
        None
    }
}

pub static REGISTRY: LazyLock<ThumbnailerRegistry> = LazyLock::new(ThumbnailerRegistry::load);

/// Parse command line arguments considering single and double quotes.
fn split_command(cmd: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in cmd.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && !in_single {
            escaped = true;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if ch.is_whitespace() && !in_single && !in_double {
            if !cur.is_empty() {
                args.push(std::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    args
}

/// Execute a thumbnailer command line with substitution of %i, %u, %o, %s.
fn execute_thumbnailer_command(
    exec: &str,
    input_path: &Path,
    input_uri: &str,
    output_path: &Path,
    size: u32,
) -> Result<(), String> {
    let tokens = split_command(exec);
    if tokens.is_empty() {
        return Err("Empty command".to_string());
    }

    let program = &tokens[0];
    let mut cmd = Command::new(program);

    for token in &tokens[1..] {
        let substituted = token
            .replace("%i", input_path.to_str().unwrap_or_default())
            .replace("%u", input_uri)
            .replace("%o", output_path.to_str().unwrap_or_default())
            .replace("%s", &size.to_string());
        cmd.arg(substituted);
    }

    // Suppress child stdio
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let status = cmd
        .status()
        .map_err(|e| format!("Failed to spawn {program}: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with status {status}"))
    }
}

/// Generate a thumbnail directly following FreeDesktop .thumbnailer spec.
/// Falls back to Tumbler D-Bus if no direct .thumbnailer is registered for the MIME type.
pub fn generate_thumbnail(
    file_path: &Path,
    mime_type: Option<&str>,
    size: ThumbnailSize,
) -> Result<PathBuf, String> {
    let uri = file_to_uri(file_path).ok_or_else(|| "Failed to construct file URI".to_string())?;
    let target_path = get_thumbnail_path(&uri, size)
        .ok_or_else(|| "Failed to determine thumbnail cache path".to_string())?;

    if is_thumbnail_valid(&target_path, file_path) {
        return Ok(target_path);
    }

    let mime = mime_type.unwrap_or("application/octet-stream");
    let temp_dir = std::env::temp_dir();
    let temp_output = temp_dir.join(format!(
        "portal-thumb-{}-{}.png",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    // 1. Check if a registered FreeDesktop .thumbnailer matches
    if let Some(spec) = REGISTRY.find_thumbnailer(mime) {
        if execute_thumbnailer_command(&spec.exec, file_path, &uri, &temp_output, size.pixel_size())
            .is_ok()
            && let Ok(raw_png) = fs::read(&temp_output)
        {
            let _ = fs::remove_file(&temp_output);
            if save_thumbnail_file(&raw_png, &uri, file_path, &target_path).is_ok() {
                return Ok(target_path);
            }
        }
        let _ = fs::remove_file(&temp_output);
    }

    // 2. Built-in fallbacks if no .thumbnailer file handled it
    // A) PDF fallback tools: evince-thumbnailer or pdftoppm
    if mime == "application/pdf" {
        if is_executable_or_in_path("evince-thumbnailer") {
            let cmd = "evince-thumbnailer -s %s %i %o";
            if execute_thumbnailer_command(cmd, file_path, &uri, &temp_output, size.pixel_size())
                .is_ok()
                && let Ok(raw_png) = fs::read(&temp_output)
            {
                let _ = fs::remove_file(&temp_output);
                if save_thumbnail_file(&raw_png, &uri, file_path, &target_path).is_ok() {
                    return Ok(target_path);
                }
            }
            let _ = fs::remove_file(&temp_output);
        }

        if is_executable_or_in_path("pdftoppm") {
            let prefix = temp_dir.join(format!(
                "portal-pdftoppm-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let mut cmd = Command::new("pdftoppm");
            cmd.args([
                "-png",
                "-singlefile",
                "-scale-to",
                &size.pixel_size().to_string(),
                file_path.to_str().unwrap_or_default(),
                prefix.to_str().unwrap_or_default(),
            ]);
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            if let Ok(status) = cmd.status()
                && status.success()
            {
                let produced_png = PathBuf::from(format!("{}.png", prefix.display()));
                if let Ok(raw_png) = fs::read(&produced_png) {
                    let _ = fs::remove_file(&produced_png);
                    if save_thumbnail_file(&raw_png, &uri, file_path, &target_path).is_ok() {
                        return Ok(target_path);
                    }
                }
            }
        }
    }

    // B) Image fallback: gdk-pixbuf-thumbnailer if not in .thumbnailer registry
    if mime.starts_with("image/") && is_executable_or_in_path("gdk-pixbuf-thumbnailer") {
        let cmd = "gdk-pixbuf-thumbnailer -s %s %u %o";
        if execute_thumbnailer_command(cmd, file_path, &uri, &temp_output, size.pixel_size())
            .is_ok()
            && let Ok(raw_png) = fs::read(&temp_output)
        {
            let _ = fs::remove_file(&temp_output);
            if save_thumbnail_file(&raw_png, &uri, file_path, &target_path).is_ok() {
                return Ok(target_path);
            }
        }
        let _ = fs::remove_file(&temp_output);
    }

    // 3. Last-resort fallback: Tumbler D-Bus if available on session bus
    if trigger_tumbler_dbus(&uri, mime) {
        // Wait briefly for Tumbler to write to cache
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(50));
            if is_thumbnail_valid(&target_path, file_path) {
                return Ok(target_path);
            }
        }
    }

    Err(format!("No thumbnailer available for {mime}"))
}

/// Fallback call to Tumbler via D-Bus org.freedesktop.thumbnails.Thumbnailer1
fn trigger_tumbler_dbus(uri: &str, mime: &str) -> bool {
    let gdbus = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.thumbnails.Thumbnailer1",
            "--object-path",
            "/org/freedesktop/thumbnails/Thumbnailer1",
            "--method",
            "org.freedesktop.thumbnails.Thumbnailer1.Queue",
            &format!("['{uri}']"),
            &format!("['{mime}']"),
            "normal",
            "default",
            "0",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if let Ok(status) = gdbus
        && status.success()
    {
        return true;
    }

    // Try busctl fallback
    let busctl = Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.freedesktop.thumbnails.Thumbnailer1",
            "/org/freedesktop/thumbnails/Thumbnailer1",
            "org.freedesktop.thumbnails.Thumbnailer1",
            "Queue",
            "asasssu",
            "1",
            uri,
            "1",
            mime,
            "normal",
            "default",
            "0",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if let Ok(status) = busctl
        && status.success()
    {
        return true;
    }

    false
}

/// Task for thumbnail worker thread pool.
struct ThumbnailTask {
    file_path: PathBuf,
    mime_type: Option<String>,
}

/// Coordinator for managing asynchronous thumbnail generation requests and avoiding duplicate work.
pub struct ThumbnailCoordinator {
    in_flight: Arc<Mutex<HashSet<PathBuf>>>,
    sender: std::sync::mpsc::Sender<ThumbnailTask>,
}

impl Default for ThumbnailCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbnailCoordinator {
    pub fn new() -> Self {
        let (task_tx, task_rx) = std::sync::mpsc::channel::<ThumbnailTask>();
        let task_rx = Arc::new(Mutex::new(task_rx));
        let in_flight = Arc::new(Mutex::new(HashSet::new()));

        // Spawn a bounded pool of 4 thumbnail generator worker threads
        for _ in 0..4 {
            let rx = task_rx.clone();
            let inflight = in_flight.clone();
            std::thread::spawn(move || {
                while let Ok(task) = {
                    let lock = rx.lock();
                    lock.recv()
                } {
                    let _ = generate_thumbnail(
                        &task.file_path,
                        task.mime_type.as_deref(),
                        ThumbnailSize::Normal,
                    );
                    {
                        let mut set = inflight.lock();
                        set.remove(&task.file_path);
                    }
                }
            });
        }

        Self {
            in_flight,
            sender: task_tx,
        }
    }

    /// Queue a file for thumbnailing if it is not already being processed.
    /// When complete, invokes `on_complete` on the GTK main thread.
    pub fn queue_thumbnail<F>(&self, file_path: PathBuf, mime_type: Option<String>, on_complete: F)
    where
        F: FnOnce(PathBuf) + Send + 'static,
    {
        {
            let mut set = self.in_flight.lock();
            if !set.insert(file_path.clone()) {
                return; // Already in flight
            }
        }

        let in_flight = self.in_flight.clone();
        std::thread::spawn(move || {
            let res = generate_thumbnail(&file_path, mime_type.as_deref(), ThumbnailSize::Normal);
            {
                let mut set = in_flight.lock();
                set.remove(&file_path);
            }
            if let Ok(thumb_path) = res {
                glib::idle_add_once(move || {
                    on_complete(thumb_path);
                });
            }
        });
    }

    /// Queue a file to the worker pool without individual callback (for batch/folder preload).
    pub fn queue_background(&self, file_path: PathBuf, mime_type: Option<String>) {
        let mut set = self.in_flight.lock();
        if set.insert(file_path.clone()) {
            let _ = self.sender.send(ThumbnailTask {
                file_path,
                mime_type,
            });
        }
    }
}

pub static COORDINATOR: LazyLock<ThumbnailCoordinator> = LazyLock::new(ThumbnailCoordinator::new);

/// Recursively refresh GtkFileThumbnail widgets matching `target_name` to display the new thumbnail.
pub fn refresh_file_thumbnail<W: IsA<Widget>>(root: &W, target_name: &str) {
    if root.type_().name() == "GtkFileThumbnail"
        && let Some(info) = root.property::<Option<gio::FileInfo>>("file-info")
        && info.display_name() == target_name
    {
        info.remove_attribute("filechooser::queried");
        info.remove_attribute("thumbnail::path");
        info.remove_attribute("standard::icon");
        root.set_property("file-info", None::<&gio::FileInfo>);
        root.set_property("file-info", &info);
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        refresh_file_thumbnail(&c, target_name);
        child = c.next_sibling();
    }
}

/// Attach thumbnail generator to a FileChooserDialog.
pub fn setup_dialog_thumbnailer(dialog: &FileChooserDialog, context: &MainContext) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(128);

    let dialog_weak = dialog.downgrade();
    context.spawn_local(async move {
        while let Some(file_name) = rx.recv().await {
            if let Some(dlg) = dialog_weak.upgrade() {
                refresh_file_thumbnail(&dlg, &file_name);
            } else {
                break;
            }
        }
    });

    fn process_item(info: &gio::FileInfo, tx: &tokio::sync::mpsc::Sender<String>) {
        if info.attribute_byte_string("thumbnail::path").is_some() {
            return;
        }
        let Some(file_obj) = info.attribute_object("standard::file") else {
            return;
        };
        let Ok(file) = file_obj.downcast::<gio::File>() else {
            return;
        };
        let Some(path) = file.path() else {
            return;
        };
        if !path.is_file() {
            return;
        }
        let name = info.display_name().to_string();
        let mime = info.content_type().map(|c| c.to_string());

        let tx_clone = tx.clone();
        let name_clone = name.clone();
        COORDINATOR.queue_thumbnail(path, mime, move |_thumb_path| {
            let _ = tx_clone.try_send(name_clone);
        });
    }

    fn attach_to_views(w: &Widget, tx: &tokio::sync::mpsc::Sender<String>) {
        let name = w.type_().name();
        if name == "GtkGridView" || name == "GtkColumnView" {
            let tx_outer = tx.clone();
            let connect_model =
                move |view: &Widget, tx_inner: &tokio::sync::mpsc::Sender<String>| {
                    if let Some(model) = view.property::<Option<gio::ListModel>>("model") {
                        for i in 0..model.n_items() {
                            if let Some(item) = model.item(i)
                                && let Ok(info) = item.downcast::<gio::FileInfo>()
                            {
                                process_item(&info, tx_inner);
                            }
                        }
                        let tx_for_signal = tx_inner.clone();
                        model.connect_items_changed(move |m, pos, _removed, added| {
                            for i in pos..(pos + added) {
                                if let Some(item) = m.item(i)
                                    && let Ok(info) = item.downcast::<gio::FileInfo>()
                                {
                                    process_item(&info, &tx_for_signal);
                                }
                            }
                        });
                    }
                };
            connect_model(w, &tx_outer);
            let tx_for_notify = tx.clone();
            w.connect_notify(Some("model"), move |view, _| {
                connect_model(view, &tx_for_notify);
            });
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            attach_to_views(&c, tx);
            child = c.next_sibling();
        }
    }

    attach_to_views(dialog.upcast_ref(), &tx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_command() {
        let cmd = "gdk-pixbuf-thumbnailer -s %s \"%u\" '%o'";
        let parts = split_command(cmd);
        assert_eq!(
            parts,
            vec!["gdk-pixbuf-thumbnailer", "-s", "%s", "%u", "%o"]
        );

        let cmd_escaped = r#"my-tool --desc "a \"b\" c" %i"#;
        let parts2 = split_command(cmd_escaped);
        assert_eq!(parts2, vec!["my-tool", "--desc", "a \"b\" c", "%i"]);
    }

    #[test]
    fn test_md5_hex() {
        let uri = "file:///home/user/test.png";
        let hash = md5_hex(uri.as_bytes());
        assert_eq!(hash.len(), 32);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_png_metadata_injection() {
        // Minimal valid 1x1 PNG
        let minimal_png: [u8; 67] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
            0x00, 0x00, 0x00, 0x0d, // IHDR length (13)
            0x49, 0x48, 0x44, 0x52, // "IHDR"
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, // 1x1 RGBA 8-bit
            0x1f, 0x15, 0xc4, 0x89, // CRC
            0x00, 0x00, 0x00, 0x0a, // IDAT length (10)
            0x49, 0x44, 0x41, 0x54, // "IDAT"
            0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, // compressed data
            0x0d, 0x0a, 0x2d, 0xb4, // CRC
            0x00, 0x00, 0x00, 0x00, // IEND length (0)
            0x49, 0x45, 0x4e, 0x44, // "IEND"
            0xae, 0x42, 0x60, 0x82, // CRC
        ];

        let injected = inject_png_metadata(&minimal_png, "file:///test/path.jpg", 123456789, 4096)
            .expect("metadata injection should succeed");

        assert!(injected.len() > minimal_png.len());
        assert_eq!(&injected[..8], &minimal_png[..8]);
        assert_eq!(&injected[12..16], b"IHDR");

        let s = String::from_utf8_lossy(&injected);
        assert!(s.contains("Thumb::URI"));
        assert!(s.contains("file:///test/path.jpg"));
        assert!(s.contains("Thumb::MTime"));
        assert!(s.contains("123456789"));
        assert!(s.contains("Thumb::Size"));
        assert!(s.contains("Software"));
        assert!(s.contains("xdg-desktop-portal-gtk4"));
    }

    #[test]
    fn test_registry_discovery() {
        let reg = ThumbnailerRegistry::load();
        let thumb = reg.find_thumbnailer("image/jpeg");
        assert!(
            thumb.is_some(),
            "At least one image thumbnailer should be registered"
        );
    }

    #[test]
    fn test_generate_thumbnail_image() {
        let test_dir = std::env::temp_dir().join("portal_test_thumb_dir");
        let _ = fs::create_dir_all(&test_dir);
        let img_path = test_dir.join("sample.png");

        let minimal_png: [u8; 67] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        fs::write(&img_path, minimal_png).unwrap();

        let res = generate_thumbnail(&img_path, Some("image/png"), ThumbnailSize::Normal);
        assert!(res.is_ok(), "generate_thumbnail failed: {:?}", res);
        let thumb_path = res.unwrap();
        assert!(
            thumb_path.is_file(),
            "thumbnail file should exist at {:?}",
            thumb_path
        );

        let _ = fs::remove_file(&thumb_path);
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_generate_thumbnail_pdf() {
        let pdf_path =
            Path::new("/home/masum/Documents/Masum-Reza-AICTE-Recommendation-Letter.pdf");
        if pdf_path.is_file() {
            let res = generate_thumbnail(pdf_path, Some("application/pdf"), ThumbnailSize::Normal);
            assert!(res.is_ok(), "PDF thumbnail generation failed: {:?}", res);
            let thumb = res.unwrap();
            assert!(thumb.is_file());
        }
    }
}

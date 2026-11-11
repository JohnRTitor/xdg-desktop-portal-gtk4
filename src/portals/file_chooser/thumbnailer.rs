use {
    gtk4::{
        FileChooserDialog, Widget,
        ffi::GtkWidget,
        gio::{
            self,
            ffi::GListModel,
            prelude::{DBusProxyExt, FileExt},
        },
        glib::{self, MainContext, prelude::ToVariant, translate::ToGlibPtr},
        prelude::{Cast, ListModelExt, ObjectExt, WidgetExt},
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
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, UNIX_EPOCH},
    },
};

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

/// All thumbnail sizes to regenerate when a file's thumbnail is missing or invalid.
pub const ALL_SIZES: [ThumbnailSize; 2] = [ThumbnailSize::Normal, ThumbnailSize::Large];

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
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .and_then(|p| fs::create_dir_all(&p).ok().map(|_| p))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("thumbnails").join(size.dir_name());
    let _ = fs::create_dir_all(&dir);
    // FreeDesktop thumbnail spec mandates 0700 on the cache directories.
    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    Some(dir)
}

/// Get the expected thumbnail file path for a URI and size.
pub fn get_thumbnail_path(uri: &str, size: ThumbnailSize) -> Option<PathBuf> {
    let dir = get_thumbnail_cache_dir(size)?;
    let hash = md5_hex(uri.as_bytes());
    Some(dir.join(format!("{hash}.png")))
}

/// Validate a cached thumbnail: it must exist, be non-empty, not be older than the source file,
/// and embed `Thumb::URI`/`Thumb::MTime` tEXt chunks that match the source file.
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
    // Thumbs older than the source are never valid.
    if thumb_mtime < file_mtime {
        return false;
    }
    // Verify FreeDesktop tEXt metadata embedded in the PNG.
    if let Some(uri) = file_to_uri(file_path) {
        let file_mtime_secs = file_mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        return is_png_metadata_valid(thumb_path, &uri, file_mtime_secs);
    }
    true
}

/// Verify that a thumbnail PNG embeds matching `Thumb::URI` and `Thumb::MTime` tEXt chunks.
fn is_png_metadata_valid(png_path: &Path, expected_uri: &str, expected_mtime_secs: u64) -> bool {
    let Ok(data) = fs::read(png_path) else {
        return false;
    };
    is_png_data_metadata_valid(&data, expected_uri, expected_mtime_secs)
}

/// Validate the tEXt chunks inside a raw PNG byte stream.
fn is_png_data_metadata_valid(
    png_data: &[u8],
    expected_uri: &str,
    expected_mtime_secs: u64,
) -> bool {
    const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if png_data.len() < 8 || &png_data[..8] != PNG_SIG {
        return false;
    }
    let mut pos = 8;
    let mut found_uri = false;
    let mut found_mtime = false;
    loop {
        if pos + 8 > png_data.len() {
            break;
        }
        let len = u32::from_be_bytes([
            png_data[pos],
            png_data[pos + 1],
            png_data[pos + 2],
            png_data[pos + 3],
        ]) as usize;
        let chunk_type = &png_data[pos + 4..pos + 8];
        let chunk_end = pos + 8 + len + 4;
        if chunk_end > png_data.len() {
            break;
        }
        if chunk_type == b"tEXt" && len >= 2 {
            let payload = &png_data[pos + 8..pos + 8 + len];
            if let Some(null_pos) = payload.iter().position(|&b| b == 0) {
                let keyword = &payload[..null_pos];
                let text = &payload[null_pos + 1..];
                if keyword == b"Thumb::URI" {
                    found_uri = text == expected_uri.as_bytes();
                } else if keyword == b"Thumb::MTime"
                    && let Ok(s) = std::str::from_utf8(text)
                {
                    let secs = s.split('.').next().and_then(|s| s.parse::<u64>().ok());
                    found_mtime = secs
                        .map(|s| s.abs_diff(expected_mtime_secs) <= 1)
                        .unwrap_or(false);
                }
            }
        } else if chunk_type == b"IEND" {
            break;
        }
        pos = chunk_end;
    }
    found_uri && found_mtime
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
    let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));

    let mut tmp = tempfile::Builder::new()
        .prefix(".thumb-")
        .suffix(".tmp")
        .permissions(fs::Permissions::from_mode(0o600))
        .tempfile_in(parent)?;
    use std::io::Write;
    tmp.write_all(&final_png)?;
    tmp.persist(target_path).map_err(|e| e.error)?;
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
    cancel: &ThumbnailCancel,
) -> Result<(), String> {
    let tokens = split_command(exec);
    if tokens.is_empty() {
        return Err("Empty command".to_string());
    }

    let program = &tokens[0];
    let prog_name = Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program);
    if matches!(prog_name, "sh" | "bash" | "dash" | "zsh" | "ksh") {
        return Err("Refusing to run a thumbnailer through a shell".to_string());
    }

    let mut cmd = Command::new(program);

    for token in &tokens[1..] {
        let substituted = token
            .replace("%i", input_path.to_str().unwrap_or_default())
            // Pass local path for %u to be compatible with tools that cannot open file:// URIs directly
            .replace("%u", input_path.to_str().unwrap_or(input_uri))
            .replace("%o", output_path.to_str().unwrap_or_default())
            .replace("%s", &size.to_string());
        cmd.arg(substituted);
    }

    // Suppress child stdio
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn {program}: {e}"))?;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CANCELLED.to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("{program} exited with status {status}"))
                };
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} timed out after 10s"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("Failed to wait for {program}: {e}")),
        }
    }
}

/// Return a unique, private directory to hold intermediate thumbnailer output.
fn private_runtime_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        })
        .and_then(|p| fs::create_dir_all(&p).ok().map(|_| p))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("xdg-desktop-portal-gtk4");
    fs::create_dir_all(&dir).ok()?;
    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    Some(dir)
}

/// Create a unique temporary PNG path in the user-private runtime directory and return it.
fn private_temp_path(prefix: &str) -> Option<PathBuf> {
    let dir = private_runtime_dir()?;
    let tmp = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(".png")
        .permissions(fs::Permissions::from_mode(0o600))
        .tempfile_in(&dir)
        .ok()?;
    let path = tmp.path().to_path_buf();
    let _ = tmp.keep();
    Some(path)
}

/// Cooperative cancellation shared by all thumbnail jobs belonging to one dialog.
///
/// A job is cheap to skip when it has not started yet, so the worker pool checks this
/// before touching the filesystem. Running jobs also poll it between sizes and fallback
/// stages; a single image decode cannot be interrupted mid-call.
#[derive(Clone, Debug, Default)]
pub struct ThumbnailCancel {
    cancelled: Arc<AtomicBool>,
}

impl ThumbnailCancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Identity comparison for the owning dialog/token, used to avoid
    /// clobbering a newer owner when an older cancelled job finishes.
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cancelled, &other.cancelled)
    }
}

impl PartialEq for ThumbnailCancel {
    fn eq(&self, other: &Self) -> bool {
        self.same(other)
    }
}

impl Eq for ThumbnailCancel {}

/// Ask glibc to return free memory from all arenas to the operating system.
///
/// `malloc_trim` is glibc-specific; other allocators either release memory eagerly or
/// expose a different API. This intentionally does nothing elsewhere.
fn trim_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Coalesce repeated trim requests so close + batch-drain do not both walk every
/// glibc arena in quick succession.
static TRIM_SCHEDULED: AtomicBool = AtomicBool::new(false);

fn schedule_trim() {
    if TRIM_SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    glib::idle_add_once(|| {
        TRIM_SCHEDULED.store(false, Ordering::Release);
        trim_allocator();
    });
}

const CANCELLED: &str = "Thumbnail generation cancelled";

fn check_cancelled(cancel: &ThumbnailCancel) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err(CANCELLED.to_string())
    } else {
        Ok(())
    }
}

/// Generate thumbnails directly following the FreeDesktop .thumbnailer spec.
///
/// Both the `normal` (128) and `large` (256) cache entries are produced so that GIO's
/// `THUMBNAIL_SIZE_AUTO` never selects a stale external thumbnail over a fresh one.
pub fn generate_thumbnail(file_path: &Path, mime_type: Option<&str>) -> Result<PathBuf, String> {
    generate_thumbnail_cancellable(file_path, mime_type, &ThumbnailCancel::new())
}

fn generate_thumbnail_cancellable(
    file_path: &Path,
    mime_type: Option<&str>,
    cancel: &ThumbnailCancel,
) -> Result<PathBuf, String> {
    check_cancelled(cancel)?;

    let uri = file_to_uri(file_path).ok_or_else(|| "Failed to construct file URI".to_string())?;
    let mime = mime_type.unwrap_or("application/octet-stream");

    // Return early only if every size tier is already valid.
    let target_normal = get_thumbnail_path(&uri, ThumbnailSize::Normal);
    let target_large = get_thumbnail_path(&uri, ThumbnailSize::Large);
    let normal_ok = target_normal
        .as_deref()
        .map(|p| is_thumbnail_valid(p, file_path))
        .unwrap_or(false);
    let large_ok = target_large
        .as_deref()
        .map(|p| is_thumbnail_valid(p, file_path))
        .unwrap_or(false);
    if normal_ok && large_ok {
        check_cancelled(cancel)?;
        return target_normal.ok_or_else(|| "no thumbnail path".to_string());
    }

    let needs_gen: Vec<ThumbnailSize> = ALL_SIZES
        .into_iter()
        .filter(|s| {
            let tp = get_thumbnail_path(&uri, *s);
            tp.map(|p| !is_thumbnail_valid(&p, file_path))
                .unwrap_or(true)
        })
        .collect();

    check_cancelled(cancel)?;

    // 1. Use a registered FreeDesktop .thumbnailer when one matches the MIME type.
    if let Some(spec) = REGISTRY.find_thumbnailer(mime) {
        for size in &needs_gen {
            check_cancelled(cancel)?;
            let Some(target_path) = get_thumbnail_path(&uri, *size) else {
                continue;
            };
            if let Some(temp_output) = private_temp_path("portal-thumb-") {
                if execute_thumbnailer_command(
                    &spec.exec,
                    file_path,
                    &uri,
                    &temp_output,
                    size.pixel_size(),
                    cancel,
                )
                .is_ok()
                    && let Ok(raw_png) = fs::read(&temp_output)
                {
                    let _ = fs::remove_file(&temp_output);
                    if cancel.is_cancelled() {
                        return Err(CANCELLED.to_string());
                    }
                    let _ = save_thumbnail_file(&raw_png, &uri, file_path, &target_path);
                } else {
                    let _ = fs::remove_file(&temp_output);
                }
            }
        }
        if let Some(tp) = get_thumbnail_path(&uri, ThumbnailSize::Normal)
            && is_thumbnail_valid(&tp, file_path)
        {
            return Ok(tp);
        }
        if let Some(tp) = get_thumbnail_path(&uri, ThumbnailSize::Large)
            && is_thumbnail_valid(&tp, file_path)
        {
            return Ok(tp);
        }
    }

    // 2. Built-in fallbacks
    // A) PDF fallback tools: evince-thumbnailer or pdftoppm
    if mime == "application/pdf" {
        for size in &needs_gen {
            check_cancelled(cancel)?;
            let Some(target_path) = get_thumbnail_path(&uri, *size) else {
                continue;
            };
            let mut produced = false;

            if is_executable_or_in_path("evince-thumbnailer")
                && let Some(temp_output) = private_temp_path("portal-thumb-")
            {
                let cmd = "evince-thumbnailer -s %s %i %o";
                if execute_thumbnailer_command(
                    cmd,
                    file_path,
                    &uri,
                    &temp_output,
                    size.pixel_size(),
                    cancel,
                )
                .is_ok()
                    && let Ok(raw_png) = fs::read(&temp_output)
                {
                    if cancel.is_cancelled() {
                        return Err(CANCELLED.to_string());
                    }
                    if save_thumbnail_file(&raw_png, &uri, file_path, &target_path).is_ok() {
                        produced = true;
                    }
                }
                let _ = fs::remove_file(&temp_output);
            }

            if !produced
                && is_executable_or_in_path("pdftoppm")
                && let Some(prefix) = private_temp_path("portal-pdftoppm-")
            {
                let prefix_str = prefix.to_str().unwrap_or_default().to_string();
                let mut cmd = Command::new("pdftoppm");
                cmd.args([
                    "-png",
                    "-singlefile",
                    "-scale-to",
                    &size.pixel_size().to_string(),
                    file_path.to_str().unwrap_or_default(),
                    &prefix_str,
                ]);
                cmd.stdout(std::process::Stdio::null());
                cmd.stderr(std::process::Stdio::null());

                let produced_png = PathBuf::from(format!("{prefix_str}.png"));
                let Ok(mut child) = cmd.spawn() else {
                    let _ = fs::remove_file(&prefix);
                    let _ = fs::remove_file(&produced_png);
                    continue;
                };
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let status = loop {
                    if cancel.is_cancelled() {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = fs::remove_file(&produced_png);
                        let _ = fs::remove_file(&prefix);
                        return Err(CANCELLED.to_string());
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => break status,
                        Ok(None) if std::time::Instant::now() >= deadline => {
                            let _ = child.kill();
                            let _ = child.wait();
                            let _ = fs::remove_file(&produced_png);
                            let _ = fs::remove_file(&prefix);
                            return Err("pdftoppm timed out after 10s".to_string());
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                        Err(e) => {
                            let _ = fs::remove_file(&produced_png);
                            let _ = fs::remove_file(&prefix);
                            return Err(format!("Failed to wait for pdftoppm: {e}"));
                        }
                    }
                };
                if status.success()
                    && let Ok(raw_png) = fs::read(&produced_png)
                {
                    if cancel.is_cancelled() {
                        let _ = fs::remove_file(&produced_png);
                        let _ = fs::remove_file(&prefix);
                        return Err(CANCELLED.to_string());
                    }
                    let _ = save_thumbnail_file(&raw_png, &uri, file_path, &target_path);
                }
                let _ = fs::remove_file(&produced_png);
                let _ = fs::remove_file(&prefix);
            }
        }
        if let Some(tp) = get_thumbnail_path(&uri, ThumbnailSize::Normal)
            && is_thumbnail_valid(&tp, file_path)
        {
            return Ok(tp);
        }
        if let Some(tp) = get_thumbnail_path(&uri, ThumbnailSize::Large)
            && is_thumbnail_valid(&tp, file_path)
        {
            return Ok(tp);
        }
    }

    // B) Image fallback: glycin-thumbnailer, then gdk-pixbuf-thumbnailer, then in-process pixbuf loader
    if mime.starts_with("image/") {
        for size in &needs_gen {
            check_cancelled(cancel)?;
            let Some(target_path) = get_thumbnail_path(&uri, *size) else {
                continue;
            };
            let mut produced = false;

            if is_executable_or_in_path("glycin-thumbnailer")
                && let Some(temp_output) = private_temp_path("portal-thumb-")
            {
                let mut cmd = Command::new("glycin-thumbnailer");
                cmd.args([
                    "-i",
                    &uri,
                    "-o",
                    temp_output.to_str().unwrap_or_default(),
                    "-s",
                    &size.pixel_size().to_string(),
                ]);
                cmd.stdout(std::process::Stdio::null());
                cmd.stderr(std::process::Stdio::null());
                if let Ok(mut child) = cmd.spawn() {
                    let deadline = std::time::Instant::now() + Duration::from_secs(10);
                    let status = loop {
                        match child.try_wait() {
                            Ok(Some(status)) => break Some(status),
                            Ok(None) if std::time::Instant::now() >= deadline => {
                                let _ = child.kill();
                                let _ = child.wait();
                                break None;
                            }
                            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                            Err(_) => break None,
                        }
                    };
                    if status.is_some_and(|s| s.success())
                        && let Ok(raw_png) = fs::read(&temp_output)
                        && save_thumbnail_file(&raw_png, &uri, file_path, &target_path).is_ok()
                    {
                        produced = true;
                    }
                }
                let _ = fs::remove_file(&temp_output);
            }

            if !produced
                && is_executable_or_in_path("gdk-pixbuf-thumbnailer")
                && let Some(temp_output) = private_temp_path("portal-thumb-")
            {
                let cmd = "gdk-pixbuf-thumbnailer -s %s %i %o";
                if execute_thumbnailer_command(
                    cmd,
                    file_path,
                    &uri,
                    &temp_output,
                    size.pixel_size(),
                    cancel,
                )
                .is_ok()
                    && let Ok(raw_png) = fs::read(&temp_output)
                {
                    if cancel.is_cancelled() {
                        return Err(CANCELLED.to_string());
                    }
                    if save_thumbnail_file(&raw_png, &uri, file_path, &target_path).is_ok() {
                        produced = true;
                    }
                }
                let _ = fs::remove_file(&temp_output);
            }

            if !produced {
                check_cancelled(cancel)?;
                let s = size.pixel_size() as i32;
                if let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_file_at_scale(file_path, s, s, true)
                    && let Ok(raw_png) = pixbuf.save_to_bufferv("png", &[])
                {
                    if cancel.is_cancelled() {
                        return Err(CANCELLED.to_string());
                    }
                    let _ = save_thumbnail_file(&raw_png, &uri, file_path, &target_path);
                }
            }
        }
        if let Some(tp) = get_thumbnail_path(&uri, ThumbnailSize::Normal)
            && is_thumbnail_valid(&tp, file_path)
        {
            return Ok(tp);
        }
        if let Some(tp) = get_thumbnail_path(&uri, ThumbnailSize::Large)
            && is_thumbnail_valid(&tp, file_path)
        {
            return Ok(tp);
        }
    }

    // 3. Last-resort fallback: Tumbler D-Bus via native GLib, if registered.
    check_cancelled(cancel)?;
    if trigger_tumbler_dbus(&uri, mime) {
        for _ in 0..10 {
            check_cancelled(cancel)?;
            std::thread::sleep(Duration::from_millis(50));
            if let Some(tp) = get_thumbnail_path(&uri, ThumbnailSize::Normal)
                && is_thumbnail_valid(&tp, file_path)
            {
                return Ok(tp);
            }
        }
    }

    if let Some(tp) = get_thumbnail_path(&uri, ThumbnailSize::Normal)
        && is_thumbnail_valid(&tp, file_path)
    {
        Ok(tp)
    } else {
        Err(format!("No thumbnailer available for {mime}"))
    }
}

/// Queue a thumbnail request to Tumbler via native GLib D-Bus.
fn trigger_tumbler_dbus(uri: &str, mime: &str) -> bool {
    let Ok(proxy) = gio::DBusProxy::for_bus_sync(
        gio::BusType::Session,
        gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES | gio::DBusProxyFlags::DO_NOT_CONNECT_SIGNALS,
        None::<&gio::DBusInterfaceInfo>,
        "org.freedesktop.thumbnails.Thumbnailer1",
        "/org/freedesktop/thumbnails/Thumbnailer1",
        "org.freedesktop.thumbnails.Thumbnailer1",
        None::<&gio::Cancellable>,
    ) else {
        return false;
    };

    let parameters = (
        vec![uri.to_string()],
        vec![mime.to_string()],
        "normal",
        "default",
        0u32,
    )
        .to_variant();

    proxy
        .call_sync(
            "Queue",
            Some(&parameters),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
        )
        .is_ok()
}

const MAX_THUMBNAIL_WORKERS: u32 = 4;

/// Coordinator for managing asynchronous thumbnail generation requests and avoiding duplicate work.
pub struct ThumbnailCoordinator {
    in_flight: Arc<Mutex<HashMap<PathBuf, ThumbnailCancel>>>,
    pool: glib::ThreadPool,
}

impl Default for ThumbnailCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbnailCoordinator {
    pub fn new() -> Self {
        Self {
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            pool: glib::ThreadPool::exclusive(MAX_THUMBNAIL_WORKERS)
                .expect("Failed to create thumbnail thread pool"),
        }
    }

    /// Queue a file for thumbnailing if it is not already being processed.
    /// When complete, invokes `on_complete` on the GTK main thread.
    ///
    /// A cancelled token makes this a no-op. Workers re-check the token before
    /// starting and between decode stages, so closing a dialog stops both queued
    /// work and most of the work already handed to the pool.
    pub fn queue_thumbnail<F>(
        &self,
        file_path: PathBuf,
        mime_type: Option<String>,
        cancel: ThumbnailCancel,
        on_complete: F,
    ) where
        F: FnOnce(PathBuf) + Send + 'static,
    {
        if cancel.is_cancelled() {
            return;
        }

        {
            let mut set = self.in_flight.lock();
            if let Some(existing) = set.get(&file_path) {
                if !existing.is_cancelled() {
                    return; // Already in flight for an active dialog
                }
                // The previous owner was cancelled; let this dialog take over.
                set.remove(&file_path);
            }
            if set.insert(file_path.clone(), cancel.clone()).is_some() {
                return; // Lost a race; the other caller owns it
            }
        }

        let inflight = self.in_flight.clone();
        let path = file_path.clone();
        let failed_cancel = cancel.clone();
        if self
            .pool
            .push(move || {
                let res = if cancel.is_cancelled() {
                    Err(CANCELLED.to_string())
                } else {
                    generate_thumbnail_cancellable(&path, mime_type.as_deref(), &cancel)
                };

                let removed_own_entry = {
                    let mut set = inflight.lock();
                    if set.get(&path).is_some_and(|owner| owner.same(&cancel)) {
                        set.remove(&path);
                        true
                    } else {
                        false
                    }
                };
                let should_trim = removed_own_entry
                    && inflight.lock().is_empty()
                    && cancel.is_cancelled();
                if should_trim {
                    // All work for the cancelled dialog has finished. Freeing
                    // the arenas from the worker thread is safe, but bounce to
                    // the GTK thread so the trim is serialized with UI frees.
                    schedule_trim();
                }

                if let Ok(thumb_path) = res
                    && !cancel.is_cancelled()
                {
                    let cancel_cb = cancel.clone();
                    glib::idle_add_once(move || {
                        if !cancel_cb.is_cancelled() {
                            on_complete(thumb_path);
                        }
                    });
                }
            })
            .is_err()
        {
            let removed_own_entry = {
                let mut set = self.in_flight.lock();
                if set.get(&file_path).is_some_and(|owner| owner.same(&failed_cancel)) {
                    set.remove(&file_path);
                    true
                } else {
                    false
                }
            };
            let should_trim = removed_own_entry
                && self.in_flight.lock().is_empty()
                && failed_cancel.is_cancelled();
            if should_trim {
                schedule_trim();
            }
        }
    }
}

pub static COORDINATOR: LazyLock<ThumbnailCoordinator> = LazyLock::new(ThumbnailCoordinator::new);

/// Decide whether an item wants a thumbnail and the cache doesn't already contain a valid one.
fn should_process(info: &gio::FileInfo) -> bool {
    if info.attribute_byte_string("thumbnail::path").is_some()
        && info.boolean("thumbnail::is-valid")
    {
        return false;
    }

    let Some(file_obj) = info.attribute_object("standard::file") else {
        return false;
    };
    let Ok(file) = file_obj.downcast::<gio::File>() else {
        return false;
    };
    let Some(path) = file.path() else {
        return false;
    };
    if !path.is_file() {
        return false;
    }

    let mime = info.content_type();
    let mime_str = mime.as_deref().unwrap_or("");
    if mime_str.starts_with("image/")
        || mime_str == "application/pdf"
        || REGISTRY.find_thumbnailer(mime_str).is_some()
    {
        return true;
    }

    if mime_str.is_empty() || mime_str == "application/octet-stream" {
        let (guessed, _) = gio::content_type_guess(path.to_str(), None);
        let guessed_str = guessed.as_str();
        return guessed_str.starts_with("image/")
            || guessed_str == "application/pdf"
            || REGISTRY.find_thumbnailer(guessed_str).is_some();
    }

    false
}

struct ViewWatcher {
    attached_views: Mutex<HashSet<usize>>,
    attached_models: Mutex<HashSet<usize>>,
    attached_stacks: Mutex<HashSet<usize>>,
    cancel: ThumbnailCancel,
    tx: tokio::sync::mpsc::UnboundedSender<(PathBuf, String, PathBuf)>,
}

impl ViewWatcher {
    fn attach(self: &Arc<Self>, root: &Widget) {
        let name = root.type_().name();
        if name == "GtkGridView" || name == "GtkColumnView" {
            let view_ptr = ToGlibPtr::<*mut GtkWidget>::to_glib_none(root).0 as usize;
            if self.attached_views.lock().insert(view_ptr) {
                let this = self.clone();
                this.connect_model(root);

                let this_notify = self.clone();
                root.connect_notify(Some("model"), move |view, _| {
                    this_notify.connect_model(view);
                });
            }
            return;
        }

        if name == "GtkStack" {
            let stack_ptr = ToGlibPtr::<*mut GtkWidget>::to_glib_none(root).0 as usize;
            if self.attached_stacks.lock().insert(stack_ptr) {
                let this_stack = self.clone();
                root.connect_notify(Some("visible-child"), move |stack, _| {
                    this_stack.attach(stack);
                });
            }
        }

        let mut child = root.first_child();
        while let Some(c) = child {
            self.attach(&c);
            child = c.next_sibling();
        }
    }

    fn connect_model(self: &Arc<Self>, view: &Widget) {
        let Some(model) = view.property::<Option<gio::ListModel>>("model") else {
            return;
        };
        let model_ptr = ToGlibPtr::<*mut GListModel>::to_glib_none(&model).0 as usize;
        if !self.attached_models.lock().insert(model_ptr) {
            return;
        }

        for i in 0..model.n_items() {
            if let Some(item) = model.item(i)
                && let Ok(info) = item.downcast::<gio::FileInfo>()
            {
                self.process_item(&info);
            }
        }

        let this = self.clone();
        model.connect_items_changed(move |m, pos, _removed, added| {
            for i in pos..(pos + added) {
                if let Some(item) = m.item(i)
                    && let Ok(info) = item.downcast::<gio::FileInfo>()
                {
                    this.process_item(&info);
                }
            }
        });
    }

    fn process_item(self: &Arc<Self>, info: &gio::FileInfo) {
        if self.cancel.is_cancelled() {
            return;
        }
        if !should_process(info) {
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
        let name = info.display_name().to_string();
        let mime = info.content_type().map(|c| c.to_string());

        let tx = self.tx.clone();
        let cancel_cb = self.cancel.clone();
        let path_clone = path.clone();
        let name_clone = name.clone();
        COORDINATOR.queue_thumbnail(path, mime, self.cancel.clone(), move |thumb_path| {
            if cancel_cb.is_cancelled() {
                return;
            }
            let _ = tx.send((path_clone, name_clone, thumb_path));
        });
    }
}

/// Attach thumbnail generator to a FileChooserDialog.
///
/// Returns the dialog's cancellation token. It is cancelled automatically when
/// the dialog is hidden or destroyed; callers only need to hold it if they want
/// to cancel earlier (for example when a D-Bus request is withdrawn).
pub fn setup_dialog_thumbnailer(
    dialog: &FileChooserDialog,
    context: &MainContext,
) -> ThumbnailCancel {
    let cancel = ThumbnailCancel::new();

    // A response or D-Bus cancellation closes/hides the dialog immediately.
    // Stopping queued thumbnail work at that point avoids decoding files for a
    // window that no longer exists (we measured work continuing after close).
    let cancel_hide = cancel.clone();
    dialog.connect_hide(move |_| cancel_hide.cancel());

    let cancel_destroy = cancel.clone();
    dialog.connect_destroy(move |_| {
        cancel_destroy.cancel();
        // The dialog and its thumbnails are gone; return any free arena pages
        // instead of keeping them until the next picker or memory pressure.
        // Defer the trim until the destroy signal has finished so the child
        // widgets' frees are included in the same pass.
        schedule_trim();
    });

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(PathBuf, String, PathBuf)>();

    let dialog_weak = dialog.downgrade();
    let cancel_loop = cancel.clone();
    context.spawn_local(async move {
        while let Some((file_path, file_name, thumb_path)) = rx.recv().await {
            if cancel_loop.is_cancelled() {
                break;
            }
            let Some(dlg) = dialog_weak.upgrade() else {
                break;
            };
            let thumb_str = thumb_path.to_string_lossy().to_string();

            // 1. Update any visible widgets
            fn refresh_widgets(root: &Widget, file_name: &str, thumb_str: &str, file_path: &Path) {
                if root.type_().name() == "GtkFileThumbnail"
                    && let Some(info) = root.property::<Option<gio::FileInfo>>("file-info")
                {
                    let matches = info.display_name() == file_name
                        || info
                            .attribute_object("standard::file")
                            .and_then(|f| f.downcast::<gio::File>().ok())
                            .and_then(|f| f.path())
                            .map(|p| p == file_path)
                            .unwrap_or(false);
                    if matches {
                        info.set_attribute_byte_string("thumbnail::path", thumb_str);
                        info.set_attribute_boolean("thumbnail::is-valid", true);
                        info.set_attribute_byte_string("thumbnail::path-large", thumb_str);
                        info.set_attribute_boolean("thumbnail::is-valid-large", true);
                        info.remove_attribute("filechooser::queried");
                        info.remove_attribute("standard::icon");
                        root.set_property("file-info", None::<&gio::FileInfo>);
                        root.set_property("file-info", &info);
                    }
                }
                let mut child = root.first_child();
                while let Some(c) = child {
                    refresh_widgets(&c, file_name, thumb_str, file_path);
                    child = c.next_sibling();
                }
            }
            refresh_widgets(dlg.upcast_ref(), &file_name, &thumb_str, &file_path);

            // 2. Also update the GFileInfo in the view's model so off-screen/recycled items stay updated
            fn update_model(root: &Widget, file_name: &str, thumb_str: &str, file_path: &Path) {
                let name = root.type_().name();
                if (name == "GtkGridView" || name == "GtkColumnView")
                    && let Some(model) = root.property::<Option<gio::ListModel>>("model")
                {
                    for i in 0..model.n_items() {
                        if let Some(item) = model.item(i)
                            && let Ok(info) = item.downcast::<gio::FileInfo>()
                        {
                            let matches = info.display_name() == file_name
                                || info
                                    .attribute_object("standard::file")
                                    .and_then(|f| f.downcast::<gio::File>().ok())
                                    .and_then(|f| f.path())
                                    .map(|p| p == file_path)
                                    .unwrap_or(false);
                            if matches {
                                info.set_attribute_byte_string("thumbnail::path", thumb_str);
                                info.set_attribute_boolean("thumbnail::is-valid", true);
                                info.set_attribute_byte_string("thumbnail::path-large", thumb_str);
                                info.set_attribute_boolean("thumbnail::is-valid-large", true);
                                info.remove_attribute("filechooser::queried");
                                info.remove_attribute("standard::icon");
                                break;
                            }
                        }
                    }
                    return;
                }
                let mut child = root.first_child();
                while let Some(c) = child {
                    update_model(&c, file_name, thumb_str, file_path);
                    child = c.next_sibling();
                }
            }
            update_model(dlg.upcast_ref(), &file_name, &thumb_str, &file_path);
        }
    });

    let watcher = Arc::new(ViewWatcher {
        attached_views: Mutex::new(HashSet::new()),
        attached_models: Mutex::new(HashSet::new()),
        attached_stacks: Mutex::new(HashSet::new()),
        cancel: cancel.clone(),
        tx,
    });

    // 1. Initial attachment pass
    watcher.attach(dialog.upcast_ref());

    // 2. Attach when the dialog is mapped/shown (template children realized)
    let watcher_map = watcher.clone();
    let dialog_weak_map = dialog.downgrade();
    dialog.connect_map(move |_| {
        if let Some(dlg) = dialog_weak_map.upgrade() {
            watcher_map.attach(dlg.upcast_ref());
        }
    });

    cancel
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
    fn test_png_metadata_injection_and_verification() {
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

        let uri = "file:///test/path.jpg";
        let mtime = 123456789;
        let injected = inject_png_metadata(&minimal_png, uri, mtime, 4096)
            .expect("metadata injection should succeed");

        assert!(injected.len() > minimal_png.len());
        assert_eq!(&injected[..8], &minimal_png[..8]);
        assert_eq!(&injected[12..16], b"IHDR");

        // Verify with our validator
        assert!(is_png_data_metadata_valid(&injected, uri, mtime));
        assert!(!is_png_data_metadata_valid(
            &injected,
            "file:///other.jpg",
            mtime
        ));
        assert!(!is_png_data_metadata_valid(&injected, uri, 9999));
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

        match generate_thumbnail(&img_path, Some("image/png")) {
            Ok(thumb_path) => {
                assert!(
                    thumb_path.is_file(),
                    "thumbnail file should exist at {:?}",
                    thumb_path
                );
                let _ = fs::remove_file(&thumb_path);
            }
            Err(e) => {
                eprintln!("no thumbnailer available in this test environment ({e}); skipping");
            }
        }

        let _ = fs::remove_file(&img_path);
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn cancelled_token_is_sticky() {
        let cancel = ThumbnailCancel::new();
        assert!(!cancel.is_cancelled());
        cancel.cancel();
        cancel.cancel();
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn cancelled_generation_returns_before_touching_the_file() {
        let cancel = ThumbnailCancel::new();
        cancel.cancel();

        let err = generate_thumbnail_cancellable(
            Path::new("/nonexistent/portal-cancelled.png"),
            Some("image/png"),
            &cancel,
        )
        .expect_err("cancelled work must not run");
        assert_eq!(err, CANCELLED);
    }

    #[test]
    fn cancelled_queue_does_not_generate_a_thumbnail() {
        let test_dir = std::env::temp_dir().join("portal_test_cancelled_queue");
        let _ = fs::create_dir_all(&test_dir);
        let img_path = test_dir.join("cancelled.png");
        let minimal_png: [u8; 67] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        fs::write(&img_path, minimal_png).unwrap();

        let uri = file_to_uri(&img_path).unwrap();
        let thumb_path = get_thumbnail_path(&uri, ThumbnailSize::Normal).unwrap();
        let _ = fs::remove_file(&thumb_path);

        let cancel = ThumbnailCancel::new();
        cancel.cancel();
        let coordinator = ThumbnailCoordinator::new();
        let (tx, rx) = std::sync::mpsc::channel();
        coordinator.queue_thumbnail(
            img_path.clone(),
            Some("image/png".into()),
            cancel,
            move |_| {
                let _ = tx.send(());
            },
        );

        std::thread::sleep(Duration::from_millis(100));
        assert!(
            rx.try_recv().is_err(),
            "cancelled work must not invoke its completion callback"
        );
        assert!(
            !thumb_path.exists(),
            "cancelled work must not write a thumbnail"
        );

        let _ = fs::remove_file(&img_path);
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_generate_thumbnail_pdf_fixture() {
        let test_dir = std::env::temp_dir().join("portal_test_pdf_dir");
        let _ = fs::create_dir_all(&test_dir);
        let pdf_path = test_dir.join("sample.pdf");

        // Minimal valid 1-page PDF fixture
        let minimal_pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/MediaBox[0 0 100 100]/Parent 2 0 R>>endobj\nxref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000052 00000 n \n0000000102 00000 n \ntrailer<</Size 4/Root 1 0 R>>\nstartxref\n178\n%%EOF\n";
        fs::write(&pdf_path, minimal_pdf).unwrap();

        let res = generate_thumbnail(&pdf_path, Some("application/pdf"));
        if let Ok(thumb) = res {
            assert!(thumb.is_file());
            let _ = fs::remove_file(&thumb);
        } else {
            eprintln!("No PDF thumbnailer available in test environment; skipped");
        }

        let _ = fs::remove_file(&pdf_path);
        let _ = fs::remove_dir_all(&test_dir);
    }
}

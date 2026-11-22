//! Thumbnail generation for the file chooser.
//!
//! Work runs on GLib's shared thread pool ([`glib::ThreadPool::shared`]), not on
//! portal-owned threads, so workers appear as `pool-*` in a thread dump:
//! `GThreadPool` has no API for naming its workers. The pool is shared rather
//! than exclusive so idle workers return to GLib's global pool and are stopped
//! after its idle timeout (15 seconds by default in GLib 2.84) instead of
//! staying resident for the process lifetime.
//!
//! Cancellation is cooperative via [`ThumbnailCancel`]: queued jobs exit before
//! touching the filesystem, running external thumbnailers are killed, and a
//! single in-process image decode cannot be interrupted mid-call.

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
    Some(escape_uri_brackets(uri))
}

/// Percent-encode square brackets per the GNOME/Cosmic file-URI convention.
///
/// `String::replace` builds a fresh string even when nothing matched, so guard
/// the replacements and move `uri` through untouched on the common branch.
fn escape_uri_brackets(uri: String) -> String {
    if uri.contains('[') || uri.contains(']') {
        uri.replace('[', "%5B").replace(']', "%5D")
    } else {
        uri
    }
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

/// Existence / size / staleness checks shared by both validation entry points.
///
/// Returns the source file's mtime in whole seconds once the thumbnail passes
/// the filesystem-level checks, or `None` when it is missing, empty, or stale.
fn thumbnail_mtime_checks(thumb_path: &Path, file_path: &Path) -> Option<u64> {
    let thumb_meta = fs::metadata(thumb_path).ok()?;
    if !thumb_meta.is_file() || thumb_meta.len() == 0 {
        return None;
    }
    let file_meta = fs::metadata(file_path).ok()?;
    let thumb_mtime = thumb_meta.modified().ok()?;
    let file_mtime = file_meta.modified().ok()?;
    // Thumbs older than the source are never valid.
    if thumb_mtime < file_mtime {
        return None;
    }
    Some(
        file_mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
}

/// Validate a cached thumbnail: it must exist, be non-empty, not be older than the source file,
/// and embed `Thumb::URI`/`Thumb::MTime` tEXt chunks that match the source file.
pub fn is_thumbnail_valid(thumb_path: &Path, file_path: &Path) -> bool {
    let Some(file_mtime_secs) = thumbnail_mtime_checks(thumb_path, file_path) else {
        return false;
    };
    // Verify FreeDesktop tEXt metadata embedded in the PNG.
    if let Some(uri) = file_to_uri(file_path) {
        return is_png_metadata_valid(thumb_path, &uri, file_mtime_secs);
    }
    true
}

/// Same validation as [`is_thumbnail_valid`] but reuses an already-computed URI,
/// avoiding a fresh `gio::File` + string allocation per thumbnail checked in a
/// single generation pass. In that pass the URI is always `Some`.
fn is_thumbnail_valid_for_uri(thumb_path: &Path, file_path: &Path, uri: &str) -> bool {
    let Some(file_mtime_secs) = thumbnail_mtime_checks(thumb_path, file_path) else {
        return false;
    };
    is_png_metadata_valid(thumb_path, uri, file_mtime_secs)
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
        match chunk_type {
            b"tEXt" if len >= 2 => {
                let payload = &png_data[pos + 8..pos + 8 + len];
                if let Some(null_pos) = payload.iter().position(|&b| b == 0) {
                    let keyword = &payload[..null_pos];
                    let text = &payload[null_pos + 1..];
                    match keyword {
                        b"Thumb::URI" => found_uri = text == expected_uri.as_bytes(),
                        b"Thumb::MTime" => {
                            if let Ok(s) = std::str::from_utf8(text) {
                                let secs = s.split('.').next().and_then(|s| s.parse::<u64>().ok());
                                found_mtime = secs
                                    .map(|s| s.abs_diff(expected_mtime_secs) <= 1)
                                    .unwrap_or(false);
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"IEND" => break,
            _ => {}
        }
        pos = chunk_end;
    }
    found_uri && found_mtime
}

/// IEEE 802.3 CRC32 lookup table, generated at compile time from the same
/// reflected polynomial (0xEDB88320) the bitwise implementation used.
const fn make_png_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if (crc & 1) != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static PNG_CRC_TABLE: [u32; 256] = make_png_crc_table();

/// IEEE 802.3 CRC32 calculation for PNG chunk checksums.
///
/// Table-driven: one shift/xor per input byte. The table is a `static`, so this
/// performs no allocation.
fn png_crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        let index = ((crc ^ b as u32) & 0xff) as usize;
        crc = (crc >> 8) ^ PNG_CRC_TABLE[index];
    }
    !crc
}

/// Byte length of a tEXt chunk holding `keyword`, a null byte and `text`.
fn text_chunk_len(keyword: &str, text: &str) -> usize {
    4 + 4 + keyword.len() + 1 + text.len() + 4
}

/// Append a standard PNG tEXt chunk (length + "tEXt" + keyword + null + text + crc)
/// into `out` in place, so no separate buffer is allocated for each chunk.
fn push_text_chunk(out: &mut Vec<u8>, keyword: &str, text: &str) {
    let data_len = (keyword.len() + 1 + text.len()) as u32;
    out.extend_from_slice(&data_len.to_be_bytes());

    let crc_start = out.len();
    out.extend_from_slice(b"tEXt");
    out.extend_from_slice(keyword.as_bytes());
    out.push(0);
    out.extend_from_slice(text.as_bytes());
    let crc = png_crc32(&out[crc_start..]);
    out.extend_from_slice(&crc.to_be_bytes());
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

    let mtime_str = mtime_secs.to_string();
    let size_str = size_bytes.to_string();
    const SOFTWARE: &str = "xdg-desktop-portal-gtk4";

    // One allocation at exactly the final size: the original PNG plus the four
    // tEXt chunks, whose sizes are known up front.
    let overhead = text_chunk_len("Thumb::URI", uri)
        + text_chunk_len("Thumb::MTime", &mtime_str)
        + text_chunk_len("Thumb::Size", &size_str)
        + text_chunk_len("Software", SOFTWARE);
    let mut result = Vec::with_capacity(png_data.len() + overhead);
    result.extend_from_slice(&png_data[..ihdr_end]);
    push_text_chunk(&mut result, "Thumb::URI", uri);
    push_text_chunk(&mut result, "Thumb::MTime", &mtime_str);
    push_text_chunk(&mut result, "Thumb::Size", &size_str);
    push_text_chunk(&mut result, "Software", SOFTWARE);
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

    // Borrow the original bytes when injection fails (invalid PNG) rather than
    // copying the whole image.
    let injected = inject_png_metadata(raw_png, uri, mtime, size);
    let final_png: &[u8] = injected.as_deref().unwrap_or(raw_png);

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
    tmp.write_all(final_png)?;
    tmp.persist(target_path).map_err(|e| e.error)?;
    Ok(())
}

/// Parsed FreeDesktop .thumbnailer entry.
///
/// The registry is built once for the process lifetime, so fields are boxed to
/// their exact size: one allocation per string rather than a `String`'s spare
/// capacity, and no per-MIME clone of the whole spec (see [`ThumbnailerRegistry`]).
#[derive(Debug, Clone)]
pub struct ThumbnailerSpec {
    pub name: Box<str>,
    pub exec: Box<str>,
    pub try_exec: Option<Box<str>>,
    pub mime_types: Box<[Box<str>]>,
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

/// Whether `path` is a regular file with at least one executable bit set.
fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Search the given `PATH` entries for an executable named `cmd`.
fn is_executable_in_path_dirs(cmd: &str, path_var: Option<&std::ffi::OsStr>) -> bool {
    let Some(path_var) = path_var else {
        return false;
    };
    std::env::split_paths(path_var).any(|dir| is_executable_file(&dir.join(cmd)))
}

/// Resolve a thumbnailer by absolute path, relative path, or `PATH` lookup.
fn is_executable_or_in_path(cmd: &str) -> bool {
    let p = Path::new(cmd);
    if p.is_absolute() || cmd.contains('/') {
        return is_executable_file(p);
    }
    is_executable_in_path_dirs(cmd, std::env::var_os("PATH").as_deref())
}

/// Sort the MIME index, tie-breaking on spec index (discovery order) so the
/// first spec found for a MIME stays first even with an unstable sort.
fn sort_registry_index(index: &mut [(Box<str>, usize)]) {
    index.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
}

/// Registry of discovered FreeDesktop thumbnailers.
///
/// A flat, MIME-sorted index into a single boxed slice of specs means each MIME
/// stores only a `(Box<str>, index)` pair and all MIMEs share one spec. Lookup
/// is a `partition_point` with no hashing, which suits small static sets.
/// Entries are stably sorted so the first spec discovered for a MIME stays first.
#[derive(Debug, Default)]
pub struct ThumbnailerRegistry {
    index: Box<[(Box<str>, usize)]>,
    specs: Box<[ThumbnailerSpec]>,
}

impl ThumbnailerRegistry {
    pub fn load() -> Self {
        let mut index = Vec::new();
        let mut specs = Vec::new();
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
            let Ok(dir_entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in dir_entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("thumbnailer") {
                    Self::load_thumbnailer_file(&path, &mut index, &mut specs);
                }
            }
        }

        sort_registry_index(&mut index);
        Self {
            index: index.into_boxed_slice(),
            specs: specs.into_boxed_slice(),
        }
    }

    fn load_thumbnailer_file(
        path: &Path,
        index: &mut Vec<(Box<str>, usize)>,
        specs: &mut Vec<ThumbnailerSpec>,
    ) {
        let Ok(content) = fs::read_to_string(path) else {
            return;
        };
        let mut in_section = false;
        let mut exec = None;
        let mut try_exec = None;
        let mut mime_types: Box<[Box<str>]> = Box::default();

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
                    exec = Some(Box::from(val));
                } else if key.eq_ignore_ascii_case("TryExec") {
                    try_exec = Some(Box::from(val));
                } else if key.eq_ignore_ascii_case("MimeType") {
                    mime_types = val
                        .split(';')
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(Box::from)
                        .collect();
                }
            }
        }

        if let Some(exec) = exec
            && !mime_types.is_empty()
        {
            let name: Box<str> = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .into();
            let spec = ThumbnailerSpec {
                name,
                exec,
                try_exec,
                mime_types,
            };
            if spec.is_available() {
                let spec_index = specs.len();
                for mime in &spec.mime_types {
                    // Only the small MIME key is duplicated, not the whole spec.
                    index.push((mime.clone(), spec_index));
                }
                specs.push(spec);
            }
        }
    }

    /// First spec indexed under `mime`, if any.
    fn spec_for_mime(&self, mime: &str) -> Option<&ThumbnailerSpec> {
        let pos = self.index.partition_point(|(key, _)| &**key < mime);
        let (key, spec_index) = self.index.get(pos)?;
        if &**key == mime {
            self.specs.get(*spec_index)
        } else {
            None
        }
    }

    pub fn find_thumbnailer(&self, mime: &str) -> Option<&ThumbnailerSpec> {
        if let Some(spec) = self.spec_for_mime(mime) {
            return Some(spec);
        }
        if let Some((category, _)) = mime.split_once('/') {
            let wildcard = format!("{category}/*");
            if let Some(spec) = self.spec_for_mime(&wildcard) {
                return Some(spec);
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
    check_cancelled(cancel)?;

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

/// Remove a freshly created temporary thumbnailer output and return the standard
/// cancellation error.
///
/// The evince and gdk-pixbuf cancel paths funnel through here; the other
/// branches unlink their temp files inline before returning the standard
/// `CANCELLED` value, so no output is leaked in the private runtime directory.
fn cancelled_after_temp_cleanup(temp_path: &Path) -> String {
    let _ = fs::remove_file(temp_path);
    CANCELLED.to_string()
}

/// Sizes whose cached thumbnail is missing or invalid, in generation order.
///
/// Derived from validity checks already performed for the early return, so the
/// work list needs no second validation pass. The final post-generation check
/// still re-validates each produced tier. Returns `(count, sizes)`; only
/// `sizes[..count]` is meaningful.
fn sizes_needing_generation(normal_ok: bool, large_ok: bool) -> (usize, [ThumbnailSize; 2]) {
    let mut sizes = [ThumbnailSize::Normal, ThumbnailSize::Large];
    let mut count = 0;
    if !normal_ok {
        sizes[count] = ThumbnailSize::Normal;
        count += 1;
    }
    if !large_ok {
        sizes[count] = ThumbnailSize::Large;
        count += 1;
    }
    (count, sizes)
}

/// The thumbnail tiers that were actually produced by one generation pass.
///
/// Each tier is `Some` only when that size's cached PNG exists and validates.
/// Keeping them separate lets the caller set `thumbnail::path` (128px) and
/// `thumbnail::path-large` (256px) from their own files instead of aliasing a
/// single image -- which would make GTK upscale the small one for large display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeneratedThumbnails {
    pub normal: Option<PathBuf>,
    pub large: Option<PathBuf>,
}

impl GeneratedThumbnails {
    /// The path to return when a caller only accepts a single thumbnail.
    /// Prefer the normal tier, falling back to large when only that was made.
    fn preferred(&self) -> Option<&PathBuf> {
        self.normal.as_ref().or(self.large.as_ref())
    }
}

/// Resolve which tiers are present after a generation pass, keeping only those
/// whose cached PNG still validates against the source file.
fn valid_generated_thumbnails(
    normal: Option<&PathBuf>,
    large: Option<&PathBuf>,
    file_path: &Path,
    uri: &str,
) -> GeneratedThumbnails {
    GeneratedThumbnails {
        normal: normal
            .filter(|p| is_thumbnail_valid_for_uri(p.as_path(), file_path, uri))
            .cloned(),
        large: large
            .filter(|p| is_thumbnail_valid_for_uri(p.as_path(), file_path, uri))
            .cloned(),
    }
}

/// Write each tier's own path into a `GFileInfo`. The normal (128px) and large
/// (256px) cache entries are separate files; setting both attributes to one
/// path makes GTK upscale the small image for large display.
fn apply_thumbnail_attributes(info: &gio::FileInfo, thumbnails: &GeneratedThumbnails) {
    if let Some(path) = thumbnails.normal.as_ref() {
        let s = path.to_string_lossy();
        info.set_attribute_byte_string("thumbnail::path", &s);
        info.set_attribute_boolean("thumbnail::is-valid", true);
    }
    if let Some(path) = thumbnails.large.as_ref() {
        let s = path.to_string_lossy();
        info.set_attribute_byte_string("thumbnail::path-large", &s);
        info.set_attribute_boolean("thumbnail::is-valid-large", true);
    }
    info.remove_attribute("filechooser::queried");
    info.remove_attribute("standard::icon");
}

/// Generate thumbnails directly following the FreeDesktop .thumbnailer spec.
///
/// Both the `normal` (128) and `large` (256) cache entries are produced so that GIO's
/// `THUMBNAIL_SIZE_AUTO` never selects a stale external thumbnail over a fresh one.
pub fn generate_thumbnail(file_path: &Path, mime_type: Option<&str>) -> Result<PathBuf, String> {
    let generated = generate_thumbnail_cancellable(file_path, mime_type, &ThumbnailCancel::new())?;
    generated
        .preferred()
        .cloned()
        .ok_or_else(|| "No thumbnail generated".to_string())
}

fn generate_thumbnail_cancellable(
    file_path: &Path,
    mime_type: Option<&str>,
    cancel: &ThumbnailCancel,
) -> Result<GeneratedThumbnails, String> {
    check_cancelled(cancel)?;

    let uri = file_to_uri(file_path).ok_or_else(|| "Failed to construct file URI".to_string())?;
    let mime = mime_type.unwrap_or("application/octet-stream");

    // Resolve each target path once. get_thumbnail_path recreates the cache
    // directory, recomputes the URI's MD5 hash and formats a filename on every
    // call, so the rest of this pass reuses these cached paths instead of
    // calling it again and again.
    let target_normal = get_thumbnail_path(&uri, ThumbnailSize::Normal);
    let target_large = get_thumbnail_path(&uri, ThumbnailSize::Large);

    // Return early only if every size tier is already valid. Check each tier
    // exactly once and reuse the URI computed above for the PNG metadata match.
    let normal_ok = target_normal
        .as_deref()
        .is_some_and(|p| is_thumbnail_valid_for_uri(p, file_path, &uri));
    let large_ok = target_large
        .as_deref()
        .is_some_and(|p| is_thumbnail_valid_for_uri(p, file_path, &uri));
    if normal_ok && large_ok {
        check_cancelled(cancel)?;
        // Both tiers were already validated above; reuse those results directly
        // so the fully-cached hot path does not re-read either PNG.
        return Ok(GeneratedThumbnails {
            normal: target_normal,
            large: target_large,
        });
    }

    // Derive the work list from those checks instead of re-reading and
    // re-hashing every thumbnail a second time.
    let (needs_gen_count, needs_gen_sizes) = sizes_needing_generation(normal_ok, large_ok);
    let needs_gen = &needs_gen_sizes[..needs_gen_count];
    let cached_target = |size: ThumbnailSize| match size {
        ThumbnailSize::Normal => target_normal.as_ref(),
        ThumbnailSize::Large => target_large.as_ref(),
    };

    check_cancelled(cancel)?;

    // 1. Use a registered FreeDesktop .thumbnailer when one matches the MIME type.
    if let Some(spec) = REGISTRY.find_thumbnailer(mime) {
        for size in needs_gen {
            check_cancelled(cancel)?;
            let Some(target_path) = cached_target(*size) else {
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
                    let _ = save_thumbnail_file(&raw_png, &uri, file_path, target_path);
                } else {
                    let _ = fs::remove_file(&temp_output);
                }
            }
        }
        let generated = valid_generated_thumbnails(
            target_normal.as_ref(),
            target_large.as_ref(),
            file_path,
            &uri,
        );
        if generated.preferred().is_some() {
            return Ok(generated);
        }
    }

    // 2. Built-in fallbacks
    // A) PDF fallback tools: evince-thumbnailer or pdftoppm
    if mime == "application/pdf" {
        for size in needs_gen {
            check_cancelled(cancel)?;
            let Some(target_path) = cached_target(*size) else {
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
                        return Err(cancelled_after_temp_cleanup(&temp_output));
                    }
                    if save_thumbnail_file(&raw_png, &uri, file_path, target_path).is_ok() {
                        produced = true;
                    }
                }
                let _ = fs::remove_file(&temp_output);
            }

            check_cancelled(cancel)?;

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
                    let _ = save_thumbnail_file(&raw_png, &uri, file_path, target_path);
                }
                let _ = fs::remove_file(&produced_png);
                let _ = fs::remove_file(&prefix);
            }
        }
        let generated = valid_generated_thumbnails(
            target_normal.as_ref(),
            target_large.as_ref(),
            file_path,
            &uri,
        );
        if generated.preferred().is_some() {
            return Ok(generated);
        }
    }

    // B) Image fallback: glycin-thumbnailer, then gdk-pixbuf-thumbnailer, then in-process pixbuf loader
    if mime.starts_with("image/") {
        for size in needs_gen {
            check_cancelled(cancel)?;
            let Some(target_path) = cached_target(*size) else {
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
                        // glycin is spawned inline here rather than through
                        // `execute_thumbnailer_command`, so this loop polls
                        // cancellation itself before each wait.
                        if cancel.is_cancelled() {
                            let _ = child.kill();
                            let _ = child.wait();
                            break None;
                        }
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
                    if !cancel.is_cancelled()
                        && status.is_some_and(|s| s.success())
                        && let Ok(raw_png) = fs::read(&temp_output)
                        && save_thumbnail_file(&raw_png, &uri, file_path, target_path).is_ok()
                    {
                        produced = true;
                    }
                }
                let _ = fs::remove_file(&temp_output);
            }

            check_cancelled(cancel)?;

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
                        return Err(cancelled_after_temp_cleanup(&temp_output));
                    }
                    if save_thumbnail_file(&raw_png, &uri, file_path, target_path).is_ok() {
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
                    let _ = save_thumbnail_file(&raw_png, &uri, file_path, target_path);
                }
            }
        }
        let generated = valid_generated_thumbnails(
            target_normal.as_ref(),
            target_large.as_ref(),
            file_path,
            &uri,
        );
        if generated.preferred().is_some() {
            return Ok(generated);
        }
    }

    // 3. Last-resort fallback: Tumbler D-Bus via native GLib, if registered.
    check_cancelled(cancel)?;
    if trigger_tumbler_dbus(&uri, mime) {
        for _ in 0..10 {
            check_cancelled(cancel)?;
            std::thread::sleep(Duration::from_millis(50));
            let generated = valid_generated_thumbnails(
                target_normal.as_ref(),
                target_large.as_ref(),
                file_path,
                &uri,
            );
            if generated.preferred().is_some() {
                return Ok(generated);
            }
        }
    }

    check_cancelled(cancel)?;

    let generated = valid_generated_thumbnails(
        target_normal.as_ref(),
        target_large.as_ref(),
        file_path,
        &uri,
    );
    if generated.preferred().is_some() {
        Ok(generated)
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
            // Use a shared pool, not an exclusive one: exclusive pools start
            // their threads eagerly and keep them for the lifetime of the
            // process, so cancelling every dialog would still leave the four
            // workers resident. Shared workers return to GLib's global pool
            // when idle and are reaped after the idle timeout.
            pool: glib::ThreadPool::shared(Some(MAX_THUMBNAIL_WORKERS))
                .expect("Failed to create thumbnail thread pool"),
        }
    }

    /// Queue a file for thumbnailing if it is not already being processed.
    /// When complete, invokes `on_complete` with the paths of whichever size
    /// tiers were actually generated, on the GTK main thread.
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
        F: FnOnce(GeneratedThumbnails) + Send + 'static,
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
                let should_trim =
                    removed_own_entry && inflight.lock().is_empty() && cancel.is_cancelled();
                if should_trim {
                    // All work for the cancelled dialog has finished. Freeing
                    // the arenas from the worker thread is safe, but bounce to
                    // the GTK thread so the trim is serialized with UI frees.
                    schedule_trim();
                }

                if let Ok(thumbnails) = res
                    && !cancel.is_cancelled()
                {
                    let cancel_cb = cancel.clone();
                    glib::idle_add_once(move || {
                        if !cancel_cb.is_cancelled() {
                            on_complete(thumbnails);
                        }
                    });
                }
            })
            .is_err()
        {
            let removed_own_entry = {
                let mut set = self.in_flight.lock();
                if set
                    .get(&file_path)
                    .is_some_and(|owner| owner.same(&failed_cancel))
                {
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
    tx: tokio::sync::mpsc::UnboundedSender<(PathBuf, String, GeneratedThumbnails)>,
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
        COORDINATOR.queue_thumbnail(path, mime, self.cancel.clone(), move |thumbnails| {
            if cancel_cb.is_cancelled() {
                return;
            }
            // `name` is used only here: move it into the closure instead of
            // cloning and leaving the original to drop.
            let _ = tx.send((path_clone, name, thumbnails));
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

    let (tx, mut rx) =
        tokio::sync::mpsc::unbounded_channel::<(PathBuf, String, GeneratedThumbnails)>();

    let dialog_weak = dialog.downgrade();
    let cancel_loop = cancel.clone();
    context.spawn_local(async move {
        // 1. Update any visible widgets
        fn refresh_widgets(
            root: &Widget,
            file_name: &str,
            thumbnails: &GeneratedThumbnails,
            file_path: &Path,
        ) {
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
                    apply_thumbnail_attributes(&info, thumbnails);
                    root.set_property("file-info", None::<&gio::FileInfo>);
                    root.set_property("file-info", &info);
                }
            }
            let mut child = root.first_child();
            while let Some(c) = child {
                refresh_widgets(&c, file_name, thumbnails, file_path);
                child = c.next_sibling();
            }
        }

        // 2. Also update the GFileInfo in the view's model so off-screen/recycled items stay updated
        fn update_model(
            root: &Widget,
            file_name: &str,
            thumbnails: &GeneratedThumbnails,
            file_path: &Path,
        ) {
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
                            apply_thumbnail_attributes(&info, thumbnails);
                            break;
                        }
                    }
                }
                return;
            }
            let mut child = root.first_child();
            while let Some(c) = child {
                update_model(&c, file_name, thumbnails, file_path);
                child = c.next_sibling();
            }
        }

        while let Some((file_path, file_name, thumbnails)) = rx.recv().await {
            if cancel_loop.is_cancelled() {
                break;
            }
            let Some(dlg) = dialog_weak.upgrade() else {
                break;
            };
            refresh_widgets(dlg.upcast_ref(), &file_name, &thumbnails, &file_path);
            update_model(dlg.upcast_ref(), &file_name, &thumbnails, &file_path);
        }
    });

    let watcher = Arc::new(ViewWatcher {
        attached_views: Mutex::new(HashSet::new()),
        attached_models: Mutex::new(HashSet::new()),
        attached_stacks: Mutex::new(HashSet::new()),
        cancel: cancel.clone(),
        tx,
    });

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
    fn executable_lookup_requires_an_executable_file() {
        let dir = std::env::temp_dir().join(format!("xdp-thumb-exec-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let plain = dir.join("plain-thumbnailer");
        fs::write(&plain, b"script").unwrap();
        let executable = dir.join("exec-thumbnailer");
        fs::write(&executable, b"script").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(!is_executable_or_in_path(plain.to_str().unwrap()));
        assert!(is_executable_or_in_path(executable.to_str().unwrap()));
        assert!(!is_executable_or_in_path(dir.to_str().unwrap()));

        // The same check applies to bare command names resolved through PATH.
        let path = dir.as_os_str();
        assert!(!is_executable_in_path_dirs("plain-thumbnailer", Some(path)));
        assert!(is_executable_in_path_dirs("exec-thumbnailer", Some(path)));
        assert!(!is_executable_in_path_dirs("exec-thumbnailer", None));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn thumbnail_cancel_is_one_arc_pointer() {
        // Clone-on-write token: a single Arc pointer, so cloning stays cheap.
        assert_eq!(std::mem::size_of::<ThumbnailCancel>(), 8);
    }

    #[test]
    fn thumbnail_size_is_two_bytes() {
        // Fieldless enum with a 256 discriminant already uses u16 as its default
        // representation; repr(u8) is impossible here (256 does not fit).
        assert_eq!(std::mem::size_of::<ThumbnailSize>(), 2);
    }

    #[test]
    fn needs_gen_matches_independent_validity_decisions() {
        // For each validity combination the derived work list must be exactly
        // the invalid tiers, in generation order.
        for normal_ok in [false, true] {
            for large_ok in [false, true] {
                let (count, sizes) = sizes_needing_generation(normal_ok, large_ok);
                let derived = &sizes[..count];
                let expected: Vec<ThumbnailSize> = ALL_SIZES
                    .into_iter()
                    .filter(|size| match size {
                        ThumbnailSize::Normal => !normal_ok,
                        ThumbnailSize::Large => !large_ok,
                    })
                    .collect();
                assert_eq!(
                    derived,
                    expected.as_slice(),
                    "normal_ok={normal_ok}, large_ok={large_ok}"
                );
            }
        }
    }

    #[test]
    fn escape_uri_brackets_encodes_both_kinds() {
        assert_eq!(
            escape_uri_brackets(String::from("file:///tmp/a[1].png")),
            "file:///tmp/a%5B1%5D.png"
        );
        assert_eq!(
            escape_uri_brackets(String::from("file:///tmp/open[.png")),
            "file:///tmp/open%5B.png"
        );
        assert_eq!(
            escape_uri_brackets(String::from("file:///tmp/close].png")),
            "file:///tmp/close%5D.png"
        );
    }

    #[test]
    fn file_to_uri_leaves_no_literal_brackets() {
        // gio's own bracket escaping is platform-dependent, but the result must
        // never contain a literal bracket: either gio encoded it or the helper did.
        for path in ["/tmp/shot[1].png", "/tmp/shot].png", "/tmp/shot1.png"] {
            let uri = file_to_uri(Path::new(path)).unwrap();
            assert!(!uri.contains('[') && !uri.contains(']'), "{path} -> {uri}");
        }
    }

    #[test]
    fn escaping_brackets_is_allocation_free_when_none_present() {
        let plain = String::from("file:///tmp/no_brackets.png");
        let scope = crate::alloc_probe::AllocScope::start();
        let out = escape_uri_brackets(plain);
        let snap = scope.finish();
        std::hint::black_box(&out);
        assert_eq!(out, "file:///tmp/no_brackets.png");
        assert_eq!(
            snap.count, 0,
            "moving a bracket-free URI through must not allocate: {snap:?}"
        );

        // Positive half: the guarded branch really does encode, so the zero
        // above is not a vacuous pass because the helper became a no-op.
        let bracketed = String::from("file:///tmp/a[1].png");
        let scope = crate::alloc_probe::AllocScope::start();
        let out = escape_uri_brackets(bracketed);
        let snap = scope.finish();
        std::hint::black_box(&out);
        assert_eq!(out, "file:///tmp/a%5B1%5D.png");
        assert!(snap.count >= 2, "encoding must allocate: {snap:?}");
    }

    #[test]
    fn file_to_uri_skips_the_two_replace_allocations_without_brackets() {
        let path = Path::new("/tmp/plain_path.png");
        let _ = file_to_uri(path); // warm up gio's lazy state

        // Baseline: the same gio work plus two unconditional replacements, so
        // the measured difference is only the guard.
        let baseline_scope = crate::alloc_probe::AllocScope::start();
        let baseline = {
            let file = gio::File::for_path(path);
            let uri = file.uri().to_string();
            if uri.starts_with("file://") {
                Some(uri.replace('[', "%5B").replace(']', "%5D"))
            } else {
                None
            }
        };
        let baseline_snap = baseline_scope.finish();

        let optimized_scope = crate::alloc_probe::AllocScope::start();
        let optimized = file_to_uri(path);
        let optimized_snap = optimized_scope.finish();

        std::hint::black_box((&baseline, &optimized));
        assert_eq!(baseline, optimized);
        assert_eq!(
            baseline_snap.count,
            optimized_snap.count + 2,
            "the guard must skip both replace() allocations: \
             baseline={baseline_snap:?} optimized={optimized_snap:?}"
        );
    }

    /// The original bitwise CRC, kept as an oracle for the table implementation.
    fn png_crc32_bitwise(data: &[u8]) -> u32 {
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

    #[test]
    fn png_crc32_table_matches_bitwise_reference() {
        // Known CRC of the "IHDR" chunk type, then deterministic slices of many
        // lengths (including empty and long inputs) compared to the bitwise
        // reference above.
        assert_eq!(png_crc32(b"IHDR"), 0xa8a1_ae0a);
        let mut data = Vec::with_capacity(1024);
        let mut state = 0x1234_5678u32;
        for _ in 0..1024 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            data.push((state >> 24) as u8);
        }
        for len in 0..=data.len() {
            assert_eq!(
                png_crc32(&data[..len]),
                png_crc32_bitwise(&data[..len]),
                "length {len}"
            );
        }
    }

    #[test]
    fn png_crc32_does_not_allocate() {
        let data = b"IHDR\x00\x00\x00\x0dpayload";
        let warm = png_crc32(data);
        let scope = crate::alloc_probe::AllocScope::start();
        let crc = png_crc32(data);
        let snap = scope.finish();
        std::hint::black_box((warm, crc));
        assert_eq!(snap.count, 0, "table CRC must not allocate: {snap:?}");
    }

    #[test]
    fn inject_png_metadata_uses_one_exact_capacity_buffer() {
        // Build a minimal valid PNG prefix outside the measured scope.
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        png.extend_from_slice(&[0x1f, 0x15, 0xc4, 0x89]);
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&[0xae, 0x42, 0x60, 0x82]);

        let uri = "file:///test/path.jpg";
        let scope = crate::alloc_probe::AllocScope::start();
        let injected = inject_png_metadata(&png, uri, 123_456_789, 4096).unwrap();
        let snap = scope.finish();
        std::hint::black_box(&injected);

        assert!(is_png_data_metadata_valid(&injected, uri, 123_456_789));
        assert_eq!(
            injected.capacity(),
            injected.len(),
            "the result must be allocated once at its exact final size: \
             capacity={} len={}",
            injected.capacity(),
            injected.len()
        );
        assert!(
            snap.count <= 3,
            "one result buffer plus two numeric strings, not a Vec per chunk: {snap:?}"
        );
    }

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
    fn cancelled_external_thumbnailer_is_not_spawned_or_is_killed() {
        let test_dir = std::env::temp_dir().join("portal_test_cancelled_external");
        let _ = fs::create_dir_all(&test_dir);
        let marker = test_dir.join("spawned");
        let _ = fs::remove_file(&marker);

        // An already-cancelled token must stop the command before spawn.
        let cancel = ThumbnailCancel::new();
        cancel.cancel();
        let err = execute_thumbnailer_command(
            "touch %o",
            Path::new("/nonexistent/input.png"),
            "file:///nonexistent/input.png",
            &marker,
            128,
            &cancel,
        )
        .expect_err("cancelled work must not run");
        assert_eq!(err, CANCELLED);
        assert!(
            !marker.exists(),
            "cancelled work must not spawn the command"
        );

        // Cancellation while the command is running must kill it promptly.
        let cancel = ThumbnailCancel::new();
        let cancel_later = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel_later.cancel();
        });
        let started = std::time::Instant::now();
        let err = execute_thumbnailer_command(
            "sleep 30",
            Path::new("/nonexistent/input.png"),
            "file:///nonexistent/input.png",
            Path::new("/nonexistent/output.png"),
            128,
            &cancel,
        )
        .expect_err("cancelled work must be interrupted");
        let elapsed = started.elapsed();
        assert_eq!(err, CANCELLED);
        assert!(
            elapsed < Duration::from_secs(5),
            "cancelled thumbnailer should be killed promptly, took {elapsed:?}"
        );
        canceller.join().unwrap();

        let _ = fs::remove_dir_all(&test_dir);
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

    #[test]
    fn cancelled_temp_cleanup_removes_file_and_returns_cancelled() {
        let test_dir = std::env::temp_dir().join("portal_test_temp_cleanup");
        let _ = fs::create_dir_all(&test_dir);
        let temp = test_dir.join("stale-thumbnailer-output.png");
        fs::write(&temp, b"not really a png").unwrap();
        assert!(temp.exists());

        // The external thumbnailers cannot be driven in-process deterministically
        // (the cancellation check sits in a narrow race after the child exits),
        // so the shared cleanup helper is exercised directly instead.
        let err = cancelled_after_temp_cleanup(&temp);
        assert_eq!(err, CANCELLED, "cancellation value must be unchanged");
        assert!(
            !temp.exists(),
            "temp output must be removed on the cancel path"
        );

        // A second cleanup must stay harmless: several branches still have an
        // unconditional `let _ = fs::remove_file(&temp_output);` on exit.
        assert_eq!(cancelled_after_temp_cleanup(&temp), CANCELLED);
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn generated_thumbnails_keep_both_tiers_distinct() {
        let test_dir = std::env::temp_dir().join("portal_test_both_tiers");
        let _ = fs::create_dir_all(&test_dir);
        let src = test_dir.join("source.png");
        let minimal_png: [u8; 67] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        fs::write(&src, minimal_png).unwrap();
        let uri = file_to_uri(&src).unwrap();

        let normal = test_dir.join("tier-normal.png");
        let large = test_dir.join("tier-large.png");
        save_thumbnail_file(&minimal_png, &uri, &src, &normal).unwrap();
        save_thumbnail_file(&minimal_png, &uri, &src, &large).unwrap();

        let both = valid_generated_thumbnails(Some(&normal), Some(&large), &src, &uri);
        assert_eq!(both.normal.as_deref(), Some(normal.as_path()));
        assert_eq!(both.large.as_deref(), Some(large.as_path()));
        assert_ne!(
            both.normal, both.large,
            "thumbnail::path and thumbnail::path-large must not alias one file"
        );
        assert_eq!(both.preferred().unwrap(), &normal);

        // When only one tier was produced, leave the other key unset rather
        // than pointing it at the surviving file.
        let normal_only = valid_generated_thumbnails(Some(&normal), None, &src, &uri);
        assert_eq!(normal_only.normal.as_deref(), Some(normal.as_path()));
        assert!(normal_only.large.is_none());
        assert_eq!(normal_only.preferred().unwrap(), &normal);

        let large_only = valid_generated_thumbnails(None, Some(&large), &src, &uri);
        assert!(large_only.normal.is_none());
        assert_eq!(large_only.large.as_deref(), Some(large.as_path()));
        assert_eq!(large_only.preferred().unwrap(), &large);

        let _ = fs::remove_dir_all(&test_dir);
    }

    /// The registry is an index sorted by (mime, discovery order). Duplicate
    /// MIME keys must resolve to the first-discovered spec, and an unmatched
    /// MIME must fall back to its `category/*` spec.
    #[test]
    fn registry_lookup_keeps_first_discovery_and_wildcard_fallback() {
        fn spec(name: &str, mime: &str) -> ThumbnailerSpec {
            ThumbnailerSpec {
                name: name.into(),
                exec: "true".into(),
                try_exec: None,
                mime_types: vec![mime.into()].into_boxed_slice(),
            }
        }

        let specs = vec![
            spec("first", "image/png"),
            spec("second", "image/png"),
            spec("wild", "image/*"),
        ]
        .into_boxed_slice();
        // Deliberately unsorted: the test must pin the sort and its discovery
        // order tie-break, not a hand-sorted index. "image/*" sorts before
        // "image/png" because '*' < 'p'.
        let mut index = vec![
            ("image/png".into(), 1usize),
            ("image/png".into(), 0usize),
            ("image/*".into(), 2usize),
        ];
        sort_registry_index(&mut index);
        let registry = ThumbnailerRegistry {
            index: index.into_boxed_slice(),
            specs,
        };

        assert_eq!(
            &*registry.find_thumbnailer("image/png").unwrap().name,
            "first",
            "duplicate MIME entries must resolve to the first discovered spec"
        );
        assert_eq!(
            &*registry.find_thumbnailer("image/jpeg").unwrap().name,
            "wild",
            "an unmatched MIME must use the category wildcard"
        );
        assert!(registry.find_thumbnailer("text/plain").is_none());
    }

    /// Pins the actual attribute writes, not just the GeneratedThumbnails values:
    /// each tier must publish its own path.
    #[test]
    fn thumbnail_attributes_use_each_tiers_own_path() {
        let normal = PathBuf::from("/cache/normal.png");
        let large = PathBuf::from("/cache/large.png");

        let info = gio::FileInfo::new();
        // Pre-set the two stale-state attributes so their removal is observable.
        info.set_attribute_boolean("filechooser::queried", true);
        info.set_attribute_string("standard::icon", "image-missing");
        apply_thumbnail_attributes(
            &info,
            &GeneratedThumbnails {
                normal: Some(normal.clone()),
                large: Some(large.clone()),
            },
        );
        assert_eq!(
            info.attribute_byte_string("thumbnail::path").as_deref(),
            Some(normal.to_str().unwrap())
        );
        assert_eq!(
            info.attribute_byte_string("thumbnail::path-large")
                .as_deref(),
            Some(large.to_str().unwrap())
        );
        assert_ne!(
            info.attribute_byte_string("thumbnail::path"),
            info.attribute_byte_string("thumbnail::path-large"),
            "the two tiers must never alias one file"
        );
        assert!(info.boolean("thumbnail::is-valid"));
        assert!(info.boolean("thumbnail::is-valid-large"));
        assert!(
            !info.has_attribute("filechooser::queried"),
            "the stale queried flag must be cleared"
        );
        assert!(
            !info.has_attribute("standard::icon"),
            "the stale generic icon must be cleared"
        );

        let info = gio::FileInfo::new();
        apply_thumbnail_attributes(
            &info,
            &GeneratedThumbnails {
                normal: Some(normal),
                large: None,
            },
        );
        assert!(
            info.attribute_byte_string("thumbnail::path-large")
                .is_none(),
            "a tier that was not produced must not publish a path"
        );
        assert!(!info.boolean("thumbnail::is-valid-large"));
    }
}

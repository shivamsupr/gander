//! Local-path validation, content hashing, and media-kind sniffing. NO download.
//!
//! The path is agent-supplied — hostile until proven otherwise. No URL branch, no
//! redirect/size-cap handling. We resolve symlinks/`..`, reject URLs/NUL/non-regular
//! files and paths outside `--allowed-root`, then hash the bytes (the cache key).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::envelope::MediaKind;
use crate::ffmpeg::ProbeInfo;

/// Path validation / probe failure → a clean `failed` envelope upstream.
#[derive(Debug, Clone)]
pub struct SourceError {
    pub message: String,
    pub error_class: String,
}

impl SourceError {
    fn new(message: impl Into<String>, error_class: &str) -> SourceError {
        SourceError {
            message: message.into(),
            error_class: error_class.to_string(),
        }
    }
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A validated, probed source file.
pub struct Source {
    pub path: PathBuf,
    pub parent: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
    pub kind: MediaKind,
    pub probe: ProbeInfo,
}

fn is_url(raw: &str) -> bool {
    // `^[a-zA-Z][a-zA-Z0-9+.-]*://` — http://, https://, file://, s3://, …
    let bytes = raw.as_bytes();
    let Some(&first) = bytes.first() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    let mut i = 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-') {
            i += 1;
        } else {
            break;
        }
    }
    raw[i..].starts_with("://")
}

/// Resolve + validate a local path. NO network. Returns `SourceError` on any issue.
pub fn validate_path(
    raw: &str,
    allowed_root: Option<&Path>,
    max_size_bytes: Option<u64>,
) -> Result<PathBuf, SourceError> {
    if raw.trim().is_empty() {
        return Err(SourceError::new("empty path", "input"));
    }
    if raw.contains('\0') {
        return Err(SourceError::new("path contains NUL byte", "input"));
    }
    if is_url(raw) {
        return Err(SourceError::new(
            format!(
                "URL input is not supported in v1: {raw:?}. \
                 Fetch a local file first."
            ),
            "is_url",
        ));
    }

    let expanded = expanduser(raw);
    // canonicalize resolves symlinks + `..`; fails if the path doesn't exist.
    let p = match fs::canonicalize(&expanded) {
        Ok(p) => p,
        Err(_) => {
            return Err(SourceError::new(
                format!("no such file: {}", expanded.display()),
                "input",
            ));
        }
    };

    let meta = match fs::symlink_metadata(&p) {
        Ok(m) => m,
        Err(_) => {
            return Err(SourceError::new(
                format!("no such file: {}", p.display()),
                "input",
            ))
        }
    };
    if !meta.is_file() {
        // rejects dirs, fifos, sockets, /dev/* (and symlinks, though canonicalize
        // already followed them).
        return Err(SourceError::new(
            format!("not a regular file: {}", p.display()),
            "not_a_file",
        ));
    }
    if !is_readable(&p) {
        return Err(SourceError::new(
            format!("not readable: {}", p.display()),
            "unreadable",
        ));
    }
    if let Some(root) = allowed_root {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if !is_within(&p, &root) {
            return Err(SourceError::new(
                format!(
                    "path {} is outside allowed root {}",
                    p.display(),
                    root.display()
                ),
                "outside_root",
            ));
        }
    }
    let sz = meta.len();
    if sz == 0 {
        return Err(SourceError::new(
            format!("empty file (0 bytes): {}", p.display()),
            "input",
        ));
    }
    if let Some(limit) = max_size_bytes {
        if sz > limit {
            return Err(SourceError::new(
                format!("file too large: {sz} bytes > limit {limit}"),
                "input",
            ));
        }
    }
    Ok(p)
}

fn is_within(child: &Path, root: &Path) -> bool {
    child.starts_with(root)
}

#[cfg(unix)]
fn is_readable(p: &Path) -> bool {
    nix::unistd::access(p, nix::unistd::AccessFlags::R_OK).is_ok()
}

#[cfg(not(unix))]
fn is_readable(p: &Path) -> bool {
    std::fs::File::open(p).is_ok()
}

/// Streamed sha256 over file BYTES — the only thing that determines cache identity.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Identity for a SET of files: sha256 over the member hashes in call order.
/// Order is part of it — the labels the model answers about follow that order.
pub fn sha256_of_hashes(hashes: &[String]) -> String {
    let mut hasher = Sha256::new();
    for h in hashes {
        hasher.update(h.as_bytes());
        hasher.update(b"\n");
    }
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// --------------------------------------------------------------------------- //
// Kind sniffing
// --------------------------------------------------------------------------- //
const IMAGE_EXT: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif",
];
const AUDIO_EXT: &[&str] = &[
    "mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "oga", "wma", "aiff", "aif",
];
const VIDEO_EXT: &[&str] = &[
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "flv", "wmv", "mpg", "mpeg", "3gp", "ts",
];
/// ffprobe codec_names that are still images even though carried in a "video" stream.
const STILL_VIDEO_CODECS: &[&str] = &[
    "mjpeg", "png", "bmp", "gif", "webp", "tiff", "ppm", "pgm", "apng", "svg",
];

fn ext_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default()
}

/// Decide IMAGE/VIDEO/AUDIO from ffprobe streams, with extension as tiebreak.
pub fn sniff_kind(path: &Path, probe: &ProbeInfo) -> Result<MediaKind, SourceError> {
    let ext = ext_lower(path);
    let has_v = probe.has_video_stream;
    let has_a = probe.has_audio_stream;
    let vcodec = probe.video_codec.as_deref();

    // A single still frame encoded as mjpeg/png is an IMAGE, not a 1-frame video.
    let is_still = has_v
        && vcodec
            .map(|c| STILL_VIDEO_CODECS.contains(&c))
            .unwrap_or(false)
        && !has_a
        && (matches!(probe.nb_video_frames, None | Some(0) | Some(1))
            || matches!(probe.duration, None | Some(0.0)));

    if has_v && !is_still {
        return Ok(MediaKind::Video);
    }
    if is_still || (!has_v && !has_a && IMAGE_EXT.contains(&ext.as_str())) {
        return Ok(MediaKind::Image);
    }
    if has_a && !has_v {
        return Ok(MediaKind::Audio);
    }
    // ffprobe failed / ambiguous → fall back to extension.
    if VIDEO_EXT.contains(&ext.as_str()) {
        return Ok(MediaKind::Video);
    }
    if AUDIO_EXT.contains(&ext.as_str()) {
        return Ok(MediaKind::Audio);
    }
    if IMAGE_EXT.contains(&ext.as_str()) {
        return Ok(MediaKind::Image);
    }
    Err(SourceError::new(
        format!(
            "cannot determine media kind for {} (ext={ext:?}, has_video={has_v}, has_audio={has_a})",
            path.display()
        ),
        "bad_kind",
    ))
}

fn expanduser(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::ProbeInfo;
    use std::io::Write;

    fn tmp_file(bytes: &[u8], ext: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(format!("f.{ext}"));
        std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        (dir, p)
    }

    #[test]
    fn empty_and_nul_rejected() {
        assert!(validate_path("", None, None).is_err());
        assert!(validate_path("a\0b", None, None).is_err());
    }

    #[test]
    fn url_rejected() {
        let e = validate_path("https://example.com/a.jpg", None, None).unwrap_err();
        assert_eq!(e.error_class, "is_url");
        assert_eq!(
            validate_path("s3://b/k", None, None)
                .unwrap_err()
                .error_class,
            "is_url"
        );
    }

    #[test]
    fn missing_file_rejected() {
        assert!(validate_path("/no/such/nope.jpg", None, None).is_err());
    }

    #[test]
    fn directory_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let e = validate_path(dir.path().to_str().unwrap(), None, None).unwrap_err();
        assert_eq!(e.error_class, "not_a_file");
    }

    #[test]
    fn empty_file_rejected() {
        let (_d, p) = tmp_file(b"", "jpg");
        assert!(validate_path(p.to_str().unwrap(), None, None).is_err());
    }

    #[test]
    fn outside_allowed_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let (_d2, f) = tmp_file(b"x", "jpg");
        let e = validate_path(f.to_str().unwrap(), Some(&root), None).unwrap_err();
        assert_eq!(e.error_class, "outside_root");
    }

    #[test]
    fn within_allowed_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let f = root.join("inside.jpg");
        std::fs::write(&f, b"x").unwrap();
        let got = validate_path(f.to_str().unwrap(), Some(&root), None).unwrap();
        assert_eq!(got, std::fs::canonicalize(&f).unwrap());
    }

    #[test]
    fn max_size_enforced() {
        let (_d, p) = tmp_file(b"hello world", "bin");
        assert!(validate_path(p.to_str().unwrap(), None, Some(3)).is_err());
        assert!(validate_path(p.to_str().unwrap(), None, Some(100)).is_ok());
    }

    #[test]
    fn sha256_identical_bytes_two_paths() {
        let bytes = b"hello world".repeat(1000);
        let (_a, pa) = tmp_file(&bytes, "bin");
        let (_b, pb) = tmp_file(&bytes, "bin");
        assert_eq!(sha256_file(&pa).unwrap(), sha256_file(&pb).unwrap());
        // known vector
        let (_c, pc) = tmp_file(b"abc", "bin");
        assert_eq!(
            sha256_file(&pc).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn probe() -> ProbeInfo {
        ProbeInfo::default()
    }

    #[test]
    fn sniff_video() {
        let mut p = probe();
        p.has_video_stream = true;
        p.video_codec = Some("h264".into());
        p.has_audio_stream = true;
        p.duration = Some(14.6);
        assert_eq!(
            sniff_kind(Path::new("c.mp4"), &p).unwrap(),
            MediaKind::Video
        );
    }

    #[test]
    fn sniff_still_image_as_image() {
        let mut p = probe();
        p.has_video_stream = true;
        p.video_codec = Some("mjpeg".into());
        p.nb_video_frames = Some(1);
        assert_eq!(
            sniff_kind(Path::new("a.jpg"), &p).unwrap(),
            MediaKind::Image
        );
    }

    #[test]
    fn sniff_audio() {
        let mut p = probe();
        p.has_audio_stream = true;
        p.audio_codec = Some("mp3".into());
        assert_eq!(
            sniff_kind(Path::new("a.mp3"), &p).unwrap(),
            MediaKind::Audio
        );
    }

    #[test]
    fn sniff_by_extension_when_probe_empty() {
        let p = probe();
        assert_eq!(
            sniff_kind(Path::new("x.png"), &p).unwrap(),
            MediaKind::Image
        );
        assert_eq!(
            sniff_kind(Path::new("x.mp4"), &p).unwrap(),
            MediaKind::Video
        );
        assert!(sniff_kind(Path::new("x.unknownext"), &p).is_err());
    }
}

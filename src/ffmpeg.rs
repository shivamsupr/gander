//! ffprobe wrapper + extraction helpers (probe / frames / audio / segment / sanitize).
//! Plain subprocess wrappers — no PTY needed (only agy needs one).
//!
//! ffprobe/ffmpeg are plain subprocesses — they behave on pipes, no PTY needed.
//! Nothing here mutates the source; derived files go to a per-call temp work dir.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::config::{FRAME_LONG_EDGE, JPEG_QV};

/// ffprobe-derived facts about a media file.
#[derive(Debug, Clone, Default)]
pub struct ProbeInfo {
    pub ok: bool,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub src_fps: Option<f64>,
    pub nb_video_frames: Option<i64>,
    pub has_video_stream: bool,
    pub has_audio_stream: bool,
    pub container: Option<String>,
    pub creation_time: Option<String>,
}

/// `ffprobe -show_format -show_streams -print_format json`. Returns an empty
/// (`ok=false`) probe on any failure — never errors.
pub fn ffprobe(path: &Path, ffprobe_bin: &str) -> ProbeInfo {
    let out = Command::new(ffprobe_bin)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output();

    let stdout = match out {
        Ok(o) if !o.stdout.is_empty() => o.stdout,
        _ => return ProbeInfo::default(),
    };
    let data: Value = match serde_json::from_slice(&stdout) {
        Ok(v) => v,
        Err(_) => return ProbeInfo::default(),
    };

    let streams = data.get("streams").and_then(Value::as_array);
    let fmt = data.get("format");
    let v = streams.and_then(|ss| {
        ss.iter()
            .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))
    });
    let a = streams.and_then(|ss| {
        ss.iter()
            .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio"))
    });

    let duration = fmt
        .and_then(|f| str_f64(f.get("duration")))
        .or_else(|| v.and_then(|v| str_f64(v.get("duration"))));

    ProbeInfo {
        ok: true,
        duration,
        width: v.and_then(|v| val_u32(v.get("width"))),
        height: v.and_then(|v| val_u32(v.get("height"))),
        video_codec: v.and_then(|v| {
            v.get("codec_name")
                .and_then(Value::as_str)
                .map(String::from)
        }),
        audio_codec: a.and_then(|a| {
            a.get("codec_name")
                .and_then(Value::as_str)
                .map(String::from)
        }),
        src_fps: v.and_then(|v| parse_rate(v.get("avg_frame_rate").and_then(Value::as_str))),
        nb_video_frames: v.and_then(|v| val_i64(v.get("nb_frames"))),
        has_video_stream: v.is_some(),
        has_audio_stream: a.is_some(),
        container: fmt.and_then(|f| {
            f.get("format_name")
                .and_then(Value::as_str)
                .map(String::from)
        }),
        creation_time: fmt
            .and_then(|f| f.get("tags"))
            .and_then(|t| t.get("creation_time"))
            .and_then(Value::as_str)
            .map(String::from),
    }
}

/// `avg_frame_rate` is `num/den` e.g. `30/1`, `30000/1001`. `0/0` → None.
fn parse_rate(r: Option<&str>) -> Option<f64> {
    let r = r?;
    let (num, den) = r.split_once('/')?;
    let n: f64 = num.trim().parse().ok()?;
    let d: f64 = den.trim().parse().ok()?;
    if d == 0.0 {
        None
    } else {
        Some(n / d)
    }
}

/// ffprobe emits numbers as JSON strings; accept either string or number.
fn str_f64(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

fn val_u32(v: Option<&Value>) -> Option<u32> {
    match v {
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(Value::Number(n)) => n.as_u64().map(|x| x as u32),
        _ => None,
    }
}

fn val_i64(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

// --------------------------------------------------------------------------- //
// Frame sampling
// --------------------------------------------------------------------------- //
/// N centers of N equal slices — avoids the exact 0.0 and duration endpoints.
pub fn even_timestamps(duration: f64, n: u32) -> Vec<f64> {
    let n = n.max(1);
    (0..n)
        .map(|i| {
            let t = duration * (i as f64 + 0.5) / n as f64;
            (t * 1000.0).round() / 1000.0
        })
        .collect()
}

/// Returns `(frame_count, mode)`, mode `"even"|"fps"`. Whichever yields fewer wins.
pub fn plan_frame_count(
    duration: f64,
    src_fps: Option<f64>,
    max_frames: u32,
    fps_override: Option<f64>,
) -> (u32, &'static str) {
    if let Some(rate) = fps_override {
        if rate > 0.0 {
            let want = (duration * rate).floor().max(1.0) as u32;
            return (want.min(max_frames), "fps");
        }
    }
    let src_total = match (duration, src_fps) {
        (d, Some(f)) if d > 0.0 && f > 0.0 => (d * f) as u32,
        _ => max_frames,
    };
    let src_total = if src_total == 0 {
        max_frames
    } else {
        src_total
    };
    (max_frames.min(src_total).max(1), "even")
}

/// N evenly-spaced JPEGs via per-frame input-seek (`-ss` before `-i`).
pub fn extract_frames_even(
    src: &Path,
    out_dir: &Path,
    duration: f64,
    n: u32,
    ffmpeg_bin: &str,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for (i, t) in even_timestamps(duration, n).into_iter().enumerate() {
        let out = out_dir.join(format!("frame_{i:03}.jpg"));
        let _ = Command::new(ffmpeg_bin)
            .args(["-v", "error", "-y", "-ss", &format!("{t:.3}"), "-i"])
            .arg(src)
            .args([
                "-frames:v",
                "1",
                "-vf",
                &format!("scale='min({FRAME_LONG_EDGE},iw)':-2,setsar=1"),
                "-map_metadata",
                "-1",
                "-q:v",
                &JPEG_QV.to_string(),
            ])
            .arg(&out)
            .output();
        if nonempty(&out) {
            paths.push(out);
        }
    }
    paths
}

/// Fixed-rate sampling via the `fps=` filter, capped by `-frames:v cap`.
pub fn extract_frames_fps(
    src: &Path,
    out_dir: &Path,
    rate: f64,
    cap: u32,
    ffmpeg_bin: &str,
) -> Vec<PathBuf> {
    let _ = Command::new(ffmpeg_bin)
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args([
            "-vf",
            &format!("fps={rate},scale='min({FRAME_LONG_EDGE},iw)':-2,setsar=1"),
            "-frames:v",
            &cap.to_string(),
            "-an",
            "-map_metadata",
            "-1",
            "-q:v",
            &JPEG_QV.to_string(),
        ])
        .arg(out_dir.join("frame_%03d.jpg"))
        .output();
    glob_sorted(out_dir, "frame_", ".jpg")
}

/// Extract audio to mono 16k WAV. Returns None if no audio came out.
pub fn extract_audio(src: &Path, out_dir: &Path, ffmpeg_bin: &str) -> Option<PathBuf> {
    let out = out_dir.join("audio.wav");
    let _ = Command::new(ffmpeg_bin)
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-map_metadata",
            "-1",
        ])
        .arg(&out)
        .output();
    nonempty(&out).then_some(out)
}

/// Stream-copy segmentation (no re-encode).
pub fn segment_video(
    src: &Path,
    out_dir: &Path,
    chunk_len_s: f64,
    ffmpeg_bin: &str,
) -> Vec<PathBuf> {
    let seg = (chunk_len_s.round() as i64).to_string();
    let _ = Command::new(ffmpeg_bin)
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-f",
            "segment",
            "-segment_time",
            &seg,
            "-reset_timestamps",
            "1",
        ])
        .arg(out_dir.join("chunk_%03d.mp4"))
        .output();
    glob_sorted(out_dir, "chunk_", ".mp4")
}

/// Re-encode an image to a sanitized temp copy (no EXIF/GPS). None if it fails.
pub fn sanitize_image(src: &Path, out_dir: &Path, ffmpeg_bin: &str) -> Option<PathBuf> {
    let out = out_dir.join("image.jpg");
    let _ = Command::new(ffmpeg_bin)
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args([
            "-map_metadata",
            "-1",
            "-vf",
            &format!("scale='min({FRAME_LONG_EDGE},iw)':-2,setsar=1"),
            "-q:v",
            &JPEG_QV.to_string(),
        ])
        .arg(&out)
        .output();
    nonempty(&out).then_some(out)
}

fn nonempty(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
}

fn glob_sorted(dir: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(prefix) && n.ends_with(suffix))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

#[allow(dead_code)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

// --------------------------------------------------------------------------- //
// Per-call temp work dir
// --------------------------------------------------------------------------- //
/// RAII temp work dir. On drop, removes the tree unless `keep` is set (in which
/// case the path is logged to stderr).
pub struct WorkDir {
    path: PathBuf,
    keep: bool,
    _temp: Option<tempfile::TempDir>,
}

impl WorkDir {
    pub fn new(keep: bool, parent: Option<&Path>) -> std::io::Result<WorkDir> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("gander-work-");
        let temp = match parent {
            Some(p) => builder.tempdir_in(p)?,
            None => builder.tempdir()?,
        };
        let path = temp.path().to_path_buf();
        if keep {
            // Leak the TempDir so its Drop doesn't delete the tree.
            let kept = temp.keep();
            Ok(WorkDir {
                path: kept,
                keep,
                _temp: None,
            })
        } else {
            Ok(WorkDir {
                path,
                keep,
                _temp: Some(temp),
            })
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if self.keep {
            eprintln!("[gander] kept temp dir: {}", self.path.display());
        }
        // Non-keep cleanup is handled by the inner TempDir's Drop.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_frame_count_cases() {
        assert_eq!(plan_frame_count(14.6, Some(30.0), 12, None), (12, "even"));
        assert_eq!(
            plan_frame_count(14.6, Some(30.0), 12, Some(1.0)),
            (12, "fps")
        );
        // fps yields fewer -> wins
        assert_eq!(plan_frame_count(10.0, Some(30.0), 12, Some(0.5)).0, 5);
        // cap wins when fps would exceed
        assert_eq!(plan_frame_count(60.0, Some(30.0), 8, Some(5.0)).0, 8);
    }

    #[test]
    fn even_timestamps_are_slice_centers() {
        let ts = even_timestamps(60.0, 3);
        assert_eq!(ts, vec![10.0, 30.0, 50.0]);
        assert_eq!(even_timestamps(10.0, 1), vec![5.0]);
    }

    #[test]
    fn parse_rate_handles_fractions() {
        assert_eq!(parse_rate(Some("30/1")), Some(30.0));
        assert_eq!(parse_rate(Some("0/0")), None);
        assert_eq!(parse_rate(Some("")), None);
        assert!((parse_rate(Some("30000/1001")).unwrap() - 29.97).abs() < 0.01);
    }
}

//! SQLite system of record. One file at `~/.gander/media.db`, one row per content
//! hash, idempotent, never poisoned by failures, invalidatable by a `SCHEMA_VERSION`
//! bump. No ORM, no daemon, no network.

use std::path::Path;

use rusqlite::{named_params, Connection, OptionalExtension, Row};

use crate::envelope::{
    BackendInfo, MediaMeta, MediaResult, Status, Structured, SCHEMA_VERSION, TOOL_VERSION,
};

const SCHEMA_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS media (
    content_sha256   TEXT    PRIMARY KEY NOT NULL,
    schema_version   TEXT    NOT NULL,
    status           TEXT    NOT NULL
                       CHECK (status IN ('ok','partial','failed')),
    media_kind       TEXT    NOT NULL
                       CHECK (media_kind IN ('image','video','audio','unknown')),
    summary          TEXT,
    rating           TEXT
                       CHECK (rating IN ('keep','review','cull') OR rating IS NULL),
    cull_reason      TEXT,
    language         TEXT,
    people_count     INTEGER,
    has_audio        INTEGER NOT NULL DEFAULT 0,
    has_transcript   INTEGER NOT NULL DEFAULT 0,
    duration_seconds REAL,
    width            INTEGER,
    height           INTEGER,
    audio_quality    TEXT,
    lighting         TEXT,
    time_of_day      TEXT,
    shot_type        TEXT,
    size_bytes       INTEGER,
    was_chunked      INTEGER NOT NULL DEFAULT 0,
    chunk_count      INTEGER NOT NULL DEFAULT 0,
    source_duration_seconds REAL,
    description      TEXT,
    transcript       TEXT,
    english_translation TEXT,
    structured_json  TEXT    NOT NULL DEFAULT '{}',
    media_json       TEXT    NOT NULL DEFAULT '{}',
    backend_json     TEXT    NOT NULL DEFAULT '{}',
    warnings_json    TEXT    NOT NULL DEFAULT '[]',
    parse_ok         INTEGER NOT NULL DEFAULT 0,
    error            TEXT,
    source_path      TEXT,
    source_filename  TEXT,
    model_used       TEXT,
    backend_used     TEXT,
    created_at       TEXT    NOT NULL,
    updated_at       TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_status     ON media(status);
CREATE INDEX IF NOT EXISTS idx_media_rating     ON media(rating);
CREATE INDEX IF NOT EXISTS idx_media_language   ON media(language);
CREATE INDEX IF NOT EXISTS idx_media_kind       ON media(media_kind);
CREATE INDEX IF NOT EXISTS idx_media_people     ON media(people_count);
CREATE INDEX IF NOT EXISTS idx_media_schema     ON media(schema_version);
CREATE INDEX IF NOT EXISTS idx_media_updated    ON media(updated_at);
CREATE INDEX IF NOT EXISTS idx_media_transcript ON media(has_transcript);
CREATE TABLE IF NOT EXISTS keywords (
    content_sha256   TEXT    NOT NULL
                       REFERENCES media(content_sha256) ON DELETE CASCADE,
    keyword          TEXT    NOT NULL,
    PRIMARY KEY (content_sha256, keyword)
);
CREATE INDEX IF NOT EXISTS idx_keywords_keyword ON keywords(keyword);
"#;

/// Open (creating dirs + file + schema on first use). One conn per process.
pub fn connect(db_path: &Path, busy_timeout_ms: i64) -> rusqlite::Result<Connection> {
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
        set_mode(parent, 0o700);
    }
    let fresh = !db_path.exists();
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(std::time::Duration::from_millis(busy_timeout_ms as u64))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA_DDL)?;
    if fresh {
        set_mode(db_path, 0o600);
    }
    Ok(conn)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

fn utcnow_iso() -> String {
    // RFC3339 to the second, UTC. Avoids pulling in chrono.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let (y, mo, d, h, mi, s) = civil_from_unix(now);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}+00:00")
}

/// Days→Y/M/D via Howard Hinnant's civil-from-days algorithm.
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s)
}

fn is_real_transcript(t: Option<&str>) -> bool {
    match t {
        None => false,
        Some(t) => {
            let t = t.trim();
            !t.is_empty() && t.to_lowercase() != "[no speech detected]"
        }
    }
}

// --------------------------------------------------------------------------- //
// Lookup (cache read)
// --------------------------------------------------------------------------- //
/// Content-hash cache read. `failed` rows never satisfy a hit; a stale
/// `schema_version` is a miss (lazy invalidation).
pub fn lookup(conn: &Connection, sha: &str) -> Option<MediaResult> {
    let r = conn
        .query_row(
            "SELECT * FROM media WHERE content_sha256 = ?1",
            [sha],
            row_to_result,
        )
        .optional()
        .ok()??;
    if r.status == Status::Failed || r.schema_version != SCHEMA_VERSION {
        return None;
    }
    Some(r)
}

/// Delete every cached row (keywords cascade). Returns the number of media rows removed.
pub fn clear_all(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM media", [])
}

/// Forget one asset by content hash (keywords cascade). Returns rows removed (0 or 1).
pub fn forget(conn: &Connection, sha: &str) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM media WHERE content_sha256 = ?1", [sha])
}

fn row_to_result(row: &Row) -> rusqlite::Result<MediaResult> {
    let status_s: String = row.get("status")?;
    let structured_json: String = row.get("structured_json")?;
    let media_json: String = row.get("media_json")?;
    let warnings_json: String = row.get("warnings_json")?;

    let structured: Structured = serde_json::from_str(&structured_json).unwrap_or_default();
    let media: MediaMeta = serde_json::from_str(&media_json).unwrap_or_default();
    let warnings: Vec<String> = serde_json::from_str(&warnings_json).unwrap_or_default();

    // A cache hit reports the cache as the backend (no model call happened).
    let backend = BackendInfo {
        model_used: String::new(),
        backend_used: "cache".into(),
        attempts: vec![],
    };

    Ok(MediaResult {
        status: Status::from_str(&status_s),
        content_sha256: row.get("content_sha256")?,
        media_kind: row.get("media_kind")?,
        error: row.get("error")?,
        error_class: None,
        warnings,
        parse_ok: row.get::<_, i64>("parse_ok")? != 0,
        cached: true,
        summary: row.get::<_, Option<String>>("summary")?.unwrap_or_default(),
        description: row
            .get::<_, Option<String>>("description")?
            .unwrap_or_default(),
        transcript: row.get("transcript")?,
        language: row.get("language")?,
        english_translation: row.get("english_translation")?,
        structured: Some(structured),
        media,
        backend,
        source_path: row.get("source_path")?,
        schema_version: row.get("schema_version")?,
        tool_version: TOOL_VERSION.to_string(),
    })
}

// --------------------------------------------------------------------------- //
// Upsert / delete
// --------------------------------------------------------------------------- //
const INSERT_SQL: &str = r#"
INSERT INTO media (
    content_sha256, schema_version, status, media_kind,
    summary, rating, cull_reason, language, people_count,
    has_audio, has_transcript, duration_seconds, width, height,
    audio_quality, lighting, time_of_day, shot_type, size_bytes,
    was_chunked, chunk_count, source_duration_seconds,
    description, transcript, english_translation,
    structured_json, media_json, backend_json, warnings_json, parse_ok,
    error, source_path, source_filename, model_used, backend_used,
    created_at, updated_at
) VALUES (
    :content_sha256, :schema_version, :status, :media_kind,
    :summary, :rating, :cull_reason, :language, :people_count,
    :has_audio, :has_transcript, :duration_seconds, :width, :height,
    :audio_quality, :lighting, :time_of_day, :shot_type, :size_bytes,
    :was_chunked, :chunk_count, :source_duration_seconds,
    :description, :transcript, :english_translation,
    :structured_json, :media_json, :backend_json, :warnings_json, :parse_ok,
    :error, :source_path, :source_filename, :model_used, :backend_used,
    :created_at, :updated_at
)
"#;

const ON_CONFLICT: &str = r#"
ON CONFLICT(content_sha256) DO UPDATE SET
  schema_version=excluded.schema_version, status=excluded.status,
  media_kind=excluded.media_kind, summary=excluded.summary,
  rating=excluded.rating, cull_reason=excluded.cull_reason,
  language=excluded.language, people_count=excluded.people_count,
  has_audio=excluded.has_audio, has_transcript=excluded.has_transcript,
  duration_seconds=excluded.duration_seconds, width=excluded.width,
  height=excluded.height, audio_quality=excluded.audio_quality,
  lighting=excluded.lighting, time_of_day=excluded.time_of_day,
  shot_type=excluded.shot_type, size_bytes=excluded.size_bytes,
  was_chunked=excluded.was_chunked, chunk_count=excluded.chunk_count,
  source_duration_seconds=excluded.source_duration_seconds,
  description=excluded.description, transcript=excluded.transcript,
  english_translation=excluded.english_translation,
  structured_json=excluded.structured_json, media_json=excluded.media_json,
  backend_json=excluded.backend_json, warnings_json=excluded.warnings_json,
  parse_ok=excluded.parse_ok, error=excluded.error,
  source_path=excluded.source_path, source_filename=excluded.source_filename,
  model_used=excluded.model_used, backend_used=excluded.backend_used,
  updated_at=excluded.updated_at
"#;

/// Idempotent write. `force` recomputes `created_at`; otherwise `ON CONFLICT` keeps it.
pub fn upsert(conn: &mut Connection, r: &MediaResult, force: bool) -> rusqlite::Result<()> {
    let now = utcnow_iso();
    let sha = r.content_sha256.clone();
    let s = r.structured.clone().unwrap_or_default();

    let structured_json = serde_json::to_string(&s).unwrap_or_else(|_| "{}".into());
    let media_json = serde_json::to_string(&r.media).unwrap_or_else(|_| "{}".into());
    let backend_json = serde_json::to_string(&r.backend).unwrap_or_else(|_| "{}".into());
    let warnings_json = serde_json::to_string(&r.warnings).unwrap_or_else(|_| "[]".into());
    let has_transcript = is_real_transcript(r.transcript.as_deref());
    let stored_transcript = if has_transcript {
        r.transcript.clone()
    } else {
        None
    };
    let source_filename = r.source_path.as_deref().and_then(|p| {
        Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    });

    let tx = conn.transaction()?;
    {
        let sql = if force {
            tx.execute("DELETE FROM media WHERE content_sha256 = ?1", [&sha])?;
            INSERT_SQL.to_string()
        } else {
            tx.execute("DELETE FROM keywords WHERE content_sha256 = ?1", [&sha])?;
            format!("{INSERT_SQL}{ON_CONFLICT}")
        };
        tx.execute(
            &sql,
            named_params! {
                ":content_sha256": sha,
                ":schema_version": r.schema_version,
                ":status": r.status.as_str(),
                ":media_kind": r.media_kind,
                ":summary": none_if_empty(&r.summary),
                ":rating": empty_to_none(&s.rating),
                ":cull_reason": none_if_empty(&s.cull_reason),
                ":language": r.language,
                ":people_count": s.people_count,
                ":has_audio": r.media.has_audio as i64,
                ":has_transcript": has_transcript as i64,
                ":duration_seconds": r.media.duration,
                ":width": r.media.width,
                ":height": r.media.height,
                ":audio_quality": s.audio_quality,
                ":lighting": s.lighting,
                ":time_of_day": s.time_of_day,
                ":shot_type": s.shot_type,
                ":size_bytes": r.media.size_bytes,
                ":was_chunked": r.media.chunked as i64,
                ":chunk_count": r.media.chunk_count,
                ":source_duration_seconds": r.media.duration,
                ":description": none_if_empty(&r.description),
                ":transcript": stored_transcript,
                ":english_translation": r.english_translation,
                ":structured_json": structured_json,
                ":media_json": media_json,
                ":backend_json": backend_json,
                ":warnings_json": warnings_json,
                ":parse_ok": r.parse_ok as i64,
                ":error": r.error,
                ":source_path": r.source_path,
                ":source_filename": source_filename,
                ":model_used": r.backend.model_used,
                ":backend_used": r.backend.backend_used,
                ":created_at": now,
                ":updated_at": now,
            },
        )?;
        insert_keywords(&tx, &sha, &s.keywords)?;
    }
    tx.commit()
}

fn insert_keywords(conn: &Connection, sha: &str, keywords: &[String]) -> rusqlite::Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    let mut stmt =
        conn.prepare("INSERT OR IGNORE INTO keywords (content_sha256, keyword) VALUES (?1, ?2)")?;
    for kw in keywords {
        let k = kw.trim().to_lowercase();
        if !k.is_empty() && seen.insert(k.clone()) {
            stmt.execute(rusqlite::params![sha, k])?;
        }
    }
    Ok(())
}

fn none_if_empty(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn empty_to_none(s: &str) -> Option<&str> {
    none_if_empty(s)
}

impl Status {
    pub fn from_str(s: &str) -> Status {
        match s {
            "ok" => Status::Ok,
            "partial" => Status::Partial,
            _ => Status::Failed,
        }
    }
}

// --------------------------------------------------------------------------- //
// Recall (read API) — used by the `recall` subcommand (M6).
// --------------------------------------------------------------------------- //
#[derive(Debug, Default, Clone)]
pub struct RecallFilters {
    pub keyword: Option<String>,
    pub text: Option<String>,
    pub rating: Option<String>,
    pub language: Option<String>,
    pub media_kind: Option<String>,
    pub min_people: Option<i64>,
    pub min_duration: Option<f64>,
    pub has_transcript: Option<bool>,
    pub has_audio: Option<bool>,
    pub chunked: Option<bool>,
    pub include_failed: bool,
    pub all_versions: bool,
    pub order_by: String,
    pub descending: bool,
    pub limit: i64,
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Read-only cache browse. Returns rows as JSON objects (the recall envelope).
pub fn recall(conn: &Connection, f: &RecallFilters) -> rusqlite::Result<Vec<serde_json::Value>> {
    let mut where_clauses: Vec<String> = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();

    if !f.include_failed {
        where_clauses.push("m.status IN ('ok','partial')".into());
    }
    if !f.all_versions {
        where_clauses.push("m.schema_version = ?".into());
        params.push(SCHEMA_VERSION.to_string().into());
    }
    if let Some(r) = &f.rating {
        where_clauses.push("m.rating = ?".into());
        params.push(r.clone().into());
    }
    if let Some(l) = &f.language {
        where_clauses.push("m.language = ?".into());
        params.push(l.clone().into());
    }
    if let Some(k) = &f.media_kind {
        where_clauses.push("m.media_kind = ?".into());
        params.push(k.clone().into());
    }
    if let Some(p) = f.min_people {
        where_clauses.push("m.people_count >= ?".into());
        params.push(p.into());
    }
    if let Some(d) = f.min_duration {
        where_clauses.push("m.duration_seconds >= ?".into());
        params.push(d.into());
    }
    match f.has_transcript {
        Some(true) => where_clauses.push("m.has_transcript = 1".into()),
        Some(false) => where_clauses.push("m.has_transcript = 0".into()),
        None => {}
    }
    match f.has_audio {
        Some(true) => where_clauses.push("m.has_audio = 1".into()),
        Some(false) => where_clauses.push("m.has_audio = 0".into()),
        None => {}
    }
    if f.chunked == Some(true) {
        where_clauses.push("m.was_chunked = 1".into());
    }
    if let Some(kw) = &f.keyword {
        where_clauses.push(
            "m.content_sha256 IN (SELECT content_sha256 FROM keywords WHERE keyword = ?)".into(),
        );
        params.push(kw.trim().to_lowercase().into());
    }
    if let Some(t) = &f.text {
        let like = format!("%{}%", escape_like(t));
        where_clauses.push(
            "(m.description LIKE ? ESCAPE '\\' OR m.summary LIKE ? ESCAPE '\\' OR m.transcript LIKE ? ESCAPE '\\')"
                .into(),
        );
        params.push(like.clone().into());
        params.push(like.clone().into());
        params.push(like.into());
    }

    let allowed = [
        "updated_at",
        "created_at",
        "rating",
        "people_count",
        "duration_seconds",
    ];
    let ob = if allowed.contains(&f.order_by.as_str()) {
        f.order_by.as_str()
    } else {
        "updated_at"
    };
    let direction = if f.descending { "DESC" } else { "ASC" };
    let limit = f.limit.clamp(1, 500);
    params.push(limit.into());

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };
    let sql = format!(
        "SELECT m.content_sha256, m.status, m.media_kind, m.rating, m.language,
                m.people_count, m.has_transcript, m.has_audio,
                m.duration_seconds, m.was_chunked, m.chunk_count,
                m.width, m.height, m.summary, m.description, m.transcript,
                m.source_filename, m.model_used, m.backend_used,
                m.created_at, m.updated_at
         FROM media m {where_sql}
         ORDER BY m.{ob} {direction}
         LIMIT ?"
    );

    let mut stmt = conn.prepare(&sql)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        let mut obj = serde_json::Map::new();
        for (i, name) in cols.iter().enumerate() {
            obj.insert(name.clone(), sql_to_json(row, i));
        }
        Ok(serde_json::Value::Object(obj))
    })?;
    rows.collect()
}

fn sql_to_json(row: &Row, i: usize) -> serde_json::Value {
    use rusqlite::types::ValueRef;
    match row.get_ref(i).unwrap_or(ValueRef::Null) {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(n) => serde_json::Value::from(n),
        ValueRef::Real(f) => serde_json::Value::from(f),
        ValueRef::Text(t) => serde_json::Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(_) => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Attempt, BackendInfo, MediaMeta, Status, Structured, Technical};

    fn temp_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = connect(&dir.path().join("media.db"), 5000).unwrap();
        (dir, conn)
    }

    fn sample() -> MediaResult {
        MediaResult {
            status: Status::Ok,
            content_sha256: "abc123".into(),
            media_kind: "video".into(),
            error: None,
            error_class: None,
            warnings: vec![],
            parse_ok: true,
            cached: false,
            summary: "A worker places a steel beam.".into(),
            description: "**Scene:** site".into(),
            transcript: Some("Esta es una prueba".into()),
            language: Some("es".into()),
            english_translation: Some("This is a test".into()),
            structured: Some(Structured {
                rating: "keep".into(),
                people_count: 1,
                keywords: vec!["steel-beam".into(), "Worker".into(), "steel-beam".into()],
                shot_type: "varies".into(),
                lighting: "varies".into(),
                audio_quality: "clear".into(),
                technical: Technical::default(),
                ..Structured::default()
            }),
            media: MediaMeta {
                duration: Some(312.4),
                width: Some(1280),
                height: Some(720),
                codec: Some("h264".into()),
                has_audio: true,
                size_bytes: Some(100),
                chunked: true,
                chunk_count: 6,
            },
            backend: BackendInfo {
                model_used: "Gemini 3.5 Flash (High)".into(),
                backend_used: "agy".into(),
                attempts: vec![Attempt {
                    backend: "agy".into(),
                    model: "Gemini 3.5 Flash (High)".into(),
                    ok: true,
                    error_class: None,
                    elapsed_s: 64.1,
                    chunk: Some(0),
                }],
            },
            source_path: Some("/x/clip_es.mp4".into()),
            schema_version: SCHEMA_VERSION.into(),
            tool_version: TOOL_VERSION.into(),
        }
    }

    #[test]
    fn round_trip() {
        let (_d, mut c) = temp_conn();
        upsert(&mut c, &sample(), false).unwrap();
        let got = lookup(&c, "abc123").unwrap();
        assert!(got.cached);
        assert_eq!(got.backend.backend_used, "cache");
        assert!(got.backend.attempts.is_empty());
        assert_eq!(got.transcript.as_deref(), Some("Esta es una prueba"));
        assert_eq!(got.structured.unwrap().shot_type, "varies");
        assert!(got.media.chunked && got.media.chunk_count == 6);
    }

    #[test]
    fn forget_and_clear_all() {
        let (_d, mut c) = temp_conn();
        upsert(&mut c, &sample(), false).unwrap();
        // forget a hash that is not present → 0 removed, row still there.
        assert_eq!(forget(&c, "nope").unwrap(), 0);
        assert!(lookup(&c, "abc123").is_some());
        // forget the real hash → 1 removed, keywords cascade away.
        assert_eq!(forget(&c, "abc123").unwrap(), 1);
        assert!(lookup(&c, "abc123").is_none());
        let kw: i64 = c
            .query_row("SELECT count(*) FROM keywords", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kw, 0);
        // clear_all on a re-seeded cache removes every row.
        upsert(&mut c, &sample(), false).unwrap();
        assert_eq!(clear_all(&c).unwrap(), 1);
        assert!(lookup(&c, "abc123").is_none());
    }

    #[test]
    fn keyword_dedup_case_insensitive() {
        let (_d, mut c) = temp_conn();
        upsert(&mut c, &sample(), false).unwrap();
        let mut stmt = c
            .prepare("SELECT keyword FROM keywords WHERE content_sha256='abc123' ORDER BY keyword")
            .unwrap();
        let kws: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(kws, vec!["steel-beam", "worker"]);
    }

    #[test]
    fn failed_never_hits() {
        let (_d, mut c) = temp_conn();
        let f = MediaResult::failed(None, "boom".into(), "backend", "dead".into(), "video");
        upsert(&mut c, &f, false).unwrap();
        assert!(lookup(&c, "dead").is_none());
    }

    #[test]
    fn stale_schema_miss() {
        let (_d, mut c) = temp_conn();
        upsert(&mut c, &sample(), false).unwrap();
        c.execute(
            "UPDATE media SET schema_version='1999-01-01.1' WHERE content_sha256='abc123'",
            [],
        )
        .unwrap();
        assert!(lookup(&c, "abc123").is_none());
    }

    #[test]
    fn force_keeps_row() {
        let (_d, mut c) = temp_conn();
        upsert(&mut c, &sample(), false).unwrap();
        upsert(&mut c, &sample(), true).unwrap();
        assert!(lookup(&c, "abc123").is_some());
    }

    fn filters(f: impl FnOnce(&mut RecallFilters)) -> RecallFilters {
        let mut rf = RecallFilters {
            order_by: "updated_at".into(),
            descending: true,
            limit: 20,
            ..Default::default()
        };
        f(&mut rf);
        rf
    }

    #[test]
    fn recall_filters() {
        let (_d, mut c) = temp_conn();
        upsert(&mut c, &sample(), false).unwrap();
        assert_eq!(
            recall(&c, &filters(|f| f.keyword = Some("steel-beam".into())))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            recall(
                &c,
                &filters(|f| {
                    f.rating = Some("keep".into());
                    f.language = Some("es".into());
                })
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            recall(
                &c,
                &filters(|f| {
                    f.chunked = Some(true);
                    f.media_kind = Some("video".into());
                })
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            recall(&c, &filters(|f| f.has_transcript = Some(true)))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            recall(&c, &filters(|f| f.rating = Some("cull".into())))
                .unwrap()
                .len(),
            0
        );
        assert!(
            !recall(&c, &filters(|f| f.text = Some("steel beam".into())))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("foo_bar"), "foo\\_bar");
    }
}

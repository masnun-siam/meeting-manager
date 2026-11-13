//! The notes database of the desktop app: one row per recording, searchable and editable.
//! Several processes write it (every `meeting process` run, the native host, the app), so the
//! migration, the merge rule and the edit rules live here once. The store logs nothing: callers
//! log `error_kind`, never an error's text.

use std::cmp::Reverse;
use std::fmt;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use rusqlite::types::Type;
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::Language;
use crate::jobs::now_utc;

const INFERRED: &str = " (inferred)";
const MY_NOTES_MAX_BYTES: usize = 1_048_576;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MIGRATIONS: &[&str] = &[SCHEMA_V1];

// `meetings_fts.rowid` is `meetings.id`. The triggers fire only for the indexed columns, so the
// frequent status-only updates never re-index a large transcript.
const SCHEMA_V1: &str = r#"
CREATE TABLE meetings (
  id INTEGER PRIMARY KEY,
  recording_path TEXT NOT NULL UNIQUE,
  source TEXT NOT NULL CHECK (source IN ('browser','import','desktop')),
  recorded_at TEXT NOT NULL,
  duration_s INTEGER,
  language TEXT CHECK (language IN ('bn','en')),
  status TEXT NOT NULL CHECK (status IN ('processing','done','no_speech','failed')),
  failed_stage TEXT,
  error TEXT,
  title TEXT,
  title_edited INTEGER NOT NULL DEFAULT 0,
  body_md TEXT,
  body_edited INTEGER NOT NULL DEFAULT 0,
  speakers TEXT NOT NULL DEFAULT '[]',
  my_notes TEXT NOT NULL DEFAULT '',
  transcript TEXT,
  event TEXT,
  vault_note_path TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  series_id TEXT GENERATED ALWAYS AS (json_extract(event, '$.series_id')) VIRTUAL
);
CREATE INDEX meetings_series_id ON meetings(series_id);
CREATE INDEX meetings_recorded_at ON meetings(recorded_at);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE VIRTUAL TABLE meetings_fts USING fts5(
  title, body_md, my_notes, speakers, transcript,
  tokenize = "unicode61 remove_diacritics 0 categories 'L* N* Co Mn Mc'"
);
CREATE TRIGGER meetings_ai AFTER INSERT ON meetings BEGIN
  INSERT INTO meetings_fts(rowid, title, body_md, my_notes, speakers, transcript)
  VALUES (new.id, coalesce(new.title,''), coalesce(new.body_md,''), new.my_notes,
    coalesce((SELECT group_concat(json_extract(value,'$.name'), ' ') FROM json_each(new.speakers)), ''),
    coalesce(new.transcript,''));
END;
CREATE TRIGGER meetings_au AFTER UPDATE OF title, body_md, my_notes, speakers, transcript ON meetings BEGIN
  DELETE FROM meetings_fts WHERE rowid = old.id;
  INSERT INTO meetings_fts(rowid, title, body_md, my_notes, speakers, transcript)
  VALUES (new.id, coalesce(new.title,''), coalesce(new.body_md,''), new.my_notes,
    coalesce((SELECT group_concat(json_extract(value,'$.name'), ' ') FROM json_each(new.speakers)), ''),
    coalesce(new.transcript,''));
END;
CREATE TRIGGER meetings_ad AFTER DELETE ON meetings BEGIN
  DELETE FROM meetings_fts WHERE rowid = old.id;
END;
"#;

const COLUMNS: &str = "id, recording_path, source, recorded_at, duration_s, language, status, \
     failed_stage, error, title, title_edited, body_md, body_edited, speakers, my_notes, \
     transcript, event, vault_note_path, created_at, updated_at";
const LIST_COLUMNS: &str = "m.id, m.recording_path, m.source, m.recorded_at, m.duration_s, \
     m.language, m.status, m.failed_stage, m.error, m.title";

pub fn default_path(home: &Path) -> PathBuf {
    home.join("Library/Application Support/meeting-manager/notes.sqlite")
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Browser,
    Import,
    Desktop,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Browser => "browser",
            Source::Import => "import",
            Source::Desktop => "desktop",
        }
    }

    fn parse(text: &str) -> Option<Source> {
        match text {
            "browser" => Some(Source::Browser),
            "import" => Some(Source::Import),
            "desktop" => Some(Source::Desktop),
            _ => None,
        }
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Processing,
    Done,
    NoSpeech,
    Failed,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Processing => "processing",
            Status::Done => "done",
            Status::NoSpeech => "no_speech",
            Status::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Option<Status> {
        match text {
            "processing" => Some(Status::Processing),
            "done" => Some(Status::Done),
            "no_speech" => Some(Status::NoSpeech),
            "failed" => Some(Status::Failed),
            _ => None,
        }
    }
}

fn language_str(language: Language) -> &'static str {
    match language {
        Language::Bn => "bn",
        Language::En => "en",
    }
}

fn parse_language(text: &str) -> Option<Language> {
    match text {
        "bn" => Some(Language::Bn),
        "en" => Some(Language::En),
        _ => None,
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SpeakerRow {
    pub label: String,
    pub name: Option<String>,
    pub evidence: String,
    pub name_edited: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EventLink {
    pub occurrence_id: String,
    pub series_id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub calendar: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Row {
    pub id: i64,
    pub recording_path: PathBuf,
    pub source: Source,
    pub recorded_at: String,
    pub duration_s: Option<i64>,
    pub language: Option<Language>,
    pub status: Status,
    pub failed_stage: Option<String>,
    pub error: Option<String>,
    pub title: Option<String>,
    pub title_edited: bool,
    pub body_md: Option<String>,
    pub body_edited: bool,
    pub speakers: Vec<SpeakerRow>,
    pub my_notes: String,
    pub transcript: Option<String>,
    pub event: Option<EventLink>,
    pub vault_note_path: Option<PathBuf>,
    pub created_at: String,
    pub updated_at: String,
}

/// What `list` and `earlier` return: never the transcript, body, speakers or "My notes".
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ListRow {
    pub id: i64,
    pub recording_path: PathBuf,
    pub source: Source,
    pub recorded_at: String,
    pub duration_s: Option<i64>,
    pub language: Option<Language>,
    pub status: Status,
    pub failed_stage: Option<String>,
    pub error: Option<String>,
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunStart<'a> {
    pub recording: &'a Path,
    pub source: Source,
    pub recorded_at: &'a str,
    pub language: Option<Language>,
}

/// The outcome of one pipeline run. A `None` never overwrites a stored value.
#[derive(Clone, Debug, PartialEq)]
pub struct RunResult {
    pub status: Status,
    pub failed_stage: Option<String>,
    pub error: Option<String>,
    pub title: Option<String>,
    pub body_md: Option<String>,
    /// Label, name, evidence.
    pub speakers: Vec<(String, Option<String>, String)>,
    pub language: Option<Language>,
    pub duration_s: Option<i64>,
    pub transcript: Option<String>,
    pub event: Option<EventLink>,
    pub source: Source,
    pub recorded_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Title(String),
    Body(String),
    MyNotes(String),
    Speaker { label: String, name: String },
}

#[derive(Debug)]
pub enum EditError {
    UnknownNote,
    UnknownSpeaker,
    NotEditable,
    TooLarge,
    Store(anyhow::Error),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::UnknownNote => f.write_str("unknown note"),
            EditError::UnknownSpeaker => f.write_str("unknown speaker"),
            EditError::NotEditable => f.write_str("note is not editable"),
            EditError::TooLarge => f.write_str("text is too large"),
            EditError::Store(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for EditError {}

/// The two open failures the app must tell apart from the rest: only these mean the file itself
/// is unusable, so only these may lead a caller to move it aside.
#[derive(Clone, Debug, PartialEq)]
pub enum OpenError {
    Newer { found: u32 },
    Corrupt,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Newer { found } => write!(
                f,
                "Notes database is from a newer version (schema {found}, this build knows {}); \
                 update the meeting tool and the app",
                MIGRATIONS.len()
            ),
            OpenError::Corrupt => f.write_str(
                "Notes database can't be opened: the file is not a valid notes database; \
                 the app's Settings › Rescan folders rebuilds it",
            ),
        }
    }
}

impl std::error::Error for OpenError {}

/// The log "kind" of a store failure: a fixed word, never a path, a value or a message.
pub fn error_kind(e: &anyhow::Error) -> &'static str {
    for cause in e.chain() {
        if let Some(open) = cause.downcast_ref::<OpenError>() {
            return match open {
                OpenError::Newer { .. } => "newer_version",
                OpenError::Corrupt => "corrupt",
            };
        }
        if let Some(EditError::Store(inner)) = cause.downcast_ref::<EditError>() {
            return error_kind(inner);
        }
        if let Some(sqlite) = cause.downcast_ref::<rusqlite::Error>() {
            return sqlite_kind(sqlite);
        }
        if cause.downcast_ref::<std::io::Error>().is_some() {
            return "io";
        }
    }
    "other"
}

fn sqlite_kind(e: &rusqlite::Error) -> &'static str {
    match e.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => "busy",
        Some(ErrorCode::ReadOnly) => "read_only",
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => "corrupt",
        Some(ErrorCode::DiskFull) => "disk_full",
        Some(ErrorCode::CannotOpen) => "cannot_open",
        Some(ErrorCode::ConstraintViolation) => "constraint",
        _ => "sqlite",
    }
}

/// The name shown for a speaker: an edited name as is, an inferred one marked (never twice),
/// otherwise the label.
pub fn shown_name(s: &SpeakerRow) -> String {
    match &s.name {
        None => s.label.clone(),
        Some(name) if s.name_edited || name.ends_with(INFERRED) => name.clone(),
        Some(name) => format!("{name}{INFERRED}"),
    }
}

/// One connection to the notes database. Not shared across threads: each caller opens its own.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating and migrating as needed) the database at `path`.
    pub fn open(path: &Path) -> anyhow::Result<Store> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        // Created before SQLite sees it: SQLite gives `-wal` and `-shm` the main file's mode.
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(path)
            .and_then(|_| fs::set_permissions(path, fs::Permissions::from_mode(0o600)))
            .with_context(|| format!("cannot create {}", path.display()))?;
        let conn =
            Connection::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        prepare(&conn, MIGRATIONS).map_err(|e| open_error(e, path))?;
        Ok(Store { conn })
    }

    /// Marks the recording's row as processing (creating it), leaving every note field alone.
    pub fn start_run(&self, run: &RunStart) -> anyhow::Result<i64> {
        let now = now_utc();
        self.conn
            .query_row(
                "INSERT INTO meetings (recording_path, source, recorded_at, language, status, \
                 created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'processing', ?5, ?5) \
                 ON CONFLICT(recording_path) DO UPDATE SET status = 'processing', \
                 failed_stage = NULL, error = NULL, source = excluded.source, \
                 recorded_at = excluded.recorded_at, \
                 language = coalesce(excluded.language, language), \
                 updated_at = excluded.updated_at RETURNING id",
                params![
                    utf8(run.recording)?,
                    run.source.as_str(),
                    run.recorded_at,
                    run.language.map(language_str),
                    now
                ],
                |r| r.get(0),
            )
            .context("cannot write the notes database")
    }

    /// Stores a run's result, keeping every edited field (FR-19), and returns the merged row.
    pub fn merge_run(&self, recording: &Path, result: &RunResult) -> anyhow::Result<Row> {
        let path = utf8(recording)?;
        // The edited marks are read under the write lock, so they cannot change before the write.
        let tx = self.write_tx()?;
        let stored = find(&tx, "recording_path", path)?;
        save(&tx, &merged(stored, recording, result, &now_utc()))?;
        let row = find(&tx, "recording_path", path)?.context("the merged note was not saved")?;
        tx.commit().context("cannot write the notes database")?;
        Ok(row)
    }

    pub fn edit(&self, id: i64, edit: Edit) -> Result<Row, EditError> {
        let tx = self.write_tx().map_err(EditError::Store)?;
        let mut row = find(&tx, "id", id)
            .map_err(EditError::Store)?
            .ok_or(EditError::UnknownNote)?;
        apply(&mut row, edit)?;
        row.updated_at = now_utc();
        save(&tx, &row).map_err(EditError::Store)?;
        tx.commit()
            .context("cannot write the notes database")
            .map_err(EditError::Store)?;
        Ok(row)
    }

    /// Clears the edited marks (not "My notes") so the next run replaces title, body and names.
    pub fn clear_edits(&self, id: i64) -> anyhow::Result<()> {
        let tx = self.write_tx()?;
        let Some(mut row) = find(&tx, "id", id)? else {
            return Ok(());
        };
        row.title_edited = false;
        row.body_edited = false;
        for speaker in &mut row.speakers {
            speaker.name_edited = false;
        }
        row.updated_at = now_utc();
        save(&tx, &row)?;
        tx.commit().context("cannot write the notes database")
    }

    pub fn set_vault_note_path(&self, id: i64, path: &Path) -> anyhow::Result<()> {
        let path = path
            .to_str()
            .context("vault note path is not valid UTF-8")?;
        self.conn
            .execute(
                "UPDATE meetings SET vault_note_path = ?1 WHERE id = ?2",
                params![path, id],
            )
            .context("cannot write the notes database")?;
        Ok(())
    }

    pub fn get(&self, id: i64) -> anyhow::Result<Option<Row>> {
        find(&self.conn, "id", id)
    }

    pub fn get_by_path(&self, path: &Path) -> anyhow::Result<Option<Row>> {
        find(&self.conn, "recording_path", utf8(path)?)
    }

    /// Every row newest first, or with a query the rows holding all its words (the last one as a
    /// prefix), by relevance then newest.
    pub fn list(&self, query: Option<&str>) -> anyhow::Result<Vec<ListRow>> {
        match query.and_then(match_expression) {
            None => self.list_rows(
                &format!(
                    "SELECT {LIST_COLUMNS} FROM meetings m ORDER BY m.recorded_at DESC, m.id DESC"
                ),
                params![],
            ),
            Some(expression) => self.list_rows(
                &format!(
                    "SELECT {LIST_COLUMNS} FROM meetings_fts JOIN meetings m \
                     ON m.id = meetings_fts.rowid WHERE meetings_fts MATCH ?1 \
                     ORDER BY bm25(meetings_fts), m.recorded_at DESC, m.id DESC"
                ),
                params![expression],
            ),
        }
    }

    /// Earlier meetings of one calendar series, newest first.
    pub fn earlier(
        &self,
        series_id: &str,
        exclude: Option<i64>,
        limit: usize,
    ) -> anyhow::Result<Vec<ListRow>> {
        self.list_rows(
            &format!(
                "SELECT {LIST_COLUMNS} FROM meetings m WHERE m.series_id = ?1 \
                 AND (?2 IS NULL OR m.id != ?2) ORDER BY m.recorded_at DESC, m.id DESC LIMIT ?3"
            ),
            params![series_id, exclude, i64::try_from(limit).unwrap_or(i64::MAX)],
        )
    }

    /// Follows a recording to its new location. No matching row is not an error.
    pub fn moved(&self, from: &Path, to: &Path) -> anyhow::Result<()> {
        self.conn
            .execute(
                "UPDATE meetings SET recording_path = ?1, updated_at = ?2 \
                 WHERE recording_path = ?3",
                params![utf8(to)?, now_utc(), utf8(from)?],
            )
            .map_err(|e| match sqlite_kind(&e) {
                "constraint" => anyhow::Error::new(e).context(format!(
                    "cannot move the note to {}: a note already holds that path",
                    to.display()
                )),
                _ => anyhow::Error::new(e).context("cannot write the notes database"),
            })?;
        Ok(())
    }

    /// Deletes the recording's row and its search entry. No matching row is not an error.
    pub fn removed(&self, path: &Path) -> anyhow::Result<()> {
        self.conn
            .execute(
                "DELETE FROM meetings WHERE recording_path = ?1",
                params![utf8(path)?],
            )
            .context("cannot write the notes database")?;
        Ok(())
    }

    pub fn paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        let mut statement = self
            .conn
            .prepare("SELECT recording_path FROM meetings ORDER BY id")
            .context("cannot read the notes database")?;
        let paths = statement
            .query_map([], |r| r.get::<_, String>(0).map(PathBuf::from))
            .and_then(Iterator::collect)
            .context("cannot read the notes database")?;
        Ok(paths)
    }

    pub fn setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .context("cannot read the notes database")
    }

    pub fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [key, value],
            )
            .context("cannot write the notes database")?;
        Ok(())
    }

    // IMMEDIATE takes the write lock before the first read: a deferred read-then-write upgrade
    // can fail at once with SQLITE_BUSY_SNAPSHOT instead of waiting for the busy timeout.
    fn write_tx(&self) -> anyhow::Result<Transaction<'_>> {
        Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
            .context("cannot write the notes database")
    }

    fn list_rows(&self, sql: &str, params: impl rusqlite::Params) -> anyhow::Result<Vec<ListRow>> {
        let mut statement = self
            .conn
            .prepare(sql)
            .context("cannot read the notes database")?;
        let rows = statement
            .query_map(params, list_row_of)
            .and_then(Iterator::collect)
            .context("cannot read the notes database")?;
        Ok(rows)
    }
}

fn prepare(conn: &Connection, steps: &[&str]) -> anyhow::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // SQLite does not call the busy handler for the switch to WAL, so several processes opening
    // a new file at once would fail at once; every step is safe to repeat, so wait here instead.
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        match set_up(conn, steps) {
            Err(e) if error_kind(&e) == "busy" && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            done => return done,
        }
    }
}

fn set_up(conn: &Connection, steps: &[&str]) -> anyhow::Result<()> {
    // Checked before switching to WAL, so a file from a newer build is not modified.
    version(conn, steps)?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))?;
    migrate(conn, steps)
}

/// The stored schema version, refused when this build does not know it.
fn version(conn: &Connection, steps: &[&str]) -> anyhow::Result<usize> {
    let found: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if found as usize > steps.len() {
        return Err(OpenError::Newer { found }.into());
    }
    Ok(found as usize)
}

fn migrate(conn: &Connection, steps: &[&str]) -> anyhow::Result<()> {
    if version(conn, steps)? == steps.len() {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    // Read again under the write lock: another process may have migrated while this one waited.
    let done = version(&tx, steps)?;
    for step in &steps[done..] {
        tx.execute_batch(step)?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {}", steps.len()))?;
    Ok(tx.commit()?)
}

fn open_error(e: anyhow::Error, path: &Path) -> anyhow::Error {
    if e.downcast_ref::<OpenError>().is_some() {
        return e.context(format!("{}", path.display()));
    }
    if error_kind(&e) == "corrupt" {
        return anyhow::Error::new(OpenError::Corrupt).context(format!("{}", path.display()));
    }
    e.context(format!("cannot open {}", path.display()))
}

fn utf8(path: &Path) -> anyhow::Result<&str> {
    // A lossy conversion would break the unique match on the path.
    path.to_str()
        .ok_or_else(|| anyhow!("recording path is not valid UTF-8"))
}

/// A stored value this build cannot read: names the column, never the value.
fn invalid(column: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        Type::Text,
        format!("invalid {column} in the notes database").into(),
    )
}

fn json<T: DeserializeOwned>(text: &str, column: &'static str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|_| invalid(column))
}

fn source_of(r: &rusqlite::Row) -> rusqlite::Result<Source> {
    Source::parse(&r.get::<_, String>("source")?).ok_or_else(|| invalid("source"))
}

fn status_of(r: &rusqlite::Row) -> rusqlite::Result<Status> {
    Status::parse(&r.get::<_, String>("status")?).ok_or_else(|| invalid("status"))
}

fn language_of(r: &rusqlite::Row) -> rusqlite::Result<Option<Language>> {
    r.get::<_, Option<String>>("language")?
        .map(|text| parse_language(&text).ok_or_else(|| invalid("language")))
        .transpose()
}

fn row_of(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get("id")?,
        recording_path: r.get::<_, String>("recording_path")?.into(),
        source: source_of(r)?,
        recorded_at: r.get("recorded_at")?,
        duration_s: r.get("duration_s")?,
        language: language_of(r)?,
        status: status_of(r)?,
        failed_stage: r.get("failed_stage")?,
        error: r.get("error")?,
        title: r.get("title")?,
        title_edited: r.get("title_edited")?,
        body_md: r.get("body_md")?,
        body_edited: r.get("body_edited")?,
        speakers: json(&r.get::<_, String>("speakers")?, "speakers")?,
        my_notes: r.get("my_notes")?,
        transcript: r.get("transcript")?,
        event: r
            .get::<_, Option<String>>("event")?
            .map(|text| json(&text, "event"))
            .transpose()?,
        vault_note_path: r
            .get::<_, Option<String>>("vault_note_path")?
            .map(PathBuf::from),
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

fn list_row_of(r: &rusqlite::Row) -> rusqlite::Result<ListRow> {
    Ok(ListRow {
        id: r.get("id")?,
        recording_path: r.get::<_, String>("recording_path")?.into(),
        source: source_of(r)?,
        recorded_at: r.get("recorded_at")?,
        duration_s: r.get("duration_s")?,
        language: language_of(r)?,
        status: status_of(r)?,
        failed_stage: r.get("failed_stage")?,
        error: r.get("error")?,
        title: r.get("title")?,
    })
}

/// The row whose `column` (`id` or `recording_path`, never caller text) equals `value`.
fn find(
    conn: &Connection,
    column: &'static str,
    value: impl rusqlite::ToSql,
) -> anyhow::Result<Option<Row>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM meetings WHERE {column} = ?1"),
        [value],
        row_of,
    )
    .optional()
    .context("cannot read the notes database")
}

/// Inserts the row, or updates the one holding its recording path. `id`, `created_at` and
/// `vault_note_path` of an existing row are never changed here.
fn save(conn: &Connection, row: &Row) -> anyhow::Result<()> {
    let event = row.event.as_ref().map(serde_json::to_string).transpose()?;
    conn.execute(
        "INSERT INTO meetings (recording_path, source, recorded_at, duration_s, language, \
         status, failed_stage, error, title, title_edited, body_md, body_edited, speakers, \
         my_notes, transcript, event, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18) \
         ON CONFLICT(recording_path) DO UPDATE SET source = excluded.source, \
         recorded_at = excluded.recorded_at, duration_s = excluded.duration_s, \
         language = excluded.language, status = excluded.status, \
         failed_stage = excluded.failed_stage, error = excluded.error, title = excluded.title, \
         title_edited = excluded.title_edited, body_md = excluded.body_md, \
         body_edited = excluded.body_edited, speakers = excluded.speakers, \
         my_notes = excluded.my_notes, transcript = excluded.transcript, \
         event = excluded.event, updated_at = excluded.updated_at",
        params![
            utf8(&row.recording_path)?,
            row.source.as_str(),
            row.recorded_at,
            row.duration_s,
            row.language.map(language_str),
            row.status.as_str(),
            row.failed_stage,
            row.error,
            row.title,
            row.title_edited,
            row.body_md,
            row.body_edited,
            serde_json::to_string(&row.speakers)?,
            row.my_notes,
            row.transcript,
            event,
            row.created_at,
            row.updated_at,
        ],
    )
    .context("cannot write the notes database")?;
    Ok(())
}

/// The row a run leaves behind: edited fields and "My notes" stay, unedited ones follow the
/// result, and a `None` in the result never erases a stored value.
fn merged(stored: Option<Row>, recording: &Path, result: &RunResult, now: &str) -> Row {
    let mut row = stored.unwrap_or_else(|| Row {
        id: 0,
        recording_path: recording.to_owned(),
        source: result.source,
        recorded_at: String::new(),
        duration_s: None,
        language: None,
        status: result.status,
        failed_stage: None,
        error: None,
        title: None,
        title_edited: false,
        body_md: None,
        body_edited: false,
        speakers: Vec::new(),
        my_notes: String::new(),
        transcript: None,
        event: None,
        vault_note_path: None,
        created_at: now.to_owned(),
        updated_at: String::new(),
    });
    row.speakers = merged_speakers(&row.speakers, &result.speakers);
    if let (false, Some(title)) = (row.title_edited, &result.title) {
        row.title = Some(with_edited_names(title, &row.speakers, &result.speakers));
    }
    if let (false, Some(body)) = (row.body_edited, &result.body_md) {
        row.body_md = Some(with_edited_names(body, &row.speakers, &result.speakers));
    }
    row.language = result.language.or(row.language);
    row.duration_s = result.duration_s.or(row.duration_s);
    row.transcript = result.transcript.clone().or(row.transcript);
    row.event = result.event.clone().or(row.event);
    row.status = result.status;
    row.failed_stage = result.failed_stage.clone();
    row.error = result.error.clone();
    row.source = result.source;
    row.recorded_at = result.recorded_at.clone();
    row.updated_at = now.to_owned();
    row
}

/// The result's speakers, with every edited stored speaker kept as it is (also one the result
/// no longer lists). An empty result keeps the stored list.
fn merged_speakers(
    stored: &[SpeakerRow],
    result: &[(String, Option<String>, String)],
) -> Vec<SpeakerRow> {
    if result.is_empty() {
        return stored.to_vec();
    }
    let edited = |label: &str| stored.iter().find(|s| s.name_edited && s.label == label);
    let mut speakers: Vec<SpeakerRow> = result
        .iter()
        .map(|(label, name, evidence)| {
            edited(label).cloned().unwrap_or_else(|| SpeakerRow {
                label: label.clone(),
                name: name.clone(),
                evidence: evidence.clone(),
                name_edited: false,
            })
        })
        .collect();
    let missing = stored
        .iter()
        .filter(|s| s.name_edited && result.iter().all(|(label, ..)| *label != s.label));
    speakers.extend(missing.cloned());
    speakers
}

/// D35: a rerun's new text names each speaker by label or inferred name; every edited speaker's
/// name is put back so a rename survives the rerun.
fn with_edited_names(
    text: &str,
    speakers: &[SpeakerRow],
    result: &[(String, Option<String>, String)],
) -> String {
    let mut swaps = Vec::new();
    for speaker in speakers.iter().filter(|s| s.name_edited) {
        let Some(edited) = &speaker.name else {
            continue;
        };
        let inferred = result
            .iter()
            .find(|(label, ..)| *label == speaker.label)
            .and_then(|(_, name, _)| name.as_deref());
        swaps.extend(swaps_to(name_forms(inferred, &speaker.label), edited));
    }
    replace_whole_words(text, &swaps)
}

fn swaps_to(forms: Vec<String>, name: &str) -> impl Iterator<Item = (String, &str)> {
    forms.into_iter().map(move |form| (form, name))
}

/// Every way a speaker can appear in a title or body: `Name (inferred)`, `Name`, the label.
fn name_forms(name: Option<&str>, label: &str) -> Vec<String> {
    let mut forms = vec![label.to_owned()];
    if let Some(name) = name {
        let base = name.strip_suffix(INFERRED).unwrap_or(name);
        forms.push(base.to_owned());
        forms.push(format!("{base}{INFERRED}"));
    }
    forms
}

/// Replaces each whole-word occurrence of a needle with its replacement, in one pass (the
/// longest needle first), so text just written is never matched again. A word ends where the
/// next character is not alphanumeric: `Speaker 1` is not found in `Speaker 10`.
fn replace_whole_words(text: &str, swaps: &[(String, &str)]) -> String {
    let mut swaps: Vec<(&str, &str)> = swaps
        .iter()
        .map(|(needle, with)| (needle.as_str(), *with))
        .filter(|(needle, _)| !needle.is_empty())
        .collect();
    swaps.sort_by_key(|(needle, _)| Reverse(needle.len()));
    let in_word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let mut out = String::with_capacity(text.len());
    let mut previous = None;
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let hit = swaps.iter().find(|(n, _)| {
            rest.starts_with(n) && !in_word(previous) && !in_word(rest[n.len()..].chars().next())
        });
        let taken = match hit {
            Some((needle, with)) => {
                out.push_str(with);
                previous = needle.chars().next_back();
                needle.len()
            }
            None => {
                out.push(c);
                previous = Some(c);
                c.len_utf8()
            }
        };
        rest = &rest[taken..];
    }
    out
}

/// FR-18: "My notes" is always editable (up to 1 MB); the rest only on a `done` note.
fn apply(row: &mut Row, edit: Edit) -> Result<(), EditError> {
    if let Edit::MyNotes(text) = edit {
        if text.len() > MY_NOTES_MAX_BYTES {
            return Err(EditError::TooLarge);
        }
        row.my_notes = text;
        return Ok(());
    }
    if row.status != Status::Done {
        return Err(EditError::NotEditable);
    }
    match edit {
        Edit::MyNotes(_) => {}
        Edit::Title(title) => {
            if title.trim().is_empty() {
                return Err(EditError::NotEditable);
            }
            row.title = Some(title);
            row.title_edited = true;
        }
        Edit::Body(body) => {
            row.body_md = Some(body);
            row.body_edited = true;
        }
        Edit::Speaker { label, name } => rename(row, &label, name)?,
    }
    Ok(())
}

/// Renames a speaker and every whole-word mention of them in the title and body. The edited
/// marks of title and body stay as they are: `merge_run` re-applies the name on a rerun (D35).
fn rename(row: &mut Row, label: &str, name: String) -> Result<(), EditError> {
    let speaker = row
        .speakers
        .iter_mut()
        .find(|s| s.label == label)
        .ok_or(EditError::UnknownSpeaker)?;
    if name.trim().is_empty() {
        return Err(EditError::NotEditable);
    }
    let swaps: Vec<_> = swaps_to(name_forms(speaker.name.as_deref(), label), &name).collect();
    row.title = row.title.take().map(|t| replace_whole_words(&t, &swaps));
    row.body_md = row.body_md.take().map(|b| replace_whole_words(&b, &swaps));
    speaker.name = Some(name);
    speaker.name_edited = true;
    Ok(())
}

/// The FTS5 expression for a typed query: each word a quoted string (so operators and column
/// filters are plain text), all required, the last one a prefix. `None` when no word is left.
fn match_expression(query: &str) -> Option<String> {
    let words: Vec<String> = query
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect();
    if words.is_empty() {
        return None;
    }
    Some(format!("{}*", words.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp;

    const REC: &str = "/r/meeting-2026-10-19_1430/recording.webm";

    fn store(name: &str) -> (PathBuf, Store) {
        let dir = temp(name);
        let store = Store::open(&dir.join("notes.sqlite")).unwrap();
        (dir, store)
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn speaker(label: &str, name: Option<&str>) -> (String, Option<String>, String) {
        (label.into(), name.map(Into::into), "evidence".into())
    }

    fn done(title: &str, body: &str) -> RunResult {
        RunResult {
            status: Status::Done,
            failed_stage: None,
            error: None,
            title: Some(title.into()),
            body_md: Some(body.into()),
            speakers: vec![
                speaker("Speaker 1", Some("Rahim")),
                speaker("Speaker 10", None),
            ],
            language: Some(Language::En),
            duration_s: Some(60),
            transcript: Some("[00:00:01] Speaker 1: hello".into()),
            event: None,
            source: Source::Browser,
            recorded_at: "2026-10-19T14:30".into(),
        }
    }

    fn failed() -> RunResult {
        RunResult {
            status: Status::Failed,
            failed_stage: Some("summarising".into()),
            error: Some("claude failed".into()),
            title: None,
            body_md: None,
            speakers: vec![],
            language: None,
            duration_s: None,
            transcript: None,
            event: None,
            ..done("", "")
        }
    }

    fn event(series: &str) -> EventLink {
        EventLink {
            occurrence_id: format!("{series}-1"),
            series_id: series.into(),
            title: "Weekly".into(),
            start: "2026-10-19T14:30:00Z".into(),
            end: "2026-10-19T15:00:00Z".into(),
            calendar: None,
        }
    }

    /// A `done` row at `path` with the given text in one place each.
    fn add(store: &Store, path: &str, recorded_at: &str, title: &str, transcript: &str) -> i64 {
        let result = RunResult {
            transcript: Some(transcript.into()),
            recorded_at: recorded_at.into(),
            speakers: vec![],
            ..done(title, "")
        };
        store.merge_run(Path::new(path), &result).unwrap().id
    }

    fn rename_to(store: &Store, id: i64, label: &str, name: &str) -> Row {
        let edit = Edit::Speaker {
            label: label.into(),
            name: name.into(),
        };
        store.edit(id, edit).unwrap()
    }

    fn ids(rows: &[ListRow]) -> Vec<i64> {
        rows.iter().map(|r| r.id).collect()
    }

    fn search(store: &Store, query: &str) -> Vec<i64> {
        ids(&store.list(Some(query)).unwrap())
    }

    #[test]
    fn fts5_json_and_generated_columns_are_available() {
        let db = Connection::open_in_memory().unwrap();
        let used: i64 = db
            .query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(used, 1);
        db.execute_batch(
            r#"CREATE VIRTUAL TABLE f USING fts5(a, tokenize = "unicode61 remove_diacritics 0 categories 'L* N* Co Mn Mc'");
               CREATE TABLE t (e TEXT, s TEXT GENERATED ALWAYS AS (json_extract(e, '$.s')) VIRTUAL);
               CREATE INDEX t_s ON t(s);
               INSERT INTO t (e) VALUES ('{"s":"x"}');"#,
        )
        .unwrap();
        let s: String = db.query_row("SELECT s FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(s, "x");
    }

    #[test]
    fn default_path_is_under_application_support() {
        assert_eq!(
            default_path(Path::new("/h")),
            Path::new("/h/Library/Application Support/meeting-manager/notes.sqlite")
        );
    }

    #[test]
    fn open_creates_the_directory_with_mode_0600_wal_and_timeout() {
        let dir = temp("store-open");
        let path = dir.join("a/b/notes.sqlite");
        let store = Store::open(&path).unwrap();
        let get = |pragma: &str| -> String {
            store
                .conn
                .query_row(&format!("PRAGMA {pragma}"), [], |r| {
                    r.get::<_, rusqlite::types::Value>(0)
                })
                .map(|v| format!("{v:?}"))
                .unwrap()
        };
        assert_eq!(mode(&path), 0o600);
        assert_eq!(get("user_version"), "Integer(1)");
        assert_eq!(get("journal_mode"), "Text(\"wal\")");
        assert_eq!(get("busy_timeout"), "Integer(5000)");
        let mut statement = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE name IN ('meetings', 'meetings_fts', 'settings', 'meetings_series_id', 'meetings_recorded_at') ORDER BY name")
            .unwrap();
        let names: Vec<String> = statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            names,
            [
                "meetings",
                "meetings_fts",
                "meetings_recorded_at",
                "meetings_series_id",
                "settings"
            ]
        );
        drop(statement);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_fixes_a_0644_file() {
        let dir = temp("store-0644");
        let path = dir.join("notes.sqlite");
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        Store::open(&path).unwrap();
        assert_eq!(mode(&path), 0o600);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wal_and_shm_files_are_0600_after_a_write() {
        let (dir, store) = store("store-wal-mode");
        store.set_setting("language", "bn").unwrap();
        for name in ["notes.sqlite", "notes.sqlite-wal", "notes.sqlite-shm"] {
            assert_eq!(mode(&dir.join(name)), 0o600, "{name}");
        }
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn migrates_from_an_empty_file() {
        let dir = temp("store-empty-file");
        let path = dir.join("notes.sqlite");
        fs::write(&path, "").unwrap();
        let store = Store::open(&path).unwrap();
        assert!(store.list(None).unwrap().is_empty());
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn migrates_from_version_1_and_keeps_rows() {
        let dir = temp("store-v1");
        let path = dir.join("notes.sqlite");
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch(SCHEMA_V1).unwrap();
        raw.execute_batch(
            "PRAGMA user_version = 1;
             INSERT INTO meetings (recording_path, source, recorded_at, status, title, created_at, updated_at)
             VALUES ('/r/a.webm', 'browser', '2026-10-19T14:30', 'done', 'Kept', 'c', 'u');",
        )
        .unwrap();
        drop(raw);
        let store = Store::open(&path).unwrap();
        let row = store.get_by_path(Path::new("/r/a.webm")).unwrap().unwrap();
        let version: u32 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row.title.as_deref(), Some("Kept"));
        assert_eq!(version, 1);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn later_steps_run_once_from_version_1() {
        let dir = temp("store-later-steps");
        let path = dir.join("notes.sqlite");
        let conn = Connection::open(&path).unwrap();
        migrate(&conn, &[SCHEMA_V1]).unwrap();
        let steps = [SCHEMA_V1, "ALTER TABLE settings ADD COLUMN x TEXT"];
        // A second ALTER would fail with "duplicate column", so passing twice proves once.
        migrate(&conn, &steps).unwrap();
        migrate(&conn, &steps).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        conn.execute_batch("SELECT x FROM settings").unwrap();
        assert_eq!(version, 2);
        drop(conn);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_newer_version_is_refused_and_left_alone() {
        let dir = temp("store-newer");
        let path = dir.join("notes.sqlite");
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch("PRAGMA user_version = 99").unwrap();
        drop(raw);
        let before = fs::read(&path).unwrap();
        let e = Store::open(&path).err().unwrap();
        let message = format!("{e:#}");
        assert_eq!(
            e.downcast_ref::<OpenError>(),
            Some(&OpenError::Newer { found: 99 })
        );
        assert!(message.contains("Notes database is from a newer version"));
        assert!(message.contains("update the meeting tool and the app"));
        assert_eq!(error_kind(&e), "newer_version");
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_corrupt_file_is_corrupt() {
        let dir = temp("store-corrupt");
        let path = dir.join("notes.sqlite");
        fs::write(&path, "private-words ".repeat(400)).unwrap();
        let e = Store::open(&path).err().unwrap();
        let message = format!("{e:#}");
        assert_eq!(e.downcast_ref::<OpenError>(), Some(&OpenError::Corrupt));
        assert!(message.contains("Notes database can't be opened"));
        assert!(message.contains(path.to_str().unwrap()));
        assert!(!message.contains("private-words"));
        assert_eq!(error_kind(&e), "corrupt");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn other_open_failures_are_not_open_errors() {
        let dir = temp("store-open-fails");
        fs::write(dir.join("file"), "").unwrap();
        let under_a_file = Store::open(&dir.join("file/notes.sqlite")).err().unwrap();
        let locked = dir.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
        let not_creatable = Store::open(&locked.join("notes.sqlite")).err().unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        for e in [under_a_file, not_creatable] {
            assert!(e.downcast_ref::<OpenError>().is_none());
            assert_eq!(error_kind(&e), "io");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn error_kind_names_each_failure() {
        let (dir, store) = store("store-error-kind");
        add(&store, "/r/a.webm", "2026-10-19T14:30", "A", "");
        add(&store, "/r/b.webm", "2026-10-19T14:31", "B", "");
        let constraint = store
            .moved(Path::new("/r/a.webm"), Path::new("/r/b.webm"))
            .err()
            .unwrap();
        let other = Store::open(&dir.join("notes.sqlite")).unwrap();
        other.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        store.conn.busy_timeout(Duration::ZERO).unwrap();
        let busy = store.set_setting("language", "bn").err().unwrap();
        let busy_edit = store.edit(1, Edit::MyNotes("x".into())).err().unwrap();
        other.conn.execute_batch("ROLLBACK").unwrap();
        let io = anyhow::Error::from(std::io::Error::other("x")).context("cannot create /p");
        let kinds = [
            (error_kind(&constraint), "constraint"),
            (error_kind(&busy), "busy"),
            (error_kind(&busy_edit.into()), "busy"),
            (error_kind(&OpenError::Corrupt.into()), "corrupt"),
            (
                error_kind(&OpenError::Newer { found: 2 }.into()),
                "newer_version",
            ),
            (error_kind(&io), "io"),
            (error_kind(&anyhow!("/p/notes.sqlite went away")), "other"),
            (error_kind(&EditError::TooLarge.into()), "other"),
            (error_kind(&rusqlite::Error::InvalidQuery.into()), "sqlite"),
        ];
        for (kind, want) in kinds {
            assert_eq!(kind, want);
        }
        for (code, want) in [
            (rusqlite::ffi::SQLITE_READONLY, "read_only"),
            (rusqlite::ffi::SQLITE_FULL, "disk_full"),
            (rusqlite::ffi::SQLITE_CANTOPEN, "cannot_open"),
            (rusqlite::ffi::SQLITE_LOCKED, "busy"),
            (rusqlite::ffi::SQLITE_NOTADB, "corrupt"),
            (rusqlite::ffi::SQLITE_CORRUPT, "corrupt"),
            (rusqlite::ffi::SQLITE_IOERR, "sqlite"),
        ] {
            let e = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
            assert_eq!(error_kind(&anyhow::Error::from(e).context("/p")), want);
        }
        drop((store, other));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn start_run_upserts_by_path_and_keeps_edits() {
        let (dir, store) = store("store-start-run");
        let start = RunStart {
            recording: Path::new(REC),
            source: Source::Browser,
            recorded_at: "2026-10-19T14:30",
            language: None,
        };
        let id = store.start_run(&start).unwrap();
        let fresh = store.get(id).unwrap().unwrap();
        assert_eq!(fresh.status, Status::Processing);
        assert_eq!((fresh.title, fresh.speakers.len()), (None, 0));

        store.merge_run(Path::new(REC), &done("T", "B")).unwrap();
        store.edit(id, Edit::Title("Mine".into())).unwrap();
        store.edit(id, Edit::MyNotes("notes".into())).unwrap();
        rename_to(&store, id, "Speaker 1", "Rahim Uddin");
        store.merge_run(Path::new(REC), &failed()).unwrap();
        let before = store.get(id).unwrap().unwrap();

        assert_eq!(store.start_run(&start).unwrap(), id);
        let after = store.get(id).unwrap().unwrap();
        assert_eq!(store.paths().unwrap().len(), 1);
        assert_eq!(after.status, Status::Processing);
        assert_eq!((&after.failed_stage, &after.error), (&None, &None));
        assert_eq!(after.language, Some(Language::En));
        assert_eq!(
            Row {
                status: before.status,
                failed_stage: before.failed_stage.clone(),
                error: before.error.clone(),
                updated_at: before.updated_at.clone(),
                ..after
            },
            before
        );
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_inserts_and_returns_the_row() {
        let (dir, store) = store("store-merge-insert");
        let result = RunResult {
            event: Some(event("series-a")),
            ..done("Standup", "We met.")
        };
        let row = store.merge_run(Path::new(REC), &result).unwrap();
        assert_eq!(row.recording_path, Path::new(REC));
        assert_eq!(row.status, Status::Done);
        assert_eq!(row.title.as_deref(), Some("Standup"));
        assert_eq!(row.body_md.as_deref(), Some("We met."));
        assert_eq!(row.speakers.len(), 2);
        assert_eq!(row.speakers[0].name.as_deref(), Some("Rahim"));
        assert!(!row.speakers[0].name_edited);
        assert_eq!(row.event, Some(event("series-a")));
        assert_eq!(row.my_notes, "");
        assert_eq!(row.created_at.len(), 20);
        assert_eq!(store.get(row.id).unwrap(), Some(row));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_keeps_edited_title_and_body_and_replaces_the_rest() {
        let (dir, store) = store("store-merge-edited");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        store.edit(id, Edit::Title("My  title\n".into())).unwrap();
        let rerun = RunResult {
            duration_s: Some(90),
            transcript: Some("new transcript".into()),
            ..done("T2", "B2")
        };
        let row = store.merge_run(Path::new(REC), &rerun).unwrap();
        assert_eq!(row.title.as_deref(), Some("My  title\n"));
        assert!(row.title_edited && !row.body_edited);
        assert_eq!(row.body_md.as_deref(), Some("B2"));
        assert_eq!(row.duration_s, Some(90));
        assert_eq!(row.transcript.as_deref(), Some("new transcript"));

        store.edit(id, Edit::Body("My body".into())).unwrap();
        let row = store.merge_run(Path::new(REC), &done("T3", "B3")).unwrap();
        assert_eq!(row.title.as_deref(), Some("My  title\n"));
        assert_eq!(row.body_md.as_deref(), Some("My body"));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_keeps_edited_speaker_names_per_label() {
        let (dir, store) = store("store-merge-speakers");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        rename_to(&store, id, "Speaker 1", "Rahim Uddin");
        let rerun = RunResult {
            speakers: vec![
                ("Speaker 1".into(), Some("Karim".into()), "new".into()),
                ("Speaker 10".into(), Some("Salma".into()), "new".into()),
            ],
            ..done("T", "B")
        };
        let row = store.merge_run(Path::new(REC), &rerun).unwrap();
        assert_eq!(
            row.speakers,
            [
                SpeakerRow {
                    label: "Speaker 1".into(),
                    name: Some("Rahim Uddin".into()),
                    evidence: "evidence".into(),
                    name_edited: true,
                },
                SpeakerRow {
                    label: "Speaker 10".into(),
                    name: Some("Salma".into()),
                    evidence: "new".into(),
                    name_edited: false,
                },
            ]
        );
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_never_changes_my_notes() {
        let (dir, store) = store("store-merge-my-notes");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        store.edit(id, Edit::MyNotes("mine".into())).unwrap();
        for result in [done("T2", "B2"), failed()] {
            let row = store.merge_run(Path::new(REC), &result).unwrap();
            assert_eq!(row.my_notes, "mine");
        }
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_reapplies_edited_speaker_names_to_the_new_unedited_text() {
        let (dir, store) = store("store-merge-reapply");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        rename_to(&store, id, "Speaker 1", "Rahim Uddin");
        let rerun = done(
            "Speaker 1 and Speaker 10",
            "Rahim (inferred) spoke. Rahim agreed with Speaker 10; Speaker 1 left. Rahimullah stayed.",
        );
        let row = store.merge_run(Path::new(REC), &rerun).unwrap();
        assert_eq!(row.title.as_deref(), Some("Rahim Uddin and Speaker 10"));
        assert_eq!(
            row.body_md.as_deref(),
            Some(
                "Rahim Uddin spoke. Rahim Uddin agreed with Speaker 10; Rahim Uddin left. Rahimullah stayed."
            )
        );
        assert!(!row.title_edited && !row.body_edited);

        // The result names the speaker as the edit does: only the mark and the label change.
        let same = RunResult {
            speakers: vec![speaker("Speaker 1", Some("Rahim Uddin"))],
            ..done("T", "Rahim Uddin (inferred), Rahim Uddin, Speaker 1.")
        };
        let row = store.merge_run(Path::new(REC), &same).unwrap();
        assert_eq!(
            row.body_md.as_deref(),
            Some("Rahim Uddin, Rahim Uddin, Rahim Uddin.")
        );
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_leaves_edited_title_and_body_alone_when_reapplying() {
        let (dir, store) = store("store-merge-reapply-edited");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        rename_to(&store, id, "Speaker 1", "Rahim Uddin");
        store
            .edit(id, Edit::Title("Speaker 1 said".into()))
            .unwrap();
        store.edit(id, Edit::Body("Rahim wrote".into())).unwrap();
        let row = store
            .merge_run(Path::new(REC), &done("Speaker 1", "Rahim"))
            .unwrap();
        assert_eq!(row.title.as_deref(), Some("Speaker 1 said"));
        assert_eq!(row.body_md.as_deref(), Some("Rahim wrote"));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_keeps_edited_speakers_missing_from_the_result() {
        let (dir, store) = store("store-merge-missing-speaker");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        rename_to(&store, id, "Speaker 1", "Rahim Uddin");
        let rerun = RunResult {
            speakers: vec![speaker("Speaker 2", Some("Salma"))],
            ..done("Speaker 1 spoke", "B")
        };
        let row = store.merge_run(Path::new(REC), &rerun).unwrap();
        let labels: Vec<_> = row.speakers.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Speaker 2", "Speaker 1"]);
        assert_eq!(row.speakers[1].name.as_deref(), Some("Rahim Uddin"));
        assert_eq!(row.title.as_deref(), Some("Rahim Uddin spoke"));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_with_none_fields_never_overwrites() {
        let (dir, store) = store("store-merge-none");
        let first = RunResult {
            event: Some(event("series-a")),
            ..done("T", "B")
        };
        let before = store.merge_run(Path::new(REC), &first).unwrap();
        let empty = RunResult {
            status: Status::Done,
            failed_stage: None,
            error: None,
            ..failed()
        };
        let after = store.merge_run(Path::new(REC), &empty).unwrap();
        assert_eq!(
            Row {
                updated_at: before.updated_at.clone(),
                ..after
            },
            before
        );
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_merge_keeps_the_earlier_summary() {
        let (dir, store) = store("store-merge-failed");
        let before = store.merge_run(Path::new(REC), &done("T", "B")).unwrap();
        for (result, status) in [
            (failed(), Status::Failed),
            (
                RunResult {
                    status: Status::NoSpeech,
                    failed_stage: None,
                    error: None,
                    ..failed()
                },
                Status::NoSpeech,
            ),
        ] {
            let after = store.merge_run(Path::new(REC), &result).unwrap();
            assert_eq!(after.status, status);
            assert_eq!(after.failed_stage, result.failed_stage);
            assert_eq!(after.error, result.error);
            assert_eq!(
                (
                    &after.title,
                    &after.body_md,
                    &after.speakers,
                    &after.transcript
                ),
                (
                    &before.title,
                    &before.body_md,
                    &before.speakers,
                    &before.transcript
                )
            );
        }
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_after_clear_edits_replaces_everything_but_my_notes() {
        let (dir, store) = store("store-clear-edits");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        store.edit(id, Edit::Title("Mine".into())).unwrap();
        store.edit(id, Edit::Body("Mine".into())).unwrap();
        store.edit(id, Edit::MyNotes("notes".into())).unwrap();
        rename_to(&store, id, "Speaker 1", "Rahim Uddin");
        store.clear_edits(id).unwrap();
        store.clear_edits(999).unwrap();
        let cleared = store.get(id).unwrap().unwrap();
        assert!(!cleared.title_edited && !cleared.body_edited);
        assert!(cleared.speakers.iter().all(|s| !s.name_edited));
        assert_eq!(cleared.my_notes, "notes");

        let row = store
            .merge_run(Path::new(REC), &done("T2", "Speaker 1 spoke"))
            .unwrap();
        assert_eq!(row.title.as_deref(), Some("T2"));
        assert_eq!(row.body_md.as_deref(), Some("Speaker 1 spoke"));
        assert_eq!(row.speakers[0].name.as_deref(), Some("Rahim"));
        assert_eq!(row.my_notes, "notes");
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_title_body_and_my_notes_set_marks() {
        let (dir, store) = store("store-edit-marks");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        let notes = store.edit(id, Edit::MyNotes("notes".into())).unwrap();
        assert_eq!(notes.my_notes, "notes");
        assert!(!notes.title_edited && !notes.body_edited);
        assert_eq!(
            (notes.title.as_deref(), notes.body_md.as_deref()),
            (Some("T"), Some("B"))
        );
        let title = store.edit(id, Edit::Title("New".into())).unwrap();
        assert_eq!(title.title.as_deref(), Some("New"));
        assert!(title.title_edited && !title.body_edited);
        let body = store.edit(id, Edit::Body("New body".into())).unwrap();
        assert_eq!(body.body_md.as_deref(), Some("New body"));
        assert!(body.body_edited);
        let renamed = rename_to(&store, id, "Speaker 10", "Salma");
        assert_eq!(renamed.speakers[1].name.as_deref(), Some("Salma"));
        assert!(renamed.speakers[1].name_edited && !renamed.speakers[0].name_edited);
        assert_eq!(store.get(id).unwrap(), Some(renamed));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_is_not_editable_by_status() {
        let (dir, store) = store("store-edit-status");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        let start = RunStart {
            recording: Path::new(REC),
            source: Source::Browser,
            recorded_at: "2026-10-19T14:30",
            language: None,
        };
        let no_speech = RunResult {
            status: Status::NoSpeech,
            ..failed()
        };
        for status in [Status::Processing, Status::Failed, Status::NoSpeech] {
            match status {
                Status::Processing => drop(store.start_run(&start).unwrap()),
                Status::Failed => drop(store.merge_run(Path::new(REC), &failed()).unwrap()),
                _ => drop(store.merge_run(Path::new(REC), &no_speech).unwrap()),
            }
            let before = store.get(id).unwrap().unwrap();
            assert_eq!(before.status, status);
            for edit in [
                Edit::Title("X".into()),
                Edit::Body("X".into()),
                Edit::Speaker {
                    label: "Speaker 1".into(),
                    name: "X".into(),
                },
            ] {
                let refused = store.edit(id, edit);
                assert!(matches!(refused, Err(EditError::NotEditable)));
            }
            assert_eq!(store.get(id).unwrap().unwrap(), before);
            let notes = format!("{status:?}");
            assert_eq!(
                store
                    .edit(id, Edit::MyNotes(notes.clone()))
                    .unwrap()
                    .my_notes,
                notes
            );
        }
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_unknown_note() {
        let (dir, store) = store("store-edit-unknown");
        let refused = store.edit(7, Edit::MyNotes("x".into()));
        assert!(matches!(refused, Err(EditError::UnknownNote)));
        assert!(store.paths().unwrap().is_empty());
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_unknown_speaker_label_is_unknown_speaker() {
        let (dir, store) = store("store-edit-unknown-speaker");
        let before = store.merge_run(Path::new(REC), &done("T", "B")).unwrap();
        let edit = Edit::Speaker {
            label: "Speaker 2".into(),
            name: "X".into(),
        };
        let refused = store.edit(before.id, edit);
        assert!(matches!(refused, Err(EditError::UnknownSpeaker)));
        assert_eq!(store.get(before.id).unwrap(), Some(before));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_empty_speaker_name_or_title_is_not_editable() {
        let (dir, store) = store("store-edit-empty");
        let before = store.merge_run(Path::new(REC), &done("T", "B")).unwrap();
        for blank in ["", " \n\t"] {
            for edit in [
                Edit::Title(blank.into()),
                Edit::Speaker {
                    label: "Speaker 1".into(),
                    name: blank.into(),
                },
            ] {
                let refused = store.edit(before.id, edit);
                assert!(matches!(refused, Err(EditError::NotEditable)));
            }
        }
        assert_eq!(store.get(before.id).unwrap(), Some(before));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn my_notes_cap_counts_bytes() {
        let (dir, store) = store("store-my-notes-cap");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        let exact = "a".repeat(1_048_576);
        assert_eq!(
            store
                .edit(id, Edit::MyNotes(exact.clone()))
                .unwrap()
                .my_notes,
            exact
        );
        let bengali = "ব".repeat(400_000);
        for too_long in ["a".repeat(1_048_577), bengali] {
            let refused = store.edit(id, Edit::MyNotes(too_long));
            assert!(matches!(refused, Err(EditError::TooLarge)));
        }
        assert_eq!(store.get(id).unwrap().unwrap().my_notes, exact);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn speaker_rename_replaces_whole_words_only() {
        let (dir, store) = store("store-rename-whole-words");
        let result = RunResult {
            speakers: vec![speaker("Speaker 1", None), speaker("Speaker 10", None)],
            ..done(
                "Speaker 1 and Speaker 10",
                "Speaker 1's plan. Speaker 10: no. Ask Speaker 1.",
            )
        };
        let id = store.merge_run(Path::new(REC), &result).unwrap().id;
        let row = rename_to(&store, id, "Speaker 1", "Salma");
        assert_eq!(row.title.as_deref(), Some("Salma and Speaker 10"));
        assert_eq!(
            row.body_md.as_deref(),
            Some("Salma's plan. Speaker 10: no. Ask Salma.")
        );
        assert!(!row.title_edited && !row.body_edited);
        assert_eq!(shown_name(&row.speakers[0]), "Salma");
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_handles_inferred_plain_and_label_forms() {
        let (dir, store) = store("store-rename-forms");
        let body = "Rahim (inferred) opened. Rahim closed. Speaker 1 left.";
        let id = store
            .merge_run(Path::new(REC), &done("T", body))
            .unwrap()
            .id;
        let row = rename_to(&store, id, "Speaker 1", "Rahim Uddin");
        assert_eq!(
            row.body_md.as_deref(),
            Some("Rahim Uddin opened. Rahim Uddin closed. Rahim Uddin left.")
        );
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_does_not_touch_a_longer_name() {
        let (dir, store) = store("store-rename-longer");
        let id = store
            .merge_run(Path::new(REC), &done("Rahimullah and Rahim", "B"))
            .unwrap()
            .id;
        let row = rename_to(&store, id, "Speaker 1", "Karim");
        assert_eq!(row.title.as_deref(), Some("Rahimullah and Karim"));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_never_doubles_inferred() {
        let (dir, store) = store("store-rename-doubled");
        let result = RunResult {
            speakers: vec![speaker("Speaker 1", Some("Rahim (inferred)"))],
            ..done("Rahim (inferred) and Rahim", "B")
        };
        let before = store.merge_run(Path::new(REC), &result).unwrap();
        assert_eq!(shown_name(&before.speakers[0]), "Rahim (inferred)");
        let row = rename_to(&store, before.id, "Speaker 1", "Karim");
        assert_eq!(row.title.as_deref(), Some("Karim and Karim"));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn second_rename_uses_the_edited_name() {
        let (dir, store) = store("store-rename-twice");
        let id = store
            .merge_run(Path::new(REC), &done("Rahim spoke", "B"))
            .unwrap()
            .id;
        rename_to(&store, id, "Speaker 1", "Rahim Uddin");
        let row = rename_to(&store, id, "Speaker 1", "Karim");
        assert_eq!(row.title.as_deref(), Some("Karim spoke"));
        assert_eq!(row.speakers[0].name.as_deref(), Some("Karim"));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replace_whole_word_unit_cases() {
        let cases = [
            ("Rahim", "X"),
            ("Rahim came, Rahim.", "X came, X."),
            ("(Rahim) \"Rahim\" Rahim's", "(X) \"X\" X's"),
            (
                "Rahimullah xRahim Rahim2 Rahim_",
                "Rahimullah xRahim Rahim2 X_",
            ),
            ("রহিম। রহিম", "X। X"),
            ("", ""),
        ]
        .map(|(text, want)| (text, vec!["Rahim".to_owned(), "রহিম".to_owned()], want));
        for (text, needles, want) in cases {
            let swaps: Vec<_> = swaps_to(needles, "X").collect();
            assert_eq!(replace_whole_words(text, &swaps), want);
        }
        assert_eq!(replace_whole_words("a b", &[(String::new(), "X")]), "a b");
        // One pass: the replacement is not matched again by a shorter needle.
        let forms: Vec<_> =
            swaps_to(name_forms(Some("Rahim"), "Speaker 1"), "Rahim Uddin").collect();
        assert_eq!(
            replace_whole_words("Rahim (inferred), Rahim", &forms),
            "Rahim Uddin, Rahim Uddin"
        );
    }

    #[test]
    fn merge_renames_all_speakers_in_one_pass() {
        let (dir, store) = store("store-merge-one-pass");
        let result = RunResult {
            speakers: vec![speaker("Speaker 1", None), speaker("Speaker 2", None)],
            ..done("T", "B")
        };
        let id = store.merge_run(Path::new(REC), &result).unwrap().id;
        // One speaker is named like the other's label: neither rename may swallow the other.
        rename_to(&store, id, "Speaker 1", "Speaker 2");
        rename_to(&store, id, "Speaker 2", "Karim");
        let rerun = RunResult {
            title: Some("Speaker 1 met Speaker 2".into()),
            ..result
        };
        let row = store.merge_run(Path::new(REC), &rerun).unwrap();
        assert_eq!(row.title.as_deref(), Some("Speaker 2 met Karim"));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn many_opens_of_a_new_file_at_once_all_succeed() {
        let dir = temp("store-open-at-once");
        for round in 0..5 {
            let path = dir.join(format!("{round}/notes.sqlite"));
            let start = std::sync::Arc::new(std::sync::Barrier::new(16));
            let opens: Vec<_> = (0..16)
                .map(|_| {
                    let (path, start) = (path.clone(), start.clone());
                    thread::spawn(move || {
                        start.wait();
                        Store::open(&path).map(drop).map_err(|e| format!("{e:#}"))
                    })
                })
                .collect();
            for open in opens {
                assert_eq!(open.join().unwrap(), Ok(()));
            }
            let store = Store::open(&path).unwrap();
            let version: u32 = store
                .conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(version, 1);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    // SQLite refuses the switch to WAL at once, without the busy timeout, while another
    // connection holds a lock on the file.
    #[test]
    fn open_waits_for_a_lock_held_before_the_file_is_in_wal_mode() {
        let dir = temp("store-open-waits");
        let path = dir.join("notes.sqlite");
        let first = Connection::open(&path).unwrap();
        first.execute_batch("BEGIN IMMEDIATE").unwrap();
        let second = {
            let path = path.clone();
            thread::spawn(move || Store::open(&path).map(drop).map_err(|e| format!("{e:#}")))
        };
        thread::sleep(Duration::from_millis(300));
        first.execute_batch("COMMIT").unwrap();
        assert_eq!(second.join().unwrap(), Ok(()));
        drop(first);
        fs::remove_dir_all(&dir).unwrap();
    }

    // With a deferred transaction the waiting side reads the old version first and fails once
    // the other side commits, instead of finding the work done.
    #[test]
    fn a_migration_waits_for_another_and_does_not_repeat_its_steps() {
        let dir = temp("store-migrate-waits");
        let path = dir.join("notes.sqlite");
        let first = Connection::open(&path).unwrap();
        first
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        first.execute_batch("BEGIN IMMEDIATE").unwrap();
        first.execute_batch(SCHEMA_V1).unwrap();
        first.execute_batch("PRAGMA user_version = 1").unwrap();
        let second = {
            let path = path.clone();
            thread::spawn(move || {
                let conn = Connection::open(&path).unwrap();
                conn.busy_timeout(BUSY_TIMEOUT).unwrap();
                migrate(&conn, MIGRATIONS).map_err(|e| format!("{e:#}"))
            })
        };
        thread::sleep(Duration::from_millis(300));
        first.execute_batch("COMMIT").unwrap();
        assert_eq!(second.join().unwrap(), Ok(()));
        drop(first);
        fs::remove_dir_all(&dir).unwrap();
    }

    // With a deferred transaction the merge reads the unedited row while the edit is still
    // uncommitted, and then overwrites the edit or fails.
    #[test]
    fn a_merge_waits_for_an_edit_in_flight_and_keeps_it() {
        let (dir, store) = store("store-merge-waits");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        store.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        store
            .conn
            .execute(
                "UPDATE meetings SET title = 'Mine', title_edited = 1 WHERE id = ?1",
                [id],
            )
            .unwrap();
        let merge = {
            let path = dir.join("notes.sqlite");
            thread::spawn(move || {
                let other = Store::open(&path).map_err(|e| format!("{e:#}"))?;
                let row = other.merge_run(Path::new(REC), &done("T2", "B2"));
                row.map(|r| (r.title, r.body_md))
                    .map_err(|e| format!("{e:#}"))
            })
        };
        thread::sleep(Duration::from_millis(300));
        store.conn.execute_batch("COMMIT").unwrap();
        assert_eq!(
            merge.join().unwrap(),
            Ok((Some("Mine".into()), Some("B2".into())))
        );
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_edit_waits_for_a_merge_in_flight_and_lands_on_top() {
        let (dir, store) = store("store-edit-waits");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        store.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        store
            .conn
            .execute("UPDATE meetings SET body_md = 'B2' WHERE id = ?1", [id])
            .unwrap();
        let edit = {
            let path = dir.join("notes.sqlite");
            thread::spawn(move || {
                let other = Store::open(&path).map_err(|e| format!("{e:#}"))?;
                let row = other.edit(id, Edit::Title("Mine".into()));
                row.map(|r| (r.title, r.body_md))
                    .map_err(|e| format!("{e:#}"))
            })
        };
        thread::sleep(Duration::from_millis(300));
        store.conn.execute_batch("COMMIT").unwrap();
        assert_eq!(
            edit.join().unwrap(),
            Ok((Some("Mine".into()), Some("B2".into())))
        );
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shown_name_cases() {
        let cases = [
            (Some("Rahim"), true, "Rahim"),
            (Some("Rahim"), false, "Rahim (inferred)"),
            (Some("Rahim (inferred)"), false, "Rahim (inferred)"),
            (None, false, "Speaker 1"),
        ];
        for (name, name_edited, want) in cases {
            let s = SpeakerRow {
                label: "Speaker 1".into(),
                name: name.map(Into::into),
                evidence: String::new(),
                name_edited,
            };
            assert_eq!(shown_name(&s), want);
        }
    }

    #[test]
    fn search_matches_english_words_case_insensitively() {
        let (dir, store) = store("store-search-case");
        let a = add(&store, "/r/a.webm", "2026-10-19T14:30", "Budget Review", "");
        add(&store, "/r/b.webm", "2026-10-19T14:31", "Hiring", "");
        assert_eq!(search(&store, "budget"), [a]);
        assert_eq!(search(&store, "BUDGET review"), [a]);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_needs_all_words() {
        let (dir, store) = store("store-search-all");
        let a = add(
            &store,
            "/r/a.webm",
            "2026-10-19T14:30",
            "Budget",
            "the plan",
        );
        add(&store, "/r/b.webm", "2026-10-19T14:31", "Budget", "nothing");
        assert_eq!(search(&store, "plan budget"), [a]);
        assert!(search(&store, "budget missing").is_empty());
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_prefix_applies_to_the_last_word_only() {
        let (dir, store) = store("store-search-prefix");
        let a = add(&store, "/r/a.webm", "2026-10-19T14:30", "budget plan", "");
        assert_eq!(search(&store, "budg"), [a]);
        assert_eq!(search(&store, "plan budg"), [a]);
        assert!(search(&store, "udget").is_empty());
        assert!(search(&store, "budg plan").is_empty());
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_finds_bengali_words_and_prefixes() {
        let (dir, store) = store("store-search-bengali");
        let budget = add(
            &store,
            "/r/a.webm",
            "2026-10-19T14:30",
            "Budget",
            "আমরা বাজেট নিয়ে কথা বললাম",
        );
        let market = add(
            &store,
            "/r/b.webm",
            "2026-10-19T14:31",
            "B",
            "বাজার থেকে ফিরে",
        );
        add(&store, "/r/c.webm", "2026-10-19T14:32", "C", "বই পড়া হলো");
        assert_eq!(search(&store, "বাজেট"), [budget]);
        assert_eq!(search(&store, "নিয়ে"), [budget]);
        assert_eq!(search(&store, "বাজে"), [budget]);
        assert_eq!(search(&store, "বাজ"), [market, budget]);
        assert_eq!(search(&store, "budget বাজেট"), [budget]);
        store
            .conn
            .execute_batch(
                "CREATE VIRTUAL TABLE temp.vocab USING fts5vocab(main, meetings_fts, row)",
            )
            .unwrap();
        let whole: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM temp.vocab WHERE term = 'বাজেট'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(whole, 1);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_covers_title_body_my_notes_speaker_names_and_transcript() {
        let (dir, store) = store("store-search-fields");
        let blank = |path: &str, change: fn(&mut RunResult)| {
            let mut result = RunResult {
                speakers: vec![],
                transcript: None,
                ..done("t", "b")
            };
            change(&mut result);
            store.merge_run(Path::new(path), &result).unwrap().id
        };
        let title = blank("/r/1.webm", |r| r.title = Some("alpha".into()));
        let body = blank("/r/2.webm", |r| r.body_md = Some("bravo".into()));
        let notes = blank("/r/3.webm", |_| {});
        store.edit(notes, Edit::MyNotes("charlie".into())).unwrap();
        let name = blank("/r/4.webm", |r| {
            r.speakers = vec![speaker("Speaker 1", Some("Delta"))];
        });
        let transcript = blank("/r/5.webm", |r| r.transcript = Some("echo".into()));
        for (word, id) in [
            ("alpha", title),
            ("bravo", body),
            ("charlie", notes),
            ("delta", name),
            ("echo", transcript),
        ] {
            assert_eq!(search(&store, word), [id], "{word}");
        }
        // Labels and evidence are not indexed, only names.
        assert!(search(&store, "evidence").is_empty());
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_query_syntax_is_inert() {
        let (dir, store) = store("store-search-syntax");
        let a = add(&store, "/r/a.webm", "2026-10-19T14:30", "budget x", "a b");
        for junk in [
            "\"", "\"\"", "*", "(", ")", "-", ":", "^", "+", "{", "- ( ) *",
        ] {
            assert_eq!(search(&store, junk), [a], "{junk}");
            assert_eq!(search(&store, &format!("budget {junk}")), [a], "{junk}");
            assert_eq!(search(&store, &format!("{junk} budget")), [a], "{junk}");
        }
        // Operators and column filters are plain words: they must be in the text to match.
        for words in [
            "AND",
            "OR",
            "NOT",
            "budget AND x",
            "budget OR zzz",
            "budget NOT x",
        ] {
            let found = search(&store, words);
            assert!(found.is_empty(), "{words}");
        }
        assert_eq!(search(&store, "NEAR(a b)"), [] as [i64; 0]);
        assert_eq!(search(&store, "NEAR("), [] as [i64; 0]);
        assert_eq!(search(&store, "title:x"), [] as [i64; 0]);
        assert_eq!(search(&store, "transcript:budget"), [] as [i64; 0]);
        // A letter-like symbol the tokenizer drops is still not a syntax error.
        for symbol in ["Ⓐ", "budget Ⓐ", "Ⓐ budget"] {
            store.list(Some(symbol)).unwrap();
        }
        assert_eq!(search(&store, "a\"b"), [a]);
        assert_eq!(search(&store, "-budget"), [a]);
        assert_eq!(search(&store, "budg*"), [a]);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn blank_query_lists_everything_newest_first() {
        let (dir, store) = store("store-list-blank");
        let old = add(&store, "/r/a.webm", "2026-10-18T09:00", "A", "");
        let tie_1 = add(&store, "/r/b.webm", "2026-10-19T14:30", "B", "");
        let tie_2 = add(&store, "/r/c.webm", "2026-10-19T14:30", "C", "");
        for query in [None, Some(""), Some("   \t")] {
            assert_eq!(ids(&store.list(query).unwrap()), [tie_2, tie_1, old]);
        }
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_follows_edits_merges_and_removal() {
        let (dir, store) = store("store-search-follows");
        let id = store
            .merge_run(Path::new(REC), &done("oldtitle", "oldbody"))
            .unwrap()
            .id;
        store.edit(id, Edit::Title("newtitle".into())).unwrap();
        assert_eq!(search(&store, "newtitle"), [id]);
        assert!(search(&store, "oldtitle").is_empty());

        store
            .merge_run(Path::new(REC), &done("x", "newbody"))
            .unwrap();
        assert_eq!(search(&store, "newbody"), [id]);
        assert!(search(&store, "oldbody").is_empty());

        rename_to(&store, id, "Speaker 1", "Salma");
        assert_eq!(search(&store, "salma"), [id]);
        assert!(search(&store, "rahim").is_empty());

        store
            .moved(Path::new(REC), Path::new("/r/moved.webm"))
            .unwrap();
        assert_eq!(search(&store, "newbody"), [id]);
        store.removed(Path::new("/r/moved.webm")).unwrap();
        assert!(search(&store, "newbody").is_empty());
        let indexed: i64 = store
            .conn
            .query_row("SELECT count(*) FROM meetings_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(indexed, 0);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn equal_relevance_orders_newest_first() {
        let (dir, store) = store("store-search-order");
        let old = add(&store, "/r/a.webm", "2026-10-18T09:00", "budget", "");
        let new = add(&store, "/r/b.webm", "2026-10-19T09:00", "budget", "");
        let dense = add(
            &store,
            "/r/c.webm",
            "2026-10-17T09:00",
            "budget budget budget",
            "budget budget",
        );
        assert_eq!(search(&store, "budget"), [dense, new, old]);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn earlier_is_newest_first_capped_and_excludes() {
        let (dir, store) = store("store-earlier");
        let in_series = |path: &str, recorded_at: &str, series: &str| {
            let result = RunResult {
                event: Some(event(series)),
                recorded_at: recorded_at.into(),
                ..done("T", "B")
            };
            store.merge_run(Path::new(path), &result).unwrap().id
        };
        let first = in_series("/r/1.webm", "2026-10-05T10:00", "weekly");
        let second = in_series("/r/2.webm", "2026-10-12T10:00", "weekly");
        let third = in_series("/r/3.webm", "2026-10-19T10:00", "weekly");
        in_series("/r/4.webm", "2026-10-20T10:00", "other");
        add(&store, "/r/5.webm", "2026-10-21T10:00", "No event", "");
        let rows: Vec<ListRow> = store.earlier("weekly", None, 10).unwrap();
        assert_eq!(ids(&rows), [third, second, first]);
        assert_eq!(
            ids(&store.earlier("weekly", Some(third), 10).unwrap()),
            [second, first]
        );
        assert_eq!(
            ids(&store.earlier("weekly", Some(third), 1).unwrap()),
            [second]
        );
        assert!(store.earlier("none", None, 10).unwrap().is_empty());
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn list_rows_carry_no_body_or_transcript() {
        let (dir, store) = store("store-list-row");
        let row = store.merge_run(Path::new(REC), &done("T", "B")).unwrap();
        let listed = store.list(None).unwrap();
        assert_eq!(
            listed,
            [ListRow {
                id: row.id,
                recording_path: REC.into(),
                source: Source::Browser,
                recorded_at: "2026-10-19T14:30".into(),
                duration_s: Some(60),
                language: Some(Language::En),
                status: Status::Done,
                failed_stage: None,
                error: None,
                title: Some("T".into()),
            }]
        );
        let json = serde_json::to_value(&listed[0]).unwrap();
        assert_eq!(json["source"], "browser");
        assert_eq!(json["status"], "done");
        assert_eq!(serde_json::to_value(Status::NoSpeech).unwrap(), "no_speech");
        let full = store.get(row.id).unwrap().unwrap();
        assert!(full.body_md.is_some() && full.transcript.is_some());
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn moved_and_removed_and_paths() {
        let (dir, store) = store("store-moved");
        let a = add(&store, "/r/a.webm", "2026-10-19T14:30", "A", "");
        add(&store, "/r/b.webm", "2026-10-19T14:31", "B", "");
        store
            .moved(Path::new("/r/a.webm"), Path::new("/archive/a.webm"))
            .unwrap();
        assert_eq!(
            store.paths().unwrap(),
            [Path::new("/archive/a.webm"), Path::new("/r/b.webm")]
        );
        assert_eq!(
            store
                .get_by_path(Path::new("/archive/a.webm"))
                .unwrap()
                .unwrap()
                .id,
            a
        );
        store.removed(Path::new("/r/b.webm")).unwrap();
        assert_eq!(store.paths().unwrap(), [Path::new("/archive/a.webm")]);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn moved_and_removed_with_no_match_are_noops() {
        let (dir, store) = store("store-moved-noop");
        let before = store.merge_run(Path::new(REC), &done("T", "B")).unwrap();
        store
            .moved(Path::new("/none"), Path::new("/other"))
            .unwrap();
        store.removed(Path::new("/none")).unwrap();
        assert_eq!(store.get(before.id).unwrap(), Some(before));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn moved_onto_an_existing_path_errors_and_keeps_both() {
        let (dir, store) = store("store-moved-clash");
        let a = add(&store, "/r/a.webm", "2026-10-19T14:30", "A", "");
        let b = add(&store, "/r/b.webm", "2026-10-19T14:31", "B", "");
        let before = (store.get(a).unwrap(), store.get(b).unwrap());
        let e = store
            .moved(Path::new("/r/a.webm"), Path::new("/r/b.webm"))
            .err()
            .unwrap();
        assert!(format!("{e:#}").contains("a note already holds that path"));
        assert_eq!((store.get(a).unwrap(), store.get(b).unwrap()), before);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_round_trip_and_overwrite() {
        let (dir, store) = store("store-settings");
        assert_eq!(store.setting("language").unwrap(), None);
        store.set_setting("language", "bn").unwrap();
        store.set_setting("hidden_calendars", "[\"a\"]").unwrap();
        store.set_setting("language", "en").unwrap();
        assert_eq!(store.setting("language").unwrap().as_deref(), Some("en"));
        assert_eq!(
            store.setting("hidden_calendars").unwrap().as_deref(),
            Some("[\"a\"]")
        );
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn get_by_path_and_vault_note_path() {
        let (dir, store) = store("store-vault-path");
        let row = store.merge_run(Path::new(REC), &done("T", "B")).unwrap();
        assert_eq!(row.vault_note_path, None);
        store
            .set_vault_note_path(row.id, Path::new("/vault/Meetings/T.md"))
            .unwrap();
        let read = store.get_by_path(Path::new(REC)).unwrap().unwrap();
        assert_eq!(
            read.vault_note_path.as_deref(),
            Some(Path::new("/vault/Meetings/T.md"))
        );
        assert_eq!(store.get(row.id).unwrap(), Some(read));
        // A rerun does not forget where the note was exported.
        let rerun = store.merge_run(Path::new(REC), &done("T2", "B2")).unwrap();
        assert_eq!(
            rerun.vault_note_path.as_deref(),
            Some(Path::new("/vault/Meetings/T.md"))
        );
        assert_eq!(store.get(999).unwrap(), None);
        assert_eq!(store.get_by_path(Path::new("/none")).unwrap(), None);
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stored_values_this_build_cannot_read_name_the_column_only() {
        let (dir, store) = store("store-invalid-column");
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        store
            .conn
            .execute("UPDATE meetings SET speakers = '{\"secret\":1}'", [])
            .unwrap();
        let message = format!("{:#}", store.get(id).err().unwrap());
        assert!(message.contains("invalid speakers"));
        assert!(!message.contains("secret"));
        drop(store);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_connections_see_each_others_writes() {
        let (dir, store) = store("store-two-connections");
        let other = Store::open(&dir.join("notes.sqlite")).unwrap();
        let id = store.merge_run(Path::new(REC), &done("T", "B")).unwrap().id;
        other.edit(id, Edit::Title("Mine".into())).unwrap();
        let row = store.merge_run(Path::new(REC), &done("T2", "B2")).unwrap();
        assert_eq!(row.title.as_deref(), Some("Mine"));
        drop((store, other));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_store_never_logs_or_prints() {
        let source = include_str!("store.rs");
        for banned in ["tracing", "println", "eprintln", "dbg"].map(|w| format!("{w}{}", "!(")) {
            assert!(!source.contains(&banned), "{banned}");
        }
        assert!(!source.contains(concat!("tracing", "::")));
    }
}

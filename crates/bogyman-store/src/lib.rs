//! Append-only event log plus a materialized session snapshot, in one SQLite
//! file (WAL mode). The hook path does a single INSERT; the query path folds
//! events newer than the last materialization lazily, so hook-only installs
//! work without a daemon.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bogyman_core::{Event, Reducer, ReducerOptions, Session, State, Vendor};
use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("time: {0}")]
    Time(#[from] time::error::Parse),
}

pub type Result<T> = std::result::Result<T, StoreError>;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS events (
  id         INTEGER PRIMARY KEY,
  at         TEXT NOT NULL,
  vendor     TEXT NOT NULL,
  session_id TEXT NOT NULL,
  source     TEXT NOT NULL,
  kind       TEXT NOT NULL,
  body       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_session ON events(vendor, session_id, id);
CREATE INDEX IF NOT EXISTS events_at ON events(at);
CREATE TABLE IF NOT EXISTS sessions (
  key         TEXT PRIMARY KEY,
  state       TEXT NOT NULL,
  observed_at TEXT NOT NULL,
  body        TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#;

const META_LAST_EVENT: &str = "last_materialized_event_id";

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

fn ts(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

impl Store {
    /// Open (creating if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Store {
            conn,
            path: path.to_path_buf(),
        })
    }

    pub fn in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Store {
            conn,
            path: PathBuf::from(":memory:"),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The hook path: one INSERT, nothing else.
    pub fn append(&self, ev: &Event) -> Result<i64> {
        let body = serde_json::to_string(ev)?;
        self.conn.execute(
            "INSERT INTO events (at, vendor, session_id, source, kind, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                ts(ev.at),
                ev.vendor.label(),
                ev.vendor_session_id,
                serde_json::to_value(ev.source)?.as_str().unwrap_or("hook"),
                ev.kind.label(),
                body
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn read_events(&self, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(p, |row| {
            let id: i64 = row.get(0)?;
            let body: String = row.get(1)?;
            Ok((id, body))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, body) = r?;
            let mut ev: Event = serde_json::from_str(&body)?;
            ev.id = Some(id);
            out.push(ev);
        }
        Ok(out)
    }

    pub fn events_after(&self, id: i64) -> Result<Vec<Event>> {
        self.read_events(
            "SELECT id, body FROM events WHERE id > ?1 ORDER BY id ASC",
            &[&id],
        )
    }

    /// Newest-first events for one session.
    pub fn events_for(&self, vendor: Vendor, session_id: &str, limit: usize) -> Result<Vec<Event>> {
        self.read_events(
            "SELECT id, body FROM events WHERE vendor = ?1 AND session_id = ?2 ORDER BY id DESC LIMIT ?3",
            &[&vendor.label(), &session_id, &(limit as i64)],
        )
    }

    /// Newest-first events across all sessions.
    pub fn recent_events(&self, limit: usize) -> Result<Vec<Event>> {
        self.read_events(
            "SELECT id, body FROM events ORDER BY id DESC LIMIT ?1",
            &[&(limit as i64)],
        )
    }

    pub fn event_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?)
    }

    /// Newest event per vendor: (vendor, at, kind label).
    pub fn last_event_per_vendor(&self) -> Result<Vec<(Vendor, OffsetDateTime, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT vendor, MAX(at), kind FROM events GROUP BY vendor")?;
        let rows = stmt.query_map([], |row| {
            let v: String = row.get(0)?;
            let at: String = row.get(1)?;
            let kind: String = row.get(2)?;
            Ok((v, at, kind))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (v, at, kind) = r?;
            if let Some(vendor) = Vendor::parse(&v) {
                out.push((vendor, OffsetDateTime::parse(&at, &Rfc3339)?, kind));
            }
        }
        Ok(out)
    }

    fn meta_i64(&self, key: &str) -> Result<i64> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
    }

    pub fn last_materialized_event_id(&self) -> Result<i64> {
        self.meta_i64(META_LAST_EVENT)
    }

    pub fn load_snapshot(&self) -> Result<HashMap<String, Session>> {
        let mut stmt = self.conn.prepare("SELECT key, body FROM sessions")?;
        let rows = stmt.query_map([], |row| {
            let k: String = row.get(0)?;
            let b: String = row.get(1)?;
            Ok((k, b))
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let (k, b) = r?;
            match serde_json::from_str::<Session>(&b) {
                Ok(s) => {
                    out.insert(k, s);
                }
                Err(_) => {
                    // An unreadable snapshot row (older schema) is dropped; it will be
                    // rebuilt from events on the next full fold.
                }
            }
        }
        Ok(out)
    }

    pub fn save_snapshot(&mut self, sessions: &[Session], last_event_id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM sessions", [])?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO sessions (key, state, observed_at, body) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for s in sessions {
                ins.execute(params![
                    s.store_key(),
                    s.state.label(),
                    ts(s.observed_at),
                    serde_json::to_string(s)?
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![META_LAST_EVENT, last_event_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Fold any events newer than the last materialization into the snapshot,
    /// finalize (stale flags, liveness) and persist. Returns every known session.
    pub fn materialize(
        &mut self,
        opts: ReducerOptions,
        now: OffsetDateTime,
        pid_alive: impl Fn(u32) -> bool,
    ) -> Result<Vec<Session>> {
        let last = self.last_materialized_event_id()?;
        let snapshot = self.load_snapshot()?;
        let new_events = self.events_after(last)?;
        let newest = new_events.iter().filter_map(|e| e.id).max().unwrap_or(last);
        let mut reducer = Reducer::new(snapshot, opts);
        reducer.apply_all(new_events.iter());
        reducer.finalize(now, pid_alive);
        let mut sessions = reducer.into_sessions();
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        self.save_snapshot(&sessions, newest)?;
        Ok(sessions)
    }

    /// Drop events older than `retention_days`, and non-live sessions not observed since then.
    pub fn gc(&mut self, retention_days: u32, now: OffsetDateTime) -> Result<(usize, usize)> {
        let cutoff = ts(now - time::Duration::days(retention_days as i64));
        let tx = self.conn.transaction()?;
        let events = tx.execute("DELETE FROM events WHERE at < ?1", [&cutoff])?;
        let live: Vec<&str> = [State::Working, State::Blocked, State::Done, State::Unknown]
            .iter()
            .map(|s| s.label())
            .collect();
        let sessions = tx.execute(
            "DELETE FROM sessions WHERE observed_at < ?1 AND state NOT IN (?2, ?3, ?4, ?5)",
            params![cutoff, live[0], live[1], live[2], live[3]],
        )?;
        tx.commit()?;
        Ok((events, sessions))
    }

    /// Clear everything. Used by tests and `doctor --reset`.
    pub fn reset(&mut self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM events; DELETE FROM sessions; DELETE FROM meta;")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bogyman_core::{EventKind, Source};
    use time::macros::datetime;

    fn ev(at: OffsetDateTime, sid: &str, kind: EventKind) -> Event {
        Event {
            id: None,
            at,
            vendor: Vendor::ClaudeCode,
            vendor_session_id: sid.into(),
            source: Source::Hook,
            kind,
            cwd: Some("/tmp/x".into()),
            pid: None,
            transcript_path: None,
            raw: serde_json::json!({"hook_event_name": "x"}),
        }
    }

    const T0: OffsetDateTime = datetime!(2026-10-26 21:00:00 UTC);

    #[test]
    fn append_then_materialize_incrementally() {
        let mut store = Store::in_memory().unwrap();
        store
            .append(&ev(T0, "aaaa", EventKind::PromptSubmitted))
            .unwrap();
        store
            .append(&ev(
                T0 + time::Duration::seconds(5),
                "aaaa",
                EventKind::PermissionRequest {
                    tool: "Bash".into(),
                    detail: Some("ls".into()),
                },
            ))
            .unwrap();
        let s = store
            .materialize(
                ReducerOptions::default(),
                T0 + time::Duration::seconds(10),
                |_| true,
            )
            .unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].state, State::Blocked);
        assert_eq!(store.last_materialized_event_id().unwrap(), 2);

        // A later event is folded onto the stored snapshot, not from scratch.
        store
            .append(&ev(
                T0 + time::Duration::seconds(20),
                "aaaa",
                EventKind::Stop { message: None },
            ))
            .unwrap();
        let s = store
            .materialize(
                ReducerOptions::default(),
                T0 + time::Duration::seconds(30),
                |_| true,
            )
            .unwrap();
        assert_eq!(s[0].state, State::Done);
        assert_eq!(s[0].turns, 1);
        assert_eq!(store.last_materialized_event_id().unwrap(), 3);
        assert_eq!(
            store
                .events_for(Vendor::ClaudeCode, "aaaa", 20)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn gc_prunes_old_events_and_dead_sessions() {
        let mut store = Store::in_memory().unwrap();
        let old = T0 - time::Duration::days(10);
        store
            .append(&ev(old, "old1", EventKind::PromptSubmitted))
            .unwrap();
        store
            .append(&ev(
                old + time::Duration::seconds(1),
                "old1",
                EventKind::SessionEnd { reason: None },
            ))
            .unwrap();
        store
            .append(&ev(T0, "new1", EventKind::PromptSubmitted))
            .unwrap();
        store
            .materialize(ReducerOptions::default(), T0, |_| true)
            .unwrap();
        let (e, s) = store.gc(7, T0).unwrap();
        assert_eq!((e, s), (2, 1));
        assert_eq!(store.event_count().unwrap(), 1);
        assert_eq!(store.load_snapshot().unwrap().len(), 1);
    }

    #[test]
    fn opens_on_disk_in_wal_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("bogyman.db");
        let store = Store::open(&path).unwrap();
        let mode: String = store
            .conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        assert!(path.exists());
    }
}

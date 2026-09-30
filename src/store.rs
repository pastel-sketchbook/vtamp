use crate::{
    library::{Record, normalized},
    model::{PlaybackStatus, State, Track},
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 1 {
            bail!("Database was created by a newer vtamp; please upgrade");
        }
        db.execute_batch("BEGIN;
            CREATE TABLE IF NOT EXISTS session (id INTEGER PRIMARY KEY CHECK(id = 1), json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS roots (path TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS tracks (id TEXT PRIMARY KEY, path TEXT UNIQUE NOT NULL, search TEXT NOT NULL, json TEXT NOT NULL);
            PRAGMA user_version = 1;
            COMMIT;")?;
        Ok(Self { db })
    }
    pub fn restore(&self) -> Result<State> {
        let saved: Option<String> = self
            .db
            .query_row("SELECT json FROM session WHERE id=1", [], |r| r.get(0))
            .optional()?;
        let mut state: State = saved
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .context("Saved session is invalid")?
            .unwrap_or_default();
        state.status = if state.current().is_some() {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Stopped
        };
        state.scanning = false;
        state.volume = state.volume.min(100);
        Ok(state)
    }
    pub fn save(&self, state: &State) -> Result<()> {
        self.db.execute("INSERT INTO session(id,json) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=excluded.json", [serde_json::to_string(state)?])?;
        Ok(())
    }
    pub fn roots(&self) -> Result<Vec<PathBuf>> {
        Ok(self
            .db
            .prepare("SELECT path FROM roots ORDER BY path")?
            .query_map([], |r| r.get::<_, String>(0).map(PathBuf::from))?
            .collect::<Result<_, _>>()?)
    }
    pub fn add_root(&self, path: &Path) -> Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO roots(path) VALUES(?1)",
            [path.to_string_lossy().as_ref()],
        )?;
        Ok(())
    }
    pub fn remove_root(&self, path: &Path) -> Result<()> {
        self.db.execute(
            "DELETE FROM roots WHERE path=?1",
            [path.to_string_lossy().as_ref()],
        )?;
        Ok(())
    }
    pub fn records(&self) -> Result<Vec<Record>> {
        let strings = self
            .db
            .prepare("SELECT json FROM tracks ORDER BY path")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        strings
            .into_iter()
            .map(|s| Ok(serde_json::from_str(&s)?))
            .collect()
    }
    pub fn replace_catalog(&mut self, records: &[Record]) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM tracks", [])?;
        {
            let mut stmt =
                tx.prepare("INSERT INTO tracks(id,path,search,json) VALUES(?1,?2,?3,?4)")?;
            for record in records {
                let t = &record.track;
                stmt.execute(params![
                    t.id,
                    t.path.to_string_lossy(),
                    normalized(&format!("{} {} {}", t.title, t.artist, t.album)),
                    serde_json::to_string(record)?
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn search(&self, query: &str, offset: usize, limit: usize) -> Result<(Vec<Track>, usize)> {
        let query = normalized(query);
        let total: i64 = self.db.query_row(
            "SELECT count(*) FROM tracks WHERE instr(search,?1)>0",
            [&query],
            |r| r.get(0),
        )?;
        let strings = self.db.prepare("SELECT json FROM tracks WHERE instr(search,?1)>0 ORDER BY search,path LIMIT ?2 OFFSET ?3")?
            .query_map(params![query, limit.clamp(1, 1000) as i64, i64::try_from(offset)?], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        let tracks = strings
            .into_iter()
            .map(|s| serde_json::from_str::<Record>(&s).map(|r| r.track))
            .collect::<Result<_, _>>()?;
        Ok((tracks, total as usize))
    }
    pub fn track(&self, id: &str) -> Result<Option<Track>> {
        let json: Option<String> = self
            .db
            .query_row("SELECT json FROM tracks WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        Ok(json
            .map(|s| serde_json::from_str::<Record>(&s).map(|r| r.track))
            .transpose()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::QueueItem;
    #[test]
    fn persisted_playback_restores_paused() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(&dir.path().join("test.db")).unwrap();
        let item = QueueItem::new(Track {
            id: "track".into(),
            path: "/music.m4a".into(),
            title: "Music".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            track_number: 1,
            duration_ms: 30000,
            cover: None,
        });
        let state = State {
            current_id: Some(item.id.clone()),
            queue: vec![item],
            status: PlaybackStatus::Playing,
            position_ms: 12000,
            volume: 42,
            ..State::default()
        };
        db.save(&state).unwrap();
        let restored = db.restore().unwrap();
        assert_eq!(restored.status, PlaybackStatus::Paused);
        assert_eq!(restored.position_ms, 12000);
        assert_eq!(restored.volume, 42);
    }
}

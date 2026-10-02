mod imports;
use crate::{
    library::{Record, normalized, search_blob},
    model::{ApiError, PlaybackStatus, Reply, ScanJob, SearchFilter, State, Track},
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

pub struct Store {
    db: Connection,
}

#[derive(serde::Serialize)]
pub struct LibraryPage {
    pub tracks: Vec<Track>,
    pub total: usize,
    pub offset: usize,
    pub query: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let mut db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 6 {
            bail!("Database was created by a newer vtamp; please upgrade");
        }
        if version == 0 {
            db.execute_batch("BEGIN;
                CREATE TABLE session (id INTEGER PRIMARY KEY CHECK(id = 1), json TEXT NOT NULL);
                CREATE TABLE roots (path TEXT PRIMARY KEY);
                CREATE TABLE tracks (id TEXT PRIMARY KEY, path TEXT UNIQUE NOT NULL, search TEXT NOT NULL, json TEXT NOT NULL);
                PRAGMA user_version = 1;
                COMMIT;")?;
        }
        if version < 2 {
            let tx = db.transaction()?;
            tx.execute_batch("ALTER TABLE tracks ADD COLUMN title_search TEXT NOT NULL DEFAULT '';
                ALTER TABLE tracks ADD COLUMN artist_search TEXT NOT NULL DEFAULT '';
                ALTER TABLE tracks ADD COLUMN album_search TEXT NOT NULL DEFAULT '';
                CREATE TABLE requests (id TEXT PRIMARY KEY, payload TEXT NOT NULL, reply TEXT NOT NULL, expires_ms INTEGER NOT NULL);
                CREATE INDEX requests_expiry ON requests(expires_ms);
                CREATE TABLE scan_jobs (id TEXT PRIMARY KEY, json TEXT NOT NULL, finished_ms INTEGER);
                PRAGMA user_version = 2;")?;
            let rows = tx
                .prepare("SELECT json FROM tracks")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for json in rows {
                let record: Record = serde_json::from_str(&json)?;
                let t = record.track;
                tx.execute("UPDATE tracks SET title_search=?1,artist_search=?2,album_search=?3 WHERE id=?4",
                    params![normalized(&t.title), normalized(&t.artist), normalized(&t.album), t.id])?;
            }
            tx.commit()?;
        }
        if version < 3 {
            db.execute_batch("BEGIN;
                CREATE TABLE import_jobs(id TEXT PRIMARY KEY,json TEXT NOT NULL,request TEXT NOT NULL,finished_ms INTEGER);
                CREATE TABLE import_items(job_id TEXT NOT NULL,position INTEGER NOT NULL,json TEXT NOT NULL,PRIMARY KEY(job_id,position));
                CREATE TABLE track_metadata(id TEXT PRIMARY KEY,video_id TEXT UNIQUE,manifest TEXT,metadata TEXT NOT NULL,title_override TEXT,artist_override TEXT);
                PRAGMA user_version = 3;
                COMMIT;")?;
        }
        if version < 4 {
            let tx = db.transaction()?;
            tx.execute_batch("ALTER TABLE track_metadata ADD COLUMN album_override TEXT;")?;
            imports::migrate_albums(&tx)?;
            tx.pragma_update(None, "user_version", 4)?;
            tx.commit()?;
        }
        if version < 5 {
            // Older binaries cannot restore a current item outside the queue.
            db.execute_batch("BEGIN; PRAGMA user_version = 5; COMMIT;")?;
        }
        if version < 6 {
            db.execute_batch("BEGIN;
                CREATE TABLE IF NOT EXISTS streams(id TEXT PRIMARY KEY, path TEXT UNIQUE NOT NULL, search TEXT NOT NULL, json TEXT NOT NULL, title_search TEXT NOT NULL, artist_search TEXT NOT NULL, album_search TEXT NOT NULL);
                CREATE VIEW IF NOT EXISTS catalog AS SELECT * FROM tracks UNION ALL SELECT * FROM streams;
                PRAGMA user_version = 6;
                COMMIT;")?;
        }
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
        state.scheduled_stop = None;
        state.stream_status = None;
        if state.current().is_some_and(|item| item.track.is_live()) {
            state.position_ms = 0;
        }
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
    pub(crate) fn delete_import(&mut self, id: &str) -> Result<()> {
        let tx = self.db.transaction()?;
        if tx.execute("DELETE FROM tracks WHERE id=?1", [id])? == 0 {
            return Err(ApiError::new("track_not_found", "Library track not found").into());
        }
        tx.execute("DELETE FROM track_metadata WHERE id=?1", [id])?;
        tx.commit()?;
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
        write_catalog(&tx, records)?;
        tx.commit()?;
        Ok(())
    }
    pub fn search(&self, query: &str, offset: usize, limit: usize) -> Result<(Vec<Track>, usize)> {
        let query = normalized(query);
        let total: i64 = self.db.query_row(
            "SELECT count(*) FROM catalog WHERE instr(search,?1)>0",
            [&query],
            |r| r.get(0),
        )?;
        let strings = self.db.prepare("SELECT json FROM catalog WHERE instr(search,?1)>0 ORDER BY search,path LIMIT ?2 OFFSET ?3")?
            .query_map(params![query, limit.clamp(1, 1000) as i64, i64::try_from(offset)?], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        let tracks = strings
            .into_iter()
            .map(|s| serde_json::from_str::<Record>(&s).map(|r| r.track))
            .collect::<Result<_, _>>()?;
        Ok((tracks, total as usize))
    }
    /// Locate an identity in the same ordering as search, without loading the
    /// whole catalog. Drop the filter only when it would hide the target.
    pub fn search_around(&self, query: &str, id: &str, limit: usize) -> Result<LibraryPage> {
        let (search, path): (String, String) = self
            .db
            .query_row("SELECT search,path FROM catalog WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?
            .ok_or_else(|| ApiError::new("track_not_found", "Library track not found"))?;
        let query = if search.contains(&normalized(query)) {
            query
        } else {
            ""
        };
        let rank: i64 = self.db.query_row(
            "SELECT count(*) FROM catalog WHERE instr(search,?1)>0 AND (search,path)<(?2,?3)",
            params![normalized(query), search, path],
            |r| r.get(0),
        )?;
        let limit = limit.clamp(1, 1000);
        let offset = usize::try_from(rank)? / limit * limit;
        let (tracks, total) = self.search(query, offset, limit)?;
        Ok(LibraryPage {
            tracks,
            total,
            offset,
            query: query.into(),
        })
    }
    pub fn search_filtered(
        &self,
        filter: &SearchFilter,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<Track>, usize)> {
        if filter.exact
            && filter.title.is_none()
            && filter.artist.is_none()
            && filter.album.is_none()
        {
            return Err(ApiError::new(
                "invalid_arguments",
                "Exact matching requires a field filter",
            )
            .into());
        }
        let mut predicates = vec!["instr(search,?)>0".to_string()];
        let mut values = vec![normalized(&filter.query)];
        for (column, value) in [
            ("title_search", &filter.title),
            ("artist_search", &filter.artist),
            ("album_search", &filter.album),
        ] {
            if let Some(value) = value {
                predicates.push(if filter.exact {
                    format!("{column}=?")
                } else {
                    format!("instr({column},?)>0")
                });
                values.push(normalized(value));
            }
        }
        for value in &filter.exclude {
            predicates.push("instr(search,?)=0".into());
            values.push(normalized(value));
        }
        let condition = predicates.join(" AND ");
        let total: i64 = self.db.query_row(
            &format!("SELECT count(*) FROM catalog WHERE {condition}"),
            rusqlite::params_from_iter(&values),
            |row| row.get(0),
        )?;
        let offset = i64::try_from(offset)?;
        let sql = format!(
            "SELECT json FROM catalog WHERE {condition} ORDER BY search,path LIMIT {} OFFSET {offset}",
            limit.clamp(1, 1000)
        );
        let records = self
            .db
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(&values), |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((
            records
                .into_iter()
                .map(|s| serde_json::from_str::<Record>(&s).map(|r| r.track))
                .collect::<Result<Vec<_>, _>>()?,
            total as usize,
        ))
    }

    pub fn replay(&self, id: &str, payload: &str, now: u64) -> Result<Option<Reply>> {
        let row: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT payload,reply FROM requests WHERE id=?1 AND expires_ms>?2",
                params![id, i64::try_from(now)?],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((previous, reply)) if previous == payload => {
                let mut reply: Reply = serde_json::from_str(&reply)?;
                // The result is historical, but its envelope uses this server's
                // protocol even when the receipt predates a database upgrade.
                reply.version = crate::model::PROTOCOL_VERSION;
                Ok(Some(reply))
            }
            Some(_) => Err(ApiError::new(
                "request_id_conflict",
                "Request ID already belongs to a different edit",
            )
            .into()),
            None => Ok(None),
        }
    }

    /// Persist the candidate and receipt together before publishing any in-memory change.
    pub fn commit_edit(
        &mut self,
        state: &State,
        receipt: Option<(&str, &str, &Reply)>,
        now: u64,
    ) -> Result<()> {
        let tx = self.db.transaction()?;
        if let Some((id, payload, reply)) = receipt {
            tx.execute(
                "DELETE FROM requests WHERE expires_ms<=?1",
                [i64::try_from(now)?],
            )?;
            let count: i64 = tx.query_row("SELECT count(*) FROM requests", [], |r| r.get(0))?;
            if count >= 10_000 {
                return Err(ApiError::new(
                    "request_log_full",
                    "Unexpired request log is full; retry after records expire",
                )
                .into());
            }
            tx.execute(
                "INSERT INTO requests(id,payload,reply,expires_ms) VALUES(?1,?2,?3,?4)",
                params![
                    id,
                    payload,
                    serde_json::to_string(reply)?,
                    i64::try_from(now.saturating_add(86_400_000))?
                ],
            )?;
        }
        tx.execute("INSERT INTO session(id,json) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=excluded.json", [serde_json::to_string(state)?])?;
        tx.commit()?;
        Ok(())
    }

    pub fn save_scan(&mut self, job: &ScanJob, records: Option<&[Record]>) -> Result<()> {
        let tx = self.db.transaction()?;
        if let Some(records) = records {
            write_catalog(&tx, records)?;
        }
        tx.execute("INSERT INTO scan_jobs(id,json,finished_ms) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET json=excluded.json,finished_ms=excluded.finished_ms",
            params![job.job_id, serde_json::to_string(job)?, job.finished_at_ms.map(i64::try_from).transpose()?])?;
        tx.execute("DELETE FROM scan_jobs WHERE finished_ms IS NOT NULL AND id NOT IN (SELECT id FROM scan_jobs WHERE finished_ms IS NOT NULL ORDER BY finished_ms DESC,rowid DESC LIMIT 100)", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn scan_job(&self, id: &str) -> Result<ScanJob> {
        let json: Option<String> = self
            .db
            .query_row("SELECT json FROM scan_jobs WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        match json {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Err(ApiError::new(
                "scan_not_found",
                "Scan job not found or no longer retained",
            )
            .into()),
        }
    }

    pub fn interrupt_scans(&mut self, now: u64) -> Result<()> {
        let rows = self
            .db
            .prepare("SELECT json FROM scan_jobs WHERE finished_ms IS NULL")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for json in rows {
            let mut job: ScanJob = serde_json::from_str(&json)?;
            job.status = "interrupted".into();
            job.finished_at_ms = Some(now);
            job.error = Some("Server restarted before the scan completed".into());
            self.save_scan(&job, None)?;
        }
        Ok(())
    }

    pub fn track(&self, id: &str) -> Result<Option<Track>> {
        let json: Option<String> = self
            .db
            .query_row("SELECT json FROM catalog WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        Ok(json
            .map(|s| serde_json::from_str::<Record>(&s).map(|r| r.track))
            .transpose()?)
    }

    /// Point a track at a regenerated cover file without touching its metadata.
    pub fn set_cover(&mut self, id: &str, cover: &Path) -> Result<Track> {
        let json: String = self
            .db
            .query_row("SELECT json FROM tracks WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| ApiError::new("track_not_found", "Library track not found"))?;
        let mut record: Record = serde_json::from_str(&json)?;
        record.track.cover = Some(cover.to_owned());
        let tx = self.db.transaction()?;
        imports::write_record(&tx, &record)?;
        tx.commit()?;
        Ok(record.track)
    }

    pub fn add_streams(&mut self, entries: &[crate::streams::Entry]) -> Result<serde_json::Value> {
        anyhow::ensure!(
            !entries.is_empty() && entries.len() <= crate::streams::MAX_ENTRIES,
            "Register 1–1000 channels at a time"
        );
        let entries = entries
            .iter()
            .map(crate::streams::Entry::validated)
            .collect::<Result<Vec<_>>>()?;
        let tx = self.db.transaction()?;
        let mut added = Vec::new();
        let mut existing = 0;
        let mut first_registered_id: Option<String> = None;
        for entry in entries {
            let track = entry.track();
            let record = Record {
                track: track.clone(),
                modified: 0,
                bytes: 0,
            };
            let count = tx.execute("INSERT INTO streams(id,path,search,json,title_search,artist_search,album_search) VALUES(?1,?2,?3,?4,?3,'','') ON CONFLICT(path) DO NOTHING", params![track.id, entry.url, normalized(&entry.name), serde_json::to_string(&record)?])?;
            if first_registered_id.is_none() {
                first_registered_id = Some(tx.query_row(
                    "SELECT id FROM streams WHERE path=?1",
                    [&entry.url],
                    |row| row.get(0),
                )?);
            }
            if count == 0 {
                existing += 1;
            } else {
                added.push(track);
            }
        }
        tx.commit()?;
        Ok(
            serde_json::json!({"added":added.len(),"existing":existing,"tracks":added,"first_registered_id":first_registered_id}),
        )
    }
    pub fn remove_stream(&self, id: &str) -> Result<()> {
        if self.db.execute("DELETE FROM streams WHERE id=?1", [id])? == 0 {
            return Err(ApiError::new("track_not_found", "Registered stream not found").into());
        }
        Ok(())
    }
}

fn write_catalog(tx: &rusqlite::Transaction<'_>, records: &[Record]) -> Result<()> {
    let mut ids = std::collections::HashSet::new();
    let mut paths = std::collections::HashSet::new();
    for record in records {
        anyhow::ensure!(
            ids.insert(&record.track.id)
                && paths.insert(
                    record
                        .track
                        .playback
                        .file()
                        .context("Catalog records must reference files")?
                ),
            "Duplicate track in catalog"
        );
    }
    tx.execute("DELETE FROM tracks", [])?;
    for record in records {
        let mut record = record.clone();
        imports::apply_metadata(tx, &mut record.track)?;
        imports::write_record(tx, &record)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::QueueItem;
    #[test]
    fn stream_registration_survives_scans_and_uses_unified_paging() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("state.db")).unwrap();
        let entry = crate::streams::Entry {
            name: "사랑 Radio".into(),
            url: "https://example.com/live".into(),
        };
        let result = store.add_streams(&[entry.clone(), entry.clone()]).unwrap();
        assert_eq!(result["added"], 1);
        assert_eq!(result["existing"], 1);
        let id = result["tracks"][0]["id"].as_str().unwrap();
        assert_eq!(result["first_registered_id"], id);
        let duplicate = store.add_streams(&[entry]).unwrap();
        assert_eq!(duplicate["first_registered_id"], id);
        assert_eq!(duplicate["added"], 0);
        store
            .replace_catalog(&[record("file", "A", "Artist", "")])
            .unwrap();
        assert_eq!(store.search("", 0, 10).unwrap().1, 2);
        let page = store.search_around("", id, 1).unwrap();
        assert_eq!(page.tracks[0].id, id);
        assert_eq!(page.offset, 1);
        assert_eq!(
            store
                .search_filtered(
                    &SearchFilter {
                        title: Some("사랑".into()),
                        ..Default::default()
                    },
                    0,
                    10
                )
                .unwrap()
                .1,
            1
        );
        store.replace_catalog(&[]).unwrap();
        assert!(store.track(id).unwrap().unwrap().is_live());
        assert!(store.records().unwrap().is_empty());
        store.remove_stream(id).unwrap();
        assert!(store.track(id).unwrap().is_none());
    }
    #[test]
    fn stream_batch_failure_rolls_back_every_registration() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("state.db")).unwrap();
        store.db.execute_batch("CREATE TRIGGER fail_stream BEFORE INSERT ON streams WHEN NEW.title_search='fail' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        let entries = [
            crate::streams::Entry {
                name: "Good".into(),
                url: "https://example.com/1".into(),
            },
            crate::streams::Entry {
                name: "Fail".into(),
                url: "https://example.com/2".into(),
            },
        ];
        assert!(store.add_streams(&entries).is_err());
        assert_eq!(store.search("", 0, 10).unwrap().1, 0);
    }
    #[test]
    fn v5_file_sessions_migrate_and_live_sessions_restore_without_position() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = Store::open(&path).unwrap();
        store
            .replace_catalog(&[record("stable", "File", "Artist", "")])
            .unwrap();
        let item = QueueItem::new(store.track("stable").unwrap().unwrap());
        let mut state = State {
            current_id: Some(item.id.clone()),
            queue: vec![item],
            position_ms: 150,
            ..Default::default()
        };
        store.save(&state).unwrap();
        store
            .db
            .execute_batch("DROP VIEW catalog; DROP TABLE streams; PRAGMA user_version=5;")
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.restore().unwrap().current().unwrap().track.id,
            "stable"
        );
        assert_eq!(store.restore().unwrap().position_ms, 150);
        state.queue[0].track = crate::streams::Entry {
            name: "Radio".into(),
            url: "https://example.com/live".into(),
        }
        .track();
        state.status = PlaybackStatus::Playing;
        state.stream_status = Some(crate::model::StreamStatus::Live);
        store.save(&state).unwrap();
        let restored = store.restore().unwrap();
        assert_eq!(restored.status, PlaybackStatus::Paused);
        assert_eq!(restored.position_ms, 0);
        assert_eq!(restored.stream_status, None);
        assert!(restored.now()["duration_ms"].is_null());
    }
    #[test]
    fn old_receipt_replays_with_current_protocol_without_losing_its_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let db = Store::open(&path).unwrap();
        let mut reply = Reply::success(serde_json::json!({"queue_revision": 7}));
        reply.version = 4;
        db.db
            .execute(
                "INSERT INTO requests VALUES(?1,?2,?3,?4)",
                params![
                    "receipt",
                    "payload",
                    serde_json::to_string(&reply).unwrap(),
                    1000
                ],
            )
            .unwrap();
        db.db.pragma_update(None, "user_version", 4).unwrap();
        drop(db);
        let db = Store::open(&path).unwrap();
        let replay = db.replay("receipt", "payload", 500).unwrap().unwrap();
        assert_eq!(replay.version, crate::model::PROTOCOL_VERSION);
        assert_eq!(replay.data, reply.data);
        assert!(db.replay("receipt", "changed", 500).is_err());
    }

    #[test]
    fn direct_session_restores_paused_with_its_queue_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let db = Store::open(&path).unwrap();
        let queued = QueueItem::new(record("queued", "Queued", "Artist", "").track);
        let direct = QueueItem::new(record("direct", "Direct", "Artist", "").track);
        let state = State {
            current_id: Some(direct.id.clone()),
            direct: Some(Box::new(direct.clone())),
            queue_cursor: Some(queued.id.clone()),
            queue: vec![queued.clone()],
            status: PlaybackStatus::Playing,
            position_ms: 500,
            volume: 12,
            ..State::default()
        };
        db.save(&state).unwrap();
        drop(db);
        let restored = Store::open(&path).unwrap().restore().unwrap();
        assert_eq!(restored.current(), Some(&direct));
        assert_eq!(restored.queue, std::slice::from_ref(&queued));
        assert_eq!(restored.queue_current_id(), Some(queued.id.as_str()));
        assert_eq!(restored.status, PlaybackStatus::Paused);
        assert_eq!(restored.position_ms, 500);
        assert_eq!(restored.volume, 12);
    }

    #[test]
    fn persisted_playback_restores_paused() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(&dir.path().join("test.db")).unwrap();
        let item = QueueItem::new(Track {
            id: "track".into(),
            playback: crate::model::PlaybackSource::File {
                path: "/music.m4a".into(),
            },
            title: "Music".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            track_number: 1,
            duration_ms: Some(30000),
            cover: None,
            source: None,
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
    fn record(id: &str, title: &str, artist: &str, album: &str) -> Record {
        Record {
            track: Track {
                id: id.into(),
                playback: crate::model::PlaybackSource::File {
                    path: format!("/{id}.wav").into(),
                },
                title: title.into(),
                artist: artist.into(),
                album: album.into(),
                track_number: 1,
                duration_ms: Some(1000),
                cover: None,
                source: None,
            },
            modified: 1,
            bytes: 1,
        }
    }
    #[test]
    fn anchored_library_page_matches_sort_order_and_only_clears_a_hiding_filter() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("state.db")).unwrap();
        // Equal metadata exercises the path tiebreaker as well as later pages.
        let mut records: Vec<_> = (0..450)
            .rev()
            .map(|i| record(&format!("{i:03}"), "Song", "ＡＲＴＩＳＴ", ""))
            .collect();
        records.push(record("other", "Before", "Other", ""));
        store.replace_catalog(&records).unwrap();
        let page = store.search_around("artist", "425", 200).unwrap();
        assert_eq!(page.query, "artist");
        assert_eq!((page.offset, page.total, page.tracks.len()), (400, 450, 50));
        assert_eq!(page.tracks[25].id, "425");
        let all = store.search_around("unrelated search", "425", 200).unwrap();
        assert_eq!(all.query, "");
        assert_eq!((all.offset, all.total), (400, 451));
        let expected = store.search("", 400, 200).unwrap().0;
        assert_eq!(all.tracks, expected);
        assert!(all.tracks.iter().any(|track| track.id == "425"));
        assert!(store.search_around("", "deleted", 200).is_err());
        // Clamp the page size before calculating the page boundary.
        let one = store.search_around("artist", "425", 0).unwrap();
        assert_eq!(one.offset, 425);
        assert_eq!(one.tracks.len(), 1);
        assert_eq!(one.tracks[0].id, "425");
    }

    #[test]
    fn v1_migration_backfills_unicode_search_without_changing_ids() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("old.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE session(id INTEGER PRIMARY KEY,json TEXT NOT NULL); CREATE TABLE roots(path TEXT PRIMARY KEY); CREATE TABLE tracks(id TEXT PRIMARY KEY,path TEXT UNIQUE NOT NULL,search TEXT NOT NULL,json TEXT NOT NULL); PRAGMA user_version=1;").unwrap();
        let r = record("stable", "사랑", "ＤＡＹ６", "Live");
        db.execute(
            "INSERT INTO tracks VALUES(?1,?2,?3,?4)",
            params![
                r.track.id,
                "/stable.wav",
                normalized("사랑 DAY6 Live"),
                serde_json::to_string(&r).unwrap()
            ],
        )
        .unwrap();
        drop(db);
        let store = Store::open(&path).unwrap();
        let filter = SearchFilter {
            title: Some("사랑".into()),
            artist: Some("day6".into()),
            exact: true,
            ..Default::default()
        };
        let (tracks, total) = store.search_filtered(&filter, 0, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(tracks[0].id, "stable");
        assert_eq!(
            store
                .db
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            6
        );
        let filter = SearchFilter {
            exclude: vec!["LIVE".into()],
            ..filter
        };
        assert_eq!(store.search_filtered(&filter, 0, 10).unwrap().1, 0);
    }
    #[test]
    fn filters_pagination_and_atomic_catalog_failure() {
        let home = tempfile::tempdir().unwrap();
        let mut store = Store::open(&home.path().join("state.db")).unwrap();
        let records = vec![
            record("a", "Love", "Artist", "Studio"),
            record("b", "Love live", "Artist", "Live"),
            record("c", "Love", "Else", "Studio"),
        ];
        store.replace_catalog(&records).unwrap();
        let filter = SearchFilter {
            query: "LOVE".into(),
            artist: Some("artist".into()),
            ..Default::default()
        };
        let (page, total) = store.search_filtered(&filter, 1, 1).unwrap();
        assert_eq!(total, 2);
        assert_eq!(page.len(), 1);
        assert!(store.search_filtered(&filter, 100, 1).unwrap().0.is_empty());
        let filter = SearchFilter {
            exclude: vec!["live".into()],
            ..filter
        };
        assert_eq!(store.search_filtered(&filter, 0, 10).unwrap().0[0].id, "a");
        assert_eq!(store.search_filtered(&filter, 0, 10).unwrap().1, 1);
        // A failed replacement must not erase the previous catalog.
        assert!(
            store
                .replace_catalog(&[records[0].clone(), records[0].clone()])
                .is_err()
        );
        assert_eq!(store.search("", 0, 10).unwrap().1, 3);
    }
    #[test]
    fn receipt_expiry_capacity_and_failed_commit_are_transactional() {
        let home = tempfile::tempdir().unwrap();
        let mut store = Store::open(&home.path().join("state.db")).unwrap();
        let reply = Reply::success(serde_json::json!({"applied":true}));
        store
            .commit_edit(&State::default(), Some(("one", "payload", &reply)), 100)
            .unwrap();
        assert!(
            store
                .replay("one", "payload", 86_400_099)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .replay("one", "payload", 86_400_100)
                .unwrap()
                .is_none()
        );
        store.db.execute_batch("DELETE FROM requests; WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO requests SELECT CAST(x AS TEXT),'p','{}',999999999 FROM n;").unwrap();
        let state = State {
            volume: 12,
            ..State::default()
        };
        let err = store
            .commit_edit(&state, Some(("full", "payload", &reply)), 200)
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<ApiError>().unwrap().code,
            "request_log_full"
        );
        assert_eq!(store.restore().unwrap().volume, 70);
        assert!(store.replay("full", "payload", 200).unwrap().is_none());
        store
            .commit_edit(&state, Some(("new", "payload", &reply)), 1_000_000_000)
            .unwrap();
        assert_eq!(store.restore().unwrap().volume, 12);
    }
    #[test]
    fn scan_history_is_bounded_and_restart_interrupts_running_jobs_and_clears_timers() {
        let home = tempfile::tempdir().unwrap();
        let mut store = Store::open(&home.path().join("state.db")).unwrap();
        for i in 0..102 {
            store
                .save_scan(
                    &ScanJob {
                        job_id: i.to_string(),
                        status: "completed".into(),
                        started_at_ms: i,
                        finished_at_ms: Some(i + 1),
                        summary: None,
                        error: None,
                    },
                    None,
                )
                .unwrap();
        }
        assert!(store.scan_job("0").is_err());
        assert!(store.scan_job("2").is_ok());
        store
            .save_scan(
                &ScanJob {
                    job_id: "active".into(),
                    status: "running".into(),
                    started_at_ms: 200,
                    finished_at_ms: None,
                    summary: None,
                    error: None,
                },
                None,
            )
            .unwrap();
        store.interrupt_scans(500).unwrap();
        assert_eq!(store.scan_job("active").unwrap().status, "interrupted");
        assert!(store.scan_job("2").is_err());
        store
            .save(&State {
                scheduled_stop: Some(crate::model::ScheduledStop::Deadline { deadline_ms: 99999 }),
                scanning: true,
                ..State::default()
            })
            .unwrap();
        let state = store.restore().unwrap();
        assert!(state.scheduled_stop.is_none());
        assert!(!state.scanning);
    }
}

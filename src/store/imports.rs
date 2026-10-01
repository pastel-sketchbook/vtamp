use super::*;
use crate::{
    imports::{ImportItem, ImportJob, ImportRequest, Publication},
    metadata::Metadata,
};
use serde_json::{Value, json};
impl Store {
    pub fn create_import(&mut self, job: &ImportJob, request: &ImportRequest) -> Result<()> {
        let active: i64 = self.db.query_row(
            "SELECT count(*) FROM import_jobs WHERE finished_ms IS NULL",
            [],
            |r| r.get(0),
        )?;
        if active >= 32 {
            bail!("At most 32 import jobs may be queued or running");
        }
        self.db.execute(
            "INSERT INTO import_jobs(id,json,request,finished_ms) VALUES(?1,?2,?3,NULL)",
            params![
                job.job_id,
                serde_json::to_string(job)?,
                serde_json::to_string(request)?
            ],
        )?;
        Ok(())
    }
    pub fn import_job(&self, id: &str) -> Result<ImportJob> {
        let s: Option<String> = self
            .db
            .query_row("SELECT json FROM import_jobs WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        serde_json::from_str(&s.ok_or_else(|| {
            ApiError::new(
                "import_not_found",
                "Import job not found or no longer retained",
            )
        })?)
        .map_err(Into::into)
    }
    pub fn import_request(&self, id: &str) -> Result<ImportRequest> {
        let s: String =
            self.db
                .query_row("SELECT request FROM import_jobs WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
        Ok(serde_json::from_str(&s)?)
    }
    pub fn import_jobs(&self) -> Result<Vec<ImportJob>> {
        self.db
            .prepare("SELECT json FROM import_jobs ORDER BY rowid DESC")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn import_items(&self, id: &str, offset: usize, limit: usize) -> Result<Vec<ImportItem>> {
        self.db.prepare("SELECT json FROM import_items WHERE job_id=?1 ORDER BY position LIMIT ?2 OFFSET ?3")?.query_map(params![id,limit.clamp(1,1000) as i64,offset.min(i64::MAX as usize) as i64],|r|r.get::<_,String>(0))?.map(|r|Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn import_status(&self, id: &str, offset: usize, limit: usize) -> Result<Value> {
        Ok(
            json!({"job":self.import_job(id)?,"items":self.import_items(id,offset,limit)?,"offset":offset}),
        )
    }
    pub fn import_plan(&mut self, job: &ImportJob, items: &[ImportItem]) -> Result<()> {
        let tx = self.db.transaction()?;
        save_job(&tx, job)?;
        for item in items {
            save_item(&tx, &job.job_id, item)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn save_import(&mut self, job: &ImportJob, item: Option<&ImportItem>) -> Result<()> {
        let tx = self.db.transaction()?;
        save_job(&tx, job)?;
        if let Some(item) = item {
            save_item(&tx, &job.job_id, item)?;
        }
        if job.terminal() {
            let pending = tx
                .prepare("SELECT json FROM import_items WHERE job_id=?1")?
                .query_map([&job.job_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for s in pending {
                let mut item: ImportItem = serde_json::from_str(&s)?;
                if !matches!(item.status.as_str(), "completed" | "skipped" | "failed") {
                    item.status = job.status.clone();
                    save_item(&tx, &job.job_id, &item)?;
                }
            }
            tx.execute("DELETE FROM import_items WHERE job_id IN (SELECT id FROM import_jobs WHERE finished_ms IS NOT NULL ORDER BY finished_ms DESC,rowid DESC LIMIT -1 OFFSET 100)",[])?;
            tx.execute("DELETE FROM import_jobs WHERE id IN (SELECT id FROM import_jobs WHERE finished_ms IS NOT NULL ORDER BY finished_ms DESC,rowid DESC LIMIT -1 OFFSET 100)",[])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn interrupt_imports(&mut self) -> Result<()> {
        for mut job in self.import_jobs()?.into_iter().filter(|j| !j.terminal()) {
            job.error =
                Some("Server restarted before import completed; retry unfinished items".into());
            job.finish("interrupted");
            self.save_import(&job, None)?;
        }
        Ok(())
    }
    pub fn retry_import(&self, id: &str) -> Result<ImportRequest> {
        let job = self.import_job(id)?;
        if !job.terminal() {
            bail!("Import is still running");
        }
        let mut request = self.import_request(id)?;
        let strings = self
            .db
            .prepare("SELECT json FROM import_items WHERE job_id=?1 ORDER BY position")?
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if !strings.is_empty() {
            let mut ids = Vec::new();
            for s in strings {
                let item: ImportItem = serde_json::from_str(&s)?;
                if !matches!(item.status.as_str(), "completed" | "skipped")
                    && crate::youtube::valid_id(&item.video_id)
                {
                    ids.push(item.video_id);
                }
            }
            if ids.is_empty() {
                bail!("No retryable unfinished videos");
            }
            request.video_ids = Some(ids);
        }
        Ok(request)
    }
    pub fn video_record(&self, id: &str) -> Result<Option<Record>> {
        let result:Option<String>=self.db.query_row("SELECT tracks.json FROM track_metadata JOIN tracks ON track_metadata.id=tracks.id WHERE video_id=?1",[id],|r|r.get(0)).optional()?;
        if let Some(s) = result {
            return Ok(Some(serde_json::from_str(&s)?));
        }
        // The file may have disappeared during a scan; keep its original library identity.
        let manifest: Option<String> = self
            .db
            .query_row(
                "SELECT manifest FROM track_metadata WHERE video_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(s) = manifest {
            let m: crate::imports::Manifest = serde_json::from_str(&s)?;
            return Ok(Some(Record {
                track: Track {
                    id: m.track_id,
                    playback: crate::model::PlaybackSource::File {
                        path: PathBuf::new(),
                    },
                    title: m.metadata.title,
                    artist: m.metadata.artist,
                    album: m.source.music_album.clone().unwrap_or_default(),
                    duration_ms: Some(0),
                    track_number: 0,
                    cover: None,
                    source: Some(m.source),
                },
                modified: 0,
                bytes: 0,
            }));
        }
        Ok(None)
    }
    pub fn commit_import(&mut self, p: &mut Publication) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO track_metadata(id,video_id,manifest,metadata,title_override,artist_override) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET manifest=excluded.manifest,metadata=excluded.metadata,title_override=coalesce(track_metadata.title_override,excluded.title_override),artist_override=coalesce(track_metadata.artist_override,excluded.artist_override)",params![p.manifest.track_id,p.manifest.source.video_id,serde_json::to_string(&p.manifest)?,serde_json::to_string(&p.manifest.metadata)?,p.manifest.title_override,p.manifest.artist_override])?;
        apply_metadata(&tx, &mut p.record.track)?;
        write_record(&tx, &p.record)?;
        save_job(&tx, &p.job)?;
        save_item(&tx, &p.job.job_id, &p.item)?;
        tx.commit()?;
        Ok(())
    }
    pub fn edit_metadata(
        &mut self,
        id: &str,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        automatic: Option<Metadata>,
    ) -> Result<Track> {
        if title.is_none() && artist.is_none() && album.is_none() && automatic.is_none() {
            bail!("Specify a title, artist or album to edit");
        }
        for s in [&title, &artist].into_iter().flatten() {
            crate::imports::validate_text(s)?;
        }
        if let Some(s) = &album
            && (s.len() > 2048 || s.chars().any(char::is_control))
        {
            bail!("Album must be at most 2048 bytes and contain no control characters");
        }
        let s: String = self
            .db
            .query_row("SELECT json FROM tracks WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| ApiError::new("track_not_found", "Library track not found"))?;
        let mut record: Record = serde_json::from_str(&s)?;
        let tx = self.db.transaction()?;
        let metadata = automatic.unwrap_or_else(|| Metadata {
            title: record.track.title.clone(),
            artist: record.track.artist.clone(),
            method: "manual".into(),
            warning: None,
        });
        // Manual edits must not replace the saved automatic result.
        tx.execute(
            "INSERT OR IGNORE INTO track_metadata(id,metadata) VALUES(?1,?2)",
            params![id, serde_json::to_string(&metadata)?],
        )?;
        if title.is_none() && artist.is_none() && album.is_none() {
            tx.execute(
                "UPDATE track_metadata SET metadata=?2 WHERE id=?1",
                params![id, serde_json::to_string(&metadata)?],
            )?;
        }
        if let Some(t) = title {
            tx.execute(
                "UPDATE track_metadata SET title_override=?2 WHERE id=?1",
                params![id, t.trim()],
            )?;
        }
        if let Some(a) = artist {
            tx.execute(
                "UPDATE track_metadata SET artist_override=?2 WHERE id=?1",
                params![id, a.trim()],
            )?;
        }
        if let Some(a) = album {
            tx.execute(
                "UPDATE track_metadata SET album_override=?2 WHERE id=?1",
                params![id, a.trim()],
            )?;
        }
        apply_metadata(&tx, &mut record.track)?;
        write_record(&tx, &record)?;
        tx.commit()?;
        Ok(record.track)
    }
}
fn save_job(tx: &rusqlite::Transaction<'_>, job: &ImportJob) -> Result<()> {
    tx.execute(
        "UPDATE import_jobs SET json=?2,finished_ms=?3 WHERE id=?1",
        params![
            job.job_id,
            serde_json::to_string(job)?,
            job.finished_at_ms.map(|n| n as i64)
        ],
    )?;
    Ok(())
}
fn save_item(tx: &rusqlite::Transaction<'_>, job: &str, item: &ImportItem) -> Result<()> {
    tx.execute("INSERT INTO import_items(job_id,position,json) VALUES(?1,?2,?3) ON CONFLICT(job_id,position) DO UPDATE SET json=excluded.json",params![job,item.index as i64,serde_json::to_string(item)?])?;
    Ok(())
}
pub(super) fn apply_metadata(tx: &rusqlite::Transaction<'_>, track: &mut Track) -> Result<()> {
    type Saved = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row:Option<Saved>=tx.query_row("SELECT metadata,title_override,artist_override,manifest,album_override FROM track_metadata WHERE id=?1",[&track.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    if let Some((json, title, artist, manifest, album)) = row {
        let m: Metadata = serde_json::from_str(&json)?;
        track.title = title.unwrap_or(m.title);
        track.artist = artist.unwrap_or(m.artist);
        if let Some(s) = manifest {
            let m: crate::imports::Manifest = serde_json::from_str(&s)?;
            track.source = Some(m.source);
        }
        track.apply_source_album();
        if let Some(album) = album {
            track.album = album;
        }
    } else if track.source.is_some()
        && let Some(parent) = track.playback.file().and_then(std::path::Path::parent)
        && let Ok(m) = crate::imports::read_manifest(parent)
    {
        tx.execute("INSERT OR IGNORE INTO track_metadata(id,video_id,manifest,metadata,title_override,artist_override) VALUES(?1,?2,?3,?4,?5,?6)",params![track.id,m.source.video_id,serde_json::to_string(&m)?,serde_json::to_string(&m.metadata)?,m.title_override,m.artist_override])?;
        track.apply_source_album();
    }
    Ok(())
}
pub(super) fn write_record(tx: &rusqlite::Transaction<'_>, record: &Record) -> Result<()> {
    let t = &record.track;
    tx.execute("INSERT INTO tracks(id,path,search,json,title_search,artist_search,album_search) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(id) DO UPDATE SET path=excluded.path,search=excluded.search,json=excluded.json,title_search=excluded.title_search,artist_search=excluded.artist_search,album_search=excluded.album_search",params![t.id,t.playback.file().context("Catalog entry is not a file")?.to_string_lossy(),normalized(&format!("{} {} {}",t.title,t.artist,t.album)),serde_json::to_string(record)?,normalized(&t.title),normalized(&t.artist),normalized(&t.album)])?;
    Ok(())
}

pub(super) fn migrate_albums(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let rows = tx
        .prepare("SELECT json FROM tracks")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut albums = std::collections::HashMap::new();
    for json in rows {
        let mut record: Record = serde_json::from_str(&json)?;
        apply_metadata(tx, &mut record.track)?;
        record.track.album = record.track.album_name().unwrap_or_default().to_owned();
        write_record(tx, &record)?;
        albums.insert(record.track.id, record.track.album);
    }
    let saved: Option<String> = tx
        .query_row("SELECT json FROM session WHERE id=1", [], |r| r.get(0))
        .optional()?;
    if let Some(json) = saved {
        let mut state: State = serde_json::from_str(&json)?;
        for item in &mut state.queue {
            if let Some(album) = albums.get(&item.track.id) {
                item.track.album = album.clone();
            } else {
                item.track.apply_source_album();
            }
        }
        tx.execute(
            "UPDATE session SET json=?1 WHERE id=1",
            [serde_json::to_string(&state)?],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn v3_migration_cleans_albums_and_preserves_queue_and_overrides() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.db");
        let mut original = Store::open(&path).unwrap();
        let mut state = State {
            volume: 37,
            position_ms: 1234,
            queue_revision: 9,
            ..Default::default()
        };
        let tx = original.db.transaction().unwrap();
        for (id, album, source_album) in [
            ("missing", "Unknown album", None),
            ("structured", "Unknown album", Some("Source album")),
            ("tagged", "File album", None),
        ] {
            let source = crate::youtube::Source {
                video_id: id.into(),
                music_album: source_album.map(str::to_owned),
                ..Default::default()
            };
            let record = Record {
                track: Track {
                    id: id.into(),
                    playback: crate::model::PlaybackSource::File {
                        path: format!("/{id}.m4a").into(),
                    },
                    title: "Manual title".into(),
                    artist: "Artist".into(),
                    album: album.into(),
                    track_number: 0,
                    duration_ms: Some(180_000),
                    cover: None,
                    source: Some(source.clone()),
                },
                modified: 0,
                bytes: 1,
            };
            write_record(&tx, &record).unwrap();
            let metadata = Metadata {
                title: "Automatic title".into(),
                artist: "Artist".into(),
                method: "code".into(),
                warning: None,
            };
            let manifest = crate::imports::Manifest {
                track_id: id.into(),
                source,
                metadata: metadata.clone(),
                title_override: None,
                artist_override: None,
            };
            tx.execute("INSERT INTO track_metadata(id,video_id,manifest,metadata,title_override) VALUES(?1,?1,?2,?3,'Manual title')",
                params![id, serde_json::to_string(&manifest).unwrap(), serde_json::to_string(&metadata).unwrap()]).unwrap();
            state.queue.push(crate::model::QueueItem::new(record.track));
        }
        tx.commit().unwrap();
        state.current_id = Some(state.queue[0].id.clone());
        state.play_next = vec![state.queue[2].id.clone()];
        original.save(&state).unwrap();
        original
            .db
            .execute_batch(
                "ALTER TABLE track_metadata DROP COLUMN album_override; PRAGMA user_version=3;",
            )
            .unwrap();
        drop(original);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.track("missing").unwrap().unwrap().album, "");
        assert_eq!(
            store.track("structured").unwrap().unwrap().album,
            "Source album"
        );
        assert_eq!(store.track("tagged").unwrap().unwrap().album, "File album");
        assert_eq!(
            store.track("structured").unwrap().unwrap().title,
            "Manual title"
        );
        assert_eq!(store.search("Unknown album", 0, 10).unwrap().1, 0);
        assert_eq!(store.search("Source album", 0, 10).unwrap().1, 1);
        let restored = store.restore().unwrap();
        assert_eq!(restored.volume, state.volume);
        assert_eq!(restored.position_ms, state.position_ms);
        assert_eq!(restored.current_id, state.current_id);
        assert_eq!(restored.play_next, state.play_next);
        assert_eq!(restored.queue_revision, state.queue_revision);
        for (i, album) in ["", "Source album", "File album"].into_iter().enumerate() {
            assert_eq!(restored.queue[i].id, state.queue[i].id);
            assert_eq!(restored.queue[i].track.album, album);
        }
        store
            .edit_metadata("structured", None, None, Some(String::new()), None)
            .unwrap();
        assert_eq!(store.track("structured").unwrap().unwrap().album, "");
    }

    #[test]
    fn v2_migration_preserves_session_and_import_history_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.db");
        let original = Store::open(&path).unwrap();
        original
            .save(&crate::model::State {
                volume: 37,
                ..Default::default()
            })
            .unwrap();
        original.db.execute_batch("DROP TABLE import_jobs; DROP TABLE import_items; DROP TABLE track_metadata; PRAGMA user_version=2;").unwrap();
        drop(original);
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.restore().unwrap().volume, 37);
        let request = ImportRequest {
            url: "https://www.youtube.com/watch?v=lO3lG-qXU14".into(),
            ..Default::default()
        };
        for _ in 0..103 {
            let mut job = ImportJob::new(&request);
            store.create_import(&job, &request).unwrap();
            job.finish("completed");
            store.save_import(&job, None).unwrap();
        }
        assert_eq!(store.import_jobs().unwrap().len(), 100);
        for _ in 0..32 {
            store
                .create_import(&ImportJob::new(&request), &request)
                .unwrap();
        }
        assert!(
            store
                .create_import(&ImportJob::new(&request), &request)
                .is_err()
        );
        store.interrupt_imports().unwrap();
        let jobs = store.import_jobs().unwrap();
        assert_eq!(jobs.len(), 100);
        assert!(jobs.iter().all(ImportJob::terminal));
        assert_eq!(
            jobs.iter().filter(|j| j.status == "interrupted").count(),
            32
        );
    }
}

//! Persistent import jobs. Worker messages are committed by the playback owner.
use crate::{
    import_config::{self, Config},
    library::{self, Record},
    metadata::{self, Metadata},
    model::unix_ms,
    platform::Paths,
    subprocess::{self, Cancel},
    youtube::{self, DownloadProgress, Source},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, mpsc},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImportRequest {
    pub url: String,
    pub playlist: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub video_ids: Option<Vec<String>>,
}
impl ImportRequest {
    pub fn validate(&mut self) -> Result<()> {
        let input = youtube::input(&self.url, self.playlist)?;
        self.url = input.url;
        self.playlist = input.playlist;
        if self.playlist && (self.title.is_some() || self.artist.is_some()) {
            bail!("Title and artist overrides require a single video");
        }
        for text in [&self.title, &self.artist].into_iter().flatten() {
            validate_text(text)?;
        }
        if let Some(ids) = &self.video_ids
            && (ids.len() > 10_000 || ids.iter().any(|s| !s.is_empty() && !youtube::valid_id(s)))
        {
            bail!("Invalid frozen playlist entries");
        }
        if let Some(ids) = &self.video_ids
            && !self.playlist
            && (ids.len() != 1 || youtube::video_url(&ids[0]) != self.url)
        {
            bail!("Single-video import entries must match the source URL");
        }
        Ok(())
    }
}
pub fn validate_text(s: &str) -> Result<()> {
    if s.trim().is_empty() || s.len() > 2048 || s.chars().any(char::is_control) {
        bail!("Metadata must be nonempty text, at most 2048 bytes, without control characters");
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportJob {
    pub job_id: String,
    pub url: String,
    pub title: String,
    pub status: String,
    pub stage: String,
    pub total: Option<usize>,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
    pub current_index: Option<usize>,
    pub current_title: Option<String>,
    pub progress: DownloadProgress,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub error: Option<String>,
    pub revision: u64,
}
impl ImportJob {
    pub fn new(request: &ImportRequest) -> Self {
        Self {
            job_id: uuid::Uuid::new_v4().to_string(),
            url: request.url.clone(),
            title: "YouTube import".into(),
            status: "queued".into(),
            stage: "queued".into(),
            total: None,
            added: 0,
            skipped: 0,
            failed: 0,
            current_index: None,
            current_title: None,
            progress: Default::default(),
            started_at_ms: unix_ms(),
            finished_at_ms: None,
            error: None,
            revision: 0,
        }
    }
    pub fn terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "partial" | "failed" | "cancelled" | "interrupted"
        )
    }
    pub fn finish(&mut self, status: &str) {
        self.status = status.into();
        self.stage = status.into();
        self.finished_at_ms = Some(unix_ms());
        self.progress.speed = None;
        self.progress.eta = None;
        self.revision += 1;
    }
    pub fn summary(&self) -> String {
        let p = &self.progress;
        let percent = match (p.bytes, p.total) {
            (Some(b), Some(t)) if t > 0 => {
                format!(" · {:.0}%", (b as f64 / t as f64 * 100.).min(100.))
            }
            _ => String::new(),
        };
        let speed = p
            .speed
            .filter(|n| n.is_finite() && *n > 0.)
            .map(|n| format!(" · {:.1} MiB/s", n / 1048576.))
            .unwrap_or_default();
        let eta = p
            .eta
            .filter(|n| n.is_finite() && *n >= 0.)
            .map(|n| format!(" · ETA {n:.0}s"))
            .unwrap_or_default();
        let bytes = p
            .bytes
            .map(|b| format!(" · {:.1} MiB", b as f64 / 1048576.))
            .unwrap_or_default();
        let elapsed = self
            .finished_at_ms
            .unwrap_or_else(unix_ms)
            .saturating_sub(self.started_at_ms)
            / 1000;
        format!(
            "{}/{} · {} · {}{}{}{}{} · {elapsed}s · Added {} · Skipped {} · Failed {}",
            self.added + self.skipped + self.failed,
            self.total
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into()),
            self.stage,
            self.current_title.as_deref().unwrap_or(&self.title),
            percent,
            bytes,
            speed,
            eta,
            self.added,
            self.skipped,
            self.failed
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportItem {
    pub index: usize,
    pub video_id: String,
    pub title: String,
    pub status: String,
    pub track_id: Option<String>,
    pub error: Option<String>,
    pub metadata: Option<Metadata>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub track_id: String,
    pub source: Source,
    pub metadata: Metadata,
    pub title_override: Option<String>,
    pub artist_override: Option<String>,
}
pub struct Publication {
    pub job: ImportJob,
    pub item: ImportItem,
    pub manifest: Manifest,
    pub stage: PathBuf,
    pub record: Record,
}
pub enum Message {
    Plan(ImportJob, Vec<ImportItem>),
    Progress(ImportJob),
    Item(ImportJob, ImportItem),
    Lookup(String, mpsc::SyncSender<Option<Record>>),
    Publish(Box<Publication>, mpsc::SyncSender<Result<(), String>>),
    Done(ImportJob),
}
pub struct Running {
    pub id: String,
    pub cancel: Cancel,
    pub thread: std::thread::JoinHandle<()>,
}
impl Running {
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
fn wait<T>(rx: mpsc::Receiver<T>, stop: &Cancel) -> Result<T> {
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(v) => return Ok(v),
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("Server stopped"),
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
        if stop.load(Ordering::Relaxed) {
            bail!("Import cancelled");
        }
    }
}
pub fn spawn(
    paths: Paths,
    mut job: ImportJob,
    request: ImportRequest,
    config: Config,
    send: impl Fn(Message) -> bool + Send + 'static,
) -> Running {
    let stop = subprocess::cancel();
    let worker_stop = stop.clone();
    let id = job.job_id.clone();
    let thread = std::thread::spawn(move || {
        let result = work(&paths, &mut job, &request, &config, &worker_stop, &send);
        if worker_stop.load(Ordering::Relaxed) {
            job.finish("cancelled");
        } else if let Err(e) = result {
            job.error = Some(format!("{e:#}"));
            job.finish("failed");
        } else {
            job.finish(if job.failed == 0 {
                "completed"
            } else if job.added + job.skipped > 0 {
                "partial"
            } else {
                "failed"
            });
        }
        send(Message::Done(job));
    });
    Running {
        id,
        cancel: stop,
        thread,
    }
}
fn update(job: &mut ImportJob, stage: &str, send: &impl Fn(Message) -> bool) {
    job.stage = stage.into();
    job.revision += 1;
    send(Message::Progress(job.clone()));
}
fn work(
    paths: &Paths,
    job: &mut ImportJob,
    request: &ImportRequest,
    config: &Config,
    stop: &Cancel,
    send: &impl Fn(Message) -> bool,
) -> Result<()> {
    // Fail only this optional feature when tools are missing. No installation side effects.
    import_config::executable(config.youtube.yt_dlp.as_deref(), "yt-dlp")?;
    import_config::executable(config.youtube.ffmpeg.as_deref(), "ffmpeg")?;
    import_config::executable(config.youtube.ffprobe.as_deref(), "ffprobe")?;
    job.status = "running".into();
    update(job, "resolving", send);
    let preview = if let Some(ids) = &request.video_ids {
        youtube::Preview {
            url: request.url.clone(),
            title: "YouTube import".into(),
            playlist: request.playlist,
            existing: None,
            items: ids
                .iter()
                .map(|id| youtube::PreviewItem {
                    video_id: id.clone(),
                    title: id.clone(),
                })
                .collect(),
        }
    } else {
        youtube::preview(&request.url, request.playlist, config, stop)?
    };
    job.title = preview.title;
    job.total = Some(preview.items.len());
    let items: Vec<_> = preview
        .items
        .into_iter()
        .enumerate()
        .map(|(index, v)| ImportItem {
            index,
            video_id: v.video_id,
            title: v.title,
            status: "queued".into(),
            track_id: None,
            error: None,
            metadata: None,
        })
        .collect();
    if !send(Message::Plan(job.clone(), items.clone())) {
        bail!("Server stopped");
    }
    for mut item in items {
        if stop.load(Ordering::Relaxed) {
            bail!("Import cancelled");
        }
        job.current_index = Some(item.index);
        job.current_title = Some(item.title.clone());
        job.progress = Default::default();
        let outcome = (|| -> Result<()> {
            if !youtube::valid_id(&item.video_id) {
                bail!("Video is unavailable or has no valid ID");
            }
            let (tx, rx) = mpsc::sync_channel(1);
            if !send(Message::Lookup(item.video_id.clone(), tx)) {
                bail!("Server stopped");
            }
            let existing = wait(rx, stop)?;
            if let Some(record) = &existing
                && record.track.path.is_file()
            {
                item.track_id = Some(record.track.id.clone());
                item.title = record.track.title.clone();
                item.status = "skipped".into();
                job.skipped += 1;
                return Ok(());
            }
            // A completed directory can survive a failed DB commit; retry adopts it.
            let final_dir = paths.data.join("imports/youtube").join(&item.video_id);
            let recovered = read_manifest(&final_dir).ok().filter(|m| {
                m.source.video_id == item.video_id && final_dir.join("audio.m4a").is_file()
            });
            let temp_root = paths.data.join("imports/.staging");
            crate::platform::private_dir(&temp_root)?;
            let temporary = tempfile::Builder::new()
                .prefix("item-")
                .tempdir_in(&temp_root)?;
            let stage = temporary.path();
            let manifest = if let Some(mut manifest) = recovered {
                if let Some(record) = existing {
                    manifest.track_id = record.track.id;
                }
                std::fs::copy(final_dir.join("audio.m4a"), stage.join("audio.m4a"))?;
                if final_dir.join("cover.jpg").is_file() {
                    std::fs::copy(final_dir.join("cover.jpg"), stage.join("cover.jpg"))?;
                }
                manifest
            } else {
                update(job, "resolving", send);
                let source = youtube::extract(&youtube::video_url(&item.video_id), config, stop)?;
                if source.video_id != item.video_id {
                    bail!("Extractor returned a different video");
                }
                update(job, "metadata", send);
                let metadata = metadata::resolve(&source, config, paths, stop);
                item.title = metadata.title.clone();
                job.current_title = Some(item.title.clone());
                update(job, "downloading", send);
                let mut last = Instant::now() - Duration::from_secs(1);
                youtube::download(&source, stage, config, stop, |progress| {
                    job.progress = progress;
                    if last.elapsed() >= Duration::from_millis(250) {
                        job.revision += 1;
                        send(Message::Progress(job.clone()));
                        last = Instant::now();
                    }
                })?;
                update(job, "processing", send);
                let mut metadata = metadata;
                if let Err(e) = youtube::cover(stage, config, stop) {
                    metadata.warning = Some(match metadata.warning {
                        Some(w) => format!("{w}; Cover unavailable: {e}"),
                        None => format!("Cover unavailable: {e}"),
                    });
                }
                Manifest {
                    track_id: existing
                        .map(|r| r.track.id)
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    source,
                    metadata,
                    title_override: request.title.clone(),
                    artist_override: request.artist.clone(),
                }
            };
            import_config::atomic_json(&stage.join("source.json"), &manifest)?;
            let audio = stage.join("audio.m4a");
            let mut track = library::read_track(&audio, manifest.track_id.clone(), &paths.cache)?;
            track.title = manifest
                .title_override
                .clone()
                .unwrap_or_else(|| manifest.metadata.title.clone());
            track.artist = manifest
                .artist_override
                .clone()
                .unwrap_or_else(|| manifest.metadata.artist.clone());
            track.source = Some(manifest.source.clone());
            let file = audio.metadata()?;
            let record = Record {
                track,
                modified: file
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos(),
                bytes: file.len(),
            };
            update(job, "indexing", send);
            item.status = "completed".into();
            item.track_id = Some(manifest.track_id.clone());
            item.metadata = Some(manifest.metadata.clone());
            item.title = record.track.title.clone();
            let mut committed = job.clone();
            committed.added += 1;
            let (tx, rx) = mpsc::sync_channel(1);
            if !send(Message::Publish(
                Box::new(Publication {
                    job: committed.clone(),
                    item: item.clone(),
                    manifest,
                    stage: stage.into(),
                    record,
                }),
                tx,
            )) {
                bail!("Server stopped");
            }
            wait(rx, stop)?.map_err(anyhow::Error::msg)?;
            *job = committed;
            Ok(())
        })();
        if stop.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("Import cancelled"));
        }
        if let Err(e) = outcome {
            item.status = "failed".into();
            item.error = Some(format!("{e:#}"));
            job.failed += 1;
        }
        job.revision += 1;
        if !send(Message::Item(job.clone(), item)) {
            bail!("Server stopped");
        }
    }
    Ok(())
}
pub fn read_manifest(dir: &std::path::Path) -> Result<Manifest> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(dir.join("source.json"))?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        bail!("Import manifest exceeds 64 KiB");
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if !youtube::valid_id(&manifest.source.video_id) {
        bail!("Invalid import manifest");
    }
    Ok(manifest)
}
pub fn publish(paths: &Paths, publication: &mut Publication) -> Result<()> {
    let root = paths.data.join("imports/youtube");
    crate::platform::private_dir(&root)?;
    let destination = root.join(&publication.manifest.source.video_id);
    if destination.exists() {
        // Only replace a directory with our own matching manifest; never arbitrary user files.
        let old = read_manifest(&destination)
            .context("Existing import directory is not managed by vtamp")?;
        if old.source.video_id != publication.manifest.source.video_id {
            bail!("Import directory identity mismatch");
        }
        let backup = root.join(format!(".replaced-{}", uuid::Uuid::new_v4()));
        std::fs::rename(&destination, &backup)?;
        if let Err(e) = std::fs::rename(&publication.stage, &destination) {
            let _ = std::fs::rename(&backup, &destination);
            return Err(e.into());
        }
        let _ = std::fs::remove_dir_all(backup);
    } else {
        std::fs::rename(&publication.stage, &destination)?;
    }
    // Remove transient download artifacts after the atomic directory publication.
    for entry in std::fs::read_dir(&destination)? {
        let p = entry?.path();
        if !matches!(
            p.file_name().and_then(|s| s.to_str()),
            Some("audio.m4a" | "cover.jpg" | "source.json")
        ) {
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(p);
            } else {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    publication.record.track.path = destination.join("audio.m4a").canonicalize()?;
    publication.record.track.cover = destination
        .join("cover.jpg")
        .is_file()
        .then(|| destination.join("cover.jpg"));
    Ok(())
}

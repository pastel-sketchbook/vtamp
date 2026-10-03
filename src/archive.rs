//! Portable library archives. No download tools or network access are involved.
mod restore;
#[cfg(test)]
mod tests;
pub(crate) use restore::{Publication, prepare, recover};

use crate::{
    library::Record, metadata::Metadata, model::Track, platform::Paths, store::Store, streams,
};
use anyhow::{Context, Result, ensure};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const FORMAT_VERSION: u32 = 1;
const MAX_MANIFEST: u64 = 64 * 1024 * 1024;
const MAX_TRACKS: usize = 100_000;
const MAX_REPORTS: usize = 1_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedMetadata {
    pub automatic: Metadata,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl SavedMetadata {
    fn effective(track: &Track) -> Self {
        Self {
            automatic: Metadata {
                title: track.title.clone(),
                artist: track.artist.clone(),
                method: "archive".into(),
                warning: None,
            },
            title: Some(track.title.clone()),
            artist: Some(track.artist.clone()),
            album: Some(track.album.clone()),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Catalog {
    pub records: Vec<Record>,
    pub metadata: BTreeMap<String, SavedMetadata>,
    pub streams: Vec<streams::Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    // Paths and IDs are archive-relative here, never trusted destination paths.
    track: Track,
    metadata: SavedMetadata,
    audio: Option<Asset>,
    cover: Option<Asset>,
    video: Option<Asset>,
    external: Option<PathBuf>,
    external_sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    tracks: Vec<Entry>,
    streams: Vec<streams::Entry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub operation: String,
    pub status: String,
    pub job_id: Option<String>,
    pub included: usize,
    pub videos: usize,
    pub references: usize,
    pub radios: usize,
    pub added: usize,
    pub duplicates: usize,
    pub reconnected: usize,
    pub missing: usize,
    pub warning_count: usize,
    pub reports: Vec<String>,
    pub error: Option<String>,
}

impl Report {
    fn note(&mut self, message: String) {
        self.warning_count += 1;
        if self.reports.len() < MAX_REPORTS {
            // Keep status replies bounded below the wire limit.
            self.reports.push(message.chars().take(2_048).collect());
        }
    }
}

fn check_stop(stop: &AtomicBool) -> Result<()> {
    ensure!(
        !stop.load(Ordering::Relaxed),
        "Archive operation interrupted"
    );
    Ok(())
}

/// Detect in-place changes while reading; callers also verify second-pass hashes.
fn hash_file(path: &Path, stop: &AtomicBool) -> Result<(u64, String)> {
    let mut file = File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let before = file.metadata()?;
    ensure!(
        before.is_file(),
        "Expected a regular file: {}",
        path.display()
    );
    let (bytes, hash) = hash_reader(&mut file, stop)?;
    let after = file.metadata()?;
    ensure!(
        before.len() == bytes && after.len() == bytes && before.modified()? == after.modified()?,
        "File changed while reading: {}",
        path.display()
    );
    Ok((bytes, hash))
}

fn hash_reader(reader: &mut impl Read, stop: &AtomicBool) -> Result<(u64, String)> {
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; 128 * 1024];
    loop {
        check_stop(stop)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        bytes = bytes
            .checked_add(count as u64)
            .context("File is too large")?;
    }
    Ok((bytes, format!("{:x}", digest.finalize())))
}

fn asset(path: &Path, name: String, stop: &AtomicBool) -> Result<Asset> {
    let (bytes, sha256) = hash_file(path, stop)?;
    Ok(Asset {
        path: name,
        bytes,
        sha256,
    })
}

/// Snapshot all metadata first, then stream bounded chunks of the actual assets.
pub fn export(paths: &Paths, output: &Path, include_local: bool) -> Result<Report> {
    let stop = AtomicBool::new(false);
    ensure!(
        !output.try_exists()?,
        "Output already exists: {}",
        output.display()
    );
    let catalog = Store::archive_snapshot(&paths.database())?;
    ensure!(
        catalog.records.len() <= MAX_TRACKS,
        "Too many tracks for archive format 1"
    );
    let mut manifest = Manifest {
        format: "vtamp-library".into(),
        version: FORMAT_VERSION,
        tracks: vec![],
        streams: catalog.streams,
    };
    let mut inputs = BTreeMap::new();
    let mut report = Report {
        operation: "export".into(),
        status: "completed".into(),
        radios: manifest.streams.len(),
        ..Default::default()
    };
    let managed = paths.data.join("imports").canonicalize().ok();
    for (index, record) in catalog.records.into_iter().enumerate() {
        let file = record
            .track
            .playback
            .file()
            .context("Expected file track")?
            .to_path_buf();
        let internal = managed.as_ref().is_some_and(|root| file.starts_with(root));
        let youtube = record.track.source.is_some();
        if youtube {
            crate::deletion::managed_path(paths, &record.track)?;
        }
        let mut entry = Entry {
            metadata: catalog
                .metadata
                .get(&record.track.id)
                .cloned()
                .unwrap_or_else(|| SavedMetadata::effective(&record.track)),
            track: record.track,
            audio: None,
            cover: None,
            video: None,
            external: None,
            external_sha256: None,
        };
        if internal || include_local || youtube {
            let extension = file
                .extension()
                .and_then(|s| s.to_str())
                .context("Audio has no extension")?;
            let a = asset(&file, format!("media/{index}/audio.{extension}"), &stop)?;
            inputs.insert(a.path.clone(), file.clone());
            entry.audio = Some(a);
            report.included += 1;
        } else {
            entry.external = Some(file.clone());
            match hash_file(&file, &stop) {
                Ok((_, hash)) => entry.external_sha256 = Some(hash),
                Err(error) => report.note(format!(
                    "External reference cannot be verified: {}: {error:#}",
                    file.display()
                )),
            }
            report.references += 1;
        }
        if let Some(cover) = entry.track.cover.as_ref() {
            if cover.try_exists()? {
                let ext = cover.extension().and_then(|s| s.to_str()).unwrap_or("jpg");
                let a = asset(cover, format!("covers/{index}.{ext}"), &stop)?;
                inputs.insert(a.path.clone(), cover.clone());
                entry.cover = Some(a);
            } else {
                report.note(format!("Cover is missing: {}", cover.display()));
            }
        }
        if youtube {
            let video = file
                .parent()
                .context("Missing audio directory")?
                .join("video.mkv");
            if video.try_exists()? {
                let a = asset(&video, format!("media/{index}/video.mkv"), &stop)?;
                inputs.insert(a.path.clone(), video);
                entry.video = Some(a);
                report.videos += 1;
            }
        }
        entry.track.id = index.to_string();
        entry.track.playback = crate::model::PlaybackSource::File {
            path: entry
                .audio
                .as_ref()
                .map(|a| PathBuf::from(&a.path))
                .unwrap_or_default(),
        };
        entry.track.cover = entry.cover.as_ref().map(|a| PathBuf::from(&a.path));
        manifest.tracks.push(entry);
    }
    validate(&manifest)?;
    let json = serde_json::to_vec(&manifest)?;
    ensure!(
        json.len() as u64 <= MAX_MANIFEST,
        "Archive manifest exceeds 64 MiB"
    );
    let parent = output.parent().context("Output has no parent directory")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let encoder = GzEncoder::new(temp.as_file_mut(), Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        append(
            &mut tar,
            "manifest.json",
            json.len() as u64,
            json.as_slice(),
        )?;
        for a in manifest.tracks.iter().flat_map(assets) {
            let path = &inputs[&a.path];
            let mut reader = CheckingReader {
                inner: File::open(path)?,
                hash: Sha256::new(),
                bytes: 0,
            };
            append(&mut tar, &a.path, a.bytes, &mut reader)?;
            ensure!(
                reader.bytes == a.bytes && format!("{:x}", reader.hash.finalize()) == a.sha256,
                "File changed during export: {}",
                path.display()
            );
            ensure!(
                reader.inner.metadata()?.len() == a.bytes,
                "File size changed during export"
            );
        }
        tar.into_inner()?.finish()?;
    }
    temp.as_file_mut().sync_all()?;
    temp.persist_noclobber(output).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    Ok(report)
}

struct CheckingReader<R> {
    inner: R,
    hash: Sha256,
    bytes: u64,
}
impl<R: Read> Read for CheckingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        self.hash.update(&buffer[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

fn append<W: Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    bytes: u64,
    reader: impl Read,
) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_size(bytes);
    header.set_mode(0o600);
    header.set_cksum();
    tar.append_data(&mut header, name, reader)?;
    Ok(())
}

fn assets(entry: &Entry) -> impl Iterator<Item = &Asset> {
    entry.audio.iter().chain(&entry.cover).chain(&entry.video)
}

fn safe_asset_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() < 256
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && (path.starts_with("media/") || path.starts_with("covers/"))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn validate(manifest: &Manifest) -> Result<BTreeMap<String, Asset>> {
    ensure!(
        manifest.format == "vtamp-library" && manifest.version == FORMAT_VERSION,
        "Unsupported library archive format"
    );
    ensure!(
        manifest.tracks.len() <= MAX_TRACKS && manifest.streams.len() <= MAX_TRACKS,
        "Too many archive entries"
    );
    let mut files = BTreeMap::new();
    let mut videos = HashSet::new();
    for entry in &manifest.tracks {
        ensure!(
            entry.audio.is_some() != entry.external.is_some(),
            "Track must contain audio or an external reference"
        );
        ensure!(
            entry.track.playback.file().is_some(),
            "Expected a file track"
        );
        ensure!(
            entry.external.as_ref().is_none_or(|p| p.is_absolute()),
            "External reference must be absolute"
        );
        ensure!(
            entry.external_sha256.as_ref().is_none_or(|h| valid_hash(h)),
            "Invalid external checksum"
        );
        for text in [
            &entry.track.title,
            &entry.track.artist,
            &entry.track.album,
            &entry.metadata.automatic.title,
            &entry.metadata.automatic.artist,
        ] {
            ensure!(
                text.len() <= 8192 && !text.chars().any(char::is_control),
                "Invalid track metadata"
            );
        }
        for text in [
            &entry.metadata.title,
            &entry.metadata.artist,
            &entry.metadata.album,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                text.len() <= 8192 && !text.chars().any(char::is_control),
                "Invalid metadata override"
            );
        }
        if let Some(source) = &entry.track.source {
            ensure!(
                crate::youtube::valid_id(&source.video_id) && videos.insert(&source.video_id),
                "Invalid or duplicate YouTube identity"
            );
            ensure!(
                entry
                    .audio
                    .as_ref()
                    .is_some_and(|a| a.path.ends_with("/audio.m4a")),
                "YouTube audio must be included as m4a"
            );
            let source_manifest = crate::imports::Manifest {
                track_id: "00000000-0000-0000-0000-000000000000".into(),
                source: source.clone(),
                metadata: entry.metadata.automatic.clone(),
                title_override: entry.metadata.title.clone(),
                artist_override: entry.metadata.artist.clone(),
            };
            ensure!(
                serde_json::to_vec_pretty(&source_manifest)?.len() < 64 * 1024,
                "YouTube source manifest exceeds 64 KiB"
            );
        } else {
            ensure!(entry.video.is_none(), "Video requires a YouTube source");
        }
        if let Some(audio) = &entry.audio {
            ensure!(
                crate::library::supported(Path::new(&audio.path)),
                "Unsupported archive audio"
            );
        }
        if let Some(cover) = &entry.cover {
            ensure!(
                cover.bytes <= 16 * 1024 * 1024,
                "Archive cover exceeds 16 MiB"
            );
        }
        for a in assets(entry) {
            ensure!(
                safe_asset_path(&a.path) && valid_hash(&a.sha256),
                "Invalid archive asset"
            );
            ensure!(
                files.insert(a.path.clone(), a.clone()).is_none(),
                "Duplicate archive asset path"
            );
        }
    }
    for stream in &manifest.streams {
        stream.validated()?;
    }
    Ok(files)
}

/// Do not use tar::unpack: the manifest is an allowlist, and links are never accepted.
fn extract(path: &Path, destination: &Path, stop: &AtomicBool) -> Result<Manifest> {
    let decoder = GzDecoder::new(File::open(path)?);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries()?;
    let mut first = entries.next().context("Archive is empty")??;
    ensure!(
        first.header().entry_type().is_file()
            && first.path()?.as_ref() == Path::new("manifest.json"),
        "Archive must begin with manifest.json"
    );
    ensure!(
        first.size() <= MAX_MANIFEST,
        "Archive manifest exceeds 64 MiB"
    );
    let manifest: Manifest = serde_json::from_reader(&mut first)?;
    let mut expected = validate(&manifest)?;
    for item in entries {
        check_stop(stop)?;
        let mut item = item?;
        ensure!(
            item.header().entry_type().is_file(),
            "Archive links and special entries are forbidden"
        );
        let name = item
            .path()?
            .to_str()
            .context("Archive paths must be UTF-8")?
            .to_owned();
        let a = expected
            .remove(&name)
            .context("Unexpected or duplicate archive file")?;
        ensure!(item.size() == a.bytes, "Archive file size mismatch");
        let target = destination.join(&name);
        fs::create_dir_all(target.parent().unwrap())?;
        let mut output = File::options().write(true).create_new(true).open(&target)?;
        let mut digest = Sha256::new();
        let mut buffer = [0; 128 * 1024];
        let mut count = 0;
        loop {
            check_stop(stop)?;
            let n = item.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
            output.write_all(&buffer[..n])?;
            count += n as u64;
        }
        ensure!(
            count == a.bytes && format!("{:x}", digest.finalize()) == a.sha256,
            "Archive checksum mismatch: {name}"
        );
        output.sync_all()?;
    }
    ensure!(expected.is_empty(), "Archive is missing declared files");
    // Consume the gzip trailer too: truncated/corrupt trailers must fail before publication.
    let mut decoder = archive.into_inner();
    let mut padding = [0; 4096];
    loop {
        check_stop(stop)?;
        let n = decoder.read(&mut padding)?;
        if n == 0 {
            break;
        }
        ensure!(
            padding[..n].iter().all(|b| *b == 0),
            "Unexpected data after tar terminator"
        );
    }
    ensure!(decoder.get_ref().metadata()?.len() > 0, "Empty archive");
    for (index, entry) in manifest.tracks.iter().enumerate() {
        check_stop(stop)?;
        if let Some(audio) = &entry.audio {
            crate::library::read_track(
                &destination.join(&audio.path),
                index.to_string(),
                &destination.join(".probe-covers"),
            )
            .context("Invalid archived audio")?;
        }
        if let Some(cover) = &entry.cover {
            crate::library::decode_image(&fs::read(destination.join(&cover.path))?)
                .context("Invalid archived cover")?;
        }
    }
    Ok(manifest)
}

pub fn preview(paths: &Paths, archive: &Path) -> Result<Report> {
    let snapshot = Store::archive_snapshot(&paths.database())?;
    let stage = tempfile::tempdir()?;
    let stop = AtomicBool::new(false);
    let manifest = extract(archive, stage.path(), &stop)?;
    let (mut report, _) = restore::plan(&manifest, &snapshot, &stop)?;
    report.operation = "dry_run".into();
    Ok(report)
}

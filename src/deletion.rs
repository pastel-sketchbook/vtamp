//! Delete only validated managed downloads; never registered local music.
use crate::{
    imports,
    model::{ApiError, State, Track},
    platform::Paths,
    store::Store,
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn directory(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_dir(),
        "Managed path is not a real directory: {}",
        path.display()
    );
    Ok(())
}

fn root(paths: &Paths) -> Result<PathBuf> {
    let imports = paths.data.join("imports");
    directory(&imports)?;
    let root = imports.join("youtube");
    directory(&root)?;
    Ok(root.canonicalize()?)
}

fn validate_files(path: &Path) -> Result<imports::Manifest> {
    directory(path)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file()
                && matches!(
                    entry.file_name().to_str(),
                    Some("audio.m4a" | "cover.jpg" | "source.json")
                ),
            "Import contains an unexpected file; refusing deletion"
        );
    }
    let manifest = imports::read_manifest(path)?;
    ensure!(
        crate::youtube::valid_id(&manifest.source.video_id),
        "Invalid managed video ID"
    );
    Ok(manifest)
}

fn managed_path(paths: &Paths, track: &Track) -> Result<PathBuf> {
    let source = track.source.as_ref().ok_or_else(|| {
        ApiError::new(
            "not_managed",
            "Only managed YouTube downloads can be deleted; local files are kept",
        )
    })?;
    ensure!(
        crate::youtube::valid_id(&source.video_id),
        "Invalid video ID"
    );
    let destination = root(paths)?.join(&source.video_id);
    ensure!(
        track.playback.file() == Some(destination.join("audio.m4a").as_path()),
        "Track is not in its managed download directory"
    );
    let manifest = validate_files(&destination)?;
    ensure!(
        manifest.track_id == track.id && manifest.source.video_id == source.video_id,
        "Managed import identity mismatch"
    );
    Ok(destination)
}

fn cleanup(path: &Path) -> Result<()> {
    // Leave the identity manifest until last so interrupted cleanup is recoverable.
    for name in ["audio.m4a", "cover.jpg", "source.json"] {
        match fs::remove_file(path.join(name)) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    fs::remove_dir(path)?;
    Ok(())
}

pub fn delete(
    paths: &Paths,
    store: &mut Store,
    state: &State,
    id: &str,
) -> Result<serde_json::Value> {
    let track = store
        .track(id)?
        .ok_or_else(|| ApiError::new("track_not_found", "Library track not found"))?;
    let destination = managed_path(paths, &track)?;
    let audio = destination.join("audio.m4a");
    if state
        .queue
        .iter()
        .chain(state.direct.as_deref())
        .any(|item| {
            item.track.id == id
                || item.track.playback.file().is_some_and(|path| {
                    path == audio || path.canonicalize().is_ok_and(|path| path == audio)
                })
        })
    {
        return Err(ApiError::new(
            "track_in_use",
            "Remove this track from Queue and switch away from direct playback before deleting",
        )
        .into());
    }
    let staging = paths.data.join("imports/.staging");
    crate::platform::private_dir(&staging)?;
    directory(&staging)?;
    let staged = staging.join(format!("delete-{}", uuid::Uuid::new_v4()));
    fs::rename(&destination, &staged).context("Cannot stage download for deletion")?;
    if let Err(error) = store.delete_import(id) {
        fs::rename(&staged, &destination)
            .context("Cannot roll back download deletion; restart the server to recover")?;
        return Err(error);
    }
    let warning = cleanup(&staged).err().map(|error| {
        format!("Library entry removed; file cleanup will retry on server start: {error}")
    });
    Ok(serde_json::json!({"deleted": id, "warning": warning}))
}

/// A reserved staging directory is a tiny filesystem journal. Before the DB
/// commit, restore its files; after the commit, finish deleting them.
pub fn recover(paths: &Paths, store: &Store) -> Result<()> {
    let staging = paths.data.join("imports/.staging");
    if !staging.try_exists()? {
        return Ok(());
    }
    directory(&paths.data.join("imports"))?;
    directory(&staging)?;
    for entry in fs::read_dir(&staging)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix("delete-")) else {
            continue;
        };
        if uuid::Uuid::parse_str(suffix).is_err() {
            continue;
        }
        let staged = entry.path();
        directory(&staged)?;
        if fs::read_dir(&staged)?.next().is_none() {
            fs::remove_dir(&staged)?;
            continue;
        }
        let manifest = validate_files(&staged)?;
        let destination = root(paths)?.join(&manifest.source.video_id);
        if let Some(track) = store.track(&manifest.track_id)? {
            ensure!(
                track.playback.file() == Some(destination.join("audio.m4a").as_path()),
                "Cannot recover deletion with mismatched track path"
            );
            ensure!(
                !destination.try_exists()?,
                "Cannot recover deletion over an existing download"
            );
            fs::rename(&staged, destination)?;
        } else {
            cleanup(&staged)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        library::Record,
        model::{PlaybackSource, QueueItem},
        youtube::Source,
    };

    fn fixture() -> (tempfile::TempDir, Paths, Store, Track) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: home.path().into(),
            runtime: home.path().join("run"),
            cache: home.path().join("cache"),
        };
        let folder = home.path().join("imports/youtube/0OeEx5SiRI0");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("audio.m4a"), b"test audio").unwrap();
        fs::write(folder.join("cover.jpg"), b"test cover").unwrap();
        let source = Source {
            video_id: "0OeEx5SiRI0".into(),
            ..Default::default()
        };
        let track = Track {
            id: "download".into(),
            playback: PlaybackSource::File {
                path: folder.join("audio.m4a").canonicalize().unwrap(),
            },
            title: "Track".into(),
            artist: "Artist".into(),
            album: String::new(),
            track_number: 0,
            duration_ms: Some(1000),
            cover: Some(folder.join("cover.jpg")),
            source: Some(source.clone()),
        };
        crate::platform::atomic_json(
            &folder.join("source.json"),
            &imports::Manifest {
                track_id: track.id.clone(),
                source,
                metadata: Default::default(),
                title_override: None,
                artist_override: None,
            },
        )
        .unwrap();
        let mut store = Store::open(&paths.database()).unwrap();
        store
            .replace_catalog(&[Record {
                track: track.clone(),
                modified: 0,
                bytes: 10,
            }])
            .unwrap();
        (home, paths, store, track)
    }

    #[test]
    fn deletes_only_managed_files_and_metadata_without_schema_change() {
        let (_home, paths, mut store, track) = fixture();
        let folder = track.playback.file().unwrap().parent().unwrap();
        let result = delete(&paths, &mut store, &State::default(), &track.id).unwrap();
        assert!(result["warning"].is_null());
        assert!(!folder.exists());
        assert!(store.track(&track.id).unwrap().is_none());
        assert!(store.video_record("0OeEx5SiRI0").unwrap().is_none());
        assert!(
            crate::library::scan(&[paths.data.join("imports/youtube")], &[], &paths.cache)
                .records
                .is_empty()
        );
        let db = rusqlite::Connection::open(paths.database()).unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            6
        );
    }

    #[test]
    fn rejects_local_files_queued_copies_and_direct_path_aliases() {
        let (_home, paths, mut store, track) = fixture();
        for direct in [false, true] {
            let mut copy = track.clone();
            copy.id = "another-id".into();
            let item = QueueItem::new(copy);
            let state = if direct {
                State {
                    direct: Some(Box::new(item)),
                    ..Default::default()
                }
            } else {
                State {
                    queue: vec![item],
                    ..Default::default()
                }
            };
            let error = delete(&paths, &mut store, &state, &track.id).unwrap_err();
            assert_eq!(
                error.downcast_ref::<ApiError>().unwrap().code,
                "track_in_use"
            );
            assert!(track.playback.file().unwrap().exists());
        }
        let mut local = track.clone();
        local.source = None;
        local.id = "local".into();
        local.playback = PlaybackSource::File {
            path: paths.data.join("original.m4a"),
        };
        fs::write(local.playback.file().unwrap(), b"original").unwrap();
        store
            .replace_catalog(&[Record {
                track: local.clone(),
                modified: 0,
                bytes: 8,
            }])
            .unwrap();
        assert!(delete(&paths, &mut store, &State::default(), &local.id).is_err());
        assert_eq!(
            fs::read(local.playback.file().unwrap()).unwrap(),
            b"original"
        );
    }

    #[test]
    fn rejects_unexpected_files_symlinks_and_mismatched_manifests() {
        let (_home, paths, mut store, track) = fixture();
        let folder = track.playback.file().unwrap().parent().unwrap();
        let extra = folder.join("personal.txt");
        fs::write(&extra, b"keep").unwrap();
        assert!(delete(&paths, &mut store, &State::default(), &track.id).is_err());
        assert!(extra.exists());
        fs::remove_file(&extra).unwrap();
        fs::remove_file(folder.join("cover.jpg")).unwrap();
        std::os::unix::fs::symlink(track.playback.file().unwrap(), folder.join("cover.jpg"))
            .unwrap();
        assert!(delete(&paths, &mut store, &State::default(), &track.id).is_err());
        fs::remove_file(folder.join("cover.jpg")).unwrap();
        let mut manifest = imports::read_manifest(folder).unwrap();
        manifest.track_id = "different".into();
        crate::platform::atomic_json(&folder.join("source.json"), &manifest).unwrap();
        assert!(delete(&paths, &mut store, &State::default(), &track.id).is_err());
        assert!(track.playback.file().unwrap().exists());
    }

    #[test]
    fn database_failure_restores_files_and_catalog() {
        let (_home, paths, mut store, track) = fixture();
        let db = rusqlite::Connection::open(paths.database()).unwrap();
        db.execute_batch("CREATE TRIGGER fail_delete BEFORE DELETE ON track_metadata BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        assert!(delete(&paths, &mut store, &State::default(), &track.id).is_err());
        assert!(track.playback.file().unwrap().exists());
        assert!(store.track(&track.id).unwrap().is_some());
        assert!(store.video_record("0OeEx5SiRI0").unwrap().is_some());
    }

    #[test]
    fn startup_recovers_deletion_on_either_side_of_database_commit() {
        for committed in [false, true] {
            let (_home, paths, mut store, track) = fixture();
            let staging = paths.data.join("imports/.staging");
            fs::create_dir_all(&staging).unwrap();
            let staged = staging.join(format!("delete-{}", uuid::Uuid::new_v4()));
            fs::rename(track.playback.file().unwrap().parent().unwrap(), &staged).unwrap();
            if committed {
                store.delete_import(&track.id).unwrap();
            }
            recover(&paths, &store).unwrap();
            assert!(!staged.exists());
            assert_eq!(track.playback.file().unwrap().exists(), !committed);
            assert_eq!(store.track(&track.id).unwrap().is_some(), !committed);
        }
    }
}

use super::*;
use crate::{
    library,
    model::{PlaybackSource, QueueItem, State},
    platform,
};
use std::time::UNIX_EPOCH;

fn empty() -> (tempfile::TempDir, Paths, Store) {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths {
        data: home.path().canonicalize().unwrap(),
        runtime: home.path().join("run"),
        cache: home.path().join("covers"),
    };
    let store = Store::open(&paths.database()).unwrap();
    (home, paths, store)
}

fn record(file: &Path, id: &str, paths: &Paths) -> Record {
    let metadata = file.metadata().unwrap();
    Record {
        track: library::read_track(file, id.into(), &paths.cache).unwrap(),
        modified: metadata
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        bytes: metadata.len(),
    }
}

fn fixture() -> (tempfile::TempDir, Paths, Store, PathBuf) {
    let (home, paths, mut store) = empty();
    let youtube = paths.data.join("imports/youtube/VIDEO000001");
    fs::create_dir_all(&youtube).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extended-mdat.m4a");
    fs::copy(&fixture, youtube.join("audio.m4a")).unwrap();
    image::RgbImage::from_pixel(8, 8, image::Rgb([50, 60, 70]))
        .save(youtube.join("cover.jpg"))
        .unwrap();
    fs::write(youtube.join("video.mkv"), b"saved-video-fixture").unwrap();
    let manifest = crate::imports::Manifest {
        track_id: "original-youtube".into(),
        source: crate::youtube::Source {
            video_id: "VIDEO000001".into(),
            video_url: "https://www.youtube.com/watch?v=VIDEO000001".into(),
            original_title: "원래 제목".into(),
            music_album: Some("Original album".into()),
            ..Default::default()
        },
        metadata: Metadata {
            title: "Title".into(),
            artist: "Artist".into(),
            method: "rules".into(),
            warning: None,
        },
        title_override: None,
        artist_override: None,
    };
    platform::atomic_json(&youtube.join("source.json"), &manifest).unwrap();
    let mut downloaded = record(&youtube.join("audio.m4a"), &manifest.track_id, &paths);
    downloaded.track.source = Some(manifest.source);
    let local = paths.data.join("original.m4a");
    fs::copy(fixture, &local).unwrap();
    let local_record = record(&local, "original-local", &paths);
    store.replace_catalog(&[downloaded, local_record]).unwrap();
    store
        .edit_metadata(
            "original-youtube",
            Some("김동률 노래".into()),
            Some("김동률".into()),
            Some(String::new()),
            None,
        )
        .unwrap();
    store
        .edit_metadata(
            "original-local",
            Some("Local title".into()),
            None,
            Some("Local album".into()),
            None,
        )
        .unwrap();
    store
        .add_streams(&[streams::Entry {
            name: "음악 Radio".into(),
            url: "https://example.com/live".into(),
        }])
        .unwrap();
    (home, paths, store, local)
}

fn restore(paths: &Paths, store: &mut Store, archive: &Path) -> Report {
    let stop = AtomicBool::new(false);
    let mut publication = prepare(
        paths,
        archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    publication
        .publish(paths, &stop, &mut Tracker::silent())
        .unwrap();
    store.commit_archive(&publication).unwrap();
    publication.finish(paths, true).unwrap();
    publication.report
}

#[test]
fn archive_roundtrip_preserves_assets_overrides_rescans_and_session() {
    let (_source, paths, _store, local) = fixture();
    let archive = paths.data.join("library.tar.gz");
    let exported = export(&paths, &archive, true).unwrap();
    assert_eq!(
        (
            exported.included,
            exported.videos,
            exported.references,
            exported.radios
        ),
        (2, 1, 0, 1)
    );
    assert!(export(&paths, &archive, true).is_err());
    let before = fs::read(&archive).unwrap();
    assert!(!before.is_empty());

    let (_target, target, mut store) = empty();
    let original = record(&local, "queue-only", &target).track;
    let state = State {
        queue: vec![QueueItem::new(original)],
        volume: 0,
        ..Default::default()
    };
    store.save(&state).unwrap();
    let report = restore(&target, &mut store, &archive);
    assert_eq!((report.added, report.radios), (2, 1));
    assert_eq!(
        serde_json::to_value(store.restore().unwrap()).unwrap(),
        serde_json::to_value(&state).unwrap()
    );
    let tracks = store.records().unwrap();
    assert_eq!(tracks.len(), 2);
    let youtube = tracks.iter().find(|r| r.track.source.is_some()).unwrap();
    assert_eq!(youtube.track.title, "김동률 노래");
    assert_eq!(youtube.track.artist, "김동률");
    assert_eq!(youtube.track.album, "");
    assert_ne!(youtube.track.id, "original-youtube");
    let managed = crate::deletion::managed_path(&target, &youtube.track).unwrap();
    assert_eq!(
        fs::read(managed.join("video.mkv")).unwrap(),
        b"saved-video-fixture"
    );
    assert_eq!(
        fs::read(youtube.track.cover.as_ref().unwrap()).unwrap(),
        fs::read(paths.data.join("imports/youtube/VIDEO000001/cover.jpg")).unwrap()
    );
    let scan = library::scan(
        &store.roots().unwrap(),
        &store.records().unwrap(),
        &target.cache,
    );
    store.replace_catalog(&scan.records).unwrap();
    assert_eq!(store.track(&youtube.track.id).unwrap().unwrap().album, "");
    assert_eq!(store.search("", 0, 10).unwrap().1, 3);

    store
        .edit_metadata(
            &youtube.track.id,
            Some("Keep destination edit".into()),
            None,
            None,
            None,
        )
        .unwrap();
    let repeated = restore(&target, &mut store, &archive);
    assert_eq!(
        (repeated.added, repeated.radios, repeated.duplicates),
        (0, 0, 3)
    );
    assert_eq!(
        store.track(&youtube.track.id).unwrap().unwrap().title,
        "Keep destination edit"
    );
    drop(store);
    let store = Store::open(&target.database()).unwrap();
    assert_eq!(store.records().unwrap().len(), 2);
    assert_eq!(fs::read(archive).unwrap(), before);
}

#[test]
fn archive_external_references_reconnect_only_matching_files_and_do_not_scan_siblings() {
    let (_home, paths, _store, local) = fixture();
    let archive = paths.data.join("refs.tar.gz");
    let report = export(&paths, &archive, false).unwrap();
    assert_eq!((report.included, report.references), (1, 1));
    let (_target, target, mut store) = empty();
    let preview_report = preview(&target, &archive).unwrap();
    assert_eq!((preview_report.added, preview_report.reconnected), (2, 1));
    let report = restore(&target, &mut store, &archive);
    assert_eq!(report.reconnected, 1);
    assert!(store.roots().unwrap().contains(&local));
    assert!(!store.roots().unwrap().contains(&paths.data));
    let tracks = store.records().unwrap();
    let linked = tracks.iter().find(|r| r.track.source.is_none()).unwrap();
    assert_eq!(
        linked.track.playback,
        PlaybackSource::File {
            path: local.clone()
        }
    );
    let scan = library::scan(&store.roots().unwrap(), &tracks, &target.cache);
    assert_eq!(scan.records.len(), 2);
    store.replace_catalog(&scan.records).unwrap();
    assert_eq!(
        store.track(&linked.track.id).unwrap().unwrap().album,
        "Local album"
    );

    fs::write(&local, b"changed").unwrap();
    let (_missing, missing, mut missing_store) = empty();
    let report = restore(&missing, &mut missing_store, &archive);
    assert_eq!(
        (report.added, report.missing, report.reconnected),
        (1, 1, 0)
    );
    assert_eq!(report.status, "partial");
    fs::remove_file(local).unwrap();
    assert_eq!(preview(&missing, &archive).unwrap().missing, 1);
}

#[test]
fn archive_snapshot_reads_wal_without_starting_server_or_creating_missing_database() {
    let (_home, paths, store, local) = fixture();
    assert!(paths.database().with_extension("db-wal").exists());
    let archive = paths.data.join("snapshot.tar.gz");
    export(&paths, &archive, false).unwrap();
    let home = tempfile::tempdir().unwrap();
    let target = Paths {
        data: home.path().join("absent"),
        runtime: home.path().join("run"),
        cache: home.path().join("cache"),
    };
    assert_eq!(preview(&target, &archive).unwrap().reconnected, 1);
    assert!(!target.data.exists());
    assert!(!target.runtime.exists());
    assert!(local.exists());
    assert_eq!(store.records().unwrap().len(), 2);
}

#[test]
fn archive_rolls_back_database_failure_and_recovers_both_commit_outcomes() {
    let (_source, paths, _store, _) = fixture();
    let archive = paths.data.join("restore.tar.gz");
    export(&paths, &archive, true).unwrap();
    let (_home, target, mut store) = empty();
    let stop = AtomicBool::new(false);
    let mut publication = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    publication
        .publish(&target, &stop, &mut Tracker::silent())
        .unwrap();
    let connection = rusqlite::Connection::open(target.database()).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_archive BEFORE INSERT ON tracks BEGIN SELECT RAISE(FAIL,'injected archive failure'); END;").unwrap();
    assert!(store.commit_archive(&publication).is_err());
    assert!(store.records().unwrap().is_empty());
    assert!(store.roots().unwrap().is_empty());
    publication.finish(&target, false).unwrap();
    assert!(!target.data.join("imports/youtube/VIDEO000001").exists());
    connection
        .execute_batch("DROP TRIGGER fail_archive")
        .unwrap();

    let mut pending = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    pending
        .publish(&target, &stop, &mut Tracker::silent())
        .unwrap();
    drop(pending); // Simulated crash before catalog transaction.
    recover(&target, &store).unwrap();
    assert!(!target.data.join("imports/youtube/VIDEO000001").exists());

    let mut committed = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    committed
        .publish(&target, &stop, &mut Tracker::silent())
        .unwrap();
    store.commit_archive(&committed).unwrap();
    drop(committed); // Simulated crash after catalog commit, before cleanup.
    recover(&target, &store).unwrap();
    assert!(
        target
            .data
            .join("imports/youtube/VIDEO000001/audio.m4a")
            .is_file()
    );
    assert_eq!(store.records().unwrap().len(), 2);
    assert_eq!(
        fs::read_dir(target.data.join("archives/.staging"))
            .unwrap()
            .count(),
        0
    );
}

fn rewrite(input: &Path, output: &Path, mutate: impl FnOnce(&mut Manifest), extra: bool) {
    let temp = tempfile::tempdir().unwrap();
    let mut manifest = extract(
        input,
        temp.path(),
        &AtomicBool::new(false),
        &mut Tracker::silent(),
    )
    .unwrap();
    let originals: Vec<_> = manifest.tracks.iter().flat_map(assets).cloned().collect();
    mutate(&mut manifest);
    let mut tar = tar::Builder::new(GzEncoder::new(
        File::create(output).unwrap(),
        Compression::fast(),
    ));
    let json = serde_json::to_vec(&manifest).unwrap();
    append(
        &mut tar,
        "manifest.json",
        json.len() as u64,
        json.as_slice(),
    )
    .unwrap();
    for a in &originals {
        append(
            &mut tar,
            &a.path,
            a.bytes,
            File::open(temp.path().join(&a.path)).unwrap(),
        )
        .unwrap();
    }
    if extra {
        let a = &originals[0];
        append(
            &mut tar,
            &a.path,
            a.bytes,
            File::open(temp.path().join(&a.path)).unwrap(),
        )
        .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
}

#[test]
fn archive_rejects_corruption_versions_duplicate_paths_traversal_and_truncation() {
    let (_home, paths, _store, _) = fixture();
    let archive = paths.data.join("valid.tar.gz");
    export(&paths, &archive, true).unwrap();
    let output = paths.data.join("bad.tar.gz");
    rewrite(&archive, &output, |m| m.version = 99, false);
    assert!(preview(&paths, &output).is_err());
    rewrite(
        &archive,
        &output,
        |m| m.tracks[0].audio.as_mut().unwrap().sha256 = "0".repeat(64),
        false,
    );
    assert!(preview(&paths, &output).is_err());
    rewrite(
        &archive,
        &output,
        |m| m.tracks[0].audio.as_mut().unwrap().path = "media/../../outside.m4a".into(),
        false,
    );
    assert!(preview(&paths, &output).is_err());
    rewrite(&archive, &output, |_| (), true);
    assert!(preview(&paths, &output).is_err());
    let bytes = fs::read(&archive).unwrap();
    for length in [bytes.len() / 2, bytes.len() - 4] {
        fs::write(&output, &bytes[..length]).unwrap();
        assert!(preview(&paths, &output).is_err());
    }
    assert_eq!(
        fs::read_dir(paths.data.join("imports/youtube"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn archive_missing_included_audio_fails_but_missing_external_reference_is_reported() {
    let (_home, paths, _store, local) = fixture();
    fs::remove_file(&local).unwrap();
    let archive = paths.data.join("missing.tar.gz");
    let report = export(&paths, &archive, false).unwrap();
    assert_eq!(report.references, 1);
    assert!(report.warning_count > 0);
    assert!(export(&paths, &paths.data.join("all.tar.gz"), true).is_err());
    fs::remove_file(paths.data.join("imports/youtube/VIDEO000001/audio.m4a")).unwrap();
    assert!(export(&paths, &paths.data.join("audio-missing.tar.gz"), false).is_err());
    assert!(!paths.data.join("audio-missing.tar.gz").exists());
}

#[test]
fn archive_recovery_preserves_committed_files_after_unregistering_every_track() {
    let (_source, paths, _store, _) = fixture();
    let archive = paths.data.join("restore.tar.gz");
    export(&paths, &archive, true).unwrap();
    let (_home, target, mut store) = empty();
    let stop = AtomicBool::new(false);
    let mut publication = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    publication
        .publish(&target, &stop, &mut Tracker::silent())
        .unwrap();
    store.commit_archive(&publication).unwrap();
    let files: Vec<_> = publication
        .records
        .iter()
        .map(|r| r.record.track.playback.file().unwrap().to_path_buf())
        .collect();
    // A deferred cleanup journal outlives later, legitimate catalog changes.
    store.replace_catalog(&[]).unwrap();
    drop(publication);
    recover(&target, &store).unwrap();
    assert!(files.iter().all(|p| p.is_file()));
    assert!(store.records().unwrap().is_empty());
}

#[test]
fn archive_rejects_links_and_missing_payloads_without_touching_external_files() {
    let (_home, paths, _store, local) = fixture();
    let valid = paths.data.join("valid.tar.gz");
    export(&paths, &valid, true).unwrap();
    let stage = tempfile::tempdir().unwrap();
    let manifest = extract(
        &valid,
        stage.path(),
        &AtomicBool::new(false),
        &mut Tracker::silent(),
    )
    .unwrap();
    let json = serde_json::to_vec(&manifest).unwrap();
    let original = fs::read(&local).unwrap();
    for entry_type in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Fifo,
    ] {
        let bad = paths.data.join("link.tar.gz");
        let mut tar = tar::Builder::new(GzEncoder::new(
            File::create(&bad).unwrap(),
            Compression::fast(),
        ));
        append(
            &mut tar,
            "manifest.json",
            json.len() as u64,
            json.as_slice(),
        )
        .unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(entry_type);
        header.set_size(0);
        header.set_mode(0o600);
        header.set_link_name(&local).unwrap();
        header.set_cksum();
        tar.append_data(
            &mut header,
            &manifest.tracks[0].audio.as_ref().unwrap().path,
            io::empty(),
        )
        .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        assert!(preview(&paths, &bad).is_err());
        assert_eq!(fs::read(&local).unwrap(), original);
    }
    let bad = paths.data.join("absent.tar.gz");
    let mut tar = tar::Builder::new(GzEncoder::new(
        File::create(&bad).unwrap(),
        Compression::fast(),
    ));
    append(
        &mut tar,
        "manifest.json",
        json.len() as u64,
        json.as_slice(),
    )
    .unwrap();
    tar.into_inner().unwrap().finish().unwrap();
    assert!(preview(&paths, &bad).is_err());
}

#[test]
fn archive_publication_failure_cleans_its_files_and_preserves_existing_destination() {
    let (_source, paths, _store, _) = fixture();
    let archive = paths.data.join("restore.tar.gz");
    export(&paths, &archive, true).unwrap();
    let (_home, target, store) = empty();
    let stop = AtomicBool::new(false);
    let mut publication = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    let occupied = publication.records[1]
        .record
        .track
        .playback
        .file()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&occupied).unwrap();
    fs::write(occupied.join("personal.txt"), b"do not delete").unwrap();
    assert!(
        publication
            .publish(&target, &stop, &mut Tracker::silent())
            .is_err()
    );
    publication.finish(&target, false).unwrap();
    assert_eq!(
        fs::read(occupied.join("personal.txt")).unwrap(),
        b"do not delete"
    );
    assert!(!target.data.join("imports/youtube/VIDEO000001").exists());
    assert!(store.records().unwrap().is_empty());
}

#[test]
fn archive_progress_reports_work_before_publication_and_exact_payload_totals() {
    let (_source, paths, _store, _) = fixture();
    let archive = paths.data.join("progress.tar.gz");
    let mut events = vec![];
    export_with_progress(&paths, &archive, true, &mut |p| {
        if p.stage == "hashing" || p.stage == "compressing" {
            assert!(!archive.exists());
        }
        events.push(p.clone());
    })
    .unwrap();
    let hashing = events
        .iter()
        .find(|p| p.stage == "hashing" && p.items_done == p.items_total)
        .unwrap();
    assert_eq!(hashing.items_total, 2);
    assert!(hashing.bytes_done > 0);
    assert_eq!(hashing.bytes_total, None);
    let compressed = events
        .iter()
        .find(|p| p.stage == "compressing" && p.items_done == p.items_total)
        .unwrap();
    assert!(compressed.items_total >= 4); // Audio, cover and video.
    assert_eq!(Some(compressed.bytes_done), compressed.bytes_total);
    assert_eq!(events.last().unwrap().stage, "completed");

    let (_target, target, _) = empty();
    let mut restored = vec![];
    preview_with_progress(&target, &archive, &mut |p| restored.push(p.clone())).unwrap();
    let extracted = restored
        .iter()
        .find(|p| p.stage == "extracting" && p.items_done == p.items_total)
        .unwrap();
    assert_eq!(extracted.bytes_done, compressed.bytes_done);
    assert_eq!(extracted.bytes_total, compressed.bytes_total);
    assert!(restored.iter().any(|p| p.stage == "validating"));
    assert!(restored.iter().any(|p| p.stage == "planning"));
}

#[test]
fn archive_names_are_readable_when_opened_with_a_standard_tar_reader() {
    let (_home, paths, _store, _) = fixture();
    let path = paths.data.join("readable.tar.gz");
    export(&paths, &path, true).unwrap();
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(&path).unwrap()));
    let names: Vec<_> = tar
        .entries()
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        names.contains(&"김동률 - 김동률 노래/김동률 - 김동률 노래.m4a".to_owned()),
        "{names:?}"
    );
    assert!(names.contains(&"김동률 - 김동률 노래/김동률 - 김동률 노래.video.mkv".to_owned()));
    assert!(names.contains(&"김동률 - 김동률 노래/cover.jpg".to_owned()));
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("media/") || name.starts_with("covers/"))
    );
    assert!(
        !names
            .iter()
            .any(|name| name.ends_with("/audio.m4a") || name.ends_with("/video.mkv"))
    );
    let (_target, target, mut store) = empty();
    assert_eq!(restore(&target, &mut store, &path).added, 2);
    let extracted = tempfile::tempdir().unwrap();
    tar::Archive::new(GzDecoder::new(File::open(&path).unwrap()))
        .unpack(extracted.path())
        .unwrap();
    let folder = extracted.path().join("김동률 - 김동률 노래");
    let track = library::read_track(
        &folder.join("김동률 - 김동률 노래.m4a"),
        "extracted".into(),
        &extracted.path().join("cache"),
    )
    .unwrap();
    assert_eq!(track.cover, Some(folder.join("cover.jpg")));
    assert_eq!(
        fs::read(folder.join("김동률 - 김동률 노래.video.mkv")).unwrap(),
        b"saved-video-fixture"
    );
}

#[test]
fn readable_names_keep_unicode_and_disambiguate_sanitized_or_duplicate_titles() {
    let mut used = HashSet::new();
    let mut track = Track {
        id: "unused".into(),
        playback: PlaybackSource::File {
            path: "/unused.m4a".into(),
        },
        title: "감사".into(),
        artist: "김동률".into(),
        album: String::new(),
        track_number: 0,
        duration_ms: None,
        cover: None,
        source: None,
    };
    assert_eq!(readable_name(&track, &mut used), "김동률 - 감사");
    assert_eq!(readable_name(&track, &mut used), "김동률 - 감사 (2)");
    track.artist.clear();
    track.title = "A/B: C?".into();
    assert_eq!(readable_name(&track, &mut used), "A B C");
    track.title = "a b c".into();
    assert_eq!(readable_name(&track, &mut used), "a b c (2)");
    track.title = "아주 긴 노래 제목 ".repeat(100);
    let long = readable_name(&track, &mut used);
    assert!(long.len() <= 80);
    assert!(long.starts_with("아주 긴 노래 제목"));
    let mut tar = tar::Builder::new(Vec::new());
    append(&mut tar, &format!("{long}/{long}.video.mkv"), 1, &[0u8][..]).unwrap();
}

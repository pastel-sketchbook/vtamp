use std::{fs::File, path::PathBuf};
use vtamp::library;

#[test]
fn extended_mdat_metadata_and_decoding() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extended-mdat.m4a");
    let cache = tempfile::tempdir().unwrap();
    let track = library::read_track(&path, "fixture".into(), cache.path()).unwrap();
    assert_eq!(track.title, "Synthetic tone");
    assert_eq!(track.artist, "vtamp tests");
    assert!(track.duration_ms >= 200);
    let decoder = rodio::Decoder::try_from(File::open(path).unwrap()).unwrap();
    assert!(decoder.take(10000).any(|sample| sample.abs() > 0.00001));
}

#[test]
#[ignore = "Set VTAMP_TEST_MUSIC_DIR to local m4a fixtures; no copyrighted media is bundled"]
fn local_m4a_decodes_indexes_and_reuses_ids() {
    let root =
        PathBuf::from(std::env::var("VTAMP_TEST_MUSIC_DIR").expect("Set VTAMP_TEST_MUSIC_DIR"));
    let cache = tempfile::tempdir().unwrap();
    let first = library::scan(std::slice::from_ref(&root), &[], cache.path());
    assert!(!first.records.is_empty());
    assert!(first.records.iter().any(|r| r.track.cover.is_some()));
    for record in first.records.iter().take(5) {
        let mut decoder =
            rodio::Decoder::try_from(File::open(&record.track.path).unwrap()).unwrap();
        assert!(decoder.by_ref().take(100_000).any(|s| s.abs() > 0.00001));
    }
    let second = library::scan(&[root], &first.records, cache.path());
    assert_eq!(first.records.len(), second.records.len());
    for (a, b) in first.records.iter().zip(second.records.iter()) {
        assert_eq!(a.track.id, b.track.id);
    }
}

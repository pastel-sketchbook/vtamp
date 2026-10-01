use std::{path::PathBuf, time::Duration};
use vtamp::{audio::decode_file, library};

#[test]
fn extended_mdat_metadata_and_decoding() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extended-mdat.m4a");
    let cache = tempfile::tempdir().unwrap();
    let track = library::read_track(&path, "fixture".into(), cache.path()).unwrap();
    assert_eq!(track.title, "Synthetic tone");
    assert_eq!(track.artist, "vtamp tests");
    assert!(track.duration_ms.unwrap() >= 200);
    let decoder = decode_file(&path).unwrap();
    assert!(decoder.take(10000).any(|sample| sample.abs() > 0.00001));
}

#[test]
fn lossless_decoding_keeps_stereo_channels_and_samples() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let wav = decode_file(&fixture.join("stereo.wav")).unwrap();
    let alac = decode_file(&fixture.join("stereo-alac.m4a")).unwrap();
    for source in [&wav, &alac] {
        assert_eq!(source.channels().get(), 2);
        assert_eq!(source.sample_rate().get(), 48000);
        assert_eq!(source.total_duration(), Some(Duration::from_millis(300)));
    }
    let wav: Vec<_> = wav.collect();
    let alac: Vec<_> = alac.collect();
    assert_eq!(wav.len(), 14400 * 2);
    assert_eq!(alac.len(), wav.len());
    assert!(wav.iter().zip(&alac).all(|(a, b)| (a - b).abs() < 0.0001));
    assert!(
        wav.as_chunks::<2>()
            .0
            .iter()
            .any(|f| (f[0] - f[1]).abs() > 0.1)
    );
}

#[test]
fn invalid_audio_returns_a_path_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.m4a");
    std::fs::write(&path, b"not an audio file").unwrap();
    let error = decode_file(&path).err().expect("invalid file must fail");
    assert!(format!("{error:#}").contains("broken.m4a"));
    assert!(decode_file(&dir.path().join("missing.m4a")).is_err());
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
        let mut decoder = decode_file(record.track.playback.file().unwrap()).unwrap();
        assert!(decoder.by_ref().take(100_000).any(|s| s.abs() > 0.00001));
    }
    let second = library::scan(&[root], &first.records, cache.path());
    assert_eq!(first.records.len(), second.records.len());
    for (a, b) in first.records.iter().zip(second.records.iter()) {
        assert_eq!(a.track.id, b.track.id);
    }
}

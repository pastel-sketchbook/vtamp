use serde_json::Value;

/// Background metering may advance its counts and the state revision while an
/// unrelated operation runs. Playback, normalization preference/gain and queue
/// revisions must still be identical.
pub fn assert_playback_unchanged(mut before: Value, mut after: Value) {
    assert!(after["revision"].as_u64().unwrap() >= before["revision"].as_u64().unwrap());
    for state in [&mut before, &mut after] {
        state.as_object_mut().unwrap().remove("revision");
        let normalization = state["normalization"].as_object_mut().unwrap();
        for count in ["ready", "pending", "failed", "unmeasurable"] {
            normalization.remove(count);
        }
    }
    assert_eq!(after, before);
}

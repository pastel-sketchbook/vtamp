//! Validate a complete queue edit without touching playback or persistent state.
use crate::{model::*, store::Store};
use anyhow::Result;
use serde_json::{Value, json};

pub fn prepare(state: &State, edit: &QueueEdit, store: &Store) -> Result<(State, Vec<Value>)> {
    if edit.operations.is_empty() || edit.operations.len() > 1000 {
        return Err(ApiError::new(
            "invalid_arguments",
            "Provide between 1 and 1000 queue operations",
        )
        .into());
    }
    let mut next = state.clone();
    let mut changes = Vec::new();
    for operation in &edit.operations {
        match operation {
            QueueOperation::Add {
                track_ids,
                after_current,
                index,
            } => {
                if track_ids.is_empty() || (*after_current && index.is_some()) {
                    return Err(ApiError::new(
                        "invalid_arguments",
                        "Add requires tracks and at most one destination",
                    )
                    .into());
                }
                if track_ids.len() > 10_000usize.saturating_sub(next.queue.len()) {
                    return Err(
                        ApiError::new("queue_limit", "Queue limit is 10,000 entries").into(),
                    );
                }
                let index = if *after_current {
                    next.queue_cursor_index().map_or(0, |i| i + 1)
                } else {
                    index.unwrap_or(next.queue.len())
                };
                if index > next.queue.len() {
                    return Err(ApiError::new(
                        "invalid_arguments",
                        "Insertion index is outside the queue",
                    )
                    .into());
                }
                let items = track_ids
                    .iter()
                    .map(|id| {
                        store.track(id)?.map(QueueItem::new).ok_or_else(|| {
                            ApiError::new("track_not_found", "Library track not found")
                                .with_details(json!({"track_id": id}))
                                .into()
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                if *after_current {
                    next.play_next
                        .splice(0..0, items.iter().map(|i| i.id.clone()));
                }
                changes.push(json!({"op":"add", "index":index, "items":items, "after_current":after_current}));
                next.queue.splice(index..index, items);
            }
            QueueOperation::Remove { queue_item_ids } => {
                if queue_item_ids.is_empty() {
                    return Err(ApiError::new(
                        "invalid_arguments",
                        "Remove requires queue entry IDs",
                    )
                    .into());
                }
                for id in queue_item_ids {
                    if next.current_id.as_ref() == Some(id) {
                        return Err(ApiError::new("current_item_protected", "Batch edits cannot remove the current entry; use queue remove explicitly").into());
                    }
                    let index = locate(&next, id)?;
                    next.queue.remove(index);
                    if next.queue_cursor.as_ref() == Some(id) {
                        next.queue_cursor = index.checked_sub(1).map(|i| next.queue[i].id.clone());
                    }
                    next.play_next.retain(|queued| queued != id);
                }
                changes.push(json!({"op":"remove", "queue_item_ids":queue_item_ids}));
            }
            QueueOperation::Move {
                queue_item_id,
                index,
            } => {
                let old = locate(&next, queue_item_id)?;
                if *index >= next.queue.len() {
                    return Err(ApiError::new(
                        "invalid_arguments",
                        "Destination index is outside the queue",
                    )
                    .into());
                }
                let item = next.queue.remove(old);
                next.queue.insert(*index, item);
                changes.push(
                    json!({"op":"move", "queue_item_id":queue_item_id, "from":old, "index":index}),
                );
            }
        }
    }
    if next.queue_changed_since(state) {
        next.queue_revision += 1;
        next.revision += 1;
    }
    Ok((next, changes))
}

fn locate(state: &State, id: &str) -> Result<usize> {
    state
        .queue
        .iter()
        .position(|item| item.id == id)
        .ok_or_else(|| {
            ApiError::new("queue_item_not_found", "Queue entry not found")
                .with_details(json!({"queue_item_id":id}))
                .into()
        })
}

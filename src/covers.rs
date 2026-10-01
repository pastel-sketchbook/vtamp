//! Re-fetch artwork for existing imports so stored covers match the current crop.

use crate::{
    import_config,
    model::{CoverJob, CoverReport, unix_ms},
    platform::Paths,
    subprocess::Cancel,
    youtube,
};
use anyhow::Result;
use std::{fs, path::PathBuf, sync::atomic::Ordering};

/// One managed import whose cover can be regenerated.
pub struct Target {
    pub track_id: String,
    pub title: String,
    pub video_id: String,
    pub cover: PathBuf,
}

pub enum Message {
    Progress(CoverJob),
    /// A cover file changed; the owner updates the library record.
    Cover {
        track_id: String,
        cover: PathBuf,
    },
    Done(CoverJob),
}

pub struct Running {
    pub id: String,
    cancel: Cancel,
    pub thread: std::thread::JoinHandle<()>,
}

impl Running {
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn spawn(
    paths: Paths,
    job: CoverJob,
    targets: Vec<Target>,
    config: import_config::Config,
    send: impl Fn(Message) -> bool + Send + 'static,
) -> Running {
    let stop = crate::subprocess::cancel();
    let worker_stop = stop.clone();
    let id = job.job_id.clone();
    let thread = std::thread::spawn(move || {
        let mut job = job;
        let result = work(&paths, &mut job, &targets, &config, &worker_stop, &send);
        job.current = None;
        job.finished_at_ms = Some(unix_ms());
        job.status = if worker_stop.load(Ordering::Relaxed) {
            "cancelled".into()
        } else if let Err(error) = result {
            job.error = Some(format!("{error:#}"));
            "failed".into()
        } else if job.failed == 0 {
            "completed".into()
        } else if job.refreshed + job.unchanged > 0 {
            "partial".into()
        } else {
            "failed".into()
        };
        send(Message::Done(job));
    });
    Running {
        id,
        cancel: stop,
        thread,
    }
}

fn work(
    paths: &Paths,
    job: &mut CoverJob,
    targets: &[Target],
    config: &import_config::Config,
    stop: &Cancel,
    send: &impl Fn(Message) -> bool,
) -> Result<()> {
    for target in targets {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // Each track gets a private stage so a failed fetch cannot leak a
        // thumbnail into the next one.
        let stage = paths
            .data
            .join("imports/.staging")
            .join(format!("cover-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<Option<PathBuf>> {
            crate::platform::private_dir(&stage)?;
            youtube::thumbnail(&target.video_id, &stage, config, stop)?;
            youtube::cover(&stage, config, stop)?;
            let fresh = stage.join("cover.jpg");
            // Re-importing the same video yields the same crop; skip the write.
            if fs::read(&target.cover).is_ok_and(|old| fs::read(&fresh).is_ok_and(|new| old == new))
            {
                return Ok(None);
            }
            // The stage and the import directory share the data filesystem.
            fs::rename(&fresh, &target.cover)?;
            Ok(Some(target.cover.clone()))
        })();
        let _ = fs::remove_dir_all(&stage);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        job.current = Some(target.title.clone());
        match result {
            Ok(Some(cover)) => {
                job.refreshed += 1;
                if !send(Message::Cover {
                    track_id: target.track_id.clone(),
                    cover,
                }) {
                    return Ok(());
                }
            }
            Ok(None) => job.unchanged += 1,
            Err(error) => {
                job.failed += 1;
                if job.reports.len() < CoverJob::MAX_REPORTS {
                    job.reports.push(CoverReport {
                        track_id: target.track_id.clone(),
                        title: target.title.clone(),
                        message: format!("{error:#}"),
                    });
                }
            }
        }
        if !send(Message::Progress(job.clone())) {
            return Ok(());
        }
    }
    Ok(())
}

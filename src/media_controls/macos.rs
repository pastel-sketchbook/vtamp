use super::{Artwork, Delivery, RemoteCommand, Snapshot, deliver};
use crate::model::{Command, PlaybackStatus};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::{
    AnyThread, MainThreadMarker,
    rc::{Retained, autoreleasepool},
    runtime::AnyObject,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
    NSImage,
};
use objc2_foundation::{NSData, NSDictionary, NSNumber, NSPoint, NSSize, NSString};
use objc2_media_player::*;
use std::{
    cell::RefCell,
    io::{Cursor, Read},
    path::PathBuf,
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};

static RUNNING: AtomicBool = AtomicBool::new(false);
thread_local! { static NATIVE: RefCell<Option<Native>> = const { RefCell::new(None) }; }

pub(super) fn run(
    task: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) -> anyhow::Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("macOS event loop needs the main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    // A CLI process can already be Prohibited; AppKit may return false for the
    // redundant policy request even though its event loop works normally.
    if app.activationPolicy() != NSApplicationActivationPolicy::Prohibited
        && app.activationPolicy() != NSApplicationActivationPolicy::Accessory
        && !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory)
    {
        eprintln!("vtamp: Cannot initialize background macOS application; media controls disabled");
        return task();
    }
    RUNNING.store(true, Ordering::Release);
    let worker = std::thread::Builder::new().name("vtamp-server".into()).spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
        DispatchQueue::main().exec_async(|| {
            NATIVE.with(|slot| { slot.borrow_mut().take(); });
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            app.stop(None);
            // stop() needs another event to wake AppKit's nextEvent wait.
            if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                NSEventType::ApplicationDefined, NSPoint::ZERO, NSEventModifierFlags::empty(), 0.0, 0, None, 0, 0, 0,
            ) { app.postEvent_atStart(&event, true); }
        });
        result.unwrap_or_else(|_| Err(anyhow::anyhow!("Server thread panicked")))
    })?;
    app.run();
    RUNNING.store(false, Ordering::Release);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("Server thread panicked"))?
}

type Sender = Arc<dyn Fn(Command) -> bool + Send + Sync>;
#[derive(Default)]
struct Pending {
    snapshot: Option<Snapshot>,
    scheduled: bool,
    closed: bool,
}
pub(super) struct Bridge {
    pending: Arc<Mutex<Pending>>,
}
impl Bridge {
    pub fn new(send: impl Fn(Command) -> bool + Send + Sync + 'static) -> Option<Self> {
        if !RUNNING.load(Ordering::Acquire) {
            return None;
        }
        let send: Sender = Arc::new(send);
        DispatchQueue::main().exec_async(move || {
            autoreleasepool(|_| {
                NATIVE.with(|slot| {
                    *slot.borrow_mut() = Some(Native::new(send));
                    tracing::info!("macOS media controls registered");
                })
            });
        });
        Some(Self {
            pending: Arc::default(),
        })
    }
    pub fn update(&self, snapshot: Snapshot) {
        let mut pending = self.pending.lock().unwrap();
        pending.snapshot = Some(snapshot);
        if pending.scheduled || pending.closed {
            return;
        }
        pending.scheduled = true;
        let shared = self.pending.clone();
        DispatchQueue::main().exec_async(move || {
            autoreleasepool(|_| {
                let snapshot = {
                    let mut pending = shared.lock().unwrap();
                    pending.scheduled = false;
                    if pending.closed {
                        return;
                    }
                    pending.snapshot.take()
                };
                if let Some(snapshot) = snapshot {
                    NATIVE.with(|slot| {
                        if let Some(native) = slot.borrow_mut().as_mut() {
                            native.update(snapshot);
                        }
                    });
                }
            })
        });
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.pending.lock().unwrap().closed = true;
        DispatchQueue::main().exec_async(|| {
            autoreleasepool(|_| {
                NATIVE.with(|slot| {
                    slot.borrow_mut().take();
                })
            });
        });
    }
}

struct Native {
    center: Retained<MPNowPlayingInfoCenter>,
    handlers: Vec<(Retained<MPRemoteCommand>, Retained<AnyObject>)>,
    available: Arc<AtomicBool>,
    last: Option<Snapshot>,
    current: Option<Snapshot>,
    artwork: Artwork<Retained<MPMediaItemArtwork>>,
    art_request: Arc<Mutex<Option<(u64, PathBuf)>>>,
    art_wake: mpsc::SyncSender<()>,
}
impl Native {
    fn new(send: Sender) -> Self {
        let available = Arc::new(AtomicBool::new(false));
        let mut handlers = vec![];
        // MediaPlayer objects, handlers and retained tokens are confined to the
        // main queue. Callbacks only enqueue Rust commands; they never block on audio.
        unsafe {
            let commands = MPRemoteCommandCenter::sharedCommandCenter();
            for (command, action) in [
                (commands.playCommand(), RemoteCommand::Play),
                (commands.pauseCommand(), RemoteCommand::Pause),
                (commands.togglePlayPauseCommand(), RemoteCommand::Toggle),
                (commands.nextTrackCommand(), RemoteCommand::Next),
                (commands.previousTrackCommand(), RemoteCommand::Previous),
                (commands.stopCommand(), RemoteCommand::Stop),
                (
                    commands.changePlaybackPositionCommand().into_super(),
                    RemoteCommand::Seek(0.0),
                ),
            ] {
                let send = send.clone();
                let available = available.clone();
                let handler = RcBlock::new(move |event: NonNull<MPRemoteCommandEvent>| {
                    if !available.load(Ordering::Acquire) {
                        return MPRemoteCommandHandlerStatus::NoActionableNowPlayingItem;
                    }
                    let action = if matches!(action, RemoteCommand::Seek(_)) {
                        // The event's dynamic class is checked before accessing positionTime.
                        let Some(event) = event
                            .as_ref()
                            .downcast_ref::<MPChangePlaybackPositionCommandEvent>()
                        else {
                            return MPRemoteCommandHandlerStatus::CommandFailed;
                        };
                        RemoteCommand::Seek(event.positionTime())
                    } else {
                        action
                    };
                    tracing::debug!(?action, "macOS media command");
                    match deliver(action, available.load(Ordering::Acquire), &*send) {
                        Delivery::Accepted => MPRemoteCommandHandlerStatus::Success,
                        Delivery::NoContent => {
                            MPRemoteCommandHandlerStatus::NoActionableNowPlayingItem
                        }
                        Delivery::Failed => MPRemoteCommandHandlerStatus::CommandFailed,
                    }
                });
                let token = command.addTargetWithHandler(&handler);
                command.setEnabled(false);
                handlers.push((command, token));
            }
            for command in [
                commands.seekForwardCommand(),
                commands.seekBackwardCommand(),
                commands.skipForwardCommand().into_super(),
                commands.skipBackwardCommand().into_super(),
                commands.changePlaybackRateCommand().into_super(),
                commands.changeRepeatModeCommand().into_super(),
                commands.changeShuffleModeCommand().into_super(),
                commands.ratingCommand().into_super(),
                commands.likeCommand().into_super(),
                commands.dislikeCommand().into_super(),
                commands.bookmarkCommand().into_super(),
                commands.enableLanguageOptionCommand(),
                commands.disableLanguageOptionCommand(),
            ] {
                command.setEnabled(false);
            }
        }
        let art_request = Arc::new(Mutex::new(None::<(u64, PathBuf)>));
        let requests = art_request.clone();
        let (art_wake, wake) = mpsc::sync_channel(1);
        // One decoder with a coalesced latest request, not one thread per song.
        if let Err(error) = std::thread::Builder::new()
            .name("vtamp-system-art".into())
            .spawn(move || {
                while wake.recv().is_ok() {
                    let request = requests.lock().unwrap().take();
                    if let Some((generation, path)) = request {
                        let image = prepare_art(&path);
                        DispatchQueue::main().exec_async(move || {
                            autoreleasepool(|_| {
                                NATIVE.with(|slot| {
                                    if let Some(native) = slot.borrow_mut().as_mut()
                                        && native.artwork.complete(generation, || {
                                            image.and_then(|bytes| native_art(&bytes))
                                        })
                                    {
                                        native.publish();
                                    }
                                })
                            })
                        });
                    }
                }
            })
        {
            tracing::warn!(%error, "System artwork worker unavailable");
        }
        Self {
            center: unsafe { MPNowPlayingInfoCenter::defaultCenter() },
            handlers,
            available,
            last: None,
            current: None,
            artwork: Artwork::default(),
            art_request,
            art_wake,
        }
    }
    fn update(&mut self, snapshot: Snapshot) {
        let changed_track =
            self.current.as_ref().and_then(|s| s.track.as_ref()) != snapshot.track.as_ref();
        let needs_publish = self.last.as_ref().is_none_or(|old| {
            let expected = old.position_at(snapshot.observed_at);
            old.revision != snapshot.revision
                || old.track != snapshot.track
                || old.status != snapshot.status
                || old.waiting != snapshot.waiting
                || expected.abs_diff(snapshot.position_ms) > 500
        });
        if changed_track {
            let generation = self.artwork.begin();
            *self.art_request.lock().unwrap() = snapshot
                .track
                .as_ref()
                .and_then(|track| track.cover.clone())
                .map(|path| (generation, path));
            let _ = self.art_wake.try_send(());
        }
        let available = snapshot.track.is_some();
        self.available.store(available, Ordering::Release);
        unsafe {
            for (command, _) in &self.handlers {
                command.setEnabled(available);
            }
        }
        self.current = Some(snapshot);
        if needs_publish {
            self.publish();
        }
    }
    fn publish(&mut self) {
        let Some(snapshot) = &self.current else {
            return;
        };
        // All dictionary keys are Apple's metadata constants with their documented
        // NSString / NSNumber / MPMediaItemArtwork value types.
        unsafe {
            if let Some(track) = &snapshot.track {
                let mut keys = vec![
                    MPMediaItemPropertyTitle,
                    MPMediaItemPropertyArtist,
                    MPMediaItemPropertyAlbumTitle,
                    MPMediaItemPropertyPlaybackDuration,
                    MPNowPlayingInfoPropertyElapsedPlaybackTime,
                    MPNowPlayingInfoPropertyPlaybackRate,
                    MPNowPlayingInfoPropertyMediaType,
                ];
                let rate = if snapshot.status == PlaybackStatus::Playing && !snapshot.waiting {
                    1.0
                } else {
                    0.0
                };
                let mut values: Vec<Retained<AnyObject>> = vec![
                    NSString::from_str(&track.title).into(),
                    NSString::from_str(&track.artist).into(),
                    NSString::from_str(&track.album).into(),
                    NSNumber::new_f64(track.duration_ms as f64 / 1000.0).into(),
                    NSNumber::new_f64(snapshot.position_at(Instant::now()) as f64 / 1000.0).into(),
                    NSNumber::new_f64(rate).into(),
                    NSNumber::new_usize(MPNowPlayingInfoMediaType::Audio.0).into(),
                ];
                if let Some(art) = &self.artwork.value {
                    keys.push(MPMediaItemPropertyArtwork);
                    values.push(art.clone().into());
                }
                let refs: Vec<_> = values.iter().map(|v| &**v).collect();
                let dictionary = NSDictionary::from_slices(&keys, &refs);
                self.center.setNowPlayingInfo(Some(&dictionary));
                let state = if snapshot.status == PlaybackStatus::Paused {
                    MPNowPlayingPlaybackState::Paused
                } else if snapshot.waiting {
                    MPNowPlayingPlaybackState::Interrupted
                } else {
                    MPNowPlayingPlaybackState::Playing
                };
                self.center.setPlaybackState(state);
            } else {
                self.center
                    .setPlaybackState(MPNowPlayingPlaybackState::Stopped);
                self.center.setNowPlayingInfo(None);
            }
        }
        self.last = Some(snapshot.clone());
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.available.store(false, Ordering::Release);
        self.art_request.lock().unwrap().take();
        unsafe {
            for (command, token) in &self.handlers {
                command.setEnabled(false);
                command.removeTarget(Some(token));
            }
            self.center.setNowPlayingInfo(None);
            self.center
                .setPlaybackState(MPNowPlayingPlaybackState::Stopped);
        }
        tracing::info!("macOS media controls released");
    }
}
fn prepare_art(path: &std::path::Path) -> Option<Vec<u8>> {
    let mut bytes = vec![];
    std::fs::File::open(path)
        .ok()?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let image = crate::library::decode_image(&bytes)
        .ok()?
        .thumbnail(512, 512);
    let mut png = Cursor::new(vec![]);
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}
fn native_art(bytes: &[u8]) -> Option<Retained<MPMediaItemArtwork>> {
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(bytes))?;
    let size = image.size();
    let handler = RcBlock::new(move |_: NSSize| NonNull::from(&*image));
    // The retained image lives in the copied request block for the artwork's lifetime.
    Some(unsafe {
        MPMediaItemArtwork::initWithBoundsSize_requestHandler(
            MPMediaItemArtwork::alloc(),
            size,
            &handler,
        )
    })
}

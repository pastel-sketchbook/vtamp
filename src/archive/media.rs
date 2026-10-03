//! Prepare portable copies without changing the Library's original media.
use super::*;
use crate::{import_config::YoutubeConfig, subprocess};
use lofty::{
    config::WriteOptions,
    file::TaggedFileExt,
    picture::{Picture, PictureType},
    tag::{Accessor, Tag, TagExt},
};
use std::{process::Command, time::Duration};

fn mp4(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("m4a") || s.eq_ignore_ascii_case("mp4"))
}

pub(super) struct Cover {
    pub bytes: Vec<u8>,
    pub jpeg: bool,
}

impl Cover {
    fn new(bytes: Vec<u8>) -> Result<Self> {
        crate::library::decode_image(&bytes)?;
        let format = image::guess_format(&bytes)?;
        ensure!(
            matches!(format, image::ImageFormat::Jpeg | image::ImageFormat::Png),
            "Artwork must be JPEG or PNG"
        );
        Ok(Self {
            bytes,
            jpeg: format == image::ImageFormat::Jpeg,
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        Self::new(bytes)
    }

    pub fn save(&self, stage: &Path, youtube: bool) -> Result<String> {
        let name = if self.jpeg || youtube {
            "cover.jpg"
        } else {
            "cover.png"
        };
        if youtube && !self.jpeg {
            crate::library::decode_image(&self.bytes)?.save(stage.join(name))?;
        } else {
            fs::write(stage.join(name), &self.bytes)?;
        }
        File::open(stage.join(name))?.sync_all()?;
        Ok(name.into())
    }
}

pub(super) fn tag_audio(path: &Path, track: &Track, cover: Option<&Cover>) -> Result<()> {
    if mp4(path) {
        let mut tag = mp4ameta::Tag::read_from_path(path)?;
        tag.set_title(&track.title);
        tag.set_artist(&track.artist);
        if track.album.is_empty() {
            tag.remove_album();
        } else {
            tag.set_album(&track.album);
        }
        if let Some(cover) = cover {
            tag.set_artwork(if cover.jpeg {
                mp4ameta::Img::jpeg(cover.bytes.clone())
            } else {
                mp4ameta::Img::png(cover.bytes.clone())
            });
        }
        tag.write_to_path(path)?;
    } else {
        let mut tagged = lofty::read_from_path(path)?;
        let kind = tagged.primary_tag_type();
        if tagged.primary_tag().is_none() {
            tagged.insert_tag(Tag::new(kind));
        }
        let tag = tagged
            .primary_tag_mut()
            .context("Audio format does not support writable tags")?;
        tag.set_title(track.title.clone());
        tag.set_artist(track.artist.clone());
        if track.album.is_empty() {
            tag.remove_album();
        } else {
            tag.set_album(track.album.clone());
        }
        if let Some(cover) = cover {
            tag.remove_picture_type(PictureType::CoverFront);
            let mut picture = Picture::from_reader(&mut io::Cursor::new(&cover.bytes))?;
            picture.set_pic_type(PictureType::CoverFront);
            tag.push_picture(picture);
        }
        tag.save_to_path(path, WriteOptions::default())?;
    }
    File::open(path)?.sync_all()?;
    Ok(())
}

pub(super) fn embedded_cover(path: &Path) -> Result<Option<Cover>> {
    let bytes = if mp4(path) {
        mp4ameta::Tag::read_from_path(path)?
            .artwork()
            .map(|a| a.data.to_vec())
    } else {
        let tagged = lofty::read_from_path(path)?;
        tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .and_then(|tag| {
                tag.pictures()
                    .iter()
                    .find(|p| p.pic_type() == PictureType::CoverFront)
                    .or_else(|| tag.pictures().first())
                    .map(|p| p.data().to_vec())
            })
    };
    bytes.map(Cover::new).transpose()
}

pub(super) struct Tools {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl Tools {
    pub fn load(paths: &Paths) -> Result<Self> {
        let config = YoutubeConfig::load(paths)?;
        Ok(Self {
            ffmpeg: subprocess::executable(config.ffmpeg.as_deref(), "ffmpeg")
                .context("Archives containing video require installed FFmpeg")?,
            ffprobe: subprocess::executable(config.ffprobe.as_deref(), "ffprobe")
                .context("Archives containing video require installed FFprobe")?,
        })
    }

    pub fn probe(&self, path: &Path, audio: bool, stop: &AtomicBool) -> Result<()> {
        let bytes = subprocess::run(
            Command::new(&self.ffprobe)
                .args([
                    "-v",
                    "error",
                    "-show_streams",
                    "-show_format",
                    "-of",
                    "json",
                ])
                .arg(path),
            None,
            stop,
            Duration::from_secs(30),
            |_| {},
        )?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let streams = value["streams"]
            .as_array()
            .context("Video has no streams")?;
        let pictures: Vec<_> = streams
            .iter()
            .filter(|s| s["codec_type"] == "video")
            .collect();
        let sounds = streams
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .count();
        ensure!(
            pictures.len() == 1 && sounds == usize::from(audio),
            "Archive video must contain one video stream and {} audio streams",
            usize::from(audio)
        );
        let width = pictures[0]["width"].as_u64().unwrap_or(0);
        let height = pictures[0]["height"].as_u64().unwrap_or(0);
        let duration = value["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        ensure!(
            width > 0
                && width <= 8192
                && height > 0
                && height <= 480
                && duration.is_finite()
                && duration > 0.0,
            "Archive video must have valid timing and be at most 480 pixels high"
        );
        Ok(())
    }

    pub fn mux(
        &self,
        video: &Path,
        audio: &Path,
        output: &Path,
        stop: &AtomicBool,
        progress: &mut Tracker<'_>,
    ) -> Result<()> {
        self.probe(video, false, stop)?;
        let mut command = Command::new(&self.ffmpeg);
        command
            .args(["-nostdin", "-v", "error", "-nostats", "-n", "-i"])
            .arg(video)
            .arg("-i")
            .arg(audio)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c",
                "copy",
                "-map_metadata",
                "1",
                "-progress",
                "pipe:1",
            ])
            .arg(output);
        Self::run(command, stop, progress)?;
        self.probe(output, true, stop)?;
        File::open(output)?.sync_all()?;
        Ok(())
    }

    pub fn silent(
        &self,
        input: &Path,
        output: &Path,
        stop: &AtomicBool,
        progress: &mut Tracker<'_>,
    ) -> Result<()> {
        let mut command = Command::new(&self.ffmpeg);
        command
            .args(["-nostdin", "-v", "error", "-nostats", "-n", "-i"])
            .arg(input)
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-sn",
                "-dn",
                "-c:v",
                "copy",
                "-progress",
                "pipe:1",
            ])
            .arg(output);
        Self::run(command, stop, progress)?;
        self.probe(output, false, stop)?;
        File::open(output)?.sync_all()?;
        Ok(())
    }

    fn run(mut command: Command, stop: &AtomicBool, progress: &mut Tracker<'_>) -> Result<()> {
        let mut previous = 0u64;
        subprocess::run(
            &mut command,
            None,
            stop,
            Duration::from_secs(6 * 3600),
            |line| {
                if let Some(bytes) = line
                    .strip_prefix("total_size=")
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    progress
                        .advance(bytes.saturating_sub(previous).min(usize::MAX as u64) as usize);
                    previous = bytes;
                }
            },
        )?;
        Ok(())
    }
}

pub(super) fn video_tools(
    paths: &Paths,
    manifest: &Manifest,
    extracted: &Path,
    stop: &AtomicBool,
    progress: &mut Tracker<'_>,
) -> Result<Option<Tools>> {
    let videos: Vec<_> = manifest
        .tracks
        .iter()
        .filter_map(|e| e.video.as_ref())
        .collect();
    if videos.is_empty() {
        return Ok(None);
    }
    let tools = Tools::load(paths)?;
    progress.begin("validating_video", videos.len(), None);
    for (index, video) in videos.into_iter().enumerate() {
        progress.item(index, &video.path);
        tools.probe(&extracted.join(&video.path), true, stop)?;
    }
    progress.end();
    Ok(Some(tools))
}

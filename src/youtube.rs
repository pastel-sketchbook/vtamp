use crate::{
    import_config::Config,
    subprocess::{self, Cancel},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, tag = "provider", rename = "youtube")]
pub struct Source {
    pub video_id: String,
    pub video_url: String,
    pub original_title: String,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub channel_url: Option<String>,
    pub description: String,
    pub music_title: Option<String>,
    pub music_artist: Option<String>,
    pub music_album: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub url: String,
    pub title: String,
    pub playlist: bool,
    pub items: Vec<PreviewItem>,
    pub existing: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewItem {
    pub video_id: String,
    pub title: String,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub url: String,
    pub playlist: bool,
}
pub fn valid_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
pub fn video_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}
pub fn input(text: &str, playlist: bool) -> Result<Input> {
    let url = url::Url::parse(text.trim()).context("Expected a YouTube URL")?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        bail!("Unsupported YouTube URL");
    }
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "youtube.com"
            | "www.youtube.com"
            | "m.youtube.com"
            | "music.youtube.com"
            | "youtu.be"
            | "www.youtu.be"
    ) {
        bail!("Only YouTube video and playlist URLs are supported");
    }
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    let segments: Vec<_> = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    let id = if host.ends_with("youtu.be") {
        segments.first().copied()
    } else if matches!(segments.first(), Some(&"shorts" | &"embed" | &"live")) {
        segments.get(1).copied()
    } else if url.path() == "/watch" {
        pairs.get("v").map(String::as_str)
    } else {
        None
    };
    if playlist || (url.path() == "/playlist" && id.is_none()) {
        let list = pairs
            .get("list")
            .context("URL does not contain a playlist ID")?;
        if list.len() > 160
            || list.is_empty()
            || !list
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            bail!("Invalid playlist ID");
        }
        return Ok(Input {
            url: format!("https://www.youtube.com/playlist?list={list}"),
            playlist: true,
        });
    }
    let id = id
        .filter(|s| valid_id(s))
        .context("URL must identify one video or a playlist")?;
    Ok(Input {
        url: video_url(id),
        playlist: false,
    })
}
pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(2048)
        .collect::<String>()
        .trim()
        .to_owned()
}
fn field(v: &Value, name: &str) -> Option<String> {
    v[name].as_str().map(clean).filter(|s| !s.is_empty())
}
pub fn source(v: &Value) -> Result<Source> {
    let id = field(v, "id")
        .filter(|s| valid_id(s))
        .context("Extractor returned an invalid video ID")?;
    if v["is_live"] == true
        || matches!(
            v["live_status"].as_str(),
            Some("is_live" | "is_upcoming" | "post_live")
        )
    {
        bail!("Live and upcoming broadcasts are not supported");
    }
    let channel_id = field(v, "channel_id").filter(|id| {
        id.starts_with("UC")
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    });
    Ok(Source {
        video_url: video_url(&id),
        video_id: id,
        original_title: field(v, "title").unwrap_or_else(|| "Untitled video".into()),
        channel_url: channel_id
            .as_ref()
            .map(|s| format!("https://www.youtube.com/channel/{s}")),
        channel_id,
        channel_name: field(v, "channel"),
        description: field(v, "description").unwrap_or_default(),
        music_title: field(v, "track"),
        music_artist: field(v, "artist"),
        music_album: field(v, "album"),
    })
}
pub fn command(config: &Config) -> Result<Command> {
    let path = subprocess::executable(config.youtube.yt_dlp.as_deref(), "yt-dlp")?;
    let mut cmd = Command::new(path);
    cmd.args([
        "--ignore-config",
        "--no-cache-dir",
        "--no-plugin-dirs",
        "--no-colors",
        "--socket-timeout",
        "20",
        "--retries",
        "2",
    ]);
    if let Ok(deno) = subprocess::executable(config.youtube.deno.as_deref(), "deno") {
        cmd.arg("--js-runtimes")
            .arg(format!("deno:{}", deno.display()));
    } else if config.youtube.deno.is_some() {
        bail!("Configured Deno executable is unavailable");
    }
    if config.youtube.chrome_cookies {
        let browser = match &config.youtube.chrome_profile {
            Some(p) => format!("chrome:{p}"),
            None => "chrome".into(),
        };
        cmd.arg("--cookies-from-browser").arg(browser);
    }
    Ok(cmd)
}
pub fn extract(url: &str, config: &Config, stop: &Cancel) -> Result<Source> {
    let bytes = subprocess::run(
        command(config)?.args([
            "--no-playlist",
            "--skip-download",
            "--dump-single-json",
            "--",
            url,
        ]),
        None,
        stop,
        Duration::from_secs(90),
        |_| {},
    )?;
    source(&serde_json::from_slice(&bytes).context("Invalid metadata from yt-dlp")?)
}
pub fn preview(text: &str, all: bool, config: &Config, stop: &Cancel) -> Result<Preview> {
    let target = input(text, all)?;
    if !target.playlist {
        let s = extract(&target.url, config, stop)?;
        return Ok(Preview {
            url: target.url,
            title: s.original_title.clone(),
            playlist: false,
            items: vec![PreviewItem {
                video_id: s.video_id,
                title: s.original_title,
            }],
            existing: None,
        });
    }
    let bytes = subprocess::run(
        command(config)?.args([
            "--flat-playlist",
            "--skip-download",
            "--dump-single-json",
            "--playlist-end",
            "10001",
            "--",
            &target.url,
        ]),
        None,
        stop,
        Duration::from_secs(120),
        |_| {},
    )?;
    let v: Value = serde_json::from_slice(&bytes)?;
    let entries = v["entries"]
        .as_array()
        .context("Cannot read playlist entries")?;
    if entries.len() > 10_000 || v["playlist_count"].as_u64().is_some_and(|n| n > 10_000) {
        bail!("Playlists are limited to 10,000 entries");
    }
    let items = entries
        .iter()
        .map(|e| PreviewItem {
            video_id: field(e, "id").filter(|s| valid_id(s)).unwrap_or_default(),
            title: field(e, "title").unwrap_or_else(|| "Unavailable video".into()),
        })
        .collect();
    Ok(Preview {
        url: target.url,
        title: field(&v, "title").unwrap_or_else(|| "YouTube playlist".into()),
        playlist: true,
        items,
        existing: None,
    })
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub bytes: Option<u64>,
    pub total: Option<u64>,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
}
pub fn download(
    source: &Source,
    stage: &Path,
    config: &Config,
    stop: &Cancel,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    std::fs::create_dir_all(stage)?;
    let bin = stage.join("tools");
    std::fs::create_dir_all(&bin)?;
    for (name, path) in [
        ("ffmpeg", config.youtube.ffmpeg.as_deref()),
        ("ffprobe", config.youtube.ffprobe.as_deref()),
    ] {
        let executable = subprocess::executable(path, name)?;
        std::os::unix::fs::symlink(executable, bin.join(name))?;
    }
    let mut cmd = command(config)?;
    cmd.args([
        "--no-playlist",
        "-f",
        "bestaudio[ext=m4a]/bestaudio",
        "-x",
        "--audio-format",
        "m4a",
        "--embed-metadata",
        "--write-thumbnail",
        "--newline",
        "--progress",
        "--progress-delta",
        "0.25",
        "--progress-template",
        "download:VTAMP_PROGRESS %(progress)j",
        "--print",
        "after_move:VTAMP_FILE %(filepath)j",
    ])
    .arg("--ffmpeg-location")
    .arg(&bin)
    .arg("-o")
    .arg(stage.join("audio.%(ext)s"))
    .arg("--")
    .arg(&source.video_url);
    subprocess::run(
        &mut cmd,
        None,
        stop,
        Duration::from_secs(6 * 3600),
        |line| {
            if let Some(json) = line.strip_prefix("VTAMP_PROGRESS ")
                && let Ok(v) = serde_json::from_str::<Value>(json)
            {
                progress(DownloadProgress {
                    bytes: v["downloaded_bytes"].as_u64(),
                    total: v["total_bytes"]
                        .as_u64()
                        .or_else(|| v["total_bytes_estimate"].as_u64()),
                    speed: v["speed"].as_f64(),
                    eta: v["eta"].as_f64(),
                });
            }
        },
    )?;
    let audio = stage.join("audio.m4a");
    if !audio.is_file() {
        bail!("yt-dlp did not produce audio.m4a");
    }
    Ok(audio)
}
/// Download a bounded-resolution picture stream separately from the music file.
pub fn download_video(
    source: &Source,
    stage: &Path,
    config: &Config,
    stop: &Cancel,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    let raw = stage.join("picture.%(ext)s");
    subprocess::run(
        command(config)?
            .args([
                "--no-playlist",
                "-f",
                "bestvideo[height<=480]/best[height<=480]",
                "--newline",
                "--progress",
                "--progress-delta",
                "0.25",
                "--progress-template",
                "download:VTAMP_PROGRESS %(progress)j",
            ])
            .arg("-o")
            .arg(&raw)
            .arg("--")
            .arg(&source.video_url),
        None,
        stop,
        Duration::from_secs(6 * 3600),
        |line| {
            if let Some(json) = line.strip_prefix("VTAMP_PROGRESS ")
                && let Ok(v) = serde_json::from_str::<Value>(json)
            {
                progress(DownloadProgress {
                    bytes: v["downloaded_bytes"].as_u64(),
                    total: v["total_bytes"]
                        .as_u64()
                        .or_else(|| v["total_bytes_estimate"].as_u64()),
                    speed: v["speed"].as_f64(),
                    eta: v["eta"].as_f64(),
                });
            }
        },
    )?;
    let input = std::fs::read_dir(stage)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.is_file()
                && p.file_stem().is_some_and(|s| s == "picture")
                && p.extension().is_none_or(|s| s != "part")
        })
        .context("yt-dlp did not produce a video")?;
    let output = stage.join(crate::video::FILE);
    let ffmpeg = subprocess::executable(config.youtube.ffmpeg.as_deref(), "ffmpeg")?;
    subprocess::run(
        Command::new(ffmpeg)
            .args(["-nostdin", "-v", "error", "-y", "-i"])
            .arg(input)
            .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-c:v", "copy"])
            .arg(&output),
        None,
        stop,
        Duration::from_secs(600),
        |_| {},
    )?;
    crate::video::probe(&output, config, stop)?;
    Ok(output)
}

/// Fetch the thumbnail for an already imported video; `cover` converts it.
pub fn thumbnail(video_id: &str, stage: &Path, config: &Config, stop: &Cancel) -> Result<()> {
    std::fs::create_dir_all(stage)?;
    let mut cmd = command(config)?;
    cmd.args(["--no-playlist", "--skip-download", "--write-thumbnail"])
        .arg("-o")
        .arg(stage.join("thumb.%(ext)s"))
        .arg("--")
        .arg(video_url(video_id));
    subprocess::run(&mut cmd, None, stop, Duration::from_secs(120), |_| {})?;
    Ok(())
}

pub fn cover(stage: &Path, config: &Config, stop: &Cancel) -> Result<()> {
    let thumbnail = std::fs::read_dir(stage)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("jpg" | "jpeg" | "png" | "webp")
            )
        })
        .context("Video has no thumbnail")?;
    let converted = stage.join("thumbnail.png");
    let ffmpeg = subprocess::executable(config.youtube.ffmpeg.as_deref(), "ffmpeg")?;
    subprocess::run(
        Command::new(ffmpeg)
            .args(["-nostdin", "-v", "error", "-y", "-i"])
            .arg(&thumbnail)
            .args([
                "-vf",
                "scale=512:512:force_original_aspect_ratio=decrease",
                "-frames:v",
                "1",
            ])
            .arg(&converted),
        None,
        stop,
        Duration::from_secs(30),
        |_| {},
    )?;
    // Keep the thumbnail's own shape, like embedded album art: the client
    // decides how to fit it, so a wide image can still be drawn in full.
    bounded_cover(crate::library::decode_image(&std::fs::read(&converted)?)?)
        .save(stage.join("cover.jpg"))?;
    Ok(())
}

/// Bound a thumbnail to the same 512-pixel cache size as album art without
/// cropping or padding it; the client chooses the visible crop.
fn bounded_cover(image: image::DynamicImage) -> image::RgbImage {
    image.thumbnail(512, 512).to_rgb8()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls_choose_video_without_accidentally_importing_playlist() {
        assert!(
            !input(
                "https://www.youtube.com/watch?v=lO3lG-qXU14&list=PLtest",
                false
            )
            .unwrap()
            .playlist
        );
        assert!(
            input(
                "https://www.youtube.com/watch?v=lO3lG-qXU14&list=PLtest",
                true
            )
            .unwrap()
            .playlist
        );
        assert!(input("https://youtube.com.evil.test/watch?v=lO3lG-qXU14", false).is_err());
        assert!(input("file:///tmp/a", false).is_err());
        assert!(input("https://youtube.com/@channel", false).is_err());
        assert_eq!(
            input("https://youtu.be/lO3lG-qXU14?t=3", false)
                .unwrap()
                .url,
            video_url("lO3lG-qXU14")
        );
    }

    #[test]
    fn thumbnails_keep_their_aspect_ratio_and_stay_bounded() {
        // Imported covers must not be cropped or padded: the client decides
        // how to fit them, so a wide image can still be drawn in full.
        for (width, height, expected) in [
            (1280, 720, (512, 288)),
            (720, 1280, (288, 512)),
            (720, 720, (512, 512)),
        ] {
            let cover = bounded_cover(image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                width,
                height,
                image::Rgb([40, 120, 180]),
            )));
            assert_eq!(
                (cover.width(), cover.height()),
                expected,
                "{width}x{height} must keep its shape"
            );
            let pad = cover
                .pixels()
                .filter(|p| p.0.iter().all(|c| c.abs_diff(24) < 8))
                .count();
            assert_eq!(pad, 0, "{width}x{height} must not gain padding");
        }
    }
}

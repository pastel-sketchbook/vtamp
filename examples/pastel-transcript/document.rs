// Adapted from Pastel Sketchbook, vtamp commit d4d0be3 (MIT).
//! On-demand transcripts for YouTube imports.
//!
//! Each showcase video publishes `public/transcripts/<videoId>.md`: YAML front
//! matter, a title, a metadata blockquote, the spoken transcript under a
//! `## Transcript` heading, and a closing capture note. The TUI fetches them
//! over HTTPS on demand and caches them on disk; transcripts are never bundled,
//! and no request is made until the viewer opens one.
//!
//! [`parse`] turns that published document into plain prose split into
//! sentences. The viewer never renders markdown, and the playing position runs
//! over the prose alone, so the heading and the closing note never take up part
//! of the timeline.
//!
//! The published prose carries no timings, so without help the viewer can only
//! spread it evenly across the track, which drifts by seconds against a real
//! speaker. Captions published for the same video carry per-word times, and the
//! prose matches them almost word for word, so [`Document::attach_captions`]
//! turns them into exact sentence start times. Captions are fetched with yt-dlp
//! the first time a transcript is opened and cached beside it; when they are
//! missing the estimate stays in place.
use crate::subprocess::{self, Cancel};
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// Fraction of prose words that must find a caption word before the caption
/// timings are trusted. A transcript rewritten by hand stops matching here.
const ALIGN_MIN_MATCH: f64 = 0.95;
/// How far ahead of the current word the aligner looks for its caption.
const ALIGN_WINDOW: usize = 15;

/// Raw file host for the published transcripts.
pub const BASE_URL: &str = "https://raw.githubusercontent.com/pastel-sketchbook/pastelsketchbook/main/homepage/public/transcripts";
/// Largest transcript body accepted, in bytes.
pub const MAX_BYTES: usize = 256 * 1024;
/// Longest run kept as one sentence. Captions without punctuation are broken at
/// a word boundary so a highlight window never covers a whole paragraph.
const MAX_SENTENCE_CHARS: usize = 280;

fn validated(video_id: &str) -> Result<()> {
    if vtamp::youtube::valid_id(video_id) {
        Ok(())
    } else {
        bail!("Invalid video ID")
    }
}

pub fn cache_path(cache_dir: &Path, video_id: &str) -> Result<PathBuf> {
    validated(video_id)?;
    Ok(cache_dir.join(format!("{video_id}.md")))
}

/// Cache path for a video's caption timings.
pub fn captions_path(cache_dir: &Path, video_id: &str) -> Result<PathBuf> {
    validated(video_id)?;
    Ok(cache_dir.join(format!("{video_id}.vtt")))
}

/// Read cached caption timings, if present.
pub fn cached_captions(cache_dir: &Path, video_id: &str) -> Option<String> {
    let path = captions_path(cache_dir, video_id).ok()?;
    let bytes = vtamp::plugin::read_bounded(&path, MAX_BYTES).ok()?;
    (bytes.len() <= MAX_BYTES).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// Download a video's own caption track with yt-dlp and cache it. Returns
/// `Ok(None)` when the video has none that describes `prose`, so the viewer keeps
/// its estimate instead of failing.
pub async fn fetch_captions(
    video_id: &str,
    prose: &str,
    cache_dir: &Path,
    stop: Cancel,
) -> Result<Option<String>> {
    validated(video_id)?;
    if let Some(cached) = cached_captions(cache_dir, video_id) {
        return Ok(Some(cached));
    }
    let video_id = video_id.to_owned();
    let prose = prose.to_owned();
    let cache_dir = cache_dir.to_path_buf();
    let vtt = tokio::task::spawn_blocking(move || {
        download_captions(&video_id, &prose, &cache_dir, &stop)
    })
    .await
    .context("Cannot fetch caption timings")?
    .context("Cannot fetch caption timings")?;
    Ok(vtt)
}

/// The video's own spoken language, as yt-dlp reports it. The published
/// transcript is written in the same language, so asking keeps this off English
/// alone.
fn spoken_language(yt_dlp: &Path, video_id: &str, stop: &Cancel) -> Option<String> {
    let bytes = subprocess::run(
        Command::new(yt_dlp)
            .args([
                "--ignore-config",
                "--no-cache-dir",
                "--no-plugin-dirs",
                "--no-colors",
                "--no-warnings",
                "--skip-download",
                "--print",
                "%(language)s",
                "--",
            ])
            .arg(vtamp::youtube::video_url(video_id)),
        None,
        stop,
        Duration::from_secs(60),
        |_| {},
    )
    .ok()?;
    let language = String::from_utf8_lossy(&bytes).trim().to_string();
    // An empty language, or `NA` for a video yt-dlp cannot place.
    (language != "NA"
        && (2..=8).contains(&language.len())
        && language
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'-'))
    .then_some(language)
}

fn download_captions(
    video_id: &str,
    prose: &str,
    cache_dir: &Path,
    stop: &Cancel,
) -> Result<Option<String>> {
    let path = subprocess::executable(None, "yt-dlp").context("yt-dlp is not available")?;
    std::fs::create_dir_all(cache_dir).context("Cannot create transcript cache")?;
    // The video's own track is the one that speaks like the transcript, so ask
    // for that language alone. Asking for more is what gets a video rate limited,
    // and a language it does not carry fails the whole request.
    let wanted = match spoken_language(&path, video_id, stop) {
        Some(language) => format!("{language}-orig,{language}"),
        // A video yt-dlp cannot place: fall back to English.
        None => "en-orig".into(),
    };
    // yt-dlp names each caption track `<output>.<language>.vtt`.
    let work = tempfile::tempdir_in(cache_dir)?;
    let output = work.path().join(video_id);
    // A track yt-dlp cannot fetch fails the whole command, so its exit status is
    // not the answer: what matters is whether a usable file landed.
    let _ = subprocess::run(
        Command::new(path)
            .args([
                "--ignore-config",
                "--no-cache-dir",
                "--no-plugin-dirs",
                "--no-colors",
                "--no-warnings",
                "--skip-download",
                "--write-auto-subs",
                "--sub-langs",
                &wanted,
                "--sub-format",
                "vtt",
                "--output",
            ])
            .arg(&output)
            .arg("--")
            .arg(vtamp::youtube::video_url(video_id)),
        None,
        stop,
        Duration::from_secs(90),
        |_| {},
    );
    let mut candidates: Vec<(bool, u64, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(work.path()).context("Cannot read the transcript cache")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        if !name.starts_with(video_id) || !name.ends_with(".vtt") {
            continue;
        }
        let size = entry.metadata().map_or(0, |meta| meta.len());
        candidates.push((name.contains("-orig."), size, entry.path()));
    }
    // `en-orig` is the video's own caption track; prefer it, then the longer.
    candidates.sort_by_key(|(original, size, _)| (!original, std::cmp::Reverse(*size)));
    // Keep the first track that actually describes the transcript: yt-dlp also
    // offers translated tracks, which are timed but speak another language.
    let mut chosen = None;
    for (_, size, file) in candidates {
        if size > MAX_BYTES as u64 {
            continue;
        }
        let Ok(bytes) = vtamp::plugin::read_bounded(&file, MAX_BYTES) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if aligns(prose, &text) {
            chosen = Some(text);
            break;
        }
    }
    let Some(text) = chosen else {
        return Ok(None);
    };
    let path = captions_path(cache_dir, video_id)?;
    // Leave one stable cache file behind and drop the per-language files yt-dlp
    // wrote. Only caption files are touched: the transcript sits here too.
    for entry in std::fs::read_dir(work.path())? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.path() != path && name.starts_with(video_id) && name.ends_with(".vtt") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let _ = atomic_write(&path, text.as_bytes());
    Ok(Some(text))
}

/// Read a cached transcript, if present. The cache is bounded like the network
/// path, so a file left by an older or interrupted write cannot be trusted for
/// its size alone.
pub fn cached(cache_dir: &Path, video_id: &str) -> Result<Option<String>> {
    let path = cache_path(cache_dir, video_id)?;
    match vtamp::plugin::read_bounded(&path, MAX_BYTES) {
        Ok(bytes) if bytes.len() > MAX_BYTES => bail!("Cached transcript is too large"),
        Ok(bytes) => Ok(Some(
            String::from_utf8(bytes).context("Cached transcript is not UTF-8")?,
        )),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).context("Cannot read cached transcript"),
    }
}

/// Fetch a transcript, using the disk cache first. Writes through on a network
/// hit; a missing upstream file is reported, never cached.
pub async fn fetch(video_id: &str, cache_dir: &Path) -> Result<String> {
    if let Some(text) = cached(cache_dir, video_id)? {
        return Ok(text);
    }
    let url = format!("{BASE_URL}/{video_id}.md");
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("Cannot fetch transcript")?
        .get(&url)
        .send()
        .await
        .context("Cannot fetch transcript")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("No transcript published for this video");
    }
    let response = response
        .error_for_status()
        .context("Cannot fetch transcript")?;
    if response
        .content_length()
        .is_some_and(|len| len > MAX_BYTES as u64)
    {
        bail!("Transcript is too large");
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.context("Cannot fetch transcript")? {
        if bytes.len() + chunk.len() > MAX_BYTES {
            bail!("Transcript is too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() > MAX_BYTES {
        bail!("Transcript is too large");
    }
    let text = String::from_utf8(bytes).context("Transcript is not UTF-8")?;
    std::fs::create_dir_all(cache_dir).context("Cannot create transcript cache")?;
    // Best effort: a failed write still returns the fetched text.
    if let Ok(path) = cache_path(cache_dir, video_id) {
        let _ = atomic_write(&path, text.as_bytes());
    }
    Ok(text)
}

/// Byte range of one sentence inside a [`Document`]'s prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sentence {
    start: usize,
    end: usize,
}

/// A published transcript reduced to what the viewer shows: the spoken prose,
/// its sentences, the publisher's closing note, and — once caption timings have
/// been aligned — when each sentence is spoken.
pub struct Document {
    prose: String,
    /// Prose length in chars. The fallback estimate maps onto this, which keeps
    /// the highlighted sentence the same at every pane width.
    chars: usize,
    sentences: Vec<Sentence>,
    captured: Option<String>,
    /// Start of each sentence in ms, ascending, when captions aligned.
    timeline: Option<Vec<u64>>,
}

impl Document {
    /// The transcript as plain prose, with no markdown left in it.
    pub fn prose(&self) -> &str {
        &self.prose
    }
    /// The publisher's closing note, such as `Captured on May 9, 2026`.
    pub fn captured(&self) -> Option<&str> {
        self.captured.as_deref()
    }
    /// Prose length in chars, the unit the playback fraction maps onto.
    pub fn chars(&self) -> usize {
        self.chars
    }
    /// How many sentences the prose was split into.
    pub fn sentence_count(&self) -> usize {
        self.sentences.len()
    }
    /// True when no prose survived parsing.
    pub fn is_empty(&self) -> bool {
        self.sentences.is_empty()
    }
    /// True when caption timings replaced the even-spread estimate.
    pub fn is_timed(&self) -> bool {
        self.timeline.is_some()
    }
    /// Start of each sentence in ms, when the timings were aligned.
    pub fn starts_ms(&self) -> Option<&[u64]> {
        self.timeline.as_deref()
    }
    /// Attach caption timings, replacing the estimate. Returns whether they
    /// aligned: a caption track that does not match the published prose leaves
    /// the estimate in place.
    pub fn attach_captions(&mut self, captions: &str) -> bool {
        match timed_sentences(&self.prose, &self.sentences, captions) {
            Some(timeline) => {
                self.timeline = Some(timeline);
                true
            }
            None => false,
        }
    }
    /// The sentence being spoken at `position_ms`. Caption timings answer
    /// directly; without them the position is spread evenly over the prose,
    /// which needs `duration_ms` and drifts by seconds against a real speaker.
    #[cfg(test)]
    pub fn sentence_playing(&self, position_ms: u64, duration_ms: u64) -> Option<usize> {
        if self.sentences.is_empty() {
            return None;
        }
        if let Some(timeline) = &self.timeline {
            return Some(
                timeline
                    .partition_point(|start| *start <= position_ms)
                    .saturating_sub(1),
            );
        }
        if duration_ms == 0 {
            return None;
        }
        let fraction = (position_ms as f64 / duration_ms as f64).clamp(0.0, 1.0);
        self.sentence_at((fraction * self.chars as f64) as usize)
    }
    /// The sentence holding `offset`, counted in chars from the prose start.
    #[cfg(test)]
    pub fn sentence_at(&self, offset: usize) -> Option<usize> {
        if self.sentences.is_empty() {
            return None;
        }
        let at = self.byte_of(offset);
        Some(
            self.sentences
                .partition_point(|sentence| sentence.start <= at)
                .saturating_sub(1),
        )
    }
    /// The text of the sentence at `index`.
    pub fn sentence(&self, index: usize) -> Option<&str> {
        let sentence = *self.sentences.get(index)?;
        Some(&self.prose[sentence.start..sentence.end])
    }
    /// Byte offset of the `offset`th char, or the prose length past its end.
    #[cfg(test)]
    fn byte_of(&self, offset: usize) -> usize {
        match self.prose.char_indices().nth(offset) {
            Some((byte, _)) => byte,
            None => self.prose.len(),
        }
    }
}

/// Parse a published transcript into prose and sentences.
///
/// Front matter, the document title, the metadata blockquote and the horizontal
/// rules are dropped; `## Transcript` opens the body and the italic note after
/// the closing rule becomes the capture note. A document without that heading is
/// read as prose from its first text line.
pub fn parse(markdown: &str) -> Document {
    let mut lines: Vec<String> = Vec::new();
    let mut captured = None;
    let mut speaking = false;
    for line in front_matter_removed(markdown).lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if rule(line) {
            // A rule closes the body; anything after it is a closing note.
            speaking = false;
            continue;
        }
        if let Some(heading) = heading(line) {
            speaking = heading.eq_ignore_ascii_case("transcript");
            continue;
        }
        if line.starts_with('>') {
            // Category, view count and watch link.
            continue;
        }
        if !speaking {
            match note(line) {
                Some(text) => {
                    captured = Some(text);
                    continue;
                }
                // No heading in this document: the body starts at the first line.
                None => speaking = true,
            }
        }
        let text = plain(line);
        if !text.is_empty() {
            lines.push(text);
        }
    }
    let prose = lines.join(" ");
    let sentences = sentences(&prose);
    Document {
        chars: prose.chars().count(),
        sentences,
        captured,
        timeline: None,
        prose,
    }
}

/// Drop the YAML front matter so parsing starts at the title.
fn front_matter_removed(markdown: &str) -> &str {
    if !markdown.starts_with("---") {
        return markdown;
    }
    match markdown.splitn(3, "---").collect::<Vec<_>>()[..] {
        [_, _, body] => body
            .strip_prefix("\r\n")
            .or(body.strip_prefix('\n'))
            .unwrap_or(body),
        _ => markdown,
    }
}

/// The text of an ATX heading line, or `None` when the line is not one.
fn heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('#')?;
    if !rest.is_empty() && !rest.starts_with('#') && !rest.starts_with(' ') {
        // `#hashtag` is prose, not a heading.
        return None;
    }
    Some(rest.trim_matches('#').trim())
}

/// Whether the line is a horizontal rule such as `---` or `***`.
fn rule(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    matches!(first, '-' | '*' | '_')
        && line.chars().count() >= 3
        && line.chars().all(|c| c == first)
}

/// The text of a fully italic line, such as `*Captured on May 9, 2026.*`.
fn note(line: &str) -> Option<String> {
    let text = line.strip_prefix('*')?.strip_suffix('*')?;
    let text = text.trim().trim_end_matches('.');
    (!text.is_empty()).then(|| text.to_owned())
}

/// Drop inline markdown and collapse the runs of spaces captions carry between
/// words, so wrapping produces even rows.
fn plain(line: &str) -> String {
    let mut text = String::with_capacity(line.len());
    let mut space = false;
    for c in links_removed(line).chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        // Emphasis and code markers never appear inside spoken words.
        if matches!(c, '*' | '`') {
            continue;
        }
        if space && !text.is_empty() {
            text.push(' ');
        }
        space = false;
        text.push(c);
    }
    text
}

/// `[label](target)` becomes `label`. Brackets that open no link are kept.
fn links_removed(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let label = open + 1;
        let Some(close) = rest[label..].find(']') else {
            break;
        };
        let link = rest[label + close + 1..].strip_prefix('(');
        if let Some(end) = link.and_then(|target| target.find(')'))
            && let Some(target) = link
        {
            out.push_str(&rest[..open]);
            out.push_str(&rest[label..label + close]);
            rest = &target[end + 1..];
            continue;
        }
        // Keep the bracket itself and look for the next one.
        out.push_str(&rest[..label]);
        rest = &rest[label..];
    }
    out.push_str(rest);
    out
}

/// Sentence end punctuation, including the marks CJK and Arabic captions use.
fn terminator(c: char) -> bool {
    matches!(
        c,
        '.' | '!' | '?' | '…' | '।' | '۔' | '。' | '！' | '？' | '؟'
    )
}

/// Split prose into sentence byte ranges.
fn sentences(prose: &str) -> Vec<Sentence> {
    let chars: Vec<(usize, char)> = prose.char_indices().collect();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < chars.len() {
        if terminator(chars[index].1) {
            let mut end = index + 1;
            while end < chars.len() && (terminator(chars[end].1) || chars[end].1.is_whitespace()) {
                end += 1;
            }
            push_capped(&chars, start, end, &mut spans);
            start = end;
            index = end;
            continue;
        }
        index += 1;
    }
    push_capped(&chars, start, chars.len(), &mut spans);
    spans
        .into_iter()
        .map(|(start, end)| Sentence {
            start: chars[start].0,
            end: chars[end - 1].0 + chars[end - 1].1.len_utf8(),
        })
        .collect()
}

/// Record `start..end` as one sentence, breaking a long run at word boundaries.
fn push_capped(
    chars: &[(usize, char)],
    mut start: usize,
    mut end: usize,
    spans: &mut Vec<(usize, usize)>,
) {
    while start < end && chars[start].1.is_whitespace() {
        start += 1;
    }
    while end > start && chars[end - 1].1.is_whitespace() {
        end -= 1;
    }
    if end <= start {
        return;
    }
    while end - start > MAX_SENTENCE_CHARS {
        let limit = start + MAX_SENTENCE_CHARS;
        // A word longer than the cap is cut at the cap, so this always advances.
        let cut = (start + 1..limit)
            .rev()
            .find(|at| chars[*at].1.is_whitespace())
            .unwrap_or(limit);
        spans.push((start, cut));
        start = cut;
        while start < end && chars[start].1.is_whitespace() {
            start += 1;
        }
    }
    spans.push((start, end));
}

/// A caption word and when it starts, in ms.
type TimedWord = (String, u64);

/// Parse a WebVTT caption track into timed words.
///
/// YouTube's automatic captions repeat each line as it grows, so a line only
/// contributes the words from where it stops repeating what came before. Words
/// carry their own inline timestamps where the track provides them and fall back
/// to the cue start where it does not.
pub fn caption_words(captions: &str) -> Vec<TimedWord> {
    let mut out: Vec<TimedWord> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut cue = 0;
    let mut previous = 0;
    for line in captions.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("WEBVTT")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
            || line.starts_with("NOTE")
        {
            continue;
        }
        match cue_start(line) {
            Some((start, _, text)) => {
                cue = start;
                caption_line(text, cue, &mut out, &mut seen, &mut previous);
            }
            // A cue's second and later lines belong to the cue still open.
            None => caption_line(line, cue, &mut out, &mut seen, &mut previous),
        }
    }
    out
}

/// The cue's start and end in ms and its text, or `None` for any other line. The
/// end time and the settings (`align:start position:0%`) follow the arrow and are
/// not spoken, so the text starts after them.
fn cue_start(line: &str) -> Option<(u64, u64, &str)> {
    let (start, rest) = line.split_once("-->")?;
    let rest = rest.trim_start();
    let (end, text) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let (start, end) = (vtt_time(start.trim())?, vtt_time(end.trim())?);
    let mut text = text.trim_start();
    // Drop the cue's settings, which are not spoken.
    while let Some(end_of_token) = text.find(char::is_whitespace).or(Some(text.len())) {
        let token = &text[..end_of_token];
        let setting = token
            .split_once(':')
            .is_some_and(|(key, _)| !key.is_empty() && !token.contains(' '));
        if !token.is_empty() && setting {
            text = text[end_of_token..].trim_start();
            continue;
        }
        break;
    }
    Some((start, end, text))
}

/// `HH:MM:SS.mmm`, the form YouTube writes, in ms.
fn vtt_time(stamp: &str) -> Option<u64> {
    let mut parts = stamp.split(':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some(hours * 3_600_000 + minutes * 60_000 + (seconds * 1000.0).round() as u64)
}

/// Emit the words of one caption line.
///
/// A caption line repeats the words of the line before it, as the rolling preview
/// of a longer line or as the flash cue that precedes one, so the repeated prefix
/// is dropped. A speaker who genuinely repeats themselves costs this one
/// sentence its exact time; keeping every echo instead would shift the time of
/// every sentence after it.
fn caption_line(
    line: &str,
    cue: u64,
    out: &mut Vec<TimedWord>,
    seen: &mut Vec<String>,
    previous: &mut usize,
) {
    let inline = inline_times(line);
    let words: Vec<String> = strip_tags(line)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if words.is_empty() {
        return;
    }
    // The longest run of words this line repeats from the end of what came
    // before, never more than the previous line held.
    let mut repeated = 0;
    for count in (1..=words.len().min(seen.len()).min(*previous)).rev() {
        if seen[seen.len() - count..] == words[..count] {
            repeated = count;
            break;
        }
    }
    for (index, word) in words.iter().enumerate().skip(repeated) {
        let at = inline.get(index).copied().flatten().unwrap_or(cue);
        out.push((word.clone(), at));
    }
    *previous = words.len();
    seen.extend(words);
}

/// Times for each word of one caption line, `None` where the track gives none.
///
/// A caption line interleaves plain words with `<hh:mm:ss.mmm><c>word</c>` marks,
/// each mark introducing the word its tag introduces.
fn inline_times(line: &str) -> Vec<Option<u64>> {
    let mut times: Vec<Option<u64>> = Vec::new();
    let mut pending: Option<u64> = None;
    for chunk in line.split('<') {
        let (tag, text) = match chunk.split_once('>') {
            Some((tag, text)) => (Some(tag), text),
            // No markup left: the rest of the line is plain text.
            None => (None, chunk),
        };
        if let Some(at) = tag.and_then(vtt_time) {
            pending = Some(at);
            continue;
        }
        // A closing tag introduces nothing.
        if matches!(tag, Some(tag) if tag.starts_with('/')) {
            continue;
        }
        for index in 0..text.split_whitespace().count() {
            // Only the first word of a run is introduced by the pending mark.
            let at = if index == 0 {
                pending.take().or_else(|| times.last().copied().flatten())
            } else {
                None
            };
            times.push(at);
        }
    }
    times
}

/// Caption text without its timing and positioning markup.
fn strip_tags(line: &str) -> String {
    let mut text = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            rest = &rest[open..];
            break;
        };
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);
    text.replace("&nbsp;", " ").replace("&amp;", "&")
}

/// Two words match when they differ only in case and punctuation.
fn same_word(left: &str, right: &str) -> bool {
    let strip = |word: &str| {
        word.chars()
            .filter(|c| c.is_alphanumeric() || *c == '\'')
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let (left, right) = (strip(left), strip(right));
    !left.is_empty() && left == right
}

/// Whether a caption track describes the published prose closely enough to time
/// it. Translations of a caption track speak another language, so only the video's
/// own track passes.
pub fn aligns(prose: &str, captions: &str) -> bool {
    timed_sentences(prose, &sentences(prose), captions).is_some()
}

/// Caption timings for each sentence, or `None` when the captions do not match
/// the published prose closely enough to trust.
fn timed_sentences(prose: &str, sentences: &[Sentence], captions: &str) -> Option<Vec<u64>> {
    let captions = caption_words(captions);
    if captions.is_empty() {
        return None;
    }
    let words: Vec<&str> = prose.split_whitespace().collect();
    // Walk both texts together, keeping the caption cursor ahead of the prose.
    let mut times: Vec<Option<u64>> = Vec::new();
    let mut cursor = 0;
    let mut matched = 0;
    for word in &words {
        let hit = captions
            .iter()
            .enumerate()
            .skip(cursor)
            .take(ALIGN_WINDOW)
            .find(|(_, (caption, _))| same_word(word, caption))
            .map(|(ahead, _)| ahead);
        match hit {
            Some(ahead) => {
                times.push(Some(captions[ahead].1));
                matched += 1;
                cursor = ahead + 1;
            }
            None => times.push(None),
        }
    }
    if words.is_empty() || (matched as f64) / (words.len() as f64) < ALIGN_MIN_MATCH {
        return None;
    }
    let mut starts = Vec::with_capacity(sentences.len());
    for sentence in sentences {
        // The word at the sentence's start, falling back to the first timed
        // word from there on.
        let first = prose[..sentence.start]
            .split_whitespace()
            .count()
            .min(times.len());
        let at = times[first..]
            .iter()
            .find_map(|time| *time)
            .unwrap_or_else(|| *starts.last().unwrap_or(&0));
        starts.push(at);
    }
    // A caption track that runs backwards is not a timing for this prose.
    if starts.windows(2).any(|pair| pair[0] > pair[1]) {
        return None;
    }
    Some(starts)
}

/// A transcript in the shape the showcase publishes: front matter, title,
/// metadata blockquote, one long spoken paragraph, closing note. Shared with the
/// overlay tests, which parse the same document.
#[cfg(test)]
pub(crate) const SAMPLE: &str = "---\ntype: source\nvideoId: abc123\ncategory: development\ntitle: \"Song\"\ncaptured: 2026-05-09T22:15:31.647Z\n---\n\n# Song\n\n> Category: [development](../../videos/development.md) · 2 views · May 9, 2026\n> [Watch on YouTube](https://youtu.be/abc123)\n\n## Transcript\n\nWelcome  to  today's  presentation.  I  am  pleased  to  introduce  Kandle.  It  targets  serverless  inference  without  the  overhead  of  a  traditional  framework.\n\n---\n*Captured on May 9, 2026.*\n";

/// The spoken prose inside [`SAMPLE`], for tests that need it on its own.
#[cfg(test)]
pub(crate) const SAMPLE_PROSE: &str = "Welcome to today's presentation. I am pleased to introduce Kandle. It targets serverless inference without the overhead of a traditional framework.";

/// A caption track in YouTube's automatic format for [`SAMPLE`]'s prose: inline
/// word times, rolling lines, and a cue's own settings.
#[cfg(test)]
pub(crate) const SAMPLE_CAPTIONS: &str = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.320 --> 00:00:02.560 align:start position:0%\n \nWelcome<00:00:00.719><c> to</c><00:00:00.960><c> today's</c><00:00:01.360><c> presentation.</c><00:00:02.320><c> I</c>\n\n00:00:02.560 --> 00:00:04.480 align:start position:0%\nWelcome to today's presentation. I am\npleased<00:00:03.040><c> to</c><00:00:03.280><c> introduce</c><00:00:03.760><c> Kandle.</c>\n\n00:00:05.000 --> 00:00:09.000 align:start position:0%\nIt<00:00:05.100><c> targets</c><00:00:05.600><c> serverless</c><00:00:06.400><c> inference</c><00:00:06.900><c> without</c><00:00:07.400><c> the</c><00:00:07.700><c> overhead</c><00:00:08.200><c> of</c><00:00:08.400><c> a</c><00:00:08.600><c> traditional</c><00:00:09.000><c> framework.</c>\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_names_are_pinned_to_valid_video_ids() {
        let dir = tempfile::tempdir().unwrap();
        assert!(cache_path(dir.path(), "CIbmold6mAE").is_ok());
        for bad in ["", "abc", "../etc/passwd", "abc/def_ghij", "CBmold6mAE!"] {
            assert!(cache_path(dir.path(), bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn fetch_reads_the_cache_without_network() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CIbmold6mAE.md"), SAMPLE).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let fetched = runtime.block_on(fetch("CIbmold6mAE", dir.path())).unwrap();
        assert_eq!(fetched, SAMPLE);
        let document = parse(&fetched);
        assert!(
            document
                .prose()
                .starts_with("Welcome to today's presentation. I am pleased to introduce Kandle."),
            "{}",
            document.prose()
        );
    }

    #[test]
    fn missing_cache_reports_without_network() {
        let dir = tempfile::tempdir().unwrap();
        assert!(cached(dir.path(), "CIbmold6mAE").unwrap().is_none());
    }

    #[test]
    fn an_oversized_cache_entry_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CIbmold6mAE.md");
        std::fs::write(&path, vec![b'x'; MAX_BYTES + 1]).unwrap();
        assert!(cached(dir.path(), "CIbmold6mAE").is_err());
    }

    #[test]
    fn parse_keeps_only_the_spoken_prose() {
        let document = parse(SAMPLE);
        let prose = document.prose();
        assert_eq!(
            prose,
            "Welcome to today's presentation. I am pleased to introduce Kandle. It targets serverless inference without the overhead of a traditional framework."
        );
        // No front matter, heading, quote, rule, link or emphasis survives.
        for marker in ["#", ">", "---", "](", "*", "Song", "Category"] {
            assert!(!prose.contains(marker), "{marker} left in {prose}");
        }
        assert_eq!(document.captured(), Some("Captured on May 9, 2026"));
        assert_eq!(document.chars(), prose.chars().count());
    }

    #[test]
    fn parse_reads_a_document_without_the_transcript_heading() {
        let document = parse("A  bare  caption.\nAnd more.");
        assert_eq!(document.prose(), "A bare caption. And more.");
        assert_eq!(document.captured(), None);
        assert_eq!(document.sentence_count(), 2);
    }

    #[test]
    fn parse_reports_a_document_with_no_prose() {
        for empty in ["", "---\ntype: source\n---\n", "# Title\n\n> Category: x\n"] {
            assert!(parse(empty).is_empty(), "{empty}");
        }
    }

    #[test]
    fn sentences_cover_the_prose_in_order_without_overlap() {
        let document = parse(SAMPLE);
        let prose = document.prose();
        let mut previous = 0;
        for index in 0..document.sentence_count() {
            let text = document.sentence(index).unwrap();
            assert!(!text.trim().is_empty(), "{index} is blank");
            let start = prose
                .find(text)
                .unwrap_or_else(|| panic!("{index} is out of order"));
            assert!(start >= previous, "{index} overlaps the previous sentence");
            previous = start + text.len();
        }
        assert_eq!(
            document.sentence(0),
            Some("Welcome to today's presentation.")
        );
        assert_eq!(
            document.sentence(document.sentence_count() - 1),
            Some(
                "It targets serverless inference without the overhead of a traditional framework."
            )
        );
        assert_eq!(document.sentence(document.sentence_count()), None);
    }

    #[test]
    fn an_unpunctuated_run_is_broken_into_word_bounded_sentences() {
        let filler = "word ".repeat(400);
        let document = parse(&filler);
        assert!(document.sentence_count() > 1);
        for index in 0..document.sentence_count() {
            let text = document.sentence(index).unwrap();
            assert!(!text.trim().is_empty(), "{index}");
            // Each break lands on a word boundary, never mid-word.
            assert!(text.starts_with("word"), "{text}");
        }
    }

    #[test]
    fn a_single_overlong_word_still_advances_the_split() {
        let document = parse(&"x".repeat(MAX_SENTENCE_CHARS * 2 + 5));
        assert_eq!(document.sentence_count(), 3);
        assert_eq!(document.sentence(0).map(str::len), Some(MAX_SENTENCE_CHARS));
    }

    #[test]
    fn sentence_at_maps_the_playback_fraction_onto_prose() {
        let document = parse(SAMPLE);
        let count = document.sentence_count();
        assert_eq!(document.sentence_at(0), Some(0));
        assert_eq!(document.sentence_at(document.chars()), Some(count - 1));
        assert_eq!(document.sentence_at(document.chars() * 10), Some(count - 1));
        // The estimate follows characters, so the opening sentence leads and the
        // half-way point lands past it, whatever the sentences weigh.
        let middle = document.sentence_at(document.chars() / 2).unwrap();
        assert!(middle > 0, "{middle} of {count}");
        assert_eq!(document.sentence_at(1), Some(0));
        assert_eq!(parse("").sentence_at(0), None);
    }

    #[test]
    fn sentence_at_survives_a_wide_document() {
        // Korean punctuation and Korean text must not split mid-character.
        let document = parse("안녕하세요. 반갑습니다! 잘 있나요? 괜찮습니다。");
        let prose = document.prose();
        for index in 0..document.sentence_count() {
            assert!(prose.contains(document.sentence(index).unwrap()));
        }
        assert_eq!(document.sentence_count(), 4);
    }

    #[test]
    fn caption_words_keep_inline_times_and_drop_rolling_repeats() {
        let words = caption_words(SAMPLE_CAPTIONS);
        assert_eq!(
            words
                .iter()
                .map(|(word, _)| word.as_str())
                .collect::<Vec<_>>(),
            [
                "Welcome",
                "to",
                "today's",
                "presentation.",
                "I",
                "am",
                "pleased",
                "to",
                "introduce",
                "Kandle.",
                "It",
                "targets",
                "serverless",
                "inference",
                "without",
                "the",
                "overhead",
                "of",
                "a",
                "traditional",
                "framework."
            ]
        );
        // The first word of a cue takes the cue start, later words their own tag.
        assert_eq!(words[0].1, 320);
        assert_eq!(words[1].1, 719);
        assert_eq!(words[4].1, 2320);
        // A cue's second line continues the same cue, and a word with no tag of
        // its own falls back to the cue start.
        assert_eq!(words[6].1, 2560);
        assert_eq!(words[10].1, 5000);
        assert_eq!(words[11].1, 5100);
    }

    #[test]
    fn caption_timings_replace_the_estimate() {
        let mut document = parse(SAMPLE);
        assert!(!document.is_timed());
        // Half way through a 20 second track the even spread still sits in the
        // first sentence, which is the drift the timings remove.
        assert_eq!(document.sentence_playing(2320, 20_000), Some(0));
        assert!(document.attach_captions(SAMPLE_CAPTIONS));
        assert!(document.is_timed());
        assert_eq!(document.starts_ms(), Some([320, 2320, 5000].as_slice()));
        assert_eq!(document.sentence_playing(0, 20_000), Some(0));
        assert_eq!(document.sentence_playing(2319, 20_000), Some(0));
        assert_eq!(document.sentence_playing(2320, 20_000), Some(1));
        assert_eq!(document.sentence_playing(4999, 20_000), Some(1));
        assert_eq!(document.sentence_playing(5000, 20_000), Some(2));
        assert_eq!(document.sentence_playing(u64::MAX, 20_000), Some(2));
        // Timed sentences no longer depend on the track's duration.
        assert_eq!(document.sentence_playing(2320, 0), Some(1));
    }

    #[test]
    fn captions_that_do_not_match_the_prose_are_ignored() {
        let mut document = parse(SAMPLE);
        assert!(
            !document.attach_captions(
                "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nsomething else entirely\n"
            )
        );
        assert!(!document.is_timed());
        // The estimate is untouched by captions that never applied.
        assert_eq!(document.sentence_playing(2320, 20_000), Some(0));
        assert!(!document.attach_captions(""));
        assert!(!parse("").attach_captions(SAMPLE_CAPTIONS));
    }

    #[test]
    fn a_caption_track_that_runs_backwards_is_rejected() {
        // Word timings that go backwards cannot describe this prose.
        let backwards = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\n\n            It<00:00:01.500><c> targets</c><00:00:01.400><c> serverless</c><00:00:01.300><c> inference</c><00:00:01.200><c> without</c><00:00:01.100><c> the</c><00:00:01.000><c> overhead</c><00:00:00.900><c> of</c><00:00:00.800><c> a</c><00:00:00.700><c> traditional</c><00:00:00.600><c> framework.</c>\n\n            Welcome<00:00:05.000><c> to</c><00:00:05.100><c> today's</c><00:00:05.200><c> presentation.</c><00:00:05.300><c> I</c><00:00:05.400><c> am</c><00:00:05.500><c> pleased</c><00:00:05.600><c> to</c><00:00:05.700><c> introduce</c><00:00:05.800><c> Kandle.</c>\n";
        let mut document = parse(SAMPLE);
        assert!(!document.attach_captions(backwards));
        assert!(!document.is_timed());
    }

    #[test]
    fn a_translated_caption_track_does_not_time_the_prose() {
        // yt-dlp offers translated caption tracks for any video. Their timings
        // are real but their words are not the transcript's, so they are refused
        // and the estimate is kept.
        let spanish = "WEBVTT\n\n00:00:00.500 --> 00:00:04.000\n\
            Bienvenidos a la presentacion de Mi cuaderno de viaje Yesu.\n\n\
            00:00:04.000 --> 00:00:09.000\n\
            Esta obra ha sido concebida como una guia integral para explorar.\n";
        assert!(!aligns(SAMPLE_PROSE, spanish));
        // The video's own track still times it.
        assert!(aligns(SAMPLE_PROSE, SAMPLE_CAPTIONS));
        assert!(!aligns("", SAMPLE_CAPTIONS));
        assert!(!aligns(SAMPLE_PROSE, ""));
    }

    #[test]
    fn cached_captions_are_read_only_within_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        assert!(cached_captions(dir.path(), "CIbmold6mAE").is_none());
        let path = captions_path(dir.path(), "CIbmold6mAE").unwrap();
        std::fs::write(&path, SAMPLE_CAPTIONS).unwrap();
        assert_eq!(
            cached_captions(dir.path(), "CIbmold6mAE").as_deref(),
            Some(SAMPLE_CAPTIONS)
        );
        std::fs::write(&path, vec![b'x'; MAX_BYTES + 1]).unwrap();
        assert!(cached_captions(dir.path(), "CIbmold6mAE").is_none());
        // An invalid video ID never reaches the file system.
        assert!(captions_path(dir.path(), "../etc/passwd").is_err());
    }

    #[test]
    fn caption_words_ignore_cue_settings_and_notes() {
        let words = caption_words(
            "WEBVTT\nKind: captions\nNOTE this is a note\n\n\
             00:00:01.500 --> 00:00:03.000 align:start position:0%\n\
             <v Roger Bingham>We<00:00:01.800><c> speak</c><00:00:02.400><c> now.</c></v>\n",
        );
        assert_eq!(
            words
                .iter()
                .map(|(word, _)| word.as_str())
                .collect::<Vec<_>>(),
            ["We", "speak", "now."]
        );
        assert_eq!(words[0].1, 1500);
        assert_eq!(words[1].1, 1800);
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("Missing cache parent")?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

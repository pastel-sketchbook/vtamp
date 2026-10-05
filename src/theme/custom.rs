//! Client-local, data-only palettes. Loading a catalog never creates files.
use super::{Palette, Theme, channels};
use crate::platform;
use anyhow::{Context, Result, bail};
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ThemeId(String);

impl ThemeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ThemeId {
    type Error = anyhow::Error;
    fn try_from(id: String) -> Result<Self> {
        if id.is_empty()
            || !id.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
        {
            bail!("Theme IDs must use lowercase letters, digits, and single separating hyphens");
        }
        Ok(Self(id))
    }
}
impl From<ThemeId> for String {
    fn from(id: ThemeId) -> Self {
        id.0
    }
}
impl From<Theme> for ThemeId {
    fn from(theme: Theme) -> Self {
        Self(theme.id().into())
    }
}
impl Default for ThemeId {
    fn default() -> Self {
        Theme::default().into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTheme {
    pub id: ThemeId,
    name: String,
    mode: Mode,
    palette: Palette,
}
impl ResolvedTheme {
    pub fn id(&self) -> &str {
        self.id.as_str()
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn mode(&self) -> &str {
        match self.mode {
            Mode::Dark => "dark",
            Mode::Light => "light",
        }
    }
    pub fn palette(&self) -> Palette {
        self.palette
    }
}
impl From<Theme> for ResolvedTheme {
    fn from(theme: Theme) -> Self {
        Self {
            id: theme.into(),
            name: theme.name().into(),
            mode: if theme.mode() == "light" {
                Mode::Light
            } else {
                Mode::Dark
            },
            palette: theme.palette(),
        }
    }
}
impl Default for ResolvedTheme {
    fn default() -> Self {
        Theme::default().into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Dark,
    Light,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    version: u32,
    name: String,
    mode: Mode,
    colors: Colors,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Colors {
    bg: String,
    panel: String,
    selection: String,
    text: String,
    muted: String,
    accent: String,
    border: String,
    warning: String,
    error: String,
    spectrum: [String; 3],
}

fn color(value: &str, role: &str) -> Result<Color> {
    let hex = value
        .strip_prefix('#')
        .filter(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .with_context(|| format!("colors.{role} must be #RRGGBB, got {value:?}"))?;
    Ok(super::rgb(u32::from_str_radix(hex, 16)?))
}

fn file_id(path: &Path) -> Result<ThemeId> {
    if path.extension().is_none_or(|ext| ext != "json") {
        bail!("Theme files must end in .json");
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .context("Theme filename is not UTF-8")?
        .to_owned()
        .try_into()
}

fn read(path: &Path) -> Result<Vec<u8>> {
    if !fs::metadata(path)?.is_file() {
        bail!("Theme is not a regular file");
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(65_537).read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        bail!("Theme file exceeds 64 KiB");
    }
    Ok(bytes)
}

fn parse(id: ThemeId, bytes: &[u8]) -> Result<ResolvedTheme> {
    let d: Definition = serde_json::from_slice(bytes)?;
    if d.version != 1 {
        bail!("Unsupported theme version {}; expected 1", d.version);
    }
    if d.name.trim().is_empty() || d.name.chars().any(char::is_control) {
        bail!("Theme name must be nonempty and contain no control characters");
    }
    let c = d.colors;
    Ok(ResolvedTheme {
        id,
        name: d.name.trim().into(),
        mode: d.mode,
        palette: Palette {
            bg: color(&c.bg, "bg")?,
            panel: color(&c.panel, "panel")?,
            selection: color(&c.selection, "selection")?,
            text: color(&c.text, "text")?,
            muted: color(&c.muted, "muted")?,
            accent: color(&c.accent, "accent")?,
            border: color(&c.border, "border")?,
            warning: color(&c.warning, "warning")?,
            error: color(&c.error, "error")?,
            spectrum: [
                color(&c.spectrum[0], "spectrum[0]")?,
                color(&c.spectrum[1], "spectrum[1]")?,
                color(&c.spectrum[2], "spectrum[2]")?,
            ],
        },
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeWarning {
    pub path: PathBuf,
    pub message: String,
}

pub struct ThemeCatalog {
    pub themes: Vec<ResolvedTheme>,
    pub warnings: Vec<ThemeWarning>,
    invalid: BTreeMap<String, String>,
}
impl Default for ThemeCatalog {
    fn default() -> Self {
        Self {
            themes: Theme::ALL.into_iter().map(Into::into).collect(),
            warnings: vec![],
            invalid: BTreeMap::new(),
        }
    }
}
impl ThemeCatalog {
    pub fn load(directory: &Path) -> Self {
        let mut catalog = Self::default();
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return catalog,
            Err(e) => {
                catalog.warn(directory, format!("Cannot read themes directory: {e}"));
                return catalog;
            }
        };
        let mut paths = vec![];
        for entry in entries {
            match entry {
                Ok(entry) if entry.path().extension().is_some_and(|s| s == "json") => {
                    paths.push(entry.path())
                }
                Ok(_) => {}
                Err(e) => catalog.warn(directory, format!("Cannot read directory entry: {e}")),
            }
        }
        // Sort IDs, not filenames: `name` precedes `name-light` despite `.json`.
        paths.sort_by(|a, b| a.file_stem().cmp(&b.file_stem()));
        for path in paths {
            let result = (|| -> Result<ResolvedTheme> {
                let id = file_id(&path)?;
                if Theme::ALL.iter().any(|t| t.id() == id.as_str()) {
                    bail!("Built-in theme IDs are reserved");
                }
                parse(id, &read(&path)?)
            })();
            match result {
                Ok(theme) => {
                    catalog.warnings.extend(palette_warnings(&path, &theme));
                    catalog.themes.push(theme);
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    if let Ok(id) = file_id(&path) {
                        catalog
                            .invalid
                            .insert(id.0, format!("{}: {message}", path.display()));
                    }
                    catalog.warn(&path, message);
                }
            }
        }
        catalog
    }
    fn warn(&mut self, path: &Path, message: String) {
        self.warnings.push(ThemeWarning {
            path: path.into(),
            message,
        });
    }
    pub fn knows(&self, id: &str) -> bool {
        self.themes.iter().any(|t| t.id() == id) || self.invalid.contains_key(id)
    }
    pub fn resolve(&self, id: &str) -> Result<ResolvedTheme> {
        if let Some(theme) = self.themes.iter().find(|t| t.id() == id) {
            return Ok(theme.clone());
        }
        if let Some(error) = self.invalid.get(id) {
            bail!("Invalid theme {id}: {error}");
        }
        bail!("Unknown theme {id:?}; use vtamp theme list to see available themes")
    }
    pub fn warning_text(&self) -> Option<String> {
        (!self.warnings.is_empty()).then(|| {
            self.warnings
                .iter()
                .map(|w| format!("{}: {}", w.path.display(), w.message))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
}

fn luminance(color: Color) -> f64 {
    let c = channels(color).map(|v| {
        let v = f64::from(v) / 255.;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    });
    c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722
}
fn contrast(a: Color, b: Color) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
fn palette_warnings(path: &Path, theme: &ResolvedTheme) -> Vec<ThemeWarning> {
    let p = theme.palette;
    let mut pairs = vec![];
    for (name, fg) in [("text", p.text), ("muted", p.muted)] {
        for (background, bg) in [("bg", p.bg), ("panel", p.panel), ("selection", p.selection)] {
            if contrast(fg, bg) < 4.5 {
                pairs.push(format!("{name}/{background} ({:.2}:1)", contrast(fg, bg)));
            }
        }
    }
    for (name, fg) in [
        ("accent", p.accent),
        ("warning", p.warning),
        ("error", p.error),
    ] {
        for (background, bg) in [("bg", p.bg), ("panel", p.panel)] {
            if contrast(fg, bg) < 4.5 {
                pairs.push(format!("{name}/{background} ({:.2}:1)", contrast(fg, bg)));
            }
        }
    }
    let mut warnings = vec![];
    if !pairs.is_empty() {
        warnings.push(ThemeWarning {
            path: path.into(),
            message: format!("Text contrast below 4.5:1: {}", pairs.join(", ")),
        });
    }
    if super::is_light(&p) != (theme.mode == Mode::Light) {
        warnings.push(ThemeWarning {
            path: path.into(),
            message: "Theme mode disagrees with the canvas/text brightness".into(),
        });
    }
    warnings
}

#[derive(Serialize)]
pub struct InstallReport {
    pub installed: Vec<String>,
    pub unchanged: Vec<String>,
    pub warnings: Vec<ThemeWarning>,
    pub path: PathBuf,
}

pub fn install(directory: &Path, files: &[PathBuf], replace: bool) -> Result<InstallReport> {
    let mut pending = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut report = InstallReport {
        installed: vec![],
        unchanged: vec![],
        warnings: vec![],
        path: directory.into(),
    };
    // Validate the entire batch and collisions before publishing anything.
    for path in files {
        let checked = (|| -> Result<_> {
            let id = file_id(path)?;
            if !seen.insert(id.as_str().to_owned()) {
                bail!("Duplicate theme ID {} in installation batch", id.as_str());
            }
            if Theme::ALL.iter().any(|t| t.id() == id.as_str()) {
                bail!("Built-in theme IDs are reserved");
            }
            let bytes = read(path)?;
            let theme = parse(id.clone(), &bytes)?;
            report.warnings.extend(palette_warnings(path, &theme));
            let destination = directory.join(format!("{}.json", id.as_str()));
            match fs::symlink_metadata(&destination) {
                Ok(meta) => {
                    if !meta.file_type().is_file() {
                        bail!(
                            "Destination is not a regular file: {}",
                            destination.display()
                        );
                    }
                    if read(&destination).is_ok_and(|existing| existing == bytes) {
                        report.unchanged.push(id.0);
                        return Ok(None);
                    }
                    if !replace {
                        bail!(
                            "{} already exists; use --replace to replace it",
                            destination.display()
                        );
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            Ok(Some((id.0, (destination, bytes))))
        })()
        .with_context(|| format!("Cannot install {}", path.display()))?;
        if let Some((id, data)) = checked {
            pending.insert(id, data);
        }
    }
    if pending.is_empty() {
        return Ok(report);
    }
    platform::private_dir(directory)?;
    for (id, (destination, bytes)) in pending {
        let temporary = directory.join(format!(".theme-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            if replace {
                fs::rename(&temporary, &destination)?;
            } else {
                fs::hard_link(&temporary, &destination)?;
                fs::remove_file(&temporary)?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&temporary);
            return Err(e).with_context(|| {
                format!(
                    "Cannot install {id}; already installed: {}",
                    report.installed.join(", ")
                )
            });
        }
        report.installed.push(id);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const PASTEL: &str = include_str!("../../themes/pastel/pastel-default.json");

    #[test]
    fn custom_theme_schema_rejects_mistakes_without_hiding_valid_neighbors() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("valid.json"), PASTEL).unwrap();
        let template: Value = serde_json::from_str(PASTEL).unwrap();
        let mut cases = vec![];
        for (field, value) in [
            ("version", json!(2)),
            ("mode", json!("auto")),
            ("name", json!("\u{1b}[31m")),
            ("typo", json!(true)),
        ] {
            let mut d = template.clone();
            d[field] = value;
            cases.push(d);
        }
        for value in [
            json!("#fff"),
            json!("red"),
            json!("#GG0000"),
            json!(16777215),
        ] {
            let mut d = template.clone();
            d["colors"]["bg"] = value;
            cases.push(d);
        }
        let mut missing = template.clone();
        missing["colors"].as_object_mut().unwrap().remove("text");
        cases.push(missing);
        let mut stops = template;
        stops["colors"]["spectrum"] = json!(["#000000", "#ffffff"]);
        cases.push(stops);
        for (i, value) in cases.iter().enumerate() {
            fs::write(
                dir.path().join(format!("invalid-{i}.json")),
                value.to_string(),
            )
            .unwrap();
        }
        fs::write(dir.path().join("nord.json"), PASTEL).unwrap();
        fs::write(dir.path().join("invalid_name.json"), PASTEL).unwrap();
        fs::write(dir.path().join("broken.json"), "{").unwrap();
        fs::write(dir.path().join("huge.json"), vec![b' '; 65_537]).unwrap();
        let catalog = ThemeCatalog::load(dir.path());
        assert_eq!(catalog.themes.len(), 10);
        assert_eq!(catalog.warnings.len(), cases.len() + 4);
        assert_eq!(
            catalog.resolve("nord").unwrap().palette(),
            Theme::Nord.palette()
        );
        assert!(
            catalog
                .resolve("invalid-0")
                .unwrap_err()
                .to_string()
                .contains("Unsupported theme version")
        );
        assert_eq!(catalog.resolve("valid").unwrap().name(), "Pastel Default");
        assert!(catalog.knows("broken"));
        assert!(!catalog.knows("absent"));
    }

    #[test]
    fn pastel_examples_have_exact_roles_and_readable_dark_and_light_palettes() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("themes/pastel");
        let catalog = ThemeCatalog::load(&directory);
        assert_eq!(catalog.themes.len(), 25);
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        let custom = &catalog.themes[9..];
        assert!(custom.windows(2).all(|pair| pair[0].id() < pair[1].id()));
        assert_eq!(custom.iter().filter(|t| t.mode() == "light").count(), 8);
        for theme in custom {
            let source: Value = serde_json::from_slice(
                &fs::read(directory.join(format!("{}.json", theme.id()))).unwrap(),
            )
            .unwrap();
            let p = theme.palette();
            for (role, actual) in [
                ("bg", p.bg),
                ("panel", p.panel),
                ("selection", p.selection),
                ("text", p.text),
                ("muted", p.muted),
                ("accent", p.accent),
                ("border", p.border),
                ("warning", p.warning),
                ("error", p.error),
            ] {
                let [r, g, b] = channels(actual);
                assert_eq!(format!("#{r:02x}{g:02x}{b:02x}"), source["colors"][role]);
            }
            for (i, actual) in p.spectrum.into_iter().enumerate() {
                let [r, g, b] = channels(actual);
                assert_eq!(
                    format!("#{r:02x}{g:02x}{b:02x}"),
                    source["colors"]["spectrum"][i]
                );
            }
        }
        let p = catalog.resolve("pastel-default").unwrap().palette();
        assert_eq!(p.bg, Color::Rgb(0x12, 0x12, 0x18));
        assert_eq!(p.accent, Color::Rgb(0, 0xd9, 0xff));
        assert_eq!(p.spectrum, [Color::Rgb(0, 0xc8, 0x50), p.accent, p.error]);
    }

    #[test]
    fn low_contrast_is_a_warning_and_unreadable_directories_keep_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let mut definition: Value = serde_json::from_str(PASTEL).unwrap();
        definition["colors"]["text"] = definition["colors"]["bg"].clone();
        fs::write(dir.path().join("faint.json"), definition.to_string()).unwrap();
        let catalog = ThemeCatalog::load(dir.path());
        assert!(catalog.resolve("faint").is_ok());
        assert!(catalog.warning_text().unwrap().contains("4.5:1"));
        let missing = ThemeCatalog::load(&dir.path().join("missing"));
        assert_eq!(missing.themes.len(), 9);
        assert!(missing.warnings.is_empty());
        assert!(!dir.path().join("missing").exists());
        let not_directory = ThemeCatalog::load(&dir.path().join("faint.json"));
        assert_eq!(not_directory.themes.len(), 9);
        assert_eq!(not_directory.warnings.len(), 1);
    }

    #[test]
    fn theme_install_prevalidates_batches_preserves_existing_files_and_rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("installed");
        let valid = dir.path().join("custom.json");
        let broken = dir.path().join("broken.json");
        fs::write(&valid, PASTEL).unwrap();
        fs::write(&broken, "broken").unwrap();
        assert!(install(&target, &[valid.clone(), broken], false).is_err());
        assert!(!target.exists());
        assert_eq!(
            install(&target, std::slice::from_ref(&valid), false)
                .unwrap()
                .installed,
            ["custom"]
        );
        assert_eq!(
            install(&target, std::slice::from_ref(&valid), false)
                .unwrap()
                .unchanged,
            ["custom"]
        );
        let changed = PASTEL.replace("Pastel Default", "My custom theme");
        fs::write(&valid, &changed).unwrap();
        assert!(install(&target, std::slice::from_ref(&valid), false).is_err());
        assert_eq!(
            fs::read_to_string(target.join("custom.json")).unwrap(),
            PASTEL
        );
        install(&target, std::slice::from_ref(&valid), true).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("custom.json")).unwrap(),
            changed
        );
        assert!(install(&target, &[valid.clone(), valid], true).is_err());
        let builtin = dir.path().join("nord.json");
        fs::write(&builtin, PASTEL).unwrap();
        assert!(install(&target, &[builtin], true).is_err());
        let linked = dir.path().join("linked.json");
        fs::write(&linked, PASTEL).unwrap();
        std::os::unix::fs::symlink(&linked, target.join("linked.json")).unwrap();
        assert!(install(&target, &[linked], true).is_err());
        assert!(
            fs::read_dir(&target).unwrap().all(|f| !f
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.'))
        );
    }
}

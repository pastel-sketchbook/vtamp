//! Server/import settings, separate from client UI preferences. Secrets are references.
use crate::{
    platform::{self, Paths},
    subprocess,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub youtube: YoutubeConfig,
    pub llm: LlmConfig,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YoutubeConfig {
    pub yt_dlp: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    pub deno: Option<PathBuf>,
    pub chrome_cookies: bool,
    pub chrome_profile: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, clap::ValueEnum, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Code,
    Api,
    Codex,
    Claude,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    pub provider: Provider,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub executable: Option<PathBuf>,
    pub key_env: Option<String>,
    pub keychain: bool,
}
impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        match fs::read(paths.data.join("imports.json")) {
            Ok(b) => {
                serde_json::from_slice(&b).context("Invalid imports.json; fix it before importing")
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, paths: &Paths) -> Result<()> {
        platform::private_dir(&paths.data)?;
        atomic_json(&paths.data.join("imports.json"), self)
    }
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_file_name(format!(".vtamp-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer_pretty(&mut f, value)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
pub fn executable(explicit: Option<&Path>, name: &str) -> Result<PathBuf> {
    let valid = |p: &Path| {
        p.is_file()
            && p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    };
    if let Some(p) = explicit {
        if p.is_absolute() && valid(p) {
            return Ok(p.to_owned());
        }
        bail!("{name} path must be an absolute executable file");
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path).filter(|p| p.is_absolute()) {
            let p = dir.join(name);
            if valid(&p) {
                return Ok(p);
            }
        }
    }
    bail!(
        "{name} is not installed or not on the server PATH. YouTube import is unavailable; local music still works."
    )
}
pub fn capabilities(config: &Config) -> Value {
    capabilities_with_cancel(config, &subprocess::cancel())
}
pub fn capabilities_with_cancel(config: &Config, stop: &subprocess::Cancel) -> Value {
    let tools = [
        ("yt-dlp", config.youtube.yt_dlp.as_deref(), "--version"),
        ("ffmpeg", config.youtube.ffmpeg.as_deref(), "-version"),
        ("ffprobe", config.youtube.ffprobe.as_deref(), "-version"),
        ("deno", config.youtube.deno.as_deref(), "--version"),
        ("codex", None, "--version"),
        ("claude", None, "--version"),
    ];
    let mut result = serde_json::Map::new();
    for (name, path, arg) in tools {
        let probe = (|| -> Result<Value> {
            let path = executable(path, name)?;
            let out = subprocess::run(
                Command::new(&path).arg(arg),
                None,
                stop,
                Duration::from_secs(5),
                |_| {},
            )?;
            let version = String::from_utf8_lossy(&out)
                .lines()
                .next()
                .unwrap_or("")
                .to_owned();
            Ok(json!({"available":true,"path":path,"version":version}))
        })();
        result.insert(
            name.into(),
            probe.unwrap_or_else(|e| json!({"available":false,"error":e.to_string()})),
        );
    }
    json!({"tools":result,"youtube":config.youtube,"llm":config.llm,"authentication_tested":false})
}
fn account(paths: &Paths) -> String {
    format!("{}", paths.data.display())
}
pub fn save_key(paths: &Paths, key: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        security_framework::passwords::set_generic_password(
            "vtamp.metadata",
            &account(paths),
            key.as_bytes(),
        )
        .context("Cannot save API key to Keychain")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, key);
        bail!("Keychain storage is supported on macOS only")
    }
}
pub fn api_key(config: &LlmConfig, paths: &Paths) -> Result<Option<String>> {
    if let Some(name) = &config.key_env {
        return Ok(Some(std::env::var(name).with_context(|| {
            format!("API key environment variable {name} is missing in this process")
        })?));
    }
    if config.keychain {
        #[cfg(target_os = "macos")]
        {
            return Ok(Some(String::from_utf8(
                security_framework::passwords::get_generic_password(
                    "vtamp.metadata",
                    &account(paths),
                )
                .context("Cannot read API key from Keychain")?,
            )?));
        }
        #[cfg(not(target_os = "macos"))]
        bail!("Keychain storage is supported on macOS only");
    }
    Ok(None)
}
pub fn prompt(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut s = String::new();
    if io::stdin().read_line(&mut s)? == 0 {
        bail!("Setup cancelled: input closed");
    }
    Ok(if s.trim().is_empty() {
        default.into()
    } else {
        s.trim().into()
    })
}
pub fn setup(paths: &Paths, llm: bool) -> Result<Value> {
    use std::io::IsTerminal;
    if !io::stdin().is_terminal() {
        bail!("Setup requires a terminal; edit imports.json for automation");
    }
    let mut config = Config::load(paths)?;
    if llm {
        let p = prompt("Metadata provider (code/api/codex/claude)", "code")?;
        config.llm = LlmConfig {
            provider: match p.as_str() {
                "code" => Provider::Code,
                "api" => Provider::Api,
                "codex" => Provider::Codex,
                "claude" => Provider::Claude,
                _ => bail!("Unknown provider"),
            },
            ..Default::default()
        };
        if config.llm.provider != Provider::Code {
            if config.llm.provider == Provider::Api {
                config.llm.endpoint = Some(prompt(
                    "Chat Completions base URL",
                    "http://localhost:1234/v1",
                )?);
            } else {
                let name = if config.llm.provider == Provider::Codex {
                    "codex"
                } else {
                    "claude"
                };
                let found = executable(None, name).ok();
                let path = prompt(
                    "CLI executable",
                    &found.map(|p| p.display().to_string()).unwrap_or_default(),
                )?;
                config.llm.executable = Some(executable(Some(Path::new(&path)), name)?);
            }
            let model = prompt("Model ID (blank uses CLI default)", "")?;
            if !model.is_empty() {
                config.llm.model = Some(model);
            }
            if config.llm.provider == Provider::Api && config.llm.model.is_none() {
                bail!("An API model ID is required");
            }
            let effort = prompt("Reasoning effort (default omits the option)", "default")?;
            if effort != "default" {
                config.llm.effort = Some(effort);
            }
            if config.llm.provider == Provider::Api {
                match prompt("API key storage (keychain/env/none)", "keychain")?.as_str() {
                    "keychain" => {
                        let key = rpassword::prompt_password("API key: ")?;
                        save_key(paths, &key)?;
                        config.llm.keychain = true;
                    }
                    "env" => {
                        config.llm.key_env =
                            Some(prompt("Environment variable", "VTAMP_LLM_API_KEY")?);
                    }
                    "none" => (),
                    _ => bail!("Unknown key storage"),
                }
            }
        }
    } else {
        println!("YouTube import uses installed tools; vtamp never installs them.");
        for (name, target) in [
            ("yt-dlp", &mut config.youtube.yt_dlp),
            ("ffmpeg", &mut config.youtube.ffmpeg),
            ("ffprobe", &mut config.youtube.ffprobe),
            ("deno", &mut config.youtube.deno),
        ] {
            let default = target
                .clone()
                .or_else(|| executable(None, name).ok())
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let path = prompt(
                &format!("{name} executable (blank leaves PATH lookup)"),
                &default,
            )?;
            *target = if path.is_empty() {
                None
            } else {
                Some(executable(Some(Path::new(&path)), name)?)
            };
        }
        config.youtube.chrome_cookies = prompt("Use Chrome cookies? (yes/no)", "no")? == "yes";
        if config.youtube.chrome_cookies {
            let profile = prompt("Chrome profile (blank uses most recently used)", "")?;
            config.youtube.chrome_profile = (!profile.is_empty()).then_some(profile);
        } else {
            config.youtube.chrome_profile = None;
        }
    }
    config.save(paths)?;
    if llm
        && config.llm.provider != Provider::Code
        && prompt("Test with the example song? (yes/no)", "yes")? == "yes"
    {
        return crate::metadata::test(&config, paths);
    }
    Ok(
        json!({"saved":true,"path":paths.data.join("imports.json"),"applies_to":"new_import_jobs","config":config}),
    )
}

pub fn youtube_available(paths: &Paths) -> bool {
    Config::load(paths)
        .ok()
        .is_some_and(|c| executable(c.youtube.yt_dlp.as_deref(), "yt-dlp").is_ok())
}

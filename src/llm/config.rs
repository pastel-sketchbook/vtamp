use crate::{
    cli::prompt,
    platform::{self, Paths},
    subprocess,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use subprocess::executable;

#[derive(Debug, Clone, Default, Serialize, Deserialize, clap::ValueEnum, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    None,
    Api,
    Codex,
    Claude,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
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
        match fs::read(paths.data.join("llm.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .context("Invalid llm.json; fix it before using the LLM"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, paths: &Paths) -> Result<()> {
        platform::private_dir(&paths.data)?;
        platform::atomic_json(&paths.data.join("llm.json"), self)
    }
}
fn account(paths: &Paths) -> String {
    format!("{}", paths.data.display())
}
pub fn save_key(paths: &Paths, key: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        security_framework::passwords::set_generic_password(
            "vtamp.llm",
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
pub fn api_key(config: &Config, paths: &Paths) -> Result<Option<String>> {
    if let Some(name) = &config.key_env {
        return Ok(Some(std::env::var(name).with_context(|| {
            format!("API key environment variable {name} is missing in this process")
        })?));
    }
    if config.keychain {
        #[cfg(target_os = "macos")]
        {
            return Ok(Some(String::from_utf8(
                security_framework::passwords::get_generic_password("vtamp.llm", &account(paths))
                    .context("Cannot read API key from Keychain")?,
            )?));
        }
        #[cfg(not(target_os = "macos"))]
        bail!("Keychain storage is supported on macOS only");
    }
    Ok(None)
}
pub fn setup(paths: &Paths) -> Result<Value> {
    use std::io::IsTerminal;
    if !io::stdin().is_terminal() {
        bail!("Setup requires a terminal; edit llm.json for automation");
    }
    println!("Connect an LLM provider. Select none to disable LLM requests.");
    let p = prompt("LLM provider (api/codex/claude/none)", "api")?;
    let mut config = Config {
        provider: match p.as_str() {
            "none" => Provider::None,
            "api" => Provider::Api,
            "codex" => Provider::Codex,
            "claude" => Provider::Claude,
            _ => bail!("Unknown provider"),
        },
        ..Default::default()
    };
    if config.provider != Provider::None {
        if config.provider == Provider::Api {
            config.endpoint = Some(prompt(
                "Chat Completions base URL",
                "http://localhost:1234/v1",
            )?);
        } else {
            let name = if config.provider == Provider::Codex {
                "codex"
            } else {
                "claude"
            };
            let found = executable(None, name).ok();
            let path = prompt(
                "CLI executable",
                &found.map(|p| p.display().to_string()).unwrap_or_default(),
            )?;
            config.executable = Some(executable(Some(Path::new(&path)), name)?);
        }
        let model = prompt(
            if config.provider == Provider::Api {
                "Model ID"
            } else {
                "Model ID (blank uses CLI default)"
            },
            "",
        )?;
        if !model.is_empty() {
            config.model = Some(model);
        }
        if config.provider == Provider::Api && config.model.is_none() {
            bail!("An API model ID is required");
        }
        let effort = prompt("Reasoning effort (default omits the option)", "default")?;
        if effort != "default" {
            config.effort = Some(effort);
        }
        if config.provider == Provider::Api {
            match prompt("API key storage (keychain/env/none)", "keychain")?.as_str() {
                "keychain" => {
                    let key = rpassword::prompt_password("API key: ")?;
                    save_key(paths, &key)?;
                    config.keychain = true;
                }
                "env" => {
                    config.key_env = Some(prompt("Environment variable", "VTAMP_LLM_API_KEY")?);
                }
                "none" => (),
                _ => bail!("Unknown key storage"),
            }
        }
    }
    config.save(paths)?;
    let mut saved = json!({"saved":true,"path":paths.data.join("llm.json"),"applies_to":"new_requests","config":config});
    if config.provider != Provider::None {
        println!("Connection test sends only a request to reply with {{\"ok\":true}}.");
        if prompt("Test connection? (yes/no)", "yes")? == "yes" {
            saved["test"] = super::test(&config, paths)?;
        }
    }
    Ok(saved)
}

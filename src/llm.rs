//! Shared JSON requests for the configured LLM provider.
mod config;
use crate::{
    platform::Paths,
    subprocess::{self, Cancel},
};
use anyhow::{Context, Result, bail};
pub use config::{Config, Provider, setup};
use serde_json::{Value, json};
use std::{process::Command, time::Duration};

pub(crate) fn request(
    config: &Config,
    paths: &Paths,
    instruction: &str,
    data: &str,
    schema: &Value,
    stop: &Cancel,
) -> Result<Value> {
    let input = format!("Schema: {schema}\nInput: {data}");
    let text = match config.provider {
        Provider::None => bail!("LLM is disabled"),
        Provider::Api => {
            let endpoint = config
                .endpoint
                .as_deref()
                .context("API endpoint is missing")?;
            let mut url = url::Url::parse(endpoint)?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                bail!("Invalid API endpoint");
            }
            let path = format!("{}/chat/completions", url.path().trim_end_matches('/'));
            url.set_path(&path);
            let mut body = json!({"model":config.model.as_deref().context("Model ID is missing")?,"messages":[{"role":"system","content":instruction},{"role":"user","content":input}],"response_format":{"type":"json_schema","json_schema":{"name":"response","strict":true,"schema":schema}}});
            if let Some(effort) = &config.effort {
                body["reasoning_effort"] = json!(effort);
            }
            let key = config::api_key(config, paths)?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let bytes = runtime.block_on(async {
                let request = async {
                    let http = reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none()).build()?;
                    for attempt in 0..2 {
                        let mut request = http.post(url.clone()).json(&body);
                        if let Some(key) = &key { request = request.bearer_auth(key); }
                        let mut response = request.send().await?;
                        let status = response.status();
                        let mut bytes = Vec::new();
                        while let Some(chunk) = response.chunk().await? {
                            if bytes.len() + chunk.len() > 1_048_576 { bail!("LLM API response exceeds 1 MiB"); }
                            bytes.extend_from_slice(&chunk);
                        }
                        if status == reqwest::StatusCode::BAD_REQUEST && attempt == 0 {
                            let error = String::from_utf8_lossy(&bytes);
                            if error.contains("response_format") || error.contains("json_schema") {
                                body.as_object_mut().unwrap().remove("response_format");
                                continue;
                            }
                        }
                        if !status.is_success() { bail!("LLM API returned HTTP {status}; check model and reasoning effort"); }
                        return Ok(bytes);
                    }
                    unreachable!()
                };
                let cancellation = async {
                    loop {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) { break; }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                };
                tokio::select! {
                    result = request => result,
                    _ = cancellation => Err(anyhow::anyhow!("LLM request cancelled")),
                    _ = tokio::time::sleep(Duration::from_secs(60)) => Err(anyhow::anyhow!("LLM API timed out")),
                }
            })?;
            let v: Value = serde_json::from_slice(&bytes)?;
            v["choices"][0]["message"]["content"]
                .as_str()
                .context("API response contains no text")?
                .to_owned()
        }
        Provider::Codex | Provider::Claude => {
            let name = if config.provider == Provider::Codex {
                "codex"
            } else {
                "claude"
            };
            let exe = subprocess::executable(config.executable.as_deref(), name)?;
            let help = subprocess::run(
                Command::new(&exe).args(if name == "codex" {
                    vec!["exec", "--help"]
                } else {
                    vec!["--help"]
                }),
                None,
                stop,
                Duration::from_secs(5),
                |_| {},
            )?;
            let help = String::from_utf8_lossy(&help);
            let required = if name == "codex" {
                vec!["--output-schema", "--ephemeral", "--ignore-user-config"]
            } else {
                vec!["--safe-mode", "--json-schema", "--tools", "--system-prompt"]
            };
            if required.iter().any(|s| !help.contains(s)) {
                bail!(
                    "Installed {name} lacks required isolation/JSON options; update it or choose another provider"
                );
            }
            let dir = tempfile::Builder::new().prefix("vtamp-llm-").tempdir()?;
            let mut cmd = Command::new(exe);
            cmd.current_dir(dir.path());
            if name == "codex" {
                let schema_path = dir.path().join("schema.json");
                std::fs::write(&schema_path, serde_json::to_vec(&schema)?)?;
                cmd.args([
                    "exec",
                    "--ephemeral",
                    "--ignore-user-config",
                    "--skip-git-repo-check",
                    "--sandbox",
                    "read-only",
                    "--output-schema",
                ])
                .arg(schema_path)
                .args([
                    "-c",
                    "approval_policy=\"never\"",
                    "-c",
                    "features.shell_tool=false",
                    "-c",
                    "features.hooks=false",
                    "-c",
                    "features.apps=false",
                    "-c",
                    "agents.enabled=false",
                    "-c",
                    "web_search=\"disabled\"",
                    "-c",
                    "project_doc_max_bytes=0",
                ]);
                if let Some(e) = &config.effort {
                    cmd.arg("-c").arg(format!(
                        "model_reasoning_effort={}",
                        serde_json::to_string(e)?
                    ));
                }
                cmd.arg("-");
            } else {
                cmd.args([
                    "--safe-mode",
                    "-p",
                    "--tools",
                    "",
                    "--disallowedTools",
                    "mcp__*",
                    "--strict-mcp-config",
                    "--disable-slash-commands",
                    "--no-session-persistence",
                    "--output-format",
                    "json",
                    "--json-schema",
                ])
                .arg(serde_json::to_string(&schema)?)
                .arg("--system-prompt")
                .arg(instruction);
                if let Some(e) = &config.effort {
                    cmd.arg("--effort").arg(e);
                }
            }
            if let Some(model) = &config.model {
                cmd.arg("--model").arg(model);
            }
            let prompt = if name == "claude" {
                input
            } else {
                format!("{instruction}\n{input}")
            };
            let bytes = subprocess::run(
                &mut cmd,
                Some(prompt.into_bytes()),
                stop,
                Duration::from_secs(60),
                |_| {},
            )?;
            let text = String::from_utf8(bytes)?;
            if name == "claude" {
                let v: Value = serde_json::from_str(&text)?;
                if v["is_error"] == true {
                    bail!("Claude failed; check its authentication and model");
                }
                serde_json::to_string(
                    v.get("structured_output")
                        .context("Claude returned no structured output")?,
                )?
            } else {
                text
            }
        }
    };
    serde_json::from_str(text.trim()).context("LLM returned invalid JSON")
}

pub fn test(config: &Config, paths: &Paths) -> Result<Value> {
    if config.provider == Provider::None {
        return Ok(json!({"provider":config.provider,"tested":false,"enabled":false}));
    }
    let schema = json!({"type":"object","additionalProperties":false,"properties":{"ok":{"type":"boolean"}},"required":["ok"]});
    let response = request(
        config,
        paths,
        "This is a connection test. Return only the JSON object {\"ok\":true}. Do not use tools, search, read files or run commands.",
        "Reply with {\"ok\":true}.",
        &schema,
        &subprocess::cancel(),
    )?;
    if response != json!({"ok":true}) {
        bail!("LLM connection test returned an unexpected response");
    }
    Ok(json!({"provider":config.provider,"tested":true,"response":response}))
}

pub fn status(config: &Config, paths: &Paths) -> Result<Value> {
    let mut value =
        json!({"config":config,"path":paths.data.join("llm.json"),"authentication_tested":false});
    let name = match config.provider {
        Provider::Codex => Some("codex"),
        Provider::Claude => Some("claude"),
        _ => None,
    };
    if let Some(name) = name {
        let probe = (|| -> Result<Value> {
            let path = subprocess::executable(config.executable.as_deref(), name)?;
            let bytes = subprocess::run(
                Command::new(&path).arg("--version"),
                None,
                &subprocess::cancel(),
                Duration::from_secs(5),
                |_| {},
            )?;
            let version = String::from_utf8_lossy(&bytes)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            Ok(json!({"available":true,"path":path,"version":version}))
        })();
        value["tool"] = probe.unwrap_or_else(|e| json!({"available":false,"error":e.to_string()}));
    }
    Ok(value)
}

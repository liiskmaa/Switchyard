//! The models the installed Gemini CLI offers: read from `session/new`'s
//! `models.availableModels` (verified with 0.70.0) through a short ACP
//! handshake in a scratch directory, with Switchyard's policy and isolated
//! home. Nothing is prompted, so no model request is made.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::launch::{AUTH_METHOD, LaunchSpec, build_args, build_env, prepare};
use crate::model::AgentProvider;
use crate::models::{MAX_MODEL_ID, ModelCatalog, ModelOption};

const TIMEOUT: Duration = Duration::from_secs(45);

fn catalog(models: Vec<ModelOption>, error: Option<String>, version: &str) -> ModelCatalog {
    ModelCatalog {
        provider: AgentProvider::Gemini,
        models,
        accepts_full_names: false,
        source: format!("Gemini CLI {version} (session/new)"),
        error,
    }
}

/// Parses `session/new`'s result into model options (the current one is the default).
pub(crate) fn parse_models(result: &Value) -> Vec<ModelOption> {
    let current = result["models"]["currentModelId"].as_str();
    result["models"]["availableModels"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let id = m["modelId"].as_str()?;
                    (id.len() <= MAX_MODEL_ID && !id.is_empty()).then(|| ModelOption {
                        id: id.to_owned(),
                        display_name: m["name"].as_str().unwrap_or(id).to_owned(),
                        is_default: Some(id) == current,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Lists the models; on any failure the catalogue carries the error.
pub async fn list_models(executable: &Path, version: &str, state_dir: &Path) -> ModelCatalog {
    match tokio::time::timeout(TIMEOUT, query(executable, state_dir)).await {
        Ok(Ok(models)) if !models.is_empty() => catalog(models, None, version),
        Ok(Ok(_)) => catalog(Vec::new(), Some("Gemini reported no models".into()), version),
        Ok(Err(e)) => catalog(Vec::new(), Some(e), version),
        Err(_) => catalog(Vec::new(), Some("Gemini did not answer in time".into()), version),
    }
}

async fn query(executable: &Path, state_dir: &Path) -> Result<Vec<ModelOption>, String> {
    let cwd = state_dir.join("probe");
    std::fs::create_dir_all(&cwd).map_err(|e| e.to_string())?;
    let spec = LaunchSpec {
        cwd: cwd.clone(),
        model: None,
        resume_session: None,
        state_dir: state_dir.to_path_buf(),
    };
    prepare(&spec).map_err(|e| e.to_string())?;
    let mut child = Command::new(executable)
        .args(build_args(&spec))
        .env_clear()
        .envs(build_env(std::env::vars_os(), executable, &spec))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "authenticate", "params": {"methodId": AUTH_METHOD}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "session/new", "params": {"cwd": cwd.display().to_string(), "mcpServers": []}}),
    ];
    for (i, req) in requests.iter().enumerate() {
        let mut bytes = serde_json::to_vec(req).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        stdin.write_all(&bytes).await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;
        let want = i as u64 + 1;
        loop {
            let line = lines.next_line().await.map_err(|e| e.to_string())?.ok_or("Gemini exited")?;
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg["id"].as_u64() != Some(want) || msg.get("method").is_some() {
                continue;
            }
            if let Some(e) = msg.get("error") {
                return Err(e["message"].as_str().unwrap_or("error").to_owned());
            }
            if want == 3 {
                drop(stdin);
                let _ = child.kill().await;
                return Ok(parse_models(&msg["result"]));
            }
            break;
        }
    }
    Err("no session".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_come_from_the_session() {
        let result = json!({"models": {"currentModelId": "auto", "availableModels": [
            {"modelId": "auto", "name": "Auto"},
            {"modelId": "gemini-3.5-flash-lite", "name": "gemini-3.5-flash-lite"},
            {"name": "no id"}
        ]}});
        let m = parse_models(&result);
        assert_eq!(
            m.iter().map(|o| (o.id.as_str(), o.is_default)).collect::<Vec<_>>(),
            [("auto", true), ("gemini-3.5-flash-lite", false)]
        );
    }
}

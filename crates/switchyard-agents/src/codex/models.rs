//! The models the installed Codex offers, from its app-server's `model/list`
//! (verified against codex-cli 0.166.0: `{"data": [{"id", "displayName",
//! "isDefault", "hidden", …}], "nextCursor"}`).
//!
//! Only `initialize`, `initialized` and `model/list` are sent: no thread is
//! opened and no turn runs, so nothing is sent to a model.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::launch::{build_args, build_env};
use crate::model::AgentProvider;
use crate::models::{ModelCatalog, ModelOption};
use crate::process::{ProcessSpec, spawn};

const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// Pages read at most (each page is one request).
const MAX_PAGES: usize = 10;

/// Asks the installed Codex which models it offers (the default picker list).
pub async fn list_models(executable: &Path, version: &str) -> ModelCatalog {
    let source = format!("`model/list` of codex app-server ({version})");
    match tokio::time::timeout(LIST_TIMEOUT, query(executable)).await {
        Ok(Ok(models)) => ModelCatalog {
            provider: AgentProvider::Codex,
            models,
            accepts_full_names: false,
            source,
            error: None,
        },
        Ok(Err(e)) => unavailable(source, e),
        Err(_) => unavailable(source, "codex app-server did not answer model/list in time".into()),
    }
}

fn unavailable(source: String, error: String) -> ModelCatalog {
    ModelCatalog {
        provider: AgentProvider::Codex,
        models: Vec::new(),
        accepts_full_names: false,
        source,
        error: Some(error),
    }
}

async fn query(executable: &Path) -> Result<Vec<ModelOption>, String> {
    let spec = ProcessSpec {
        program: executable.to_path_buf(),
        args: build_args(),
        cwd: std::env::temp_dir(),
        env: build_env(std::env::vars_os(), executable),
    };
    let mut child = spawn(&spec).map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("codex app-server has no stdin")?;
    let stdout = child.stdout.take().ok_or("codex app-server has no stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut send = async |v: Value| -> Result<(), String> {
        let mut line = v.to_string();
        line.push('\n');
        stdin.write_all(line.as_bytes()).await.map_err(|e| e.to_string())
    };
    send(json!({ "id": 1, "method": "initialize", "params": {
        "clientInfo": { "name": "switchyard", "title": "Switchyard", "version": env!("CARGO_PKG_VERSION") } } }))
    .await?;
    response(&mut lines, 1).await?;
    send(json!({ "method": "initialized", "params": {} })).await?;
    let mut models = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_PAGES {
        let id = 2 + page as u64;
        send(json!({ "id": id, "method": "model/list", "params": { "includeHidden": false, "cursor": cursor } }))
            .await?;
        let result = response(&mut lines, id).await?;
        models.extend(parse_models(&result)?);
        cursor = result.get("nextCursor").and_then(Value::as_str).map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    drop(send);
    let _ = child.start_kill();
    Ok(models)
}

async fn response<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    id: u64,
) -> Result<Value, String> {
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(e) = v.get("error") {
            let msg = e.get("message").and_then(Value::as_str).unwrap_or("error");
            return Err(format!("codex app-server refused request {id}: {msg}"));
        }
        return v.get("result").cloned().ok_or_else(|| format!("codex app-server sent no result for {id}"));
    }
    Err("codex app-server closed its output".into())
}

/// The visible models of one `model/list` page.
pub(crate) fn parse_models(result: &Value) -> Result<Vec<ModelOption>, String> {
    let data = result.get("data").and_then(Value::as_array).ok_or("model/list returned no data")?;
    Ok(data
        .iter()
        .filter(|m| !m.get("hidden").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|m| {
            let id = m.get("id").and_then(Value::as_str)?.to_owned();
            let display_name = m.get("displayName").and_then(Value::as_str).unwrap_or(&id).to_owned();
            let is_default = m.get("isDefault").and_then(Value::as_bool).unwrap_or(false);
            Some(ModelOption { id, display_name, is_default })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parses_the_verified_model_list_shape() {
        let page = json!({ "data": [
            { "id": "gpt-6-luna", "displayName": "GPT-6-Luna", "isDefault": true, "hidden": false },
            { "id": "gpt-5.6-terra", "displayName": "GPT-5.6-Terra", "isDefault": false, "hidden": false },
            { "id": "internal", "displayName": "x", "isDefault": false, "hidden": true }
        ], "nextCursor": null });
        let models = parse_models(&page).unwrap();
        assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gpt-6-luna", "gpt-5.6-terra"]);
        assert!(models[0].is_default && !models[1].is_default);
        assert!(parse_models(&json!({})).is_err());
    }
}

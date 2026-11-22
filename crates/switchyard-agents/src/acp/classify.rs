//! Maps ACP permission requests (as built by [`super::protocol`]) to
//! normalized operations. ACP tool calls carry no raw input: a kind, a title
//! (for `execute`, the command line), locations and, for edits, diffs.
//! Anything unexpected never fails open: it becomes an operation with a
//! `problem` (denied by the system).

use serde_json::Value;
use switchyard_policy::{Operation, OperationCategory};

use crate::event::PermissionRequest;

pub const ADAPTER: &str = "gemini";
pub const MAX_COMMAND_BYTES: usize = 4 * 1024;

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn valid_path(p: &str) -> bool {
    !p.is_empty() && !p.contains('\0')
}

fn location_paths(input: &Value) -> Option<Vec<String>> {
    let list = input.get("locations").and_then(Value::as_array)?;
    let mut out = Vec::new();
    for l in list {
        let p = str_field(l, "path")?;
        if !valid_path(p) {
            return None;
        }
        if !out.iter().any(|x: &String| x == p) {
            out.push(p.to_owned());
        }
    }
    Some(out)
}

/// Classifies one ACP permission request.
pub fn classify(request: &PermissionRequest) -> Operation {
    let input = &request.input;
    let kind = request.tool_name.as_str();
    let unclassified = |category, problem: &str| Operation::unclassified(ADAPTER, kind, category, problem);
    if input.get("can_allow_once").and_then(Value::as_bool) != Some(true) {
        return unclassified(
            OperationCategory::Unknown,
            "the agent offered no one-time approval for this request, and Switchyard never grants standing ones",
        );
    }
    let with_paths = |category: OperationCategory, paths: Vec<String>| Operation {
        adapter: ADAPTER.into(),
        tool: kind.into(),
        category,
        paths,
        command: None,
        background: false,
        unexpected_fields: Vec::new(),
        problem: None,
    };
    match kind {
        "read" => match location_paths(input) {
            Some(p) if !p.is_empty() => with_paths(OperationCategory::FileRead, p),
            _ => unclassified(OperationCategory::FileRead, "the request does not say which file it reads"),
        },
        "search" => match location_paths(input) {
            Some(p) if !p.is_empty() => with_paths(OperationCategory::Search, p),
            // A search without a location searches the workspace.
            Some(_) | None => with_paths(OperationCategory::Search, vec![".".into()]),
        },
        "edit" => {
            let diffs = input.get("diffs").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut paths = Vec::new();
            let mut new_files = 0;
            for d in &diffs {
                let Some(p) = str_field(d, "path").filter(|p| valid_path(p)) else {
                    return unclassified(OperationCategory::FileEdit, "a change has no valid path");
                };
                if d.get("new_file").and_then(Value::as_bool) == Some(true) {
                    new_files += 1;
                }
                if !paths.iter().any(|x: &String| x == p) {
                    paths.push(p.to_owned());
                }
            }
            if paths.is_empty() {
                match location_paths(input) {
                    Some(p) if !p.is_empty() => paths = p,
                    _ => {
                        return unclassified(
                            OperationCategory::FileEdit,
                            "the request does not say which file it changes",
                        );
                    }
                }
            }
            // The agent's own tool name decides when it is known: write_file
            // creates or replaces a whole file; replace edits part of one.
            match str_field(input, "tool") {
                Some("write_file") => return with_paths(OperationCategory::FileWrite, paths),
                Some("replace") => return with_paths(OperationCategory::FileEdit, paths),
                _ => {}
            }
            let category = match (new_files, diffs.len()) {
                (n, d) if d > 0 && n == d => OperationCategory::FileWrite,
                (0, _) => OperationCategory::FileEdit,
                _ => {
                    return unclassified(
                        OperationCategory::FileEdit,
                        "a change that both creates and edits files is not supported; ask the agent to do one at a time",
                    );
                }
            };
            with_paths(category, paths)
        }
        "delete" => match location_paths(input) {
            Some(p) if !p.is_empty() => with_paths(OperationCategory::FileDelete, p),
            _ => {
                unclassified(OperationCategory::FileDelete, "the request does not say which file it deletes")
            }
        },
        "execute" => {
            let Some(command) = str_field(input, "title").map(str::trim).filter(|c| !c.is_empty()) else {
                return unclassified(OperationCategory::Shell, "the command is missing or empty");
            };
            if command.contains('\0') {
                return unclassified(OperationCategory::Shell, "the command contains a NUL byte");
            }
            Operation {
                command: Some(crate::event::truncate(command, MAX_COMMAND_BYTES).0),
                ..with_paths(OperationCategory::Shell, Vec::new())
            }
        }
        "fetch" => Operation { problem: None, ..unclassified(OperationCategory::Network, "") },
        "move" => unclassified(
            OperationCategory::Unknown,
            "moving files is not supported; ask the agent to write and delete separately",
        ),
        _ => unclassified(OperationCategory::Unknown, "Switchyard does not support this kind of request"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn req(kind: &str, input: Value) -> PermissionRequest {
        let mut input = input;
        input["can_allow_once"] = json!(true);
        PermissionRequest {
            native_request_id: "x/1".into(),
            tool_name: kind.into(),
            tool_use_id: Some("t".into()),
            input,
            description: None,
            details: Value::Null,
        }
    }

    #[test]
    fn kinds_map_to_operations() {
        let read =
            classify(&req("read", json!({"title": "notes.txt", "locations": [{"path": "/w/notes.txt"}]})));
        assert_eq!(
            (read.category, read.paths.clone(), read.problem.clone()),
            (OperationCategory::FileRead, vec!["/w/notes.txt".to_owned()], None)
        );
        let new = classify(&req("edit", json!({"diffs": [{"path": "/w/a.txt", "new_file": true}]})));
        assert_eq!(new.category, OperationCategory::FileWrite);
        let edit = classify(&req("edit", json!({"diffs": [{"path": "/w/a.txt", "new_file": false}]})));
        assert_eq!(edit.category, OperationCategory::FileEdit);
        // The native tool name wins over the diff.
        let wf = classify(&req(
            "edit",
            json!({"tool": "write_file", "diffs": [{"path": "/w/a.txt", "new_file": false}]}),
        ));
        assert_eq!(wf.category, OperationCategory::FileWrite);
        let rp = classify(&req(
            "edit",
            json!({"tool": "replace", "diffs": [{"path": "/w/a.txt", "new_file": true}]}),
        ));
        assert_eq!(rp.category, OperationCategory::FileEdit);
        let sh = classify(&req("execute", json!({"title": "ls -la"})));
        assert_eq!((sh.category, sh.command.as_deref()), (OperationCategory::Shell, Some("ls -la")));
        let search = classify(&req("search", json!({"title": "grep foo"})));
        assert_eq!((search.category, search.paths), (OperationCategory::Search, vec![".".to_owned()]));
        assert_eq!(classify(&req("fetch", json!({}))).category, OperationCategory::Network);
    }

    #[test]
    fn anything_unclear_is_refused() {
        for (kind, input) in [
            ("read", json!({"title": "x"})),
            ("execute", json!({"title": "  "})),
            (
                "edit",
                json!({"diffs": [{"path": "/w/a", "new_file": true}, {"path": "/w/b", "new_file": false}]}),
            ),
            ("move", json!({})),
            ("think", json!({})),
            ("other", json!({})),
        ] {
            assert!(classify(&req(kind, input)).problem.is_some(), "{kind}");
        }
        let mut no_once = req("execute", json!({"title": "ls"}));
        no_once.input["can_allow_once"] = json!(false);
        assert_eq!(classify(&no_once).category, OperationCategory::Unknown);
    }
}

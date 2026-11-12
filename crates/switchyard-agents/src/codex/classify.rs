//! Maps Codex approval requests to normalized operations (ADR 0021).
//!
//! Input is the request built by [`super::protocol`]. Classification never
//! fails open: anything unexpected becomes an operation with a `problem`
//! (denied by the system) or with `unexpected_fields` (never allowed
//! automatically).

use serde_json::Value;
use switchyard_policy::{Operation, OperationCategory};

use crate::event::PermissionRequest;

pub const ADAPTER: &str = "codex";
/// Longest command text kept in a normalized operation.
pub const MAX_COMMAND_BYTES: usize = 4 * 1024;

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn unexpected(input: &Value) -> Vec<String> {
    input
        .get("unexpected_fields")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Classifies one Codex server request.
pub fn classify(request: &PermissionRequest) -> Operation {
    let tool = request.tool_name.as_str();
    let input = &request.input;
    let unclassified = |category, problem: &str| Operation::unclassified(ADAPTER, tool, category, problem);
    if input.get("other_thread").and_then(Value::as_bool) == Some(true) {
        return unclassified(
            OperationCategory::Unknown,
            "the request comes from a Codex sub-agent thread, which Switchyard does not support",
        );
    }
    match tool {
        "shell" => classify_command(request),
        "apply_patch" => classify_patch(request),
        _ => {
            unclassified(OperationCategory::Unknown, "Switchyard does not support this kind of Codex request")
        }
    }
}

fn classify_command(request: &PermissionRequest) -> Operation {
    let input = &request.input;
    let unclassified = |category, problem: &str| Operation::unclassified(ADAPTER, "shell", category, problem);
    if input.get("network").is_some_and(|n| !n.is_null()) {
        // A managed-network prompt: access to a host, not a command.
        return Operation { problem: None, ..unclassified(OperationCategory::Network, "") };
    }
    if str_field(input, "kind") != Some("command") {
        return unclassified(OperationCategory::Unknown, "only command approvals are supported");
    }
    let Some(command) = str_field(input, "command").filter(|c| !c.trim().is_empty()) else {
        return unclassified(OperationCategory::Shell, "the command is missing or empty");
    };
    if command.contains('\0') {
        return unclassified(OperationCategory::Shell, "the command contains a NUL byte");
    }
    let mut unexpected_fields = unexpected(input);
    if input.get("cwd_is_session_cwd").and_then(Value::as_bool) != Some(true) {
        // The command would run somewhere other than the workspace root.
        unexpected_fields.push("cwd".into());
    }
    unexpected_fields.sort();
    Operation {
        adapter: ADAPTER.into(),
        tool: "shell".into(),
        category: OperationCategory::Shell,
        paths: Vec::new(),
        command: Some(crate::event::truncate(command, MAX_COMMAND_BYTES).0),
        background: false,
        unexpected_fields,
        problem: None,
    }
}

fn classify_patch(request: &PermissionRequest) -> Operation {
    let input = &request.input;
    let unclassified =
        |problem: &str| Operation::unclassified(ADAPTER, "apply_patch", OperationCategory::FileEdit, problem);
    let Some(changes) = input.get("changes").and_then(Value::as_array) else {
        return unclassified("the patch's file list was not reported before the approval request");
    };
    if changes.is_empty() {
        return unclassified("the patch changes no files");
    }
    let mut category = None;
    let mut paths = Vec::new();
    for change in changes {
        let Some(path) = str_field(change, "path").filter(|p| !p.is_empty() && !p.contains('\0')) else {
            return unclassified("a change has no valid path");
        };
        if change.get("move_path").is_some_and(|m| !m.is_null()) {
            return unclassified("moving files is not supported; ask Codex to write and delete separately");
        }
        let this = match str_field(change, "kind") {
            Some("add") => OperationCategory::FileWrite,
            Some("update") => OperationCategory::FileEdit,
            Some("delete") => OperationCategory::FileDelete,
            _ => return unclassified("a change has an unknown kind"),
        };
        match category {
            None => category = Some(this),
            Some(c) if c == this => {}
            Some(_) => {
                return unclassified(
                    "a patch that mixes added, edited and deleted files is not supported; ask Codex to \
                     apply each kind of change in a separate patch",
                );
            }
        }
        paths.push(path.to_owned());
    }
    let mut unexpected_fields = unexpected(input);
    if input.get("grant_root").is_some_and(|g| !g.is_null()) {
        // Codex asks for session-wide write access to a root; never granted
        // and never allowed automatically.
        unexpected_fields.push("grant_root".into());
    }
    unexpected_fields.sort();
    Operation {
        adapter: ADAPTER.into(),
        tool: "apply_patch".into(),
        category: category.unwrap_or(OperationCategory::FileEdit),
        paths,
        command: None,
        background: false,
        unexpected_fields,
        problem: None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn req(tool: &str, input: Value) -> PermissionRequest {
        PermissionRequest {
            native_request_id: "1".into(),
            tool_name: tool.into(),
            tool_use_id: None,
            input,
            description: None,
            details: Value::Null,
        }
    }

    fn command(extra: Value) -> Value {
        let mut v = json!({"kind": "command", "command": "ls", "cwd": "/ws", "cwd_is_session_cwd": true,
            "network": null, "other_thread": false, "unexpected_fields": []});
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    #[test]
    fn commands_are_shell_operations_even_when_they_read() {
        let op = classify(&req("shell", command(json!({"command": "cat notes.txt"}))));
        assert_eq!(op.category, OperationCategory::Shell, "a command is never downgraded to a read");
        assert_eq!(op.command.as_deref(), Some("cat notes.txt"));
        assert!(op.problem.is_none() && op.unexpected_fields.is_empty());
    }

    #[test]
    fn command_in_another_directory_is_never_automatic() {
        let op = classify(&req("shell", command(json!({"cwd": "/elsewhere", "cwd_is_session_cwd": false}))));
        assert_eq!(op.unexpected_fields, vec!["cwd"]);
        let op = classify(&req("shell", command(json!({"unexpected_fields": ["additionalPermissions"]}))));
        assert_eq!(op.unexpected_fields, vec!["additionalPermissions"]);
    }

    #[test]
    fn non_command_and_network_requests() {
        let op = classify(&req("shell", command(json!({"network": {"host": "example.com"}}))));
        assert_eq!((op.category, op.problem.is_none()), (OperationCategory::Network, true));
        let op = classify(&req("shell", command(json!({"kind": "stdin"}))));
        assert_eq!(op.category, OperationCategory::Unknown);
        assert!(classify(&req("shell", command(json!({"command": "  "})))).problem.is_some());
    }

    #[test]
    fn patches_map_by_change_kind() {
        let patch = |changes: Value| json!({"changes": changes, "grant_root": null, "other_thread": false, "unexpected_fields": []});
        let op = classify(&req(
            "apply_patch",
            patch(json!([{"path": "/ws/a", "kind": "add"}, {"path": "/ws/b", "kind": "add"}])),
        ));
        assert_eq!((op.category, op.paths.len()), (OperationCategory::FileWrite, 2));
        assert_eq!(
            classify(&req("apply_patch", patch(json!([{"path": "/ws/a", "kind": "update"}])))).category,
            OperationCategory::FileEdit
        );
        assert_eq!(
            classify(&req("apply_patch", patch(json!([{"path": "/ws/a", "kind": "delete"}])))).category,
            OperationCategory::FileDelete
        );
        for bad in [
            json!([{"path": "/ws/a", "kind": "add"}, {"path": "/ws/b", "kind": "delete"}]),
            json!([{"path": "/ws/a", "kind": "update", "move_path": "/ws/c"}]),
            json!([{"path": "/ws/a", "kind": "rename"}]),
            json!([{"kind": "add"}]),
            json!([]),
            Value::Null,
        ] {
            assert!(classify(&req("apply_patch", patch(bad.clone()))).problem.is_some(), "{bad}");
        }
        let mut g = patch(json!([{"path": "/ws/a", "kind": "add"}]));
        g["grant_root"] = json!("/");
        assert_eq!(classify(&req("apply_patch", g)).unexpected_fields, vec!["grant_root"]);
    }

    #[test]
    fn everything_else_is_denied() {
        for tool in ["item/permissions/requestApproval", "mcpServer/elicitation/request", "whatever"] {
            let op = classify(&req(tool, json!({})));
            assert_eq!(op.category, OperationCategory::Unknown);
            assert!(op.problem.is_some());
        }
        let op = classify(&req("shell", command(json!({"other_thread": true}))));
        assert!(op.problem.as_deref().is_some_and(|p| p.contains("sub-agent")));
    }
}

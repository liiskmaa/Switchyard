//! Maps Claude Code permission requests to normalized operations
//! (ADR 0017). Claude tool names and input shapes stay in this module.
//!
//! Classification never fails open: anything unexpected becomes an
//! operation with a `problem` (denied by the system) or with
//! `unexpected_fields` (never allowed automatically).

use serde_json::{Map, Value};
use switchyard_policy::{Operation, OperationCategory};

use super::launch::TOOLS;
use crate::event::PermissionRequest;

pub const ADAPTER: &str = "claude_code";
/// Longest command text kept in a normalized operation.
pub const MAX_COMMAND_BYTES: usize = 4 * 1024;

struct ToolSpec {
    name: &'static str,
    category: OperationCategory,
    /// Input field holding the target path, and whether it is required.
    path_field: Option<(&'static str, bool)>,
    known: &'static [&'static str],
}

const SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: "Read",
        category: OperationCategory::FileRead,
        path_field: Some(("file_path", true)),
        known: &["file_path", "offset", "limit", "pages"],
    },
    ToolSpec {
        name: "Write",
        category: OperationCategory::FileWrite,
        path_field: Some(("file_path", true)),
        known: &["file_path", "content"],
    },
    ToolSpec {
        name: "Edit",
        category: OperationCategory::FileEdit,
        path_field: Some(("file_path", true)),
        known: &["file_path", "old_string", "new_string", "replace_all"],
    },
    ToolSpec {
        name: "NotebookEdit",
        category: OperationCategory::FileEdit,
        path_field: Some(("notebook_path", true)),
        known: &["notebook_path", "cell_id", "new_source", "cell_type", "edit_mode"],
    },
    ToolSpec {
        name: "Glob",
        category: OperationCategory::Search,
        path_field: Some(("path", false)),
        known: &["pattern", "path"],
    },
    ToolSpec {
        name: "Grep",
        category: OperationCategory::Search,
        path_field: Some(("path", false)),
        known: &[
            "pattern",
            "path",
            "glob",
            "output_mode",
            "-A",
            "-B",
            "-C",
            "-n",
            "-i",
            "type",
            "head_limit",
            "offset",
            "multiline",
            "context",
        ],
    },
    ToolSpec {
        name: "Bash",
        category: OperationCategory::Shell,
        path_field: None,
        known: &["command", "description", "timeout", "run_in_background", "dangerouslyDisableSandbox"],
    },
];

/// Tools Switchyard enables at launch (`--tools`).
pub fn enabled_tools() -> impl Iterator<Item = &'static str> {
    TOOLS.split(',')
}

fn category_of_unlisted(tool: &str) -> OperationCategory {
    match tool {
        "WebFetch" | "WebSearch" => OperationCategory::Network,
        t if t.starts_with("mcp__") => OperationCategory::ExternalTool,
        _ => OperationCategory::Unknown,
    }
}

fn bounded(text: &str) -> String {
    crate::event::truncate(text, MAX_COMMAND_BYTES).0
}

fn str_field<'a>(input: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{key} is not a string")),
    }
}

fn check_path(path: &str, field: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err(format!("{field} is empty"));
    }
    if path.contains('\0') {
        return Err(format!("{field} contains a NUL byte"));
    }
    Ok(())
}

/// Classifies one `can_use_tool` request.
pub fn classify(request: &PermissionRequest) -> Operation {
    let tool = request.tool_name.as_str();
    let unclassified = |category, problem: String| Operation::unclassified(ADAPTER, tool, category, problem);
    if tool.is_empty() || tool == "unknown" {
        return unclassified(OperationCategory::Unknown, "the request names no tool".into());
    }
    let Some(spec) = SPECS.iter().find(|s| s.name == tool) else {
        let category = category_of_unlisted(tool);
        return unclassified(category, format!("the tool {tool} is not enabled for Switchyard sessions"));
    };
    if !enabled_tools().any(|t| t == tool) {
        return unclassified(
            spec.category,
            format!("the tool {tool} is not enabled for Switchyard sessions"),
        );
    }
    let Some(input) = request.input.as_object() else {
        return unclassified(spec.category, "the tool input is not an object".into());
    };
    let mut op = Operation {
        adapter: ADAPTER.into(),
        tool: tool.into(),
        category: spec.category,
        paths: Vec::new(),
        command: None,
        background: false,
        unexpected_fields: input.keys().filter(|k| !spec.known.contains(&k.as_str())).cloned().collect(),
        problem: None,
    };
    op.unexpected_fields.sort();
    let result: Result<(), String> = (|| {
        if let Some((field, required)) = spec.path_field {
            match str_field(input, field)? {
                Some(path) => {
                    check_path(path, field)?;
                    op.paths.push(path.to_owned());
                }
                None if required => return Err(format!("{field} is missing")),
                None => {}
            }
        }
        match tool {
            "Bash" => {
                let command = str_field(input, "command")?.ok_or("command is missing")?;
                if command.trim().is_empty() {
                    return Err("command is empty".into());
                }
                op.command = Some(bounded(command));
                op.background = matches!(input.get("run_in_background"), Some(Value::Bool(true)));
                if matches!(input.get("dangerouslyDisableSandbox"), Some(Value::Bool(true))) {
                    // Never allowed automatically.
                    op.unexpected_fields.push("dangerouslyDisableSandbox".into());
                }
            }
            "Glob" | "Grep" => {
                let pattern = str_field(input, "pattern")?.ok_or("pattern is missing")?;
                if tool == "Glob"
                    && (pattern.starts_with('/')
                        || pattern.starts_with('~')
                        || pattern.split('/').any(|c| c == ".."))
                {
                    // The pattern itself reaches outside the search root:
                    // record it as an extra target that cannot lie inside.
                    op.paths.push(pattern.to_owned());
                }
                if let Some(glob) = str_field(input, "glob")?
                    && (glob.starts_with('/') || glob.split('/').any(|c| c == ".."))
                {
                    op.paths.push(glob.to_owned());
                }
            }
            _ => {}
        }
        Ok(())
    })();
    if let Err(problem) = result {
        op.problem = Some(problem);
    }
    op
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn req(tool: &str, input: Value) -> PermissionRequest {
        PermissionRequest {
            native_request_id: "r".into(),
            tool_name: tool.into(),
            tool_use_id: None,
            input,
            description: None,
            details: Value::Null,
        }
    }

    #[test]
    fn every_enabled_tool_is_classified() {
        for tool in enabled_tools() {
            assert!(SPECS.iter().any(|s| s.name == tool), "{tool} has no classification");
        }
    }

    #[test]
    fn claude_tools_map_to_categories() {
        use OperationCategory::*;
        let cases = [
            ("Read", json!({"file_path": "/w/a.rs", "offset": 1, "limit": 10}), FileRead, vec!["/w/a.rs"]),
            ("Write", json!({"file_path": "/w/a", "content": "x"}), FileWrite, vec!["/w/a"]),
            ("Edit", json!({"file_path": "a", "old_string": "x", "new_string": "y"}), FileEdit, vec!["a"]),
            (
                "NotebookEdit",
                json!({"notebook_path": "n.ipynb", "new_source": "x"}),
                FileEdit,
                vec!["n.ipynb"],
            ),
            ("Glob", json!({"pattern": "*.txt", "path": "/w"}), Search, vec!["/w"]),
            ("Glob", json!({"pattern": "**/*.rs"}), Search, vec![]),
            ("Grep", json!({"pattern": "x", "-i": true, "output_mode": "content"}), Search, vec![]),
        ];
        for (tool, input, category, paths) in cases {
            let op = classify(&req(tool, input));
            assert_eq!(op.problem, None, "{tool}");
            assert_eq!(
                (op.category, op.paths.iter().map(String::as_str).collect::<Vec<_>>()),
                (category, paths)
            );
            assert!(op.unexpected_fields.is_empty(), "{tool}: {:?}", op.unexpected_fields);
            // File contents never enter the operation.
            assert!(!serde_json::to_string(&op).unwrap().contains("\"x\""), "{tool}");
        }
        let bash = classify(&req(
            "Bash",
            json!({"command": "cargo test", "description": "run tests", "timeout": 5}),
        ));
        assert_eq!(
            (bash.category, bash.command.as_deref(), bash.background),
            (Shell, Some("cargo test"), false)
        );
    }

    #[test]
    fn unknown_disabled_and_malformed_requests_carry_a_problem() {
        use OperationCategory::*;
        for (tool, input, category) in [
            ("unknown", json!({}), Unknown),
            ("", json!({}), Unknown),
            ("Teleport", json!({"to": "x"}), Unknown),
            ("WebFetch", json!({"url": "https://x"}), Network),
            ("mcp__srv__tool", json!({}), ExternalTool),
            ("Task", json!({"prompt": "x"}), Unknown),
            ("Read", Value::Null, FileRead),
            ("Read", json!("a"), FileRead),
            ("Read", json!({}), FileRead),
            ("Read", json!({"file_path": 3}), FileRead),
            ("Read", json!({"file_path": ""}), FileRead),
            ("Write", json!({"file_path": "a\u{0}b", "content": ""}), FileWrite),
            ("Bash", json!({}), Shell),
            ("Bash", json!({"command": "  "}), Shell),
            ("Bash", json!({"command": ["ls"]}), Shell),
            ("Glob", json!({"path": "/w"}), Search),
        ] {
            let op = classify(&req(tool, input.clone()));
            assert!(op.problem.is_some(), "{tool} {input} must not classify cleanly");
            assert_eq!(op.category, category, "{tool} {input}");
        }
    }

    #[test]
    fn unexpected_and_risky_fields_are_reported() {
        let op = classify(&req("Write", json!({"file_path": "a", "content": "", "mode": "0777"})));
        assert_eq!(op.unexpected_fields, ["mode"]);
        let op = classify(&req("Bash", json!({"command": "ls", "dangerouslyDisableSandbox": true})));
        assert_eq!(op.unexpected_fields, ["dangerouslyDisableSandbox"]);
        let op = classify(&req("Bash", json!({"command": "sleep 99", "run_in_background": true})));
        assert!(op.background);
        // Patterns that escape the search root become extra targets.
        let op = classify(&req("Glob", json!({"pattern": "../../**/id_rsa", "path": "/w"})));
        assert_eq!(op.paths, ["/w", "../../**/id_rsa"]);
        let op = classify(&req("Grep", json!({"pattern": "k", "glob": "/etc/*"})));
        assert_eq!(op.paths, ["/etc/*"]);
    }
}

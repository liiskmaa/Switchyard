//! Agent installations and project workspaces.

use rusqlite::{Connection, OptionalExtension, params};
use switchyard_agents::AgentProvider;
use switchyard_events::new_id;
use uuid::Uuid;

use crate::{StorageError, parse_uuid};

/// Returns the agent row for `(provider, executable)`, creating it or
/// refreshing its detected version.
pub fn upsert_agent(
    conn: &Connection,
    provider: AgentProvider,
    display_name: &str,
    executable: &str,
    cli_version: &str,
    now_ms: i64,
) -> Result<Uuid, StorageError> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM agents WHERE vendor = ?1 AND executable = ?2",
            params![provider.as_str(), executable],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        conn.execute(
            "UPDATE agents SET cli_version = ?2, display_name = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, cli_version, display_name, now_ms],
        )?;
        return parse_uuid(&id, "agents.id");
    }
    let id = new_id();
    conn.execute(
        "INSERT INTO agents (id, vendor, display_name, executable, cli_version, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![id.to_string(), provider.as_str(), display_name, executable, cli_version, now_ms],
    )?;
    Ok(id)
}

/// Returns the project's primary workspace (its registered checkout),
/// creating it on first use.
pub fn primary_workspace(
    conn: &Connection,
    project_id: Uuid,
    path: &str,
    now_ms: i64,
) -> Result<Uuid, StorageError> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM workspaces WHERE project_id = ?1 AND kind = 'primary'",
            [project_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return parse_uuid(&id, "workspaces.id");
    }
    let id = new_id();
    conn.execute(
        "INSERT INTO workspaces (id, project_id, kind, path, status, created_at, updated_at) \
         VALUES (?1, ?2, 'primary', ?3, 'active', ?4, ?4)",
        params![id.to_string(), project_id.to_string(), path, now_ms],
    )
    .map_err(|e| StorageError::from_unique(e, || format!("workspace path {path} is already in use")))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{insert_project, temp_storage};

    #[test]
    fn agents_and_workspaces_are_upserted() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo/a");
        storage
            .with_conn(|c| {
                let a = upsert_agent(c, AgentProvider::ClaudeCode, "Claude Code", "/bin/claude", "1.0", 1)?;
                let b = upsert_agent(c, AgentProvider::ClaudeCode, "Claude Code", "/bin/claude", "1.1", 2)?;
                assert_eq!(a, b);
                let v: String = c.query_row("SELECT cli_version FROM agents", [], |r| r.get(0))?;
                assert_eq!(v, "1.1");
                let w1 = primary_workspace(c, project, "/repo/a", 1)?;
                let w2 = primary_workspace(c, project, "/repo/a", 2)?;
                assert_eq!(w1, w2);
                Ok(())
            })
            .unwrap();
    }
}

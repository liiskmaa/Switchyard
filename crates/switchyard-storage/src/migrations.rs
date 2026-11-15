//! Forward-only migrations tracked with `PRAGMA user_version`.
//!
//! Migration files live in the repository's top-level `migrations/` directory
//! and are embedded at compile time. Each migration runs in its own
//! transaction together with the version bump.

use rusqlite::Connection;

use crate::error::StorageError;

/// `(version, sql)` in ascending order. Versions are contiguous from 1.
pub const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../../../migrations/0001_initial.sql")),
    (2, include_str!("../../../migrations/0002_agent_sessions.sql")),
    (3, include_str!("../../../migrations/0003_workspaces.sql")),
    (4, include_str!("../../../migrations/0004_policies.sql")),
    (5, include_str!("../../../migrations/0005_tasks.sql")),
    (6, include_str!("../../../migrations/0006_integrations.sql")),
    (7, include_str!("../../../migrations/0007_integration_operations.sql")),
    (8, include_str!("../../../migrations/0008_workflows.sql")),
    (9, include_str!("../../../migrations/0009_workflow_delivery.sql")),
    (10, include_str!("../../../migrations/0010_organizations.sql")),
];

pub const LATEST_SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub(crate) fn current_version(conn: &Connection) -> Result<u32, StorageError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

pub(crate) fn apply(conn: &mut Connection) -> Result<(), StorageError> {
    let current = current_version(conn)?;
    if current > LATEST_SCHEMA_VERSION {
        return Err(StorageError::UnsupportedSchema { found: current, supported: LATEST_SCHEMA_VERSION });
    }
    for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql).map_err(|source| StorageError::Migration { version, source })?;
        tx.pragma_update(None, "user_version", version)
            .map_err(|source| StorageError::Migration { version, source })?;
        tx.commit().map_err(|source| StorageError::Migration { version, source })?;
        tracing::info!(version, "applied database migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Storage;

    #[test]
    fn versions_are_contiguous() {
        for (i, (v, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(*v as usize, i + 1);
        }
    }

    #[test]
    fn reopening_is_idempotent_and_creates_all_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        drop(Storage::open(&path).unwrap());
        let storage = Storage::open(&path).unwrap();
        let tables: Vec<String> = storage
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' \
                     AND name NOT LIKE 'sqlite_%' ORDER BY name",
                )?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .unwrap();
        assert_eq!(
            tables,
            [
                "agents",
                "approvals",
                "decisions",
                "events",
                "findings",
                "integration_operations",
                "integration_settings",
                "integrations",
                "messages",
                "org_context",
                "org_settings",
                "organizations",
                "policy_evaluations",
                "policy_rule_revisions",
                "policy_rules",
                "projects",
                "sessions",
                "task_assignments",
                "task_dependencies",
                "task_proposals",
                "task_workflows",
                "tasks",
                "validation_commands",
                "validation_runs",
                "validation_steps",
                "workflow_settings",
                "workspaces"
            ]
        );
        let version = storage.with_conn(|c| current_version(c)).unwrap();
        assert_eq!(version, LATEST_SCHEMA_VERSION);
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            let c = Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION + 1).unwrap();
        }
        assert!(matches!(Storage::open(&path), Err(StorageError::UnsupportedSchema { .. })));
    }

    /// Milestone 2 sessions ran in the project checkout. After the upgrade
    /// they keep their workspace link, which is now a legacy record that
    /// cannot accept sessions; no session or approval is lost.
    #[test]
    fn upgrade_from_v2_keeps_sessions_attached_to_a_legacy_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            let mut c = Connection::open(&path).unwrap();
            c.pragma_update(None, "foreign_keys", true).unwrap();
            for &(version, sql) in &MIGRATIONS[..2] {
                let tx = c.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.pragma_update(None, "user_version", version).unwrap();
                tx.commit().unwrap();
            }
            c.execute_batch(
                "INSERT INTO projects VALUES ('p1', 'demo', '/repo', 1, 1);
                 INSERT INTO workspaces (id, project_id, kind, path, status, created_at, updated_at)
                     VALUES ('w1', 'p1', 'primary', '/repo', 'active', 1, 2);
                 INSERT INTO agents VALUES ('a1', 'claude_code', 'Claude Code', '/bin/claude', '2.1.322', 1, 1);
                 INSERT INTO sessions (id, project_id, workspace_id, agent_id, provider, native_session_id, cwd,
                     state, started_at, last_activity_at)
                     VALUES ('s1', 'p1', 'w1', 'a1', 'claude_code', 'n1', '/repo', 'stopped', 1, 1);
                 INSERT INTO approvals (id, project_id, session_id, kind, tool_name, native_request_id, request,
                     status, requested_at, expires_at, decided_at, decision_source)
                     VALUES ('ap1', 'p1', 's1', 'tool_use', 'Write', 'r1', '{}', 'approved', 1, 2, 2, 'operator');",
            )
            .unwrap();
        }
        let storage = Storage::open(&path).unwrap();
        storage
            .with_conn(|c| {
                let (ws, kind, status, archived): (Option<String>, String, String, Option<i64>) = c
                    .query_row(
                        "SELECT s.workspace_id, w.kind, w.status, w.archived_at
                     FROM sessions s JOIN workspaces w ON w.id = s.workspace_id WHERE s.id = 's1'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?;
                assert_eq!(ws.as_deref(), Some("w1"));
                assert_eq!((kind.as_str(), status.as_str()), ("legacy_checkout", "archived"));
                assert!(archived.is_some());
                let approvals: i64 = c.query_row("SELECT COUNT(*) FROM approvals", [], |r| r.get(0))?;
                assert_eq!(approvals, 1);
                // New sessions must name a workspace.
                let err = c.execute(
                    "INSERT INTO sessions (id, project_id, agent_id, provider, cwd, state, started_at,
                         last_activity_at) VALUES ('s2', 'p1', 'a1', 'claude_code', '/x', 'starting', 1, 1)",
                    [],
                );
                assert!(err.is_err());
                Ok(())
            })
            .unwrap();
    }

    fn apply_up_to(c: &mut Connection, last: u32) {
        c.pragma_update(None, "foreign_keys", true).unwrap();
        for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v <= last) {
            let tx = c.transaction().unwrap();
            tx.execute_batch(sql).unwrap();
            tx.pragma_update(None, "user_version", version).unwrap();
            tx.commit().unwrap();
        }
    }

    const V3_FIXTURE: &str = "INSERT INTO projects VALUES ('p1', 'demo', '/repo', 1, 1);
         INSERT INTO workspaces (id, project_id, kind, name, path, branch, base_commit, status, created_at,
             updated_at, last_activity_at)
             VALUES ('w1', 'p1', 'worktree', 'one', '/data/w1', 'switchyard/w1', 'abc', 'active', 1, 1, 1);
         INSERT INTO agents VALUES ('a1', 'claude_code', 'Claude Code', '/bin/claude', '2.1.322', 1, 1);
         INSERT INTO sessions (id, project_id, workspace_id, agent_id, provider, native_session_id, cwd,
             state, started_at, last_activity_at)
             VALUES ('s1', 'p1', 'w1', 'a1', 'claude_code', 'n1', '/data/w1', 'waiting_for_approval', 1, 1);
         INSERT INTO approvals (id, project_id, session_id, kind, tool_name, native_request_id, request,
             status, requested_at, expires_at, decided_at, decision_source, decided_by)
             VALUES ('ap1', 'p1', 's1', 'tool_use', 'Write', 'r1', '{\"input\":{}}', 'approved', 1, 2, 2,
                     'operator', 'desktop');
         INSERT INTO approvals (id, project_id, session_id, kind, tool_name, native_request_id, request,
             status, requested_at, expires_at)
             VALUES ('ap2', 'p1', 's1', 'tool_use', 'Bash', 'r2', '{\"input\":{\"command\":\"ls\"}}',
                     'pending', 3, 4);";

    /// Upgrading keeps every approval exactly as it was (a pending one stays
    /// pending), creates no policy rule and drops the never-used
    /// `permissions` table.
    #[test]
    fn upgrade_from_v3_keeps_approvals_and_creates_no_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            let mut c = Connection::open(&path).unwrap();
            apply_up_to(&mut c, 3);
            c.execute_batch(V3_FIXTURE).unwrap();
        }
        let storage = Storage::open(&path).unwrap();
        storage
            .with_conn(|c| {
                #[allow(clippy::type_complexity)]
                let rows: Vec<(String, String, Option<String>, Option<String>, Option<String>)> = c
                    .prepare("SELECT id, status, decision_source, decided_by, evaluation_id FROM approvals ORDER BY id")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                    .collect::<Result<_, _>>()?;
                assert_eq!(
                    rows,
                    [
                        ("ap1".into(), "approved".into(), Some("operator".into()), Some("desktop".into()), None),
                        ("ap2".into(), "pending".into(), None, None, None),
                    ]
                );
                let count = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
                assert_eq!(count("SELECT COUNT(*) FROM policy_rules")?, 0, "no permissive (or any) rule is created");
                assert_eq!(count("SELECT COUNT(*) FROM policy_rule_revisions")?, 0);
                assert_eq!(count("SELECT COUNT(*) FROM policy_evaluations")?, 0);
                assert_eq!(count("SELECT COUNT(*) FROM sqlite_master WHERE name = 'permissions'")?, 0);
                assert_eq!(count("SELECT COUNT(*) FROM sessions WHERE workspace_id = 'w1'")?, 1);
                assert_eq!(count("SELECT COUNT(*) FROM workspaces WHERE status = 'active'")?, 1);
                // `policy` can only ever deny.
                assert!(c.execute("UPDATE approvals SET status = 'approved', decided_at = 5, decision_source = 'policy' WHERE id = 'ap2'", []).is_err());
                Ok(())
            })
            .unwrap();
    }

    /// Migration 0010 rebuilds `task_workflows` (to accept `organization`):
    /// every existing workflow row survives unchanged.
    #[test]
    fn upgrade_to_v10_keeps_workflows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            let mut c = Connection::open(&path).unwrap();
            apply_up_to(&mut c, 9);
            c.execute_batch(
                "INSERT INTO projects VALUES ('p1', 'demo', '/repo', 1, 1);
                 INSERT INTO tasks (id, project_id, title, kind, priority, status, origin, created_at, updated_at)
                     VALUES ('t1', 'p1', 'T', 'general', 'normal', 'completed', 'operator', 1, 1);
                 INSERT INTO task_workflows (task_id, project_id, definition, version, config, status, state,
                     last_event_seq, revision, started_by, created_at, updated_at)
                     VALUES ('t1', 'p1', 'engineering', 1, '{}', 'delivered', '{\"a\":1}', 42, 7, 'me', 1, 2);",
            )
            .unwrap();
        }
        let storage = Storage::open(&path).unwrap();
        storage
            .with_conn(|c| {
                let row: (String, String, i64, i64, String) = c.query_row(
                    "SELECT definition, status, last_event_seq, revision, state FROM task_workflows WHERE task_id = 't1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )?;
                assert_eq!(row, ("engineering".into(), "delivered".into(), 42, 7, "{\"a\":1}".into()));
                c.execute(
                    "INSERT INTO tasks (id, project_id, title, kind, priority, status, origin, created_at, updated_at)
                     VALUES ('t2', 'p1', 'T', 'general', 'normal', 'ready', 'operator', 1, 1)",
                    [],
                )?;
                c.execute(
                    "INSERT INTO task_workflows (task_id, project_id, definition, version, config, status, state,
                     started_by, created_at, updated_at) VALUES ('t2', 'p1', 'organization', 1, '{}', 'running', '{}', 'me', 1, 1)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// The reserved `permissions` table was never written by Core; if
    /// something put rows there, the migration refuses (atomically) instead
    /// of dropping them.
    #[test]
    fn upgrade_refuses_to_drop_a_non_empty_permissions_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            let mut c = Connection::open(&path).unwrap();
            apply_up_to(&mut c, 3);
            c.execute_batch(V3_FIXTURE).unwrap();
            c.execute_batch("INSERT INTO permissions VALUES ('x', 'p1', NULL, 'Bash(*)', 'allow', 1);")
                .unwrap();
        }
        assert!(matches!(Storage::open(&path), Err(StorageError::Migration { version: 4, .. })));
        let c = Connection::open(&path).unwrap();
        let version: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 3, "the failed migration left nothing half-applied");
        let kept: i64 = c.query_row("SELECT COUNT(*) FROM permissions", [], |r| r.get(0)).unwrap();
        let approvals: i64 = c.query_row("SELECT COUNT(*) FROM approvals", [], |r| r.get(0)).unwrap();
        assert_eq!((kept, approvals), (1, 2));
    }

    /// Upgrading an M6 database (schema 5) keeps tasks and assignments, adds
    /// no integration state and leaves earlier reviews without a recorded
    /// tree (so they cannot satisfy an integration's review requirement).
    #[test]
    fn upgrade_from_v8_keeps_workflows_and_allows_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            let mut c = Connection::open(&path).unwrap();
            apply_up_to(&mut c, 8);
            c.execute_batch(V3_FIXTURE).unwrap();
            c.execute_batch(
                "INSERT INTO tasks (id, project_id, title, kind, priority, status, origin, created_at, updated_at)
                     VALUES ('t1', 'p1', 'Fix it', 'implementation', 'normal', 'awaiting_review', 'operator', 1, 1);
                 INSERT INTO task_workflows (task_id, project_id, definition, version, config, workspace_id, status,
                     stage, step_key, reason, state, last_event_seq, revision, started_by, created_at, updated_at,
                     ended_at)
                     VALUES ('t1', 'p1', 'engineering', 1, '{}', 'w1', 'completed', 'delivery', 'review#2', 'r',
                     '{\"status\":\"completed\"}', 42, 7, 'desktop', 1, 2, 3);",
            )
            .unwrap();
        }
        let storage = Storage::open(&path).unwrap();
        storage
            .with_conn(|c| {
                let row: (String, String, i64, i64, Option<String>) = c.query_row(
                    "SELECT status, step_key, last_event_seq, revision, integration_id FROM task_workflows",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )?;
                assert_eq!(row, ("completed".into(), "review#2".into(), 42, 7, None), "the M8.1 row is kept");
                c.execute_batch("UPDATE task_workflows SET status = 'delivered'")?;
                assert!(c.execute_batch("UPDATE task_workflows SET status = 'bogus'").is_err());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn upgrade_from_v5_keeps_tasks_and_records_no_review_tree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            let mut c = Connection::open(&path).unwrap();
            apply_up_to(&mut c, 5);
            c.execute_batch(V3_FIXTURE).unwrap();
            c.execute_batch(
                "INSERT INTO tasks (id, project_id, title, kind, priority, status, origin, created_at, updated_at)
                     VALUES ('t1', 'p1', 'Fix it', 'implementation', 'normal', 'awaiting_review', 'operator', 1, 1);
                 INSERT INTO task_assignments (id, task_id, project_id, role, provider, workspace_id, session_id,
                     status, review_fingerprint, outcome, created_at)
                     VALUES ('as1', 't1', 'p1', 'review', 'codex', 'w1', 's1', 'reported', 'fp', 'review: approved', 2);",
            )
            .unwrap();
        }
        let storage = Storage::open(&path).unwrap();
        storage
            .with_conn(|c| {
                let count = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
                assert_eq!(count("PRAGMA user_version")?, i64::from(LATEST_SCHEMA_VERSION));
                assert_eq!(count("SELECT COUNT(*) FROM tasks WHERE status = 'awaiting_review'")?, 1);
                let row: (String, Option<String>, Option<String>) = c.query_row(
                    "SELECT outcome, review_fingerprint, review_tree FROM task_assignments WHERE id = 'as1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                assert_eq!(row, ("review: approved".into(), Some("fp".into()), None));
                for table in ["integrations", "integration_settings", "validation_commands", "validation_runs", "validation_steps"] {
                    assert_eq!(count(&format!("SELECT COUNT(*) FROM {table}"))?, 0, "{table}");
                }
                assert_eq!(count("SELECT COUNT(*) FROM sessions WHERE workspace_id = 'w1'")?, 1);
                // The upgraded rows can be referenced by an integration.
                c.execute_batch(
                    "INSERT INTO integrations (id, project_id, task_id, workspace_id, source_branch, source_commit,
                         target_branch, target_commit, status, preflight, prepared_at, created_at, updated_at)
                         VALUES ('i1', 'p1', 't1', 'w1', 'switchyard/w1', 'abc', 'main', 'def', 'open', '{}', 3, 3, 3);",
                )?;
                Ok(())
            })
            .unwrap();
    }
}

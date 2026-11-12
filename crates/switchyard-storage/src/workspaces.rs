//! Workspaces: isolated working copies of a project.

use rusqlite::{Connection, OptionalExtension, Row, params};
use switchyard_workspaces::{WorkspaceKind, WorkspaceStatus};
use uuid::Uuid;

use crate::{StorageError, parse_enum, parse_uuid};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub kind: WorkspaceKind,
    pub name: String,
    pub path: String,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub base_commit: Option<String>,
    pub status: WorkspaceStatus,
    pub status_reason: Option<String>,
    pub dirty: Option<bool>,
    pub git_checked_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_activity_at_ms: i64,
    pub archived_at_ms: Option<i64>,
}

const COLUMNS: &str = "id, project_id, kind, name, path, branch, base_ref, base_commit, status, status_reason, \
                       dirty, git_checked_at, created_at, updated_at, last_activity_at, archived_at";

fn from_row(r: &Row<'_>) -> rusqlite::Result<Result<WorkspaceRecord, StorageError>> {
    let id: String = r.get(0)?;
    let project: String = r.get(1)?;
    let kind: String = r.get(2)?;
    let status: String = r.get(8)?;
    let dirty: Option<i64> = r.get(10)?;
    let partial = WorkspaceRecord {
        id: Uuid::nil(),
        project_id: Uuid::nil(),
        kind: WorkspaceKind::Worktree,
        name: r.get(3)?,
        path: r.get(4)?,
        branch: r.get(5)?,
        base_ref: r.get(6)?,
        base_commit: r.get(7)?,
        status: WorkspaceStatus::Provisioning,
        status_reason: r.get(9)?,
        dirty: dirty.map(|d| d != 0),
        git_checked_at_ms: r.get(11)?,
        created_at_ms: r.get(12)?,
        updated_at_ms: r.get(13)?,
        last_activity_at_ms: r.get(14)?,
        archived_at_ms: r.get(15)?,
    };
    Ok((|| {
        Ok(WorkspaceRecord {
            id: parse_uuid(&id, "workspaces.id")?,
            project_id: parse_uuid(&project, "workspaces.project_id")?,
            kind: parse_enum(&kind, "workspaces.kind")?,
            status: parse_enum(&status, "workspaces.status")?,
            ..partial
        })
    })())
}

fn collect(
    stmt: &mut rusqlite::Statement<'_>,
    args: impl rusqlite::Params,
) -> Result<Vec<WorkspaceRecord>, StorageError> {
    let rows = stmt.query_map(args, from_row)?;
    rows.map(|r| r.map_err(StorageError::from).and_then(|inner| inner)).collect()
}

pub fn insert(conn: &Connection, w: &WorkspaceRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!("INSERT INTO workspaces ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"),
        params![
            w.id.to_string(),
            w.project_id.to_string(),
            w.kind.as_str(),
            w.name,
            w.path,
            w.branch,
            w.base_ref,
            w.base_commit,
            w.status.as_str(),
            w.status_reason,
            w.dirty.map(i64::from),
            w.git_checked_at_ms,
            w.created_at_ms,
            w.updated_at_ms,
            w.last_activity_at_ms,
            w.archived_at_ms,
        ],
    )
    .map_err(|e| {
        StorageError::from_unique(e, || {
            format!("workspace path or branch already in use ({}, {:?})", w.path, w.branch)
        })
    })?;
    Ok(())
}

pub fn get(conn: &Connection, id: Uuid) -> Result<WorkspaceRecord, StorageError> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {COLUMNS} FROM workspaces WHERE id = ?1"))?;
    collect(&mut stmt, [id.to_string()])?
        .pop()
        .ok_or_else(|| StorageError::NotFound(format!("workspace {id}")))
}

pub fn find(conn: &Connection, id: Uuid) -> Result<Option<WorkspaceRecord>, StorageError> {
    match get(conn, id) {
        Ok(w) => Ok(Some(w)),
        Err(StorageError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A project's workspaces, oldest first. Archived, cleaned-up and legacy
/// records only with `include_archived`.
pub fn list(
    conn: &Connection,
    project_id: Uuid,
    include_archived: bool,
) -> Result<Vec<WorkspaceRecord>, StorageError> {
    let filter = if include_archived {
        ""
    } else {
        "AND status NOT IN ('archived', 'cleanup_pending', 'removed') AND kind = 'worktree'"
    };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM workspaces WHERE project_id = ?1 {filter} ORDER BY created_at, id"
    ))?;
    collect(&mut stmt, [project_id.to_string()])
}

/// Worktree workspaces that still own something on disk (startup
/// reconciliation).
pub fn list_unremoved_worktrees(conn: &Connection) -> Result<Vec<WorkspaceRecord>, StorageError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM workspaces WHERE kind = 'worktree' AND status <> 'removed' ORDER BY created_at"
    ))?;
    collect(&mut stmt, [])
}

/// Number of workspaces ever created for a project (default names).
pub fn count_for_project(conn: &Connection, project_id: Uuid) -> Result<i64, StorageError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM workspaces WHERE project_id = ?1 AND kind = 'worktree'",
        [project_id.to_string()],
        |r| r.get(0),
    )?)
}

/// Worktree workspaces whose files may still exist (blocks removing the
/// project registration, which would orphan them).
pub fn count_unremoved_worktrees(conn: &Connection, project_id: Uuid) -> Result<i64, StorageError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM workspaces WHERE project_id = ?1 AND kind = 'worktree' AND status <> 'removed'",
        [project_id.to_string()],
        |r| r.get(0),
    )?)
}

/// Field updates applied together with a status transition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceChanges {
    pub status_reason: Option<Option<String>>,
    pub dirty: Option<Option<bool>>,
    pub git_checked_at_ms: Option<Option<i64>>,
    /// Counts as activity (session attached/detached, operator action).
    pub activity: bool,
}

/// Validates and applies a status transition plus `changes`. Returns the
/// record before and after. Fails with `Conflict` on an illegal transition.
pub fn transition(
    conn: &Connection,
    id: Uuid,
    to: WorkspaceStatus,
    changes: &WorkspaceChanges,
    now_ms: i64,
) -> Result<(WorkspaceRecord, WorkspaceRecord), StorageError> {
    let before = get(conn, id)?;
    let mut after = before.clone();
    after.status =
        before.status.transition(to).map_err(|e| StorageError::Conflict(format!("workspace {id}: {e}")))?;
    if let Some(reason) = &changes.status_reason {
        after.status_reason = reason.clone();
    }
    if let Some(dirty) = changes.dirty {
        after.dirty = dirty;
    }
    if let Some(checked) = changes.git_checked_at_ms {
        after.git_checked_at_ms = checked;
    }
    if after.status.is_archived() {
        after.archived_at_ms = after.archived_at_ms.or(Some(now_ms));
    } else {
        after.archived_at_ms = None;
    }
    if changes.activity {
        after.last_activity_at_ms = now_ms;
    }
    after.updated_at_ms = now_ms;
    conn.execute(
        "UPDATE workspaces SET status = ?2, status_reason = ?3, dirty = ?4, git_checked_at = ?5, \
         archived_at = ?6, last_activity_at = ?7, updated_at = ?8 WHERE id = ?1",
        params![
            id.to_string(),
            after.status.as_str(),
            after.status_reason,
            after.dirty.map(i64::from),
            after.git_checked_at_ms,
            after.archived_at_ms,
            after.last_activity_at_ms,
            now_ms,
        ],
    )?;
    Ok((before, after))
}

pub fn touch(conn: &Connection, id: Uuid, now_ms: i64) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE workspaces SET last_activity_at = ?2 WHERE id = ?1",
        params![id.to_string(), now_ms],
    )?;
    Ok(())
}

/// The workspace a session runs in, with its display name.
pub fn for_session(conn: &Connection, session_id: Uuid) -> Result<Option<(Uuid, String)>, StorageError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT w.id, w.name FROM sessions s JOIN workspaces w ON w.id = s.workspace_id WHERE s.id = ?1",
            [session_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(id, name)| Ok((parse_uuid(&id, "workspaces.id")?, name))).transpose()
}

/// The live session attached to a workspace, if any.
pub fn live_session(conn: &Connection, workspace_id: Uuid) -> Result<Option<Uuid>, StorageError> {
    let live = crate::stats::ACTIVE_SESSION_STATUSES;
    let id: Option<String> = conn
        .query_row(
            &format!("SELECT id FROM sessions WHERE workspace_id = ?1 AND state IN ({live}) LIMIT 1"),
            [workspace_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    id.map(|s| parse_uuid(&s, "sessions.id")).transpose()
}

#[cfg(test)]
pub(crate) mod tests {
    use switchyard_events::new_id;

    use super::*;
    use crate::test_support::{insert_project, temp_storage};

    pub(crate) fn sample(project_id: Uuid, status: WorkspaceStatus) -> WorkspaceRecord {
        let id = new_id();
        WorkspaceRecord {
            id,
            project_id,
            kind: WorkspaceKind::Worktree,
            name: "Workspace 1".into(),
            path: format!("/data/workspaces/{project_id}/{id}"),
            branch: Some(format!("switchyard/{id}")),
            base_ref: Some("HEAD".into()),
            base_commit: Some("abc".into()),
            status,
            status_reason: None,
            dirty: None,
            git_checked_at_ms: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            last_activity_at_ms: 1,
            archived_at_ms: None,
        }
    }

    #[test]
    fn lifecycle_is_enforced_and_archive_time_tracked() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo");
        storage
            .with_conn(|c| {
                let w = sample(project, WorkspaceStatus::Provisioning);
                insert(c, &w)?;
                assert_eq!(get(c, w.id)?, w);
                assert!(list(c, project, false)?.iter().any(|x| x.id == w.id));
                let (_, ready) = transition(
                    c,
                    w.id,
                    WorkspaceStatus::Ready,
                    &WorkspaceChanges { dirty: Some(Some(false)), ..Default::default() },
                    5,
                )?;
                assert_eq!(
                    (ready.status, ready.dirty, ready.updated_at_ms),
                    (WorkspaceStatus::Ready, Some(false), 5)
                );
                assert!(matches!(
                    transition(c, w.id, WorkspaceStatus::Removed, &WorkspaceChanges::default(), 6),
                    Err(StorageError::Conflict(_))
                ));
                let (_, archived) =
                    transition(c, w.id, WorkspaceStatus::Archived, &WorkspaceChanges::default(), 7)?;
                assert_eq!(archived.archived_at_ms, Some(7));
                assert!(list(c, project, false)?.is_empty());
                assert_eq!(list(c, project, true)?.len(), 1);
                let (_, restored) =
                    transition(c, w.id, WorkspaceStatus::Ready, &WorkspaceChanges::default(), 8)?;
                assert_eq!(restored.archived_at_ms, None);
                assert_eq!(count_unremoved_worktrees(c, project)?, 1);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn paths_and_branches_are_unique() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo");
        storage
            .with_conn(|c| {
                let a = sample(project, WorkspaceStatus::Ready);
                insert(c, &a)?;
                let mut b = sample(project, WorkspaceStatus::Ready);
                b.branch = a.branch.clone();
                assert!(matches!(insert(c, &b), Err(StorageError::Conflict(_))));
                let mut d = sample(project, WorkspaceStatus::Ready);
                d.path = a.path.clone();
                assert!(matches!(insert(c, &d), Err(StorageError::Conflict(_))));
                Ok(())
            })
            .unwrap();
    }
}

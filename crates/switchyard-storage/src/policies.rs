//! Policy rules, their append-only revision history, per-project policy
//! settings (auto mode, ADR 0037), and the append-only evaluation audit trail
//! (ADRs 0016, 0018).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use switchyard_policy::{Conditions, Decision, OperationCategory, Rule};
use uuid::Uuid;

use crate::{StorageError, parse_enum, parse_uuid};

/// The stored state of one rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub category: OperationCategory,
    pub decision: Decision,
    pub conditions: Conditions,
    pub description: String,
    pub enabled: bool,
    pub archived: bool,
    pub revision: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub archived_at_ms: Option<i64>,
}

impl RuleRecord {
    /// The evaluator's view of the rule.
    pub fn to_rule(&self) -> Rule {
        Rule {
            id: self.id,
            revision: self.revision,
            project_id: self.project_id,
            workspace_id: self.workspace_id,
            category: self.category,
            decision: self.decision,
            conditions: self.conditions.clone(),
            description: self.description.clone(),
            enabled: self.enabled,
            archived: self.archived,
            created_at_ms: self.created_at_ms,
        }
    }
}

const RULE_COLUMNS: &str = "id, project_id, workspace_id, category, decision, conditions, description, enabled, \
                            status, revision, created_at, updated_at, archived_at";

fn rule_from_row(r: &Row<'_>) -> rusqlite::Result<Result<RuleRecord, StorageError>> {
    let id: String = r.get(0)?;
    let project: String = r.get(1)?;
    let workspace: Option<String> = r.get(2)?;
    let category: String = r.get(3)?;
    let decision: String = r.get(4)?;
    let conditions: String = r.get(5)?;
    let description: String = r.get(6)?;
    let enabled: i64 = r.get(7)?;
    let status: String = r.get(8)?;
    let revision: i64 = r.get(9)?;
    let created: i64 = r.get(10)?;
    let updated: i64 = r.get(11)?;
    let archived_at: Option<i64> = r.get(12)?;
    Ok((|| {
        Ok(RuleRecord {
            id: parse_uuid(&id, "policy_rules.id")?,
            project_id: parse_uuid(&project, "policy_rules.project_id")?,
            workspace_id: workspace
                .as_deref()
                .map(|w| parse_uuid(w, "policy_rules.workspace_id"))
                .transpose()?,
            category: parse_enum(&category, "policy_rules.category")?,
            decision: parse_enum(&decision, "policy_rules.decision")?,
            conditions: serde_json::from_str(&conditions)
                .map_err(|e| StorageError::Corrupt(format!("policy_rules.conditions: {e}")))?,
            description,
            enabled: enabled != 0,
            archived: status == "archived",
            revision: u32::try_from(revision)
                .map_err(|_| StorageError::Corrupt(format!("policy_rules.revision: {revision}")))?,
            created_at_ms: created,
            updated_at_ms: updated,
            archived_at_ms: archived_at,
        })
    })())
}

fn collect_rules(
    stmt: &mut rusqlite::Statement<'_>,
    args: impl rusqlite::Params,
) -> Result<Vec<RuleRecord>, StorageError> {
    let rows = stmt.query_map(args, rule_from_row)?;
    rows.map(|r| r.map_err(StorageError::from).and_then(|inner| inner)).collect()
}

fn conditions_json(c: &Conditions) -> Result<String, StorageError> {
    serde_json::to_string(c).map_err(|e| StorageError::Corrupt(format!("cannot encode conditions: {e}")))
}

pub fn insert_rule(conn: &Connection, rule: &RuleRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!(
            "INSERT INTO policy_rules ({RULE_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"
        ),
        params![
            rule.id.to_string(),
            rule.project_id.to_string(),
            rule.workspace_id.map(|w| w.to_string()),
            rule.category.as_str(),
            rule.decision.as_str(),
            conditions_json(&rule.conditions)?,
            rule.description,
            i64::from(rule.enabled),
            if rule.archived { "archived" } else { "active" },
            i64::from(rule.revision),
            rule.created_at_ms,
            rule.updated_at_ms,
            rule.archived_at_ms,
        ],
    )?;
    Ok(())
}

/// Writes `rule` if the stored revision is still `expected_revision`.
/// Fails with `Conflict` otherwise (someone else changed it).
pub fn update_rule(conn: &Connection, rule: &RuleRecord, expected_revision: u32) -> Result<(), StorageError> {
    let changed = conn.execute(
        "UPDATE policy_rules SET decision = ?3, conditions = ?4, description = ?5, enabled = ?6, status = ?7, \
         revision = ?8, updated_at = ?9, archived_at = ?10 WHERE id = ?1 AND revision = ?2",
        params![
            rule.id.to_string(),
            i64::from(expected_revision),
            rule.decision.as_str(),
            conditions_json(&rule.conditions)?,
            rule.description,
            i64::from(rule.enabled),
            if rule.archived { "archived" } else { "active" },
            i64::from(rule.revision),
            rule.updated_at_ms,
            rule.archived_at_ms,
        ],
    )?;
    if changed == 0 {
        return Err(StorageError::Conflict(format!(
            "policy rule {} is no longer at revision {expected_revision}",
            rule.id
        )));
    }
    Ok(())
}

pub fn get_rule(conn: &Connection, id: Uuid) -> Result<RuleRecord, StorageError> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {RULE_COLUMNS} FROM policy_rules WHERE id = ?1"))?;
    collect_rules(&mut stmt, [id.to_string()])?
        .pop()
        .ok_or_else(|| StorageError::NotFound(format!("policy rule {id}")))
}

/// Rules of a project (project and workspace scope), oldest first.
pub fn list_rules(
    conn: &Connection,
    project_id: Uuid,
    include_archived: bool,
) -> Result<Vec<RuleRecord>, StorageError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {RULE_COLUMNS} FROM policy_rules WHERE project_id = ?1 AND (?2 OR status = 'active') \
         ORDER BY created_at, id"
    ))?;
    collect_rules(&mut stmt, params![project_id.to_string(), include_archived])
}

/// Enabled, active rules that can apply to a request in `workspace_id` of
/// `project_id`: project rules plus that workspace's rules.
pub fn applicable_rules(
    conn: &Connection,
    project_id: Uuid,
    workspace_id: Option<Uuid>,
) -> Result<Vec<Rule>, StorageError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {RULE_COLUMNS} FROM policy_rules WHERE project_id = ?1 AND status = 'active' AND enabled = 1 \
         AND (workspace_id IS NULL OR workspace_id = ?2) ORDER BY created_at, id"
    ))?;
    Ok(collect_rules(&mut stmt, params![project_id.to_string(), workspace_id.map(|w| w.to_string())])?
        .iter()
        .map(RuleRecord::to_rule)
        .collect())
}

/// The current global policy revision (0 before the first change). Rule
/// changes and settings changes share it.
pub fn current_revision(conn: &Connection) -> Result<i64, StorageError> {
    Ok(conn.query_row(
        "SELECT MAX((SELECT COALESCE(MAX(policy_revision), 0) FROM policy_rule_revisions), \
                    (SELECT COALESCE(MAX(policy_revision), 0) FROM policy_settings_revisions))",
        [],
        |r| r.get(0),
    )?)
}

/// A project's policy settings (ADR 0037). A project that never changed them
/// has auto mode off at revision 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPolicySettings {
    pub project_id: Uuid,
    pub auto_mode: bool,
    pub revision: u32,
    pub updated_at_ms: Option<i64>,
    pub updated_by: Option<String>,
}

pub fn project_settings(conn: &Connection, project_id: Uuid) -> Result<ProjectPolicySettings, StorageError> {
    let row = conn
        .query_row(
            "SELECT auto_mode, revision, updated_at, updated_by FROM policy_project_settings WHERE project_id = ?1",
            [project_id.to_string()],
            |r| {
                Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<String>>(3)?))
            },
        )
        .optional()?;
    Ok(match row {
        Some((auto_mode, revision, updated_at, updated_by)) => ProjectPolicySettings {
            project_id,
            auto_mode,
            revision: u32::try_from(revision)
                .map_err(|_| StorageError::Corrupt("settings revision".into()))?,
            updated_at_ms: Some(updated_at),
            updated_by,
        },
        None => ProjectPolicySettings {
            project_id,
            auto_mode: false,
            revision: 0,
            updated_at_ms: None,
            updated_by: None,
        },
    })
}

/// Whether auto mode is on for the project. Read inside the transaction that
/// evaluates, like the rules.
pub fn auto_mode(conn: &Connection, project_id: Uuid) -> Result<bool, StorageError> {
    Ok(project_settings(conn, project_id)?.auto_mode)
}

/// Sets auto mode when the stored revision is `expected_revision` (0 when
/// never set). Appends the history row and returns the new settings and the
/// new global policy revision. Must run in one transaction with its event.
pub fn set_auto_mode(
    conn: &Connection,
    project_id: Uuid,
    auto_mode: bool,
    expected_revision: u32,
    changed_by: Option<&str>,
    now_ms: i64,
) -> Result<(ProjectPolicySettings, i64), StorageError> {
    let current = project_settings(conn, project_id)?;
    if current.revision != expected_revision {
        return Err(StorageError::Conflict(format!(
            "policy settings changed (revision {} is current, {} was expected)",
            current.revision, expected_revision
        )));
    }
    let revision = current.revision + 1;
    conn.execute(
        "INSERT INTO policy_project_settings (project_id, auto_mode, revision, updated_at, updated_by) \
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (project_id) DO UPDATE SET \
         auto_mode = excluded.auto_mode, revision = excluded.revision, \
         updated_at = excluded.updated_at, updated_by = excluded.updated_by",
        params![project_id.to_string(), i64::from(auto_mode), i64::from(revision), now_ms, changed_by],
    )?;
    let policy_revision = current_revision(conn)? + 1;
    conn.execute(
        "INSERT INTO policy_settings_revisions (project_id, revision, policy_revision, auto_mode, changed_at, \
         changed_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            project_id.to_string(),
            i64::from(revision),
            policy_revision,
            i64::from(auto_mode),
            now_ms,
            changed_by
        ],
    )?;
    Ok((project_settings(conn, project_id)?, policy_revision))
}

/// One entry of a rule's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRevision {
    pub rule_id: Uuid,
    pub revision: u32,
    pub policy_revision: i64,
    pub change: String,
    pub rule: RuleRecord,
    pub changed_at_ms: i64,
    pub changed_by: Option<String>,
}

/// Appends the snapshot of `rule` after `change`; returns the new global
/// policy revision. Must run in the transaction that changed the rule.
pub fn append_revision(
    conn: &Connection,
    rule: &RuleRecord,
    change: &str,
    changed_by: Option<&str>,
    now_ms: i64,
) -> Result<i64, StorageError> {
    let next = current_revision(conn)? + 1;
    let snapshot =
        serde_json::to_string(rule).map_err(|e| StorageError::Corrupt(format!("cannot encode rule: {e}")))?;
    conn.execute(
        "INSERT INTO policy_rule_revisions (rule_id, revision, policy_revision, project_id, workspace_id, change, \
         rule, changed_at, changed_by) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            rule.id.to_string(),
            i64::from(rule.revision),
            next,
            rule.project_id.to_string(),
            rule.workspace_id.map(|w| w.to_string()),
            change,
            snapshot,
            now_ms,
            changed_by,
        ],
    )?;
    Ok(next)
}

pub fn list_revisions(conn: &Connection, rule_id: Uuid) -> Result<Vec<RuleRevision>, StorageError> {
    let mut stmt = conn.prepare_cached(
        "SELECT rule_id, revision, policy_revision, change, rule, changed_at, changed_by FROM policy_rule_revisions \
         WHERE rule_id = ?1 ORDER BY revision",
    )?;
    let rows = stmt.query_map([rule_id.to_string()], |r| {
        let id: String = r.get(0)?;
        let revision: i64 = r.get(1)?;
        let snapshot: String = r.get(4)?;
        Ok((
            id,
            revision,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            snapshot,
            r.get::<_, i64>(5)?,
            r.get(6)?,
        ))
    })?;
    rows.map(|row| {
        let (id, revision, policy_revision, change, snapshot, changed_at_ms, changed_by) = row?;
        Ok(RuleRevision {
            rule_id: parse_uuid(&id, "policy_rule_revisions.rule_id")?,
            revision: u32::try_from(revision)
                .map_err(|_| StorageError::Corrupt(format!("policy_rule_revisions.revision: {revision}")))?,
            policy_revision,
            change,
            rule: serde_json::from_str(&snapshot)
                .map_err(|e| StorageError::Corrupt(format!("policy_rule_revisions.rule: {e}")))?,
            changed_at_ms,
            changed_by,
        })
    })
    .collect()
}

/// One row of the evaluation audit trail.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationRecord {
    pub id: Uuid,
    pub evaluated_at_ms: i64,
    /// `request`, `reevaluation` or `decision_check`.
    pub trigger: String,
    pub project_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub session_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub adapter: String,
    pub tool_name: String,
    pub native_request_id: String,
    pub category: OperationCategory,
    /// `{"operation": …, "paths": …}`.
    pub input: Value,
    pub decision: Decision,
    pub reason: String,
    pub explanation: String,
    pub deciding_rule_id: Option<Uuid>,
    pub deciding_rule_revision: Option<u32>,
    pub matched_rules: Value,
    pub restrictions: Value,
    pub policy_revision: i64,
    pub system_version: u32,
    pub approval_id: Option<Uuid>,
}

const EVAL_COLUMNS: &str = "id, evaluated_at, trigger, project_id, workspace_id, session_id, agent_id, adapter, \
                            tool_name, native_request_id, category, input, decision, reason, explanation, \
                            deciding_rule_id, deciding_rule_revision, matched_rules, restrictions, policy_revision, \
                            system_version, approval_id";

pub fn insert_evaluation(conn: &Connection, e: &EvaluationRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!(
            "INSERT INTO policy_evaluations ({EVAL_COLUMNS}) VALUES \
             (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)"
        ),
        params![
            e.id.to_string(),
            e.evaluated_at_ms,
            e.trigger,
            e.project_id.to_string(),
            e.workspace_id.map(|w| w.to_string()),
            e.session_id.to_string(),
            e.agent_id.map(|a| a.to_string()),
            e.adapter,
            e.tool_name,
            e.native_request_id,
            e.category.as_str(),
            e.input.to_string(),
            e.decision.as_str(),
            e.reason,
            e.explanation,
            e.deciding_rule_id.map(|r| r.to_string()),
            e.deciding_rule_revision.map(i64::from),
            e.matched_rules.to_string(),
            e.restrictions.to_string(),
            e.policy_revision,
            i64::from(e.system_version),
            e.approval_id.map(|a| a.to_string()),
        ],
    )?;
    Ok(())
}

fn eval_from_row(r: &Row<'_>) -> rusqlite::Result<Result<EvaluationRecord, StorageError>> {
    let get_uuid = |i: usize, col: &'static str| -> rusqlite::Result<Result<Uuid, StorageError>> {
        let s: String = r.get(i)?;
        Ok(parse_uuid(&s, col))
    };
    let opt_uuid = |i: usize, col: &'static str| -> rusqlite::Result<Result<Option<Uuid>, StorageError>> {
        let s: Option<String> = r.get(i)?;
        Ok(s.as_deref().map(|v| parse_uuid(v, col)).transpose())
    };
    let json = |i: usize, col: &'static str| -> rusqlite::Result<Result<Value, StorageError>> {
        let s: String = r.get(i)?;
        Ok(serde_json::from_str(&s).map_err(|e| StorageError::Corrupt(format!("{col}: {e}"))))
    };
    let id = get_uuid(0, "policy_evaluations.id")?;
    let project = get_uuid(3, "policy_evaluations.project_id")?;
    let workspace = opt_uuid(4, "policy_evaluations.workspace_id")?;
    let session = get_uuid(5, "policy_evaluations.session_id")?;
    let agent = opt_uuid(6, "policy_evaluations.agent_id")?;
    let category: String = r.get(10)?;
    let input = json(11, "policy_evaluations.input")?;
    let decision: String = r.get(12)?;
    let rule = opt_uuid(15, "policy_evaluations.deciding_rule_id")?;
    let rule_revision: Option<i64> = r.get(16)?;
    let matched = json(17, "policy_evaluations.matched_rules")?;
    let restrictions = json(18, "policy_evaluations.restrictions")?;
    let system_version: i64 = r.get(20)?;
    let approval = opt_uuid(21, "policy_evaluations.approval_id")?;
    let (
        evaluated_at_ms,
        trigger,
        adapter,
        tool_name,
        native_request_id,
        reason,
        explanation,
        policy_revision,
    ) = (r.get(1)?, r.get(2)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(13)?, r.get(14)?, r.get(19)?);
    Ok((|| {
        Ok(EvaluationRecord {
            id: id?,
            evaluated_at_ms,
            trigger,
            project_id: project?,
            workspace_id: workspace?,
            session_id: session?,
            agent_id: agent?,
            adapter,
            tool_name,
            native_request_id,
            category: parse_enum(&category, "policy_evaluations.category")?,
            input: input?,
            decision: parse_enum(&decision, "policy_evaluations.decision")?,
            reason,
            explanation,
            deciding_rule_id: rule?,
            deciding_rule_revision: rule_revision.and_then(|v| u32::try_from(v).ok()),
            matched_rules: matched?,
            restrictions: restrictions?,
            policy_revision,
            system_version: u32::try_from(system_version).unwrap_or(0),
            approval_id: approval?,
        })
    })())
}

pub fn get_evaluation(conn: &Connection, id: Uuid) -> Result<Option<EvaluationRecord>, StorageError> {
    let mut stmt =
        conn.prepare_cached(&format!("SELECT {EVAL_COLUMNS} FROM policy_evaluations WHERE id = ?1"))?;
    stmt.query_row([id.to_string()], eval_from_row).optional()?.transpose()
}

#[derive(Debug, Clone, Default)]
pub struct EvaluationFilter {
    pub project_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub rule_id: Option<Uuid>,
    pub decision: Option<Decision>,
}

/// Newest first.
pub fn list_evaluations(
    conn: &Connection,
    f: &EvaluationFilter,
    limit: u32,
) -> Result<Vec<EvaluationRecord>, StorageError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {EVAL_COLUMNS} FROM policy_evaluations WHERE (?1 IS NULL OR project_id = ?1) \
         AND (?2 IS NULL OR workspace_id = ?2) AND (?3 IS NULL OR session_id = ?3) \
         AND (?4 IS NULL OR deciding_rule_id = ?4 OR EXISTS (SELECT 1 FROM json_each(matched_rules) \
              WHERE json_extract(value, '$.id') = ?4)) \
         AND (?5 IS NULL OR decision = ?5) ORDER BY evaluated_at DESC, id DESC LIMIT ?6"
    ))?;
    let rows = stmt.query_map(
        params![
            f.project_id.map(|v| v.to_string()),
            f.workspace_id.map(|v| v.to_string()),
            f.session_id.map(|v| v.to_string()),
            f.rule_id.map(|v| v.to_string()),
            f.decision.map(Decision::as_str),
            limit
        ],
        eval_from_row,
    )?;
    rows.map(|r| r.map_err(StorageError::from).and_then(|inner| inner)).collect()
}

#[cfg(test)]
mod tests {
    use switchyard_events::new_id;

    use super::*;
    use crate::test_support::{insert_project, temp_storage};

    fn rule(project: Uuid) -> RuleRecord {
        RuleRecord {
            id: new_id(),
            project_id: project,
            workspace_id: None,
            category: OperationCategory::FileEdit,
            decision: Decision::Allow,
            conditions: Conditions { path_prefix: Some("docs".into()), command: None },
            description: "docs".into(),
            enabled: true,
            archived: false,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            archived_at_ms: None,
        }
    }

    #[test]
    fn revisions_are_global_append_only_and_guarded() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo");
        storage
            .with_conn(|c| {
                assert_eq!(current_revision(c)?, 0);
                let mut r = rule(project);
                insert_rule(c, &r)?;
                assert_eq!(append_revision(c, &r, "created", Some("t"), 1)?, 1);
                let mut next = r.clone();
                next.revision = 2;
                next.decision = Decision::Ask;
                update_rule(c, &next, 1)?;
                assert_eq!(append_revision(c, &next, "updated", None, 2)?, 2);
                // A stale writer loses.
                r.revision = 2;
                assert!(matches!(update_rule(c, &r, 1), Err(StorageError::Conflict(_))));
                let history = list_revisions(c, next.id)?;
                assert_eq!(
                    history.iter().map(|h| (h.revision, h.policy_revision)).collect::<Vec<_>>(),
                    [(1, 1), (2, 2)]
                );
                assert_eq!(history[0].rule.decision, Decision::Allow, "snapshots keep the old state");
                assert!(c.execute("DELETE FROM policy_rule_revisions", []).is_err());
                assert!(c.execute("UPDATE policy_rule_revisions SET change = 'x'", []).is_err());
                assert_eq!(get_rule(c, next.id)?.decision, Decision::Ask);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn auto_mode_is_off_by_default_versioned_and_shares_the_global_revision() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo");
        storage
            .with_conn(|c| {
                let s = project_settings(c, project)?;
                assert_eq!((s.auto_mode, s.revision), (false, 0));
                let r = rule(project);
                insert_rule(c, &r)?;
                assert_eq!(append_revision(c, &r, "created", None, 1)?, 1);
                let (s, global) = set_auto_mode(c, project, true, 0, Some("op"), 2)?;
                assert_eq!((s.auto_mode, s.revision, global), (true, 1, 2));
                assert!(auto_mode(c, project)?);
                assert_eq!(current_revision(c)?, 2);
                // The next rule change takes the next global revision.
                let mut next = r.clone();
                next.revision = 2;
                update_rule(c, &next, 1)?;
                assert_eq!(append_revision(c, &next, "updated", None, 3)?, 3);
                // A stale writer loses.
                assert!(matches!(
                    set_auto_mode(c, project, false, 0, None, 4),
                    Err(StorageError::Conflict(_))
                ));
                let (s, global) = set_auto_mode(c, project, false, 1, None, 4)?;
                assert_eq!((s.auto_mode, s.revision, global), (false, 2, 4));
                assert!(c.execute("DELETE FROM policy_settings_revisions", []).is_err());
                assert!(c.execute("UPDATE policy_settings_revisions SET auto_mode = 1", []).is_err());
                // A colliding global revision is refused either way.
                assert!(
                    c.execute(
                        "INSERT INTO policy_settings_revisions VALUES (?1, 9, 3, 1, 5, NULL)",
                        [project.to_string()]
                    )
                    .is_err()
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn schema_refuses_unsafe_rows() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo");
        let other = insert_project(&storage, "/other");
        storage
            .with_conn(|c| {
                // A workspace of another project.
                c.execute(
                    "INSERT INTO workspaces (id, project_id, kind, name, path, branch, base_commit, status, \
                     created_at, updated_at, last_activity_at) \
                     VALUES (?1, ?2, 'worktree', 'w', '/x', 'switchyard/x', 'abc', 'ready', 1, 1, 1)",
                    [Uuid::nil().to_string(), other.to_string()],
                )?;
                let mut r = rule(project);
                r.workspace_id = Some(Uuid::nil());
                assert!(insert_rule(c, &r).is_err(), "cross-project workspace rule");
                // Broad shell allow.
                let mut r = rule(project);
                r.category = OperationCategory::Shell;
                r.conditions = Conditions::default();
                assert!(insert_rule(c, &r).is_err(), "broad shell allow");
                // Allow for network.
                let mut r = rule(project);
                r.category = OperationCategory::Network;
                r.conditions = Conditions::default();
                assert!(insert_rule(c, &r).is_err());
                // Unknown category.
                assert!(
                    c.execute(
                        "INSERT INTO policy_rules (id, project_id, category, decision, conditions, description, \
                         enabled, status, revision, created_at, updated_at) VALUES ('x', ?1, 'unknown', 'deny', \
                         '{}', 'd', 1, 'active', 1, 1, 1)",
                        [project.to_string()],
                    )
                    .is_err()
                );
                // Scope cannot change after creation.
                let r = rule(project);
                insert_rule(c, &r)?;
                assert!(c.execute("UPDATE policy_rules SET project_id = ?1", [other.to_string()]).is_err());
                assert!(c.execute("UPDATE policy_rules SET category = 'file_read'", []).is_err());
                Ok(())
            })
            .unwrap();
    }
}

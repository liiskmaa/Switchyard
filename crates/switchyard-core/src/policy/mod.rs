//! Approval policies in Core (ADRs 0016–0019).
//!
//! * [`PolicyService`] manages rules: every change validates, writes the
//!   rule, appends its revision and journals `policy.rule_changed` in one
//!   transaction.
//! * [`evaluate_in_tx`] is the single place requests are evaluated: it loads
//!   the applicable rules, the project's auto mode (ADR 0037) and the current
//!   revision, runs the pure evaluator and records the result, all inside the
//!   caller's transaction.
//! * Auto mode changes go through [`PolicyService::set_auto_mode`]: setting,
//!   history row (next global revision) and `policy.settings_changed` in one
//!   transaction.
//! * [`paths`] computes path facts before evaluation (the only I/O).
//!
//! Policies authorize what Switchyard answers; they do not sandbox what an
//! allowed tool does (ADR 0019).

pub(crate) mod paths;

use serde_json::{Value, json};
use switchyard_events::{DomainEvent, EventBus, EventContext, EventDraft, new_id, now_ms};
use switchyard_policy::{
    Conditions, Decision, Evaluation, EvaluationInput, OperationCategory, RuleError, RuleScope,
    SYSTEM_POLICY_VERSION, evaluate, normalize_path_prefix, same_match, system_restrictions, validate_rule,
};
use switchyard_protocol::methods::{PoliciesCreateParams, PoliciesEvaluationsParams, PoliciesUpdateParams};
use switchyard_protocol::model::{
    EffectivePolicy, PolicyCategoryInfo, PolicyEvaluation, PolicyRule, PolicyRuleRevision, PolicySettings,
    WorkspaceKind, WorkspaceStatus,
};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::policies::{self as repo, EvaluationFilter, EvaluationRecord, RuleRecord};
use switchyard_storage::{Storage, StorageError, Transaction, projects, workspaces};
use uuid::Uuid;

use crate::handlers::errors;
use crate::journal::{self, draft};

/// Longest target text (path or command) carried in a journal event.
const MAX_EVENT_TARGET: usize = 512;

fn rule_error(e: RuleError) -> ProtocolError {
    match e {
        RuleError::Invalid(m) => ProtocolError::new(ErrorCode::InvalidRule, m),
        RuleError::Unsupported(m) => ProtocolError::new(ErrorCode::UnsupportedOperation, m),
    }
}

fn not_found(id: Uuid) -> ProtocolError {
    ProtocolError::new(ErrorCode::PolicyRuleNotFound, format!("policy rule {id} not found"))
        .with_data(json!({ "rule_id": id }))
}

fn workspace_name(
    conn: &switchyard_storage::Connection,
    id: Option<Uuid>,
) -> Result<Option<String>, StorageError> {
    match id {
        None => Ok(None),
        Some(id) => match workspaces::get(conn, id) {
            Ok(w) => Ok(Some(w.name)),
            Err(StorageError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        },
    }
}

pub(crate) fn to_wire(r: RuleRecord, workspace_name: Option<String>) -> PolicyRule {
    PolicyRule {
        id: r.id,
        project_id: r.project_id,
        workspace_id: r.workspace_id,
        workspace_name,
        scope: if r.workspace_id.is_some() { RuleScope::Workspace } else { RuleScope::Project },
        category: r.category,
        decision: r.decision,
        conditions: r.conditions,
        description: r.description,
        enabled: r.enabled,
        archived: r.archived,
        revision: r.revision,
        created_at_ms: r.created_at_ms,
        updated_at_ms: r.updated_at_ms,
        archived_at_ms: r.archived_at_ms,
    }
}

fn changed_event(r: &RuleRecord, change: &str, policy_revision: i64) -> DomainEvent {
    DomainEvent::PolicyRuleChanged {
        rule_id: r.id,
        change: change.into(),
        revision: r.revision,
        policy_revision,
        scope: if r.workspace_id.is_some() { "workspace" } else { "project" }.into(),
        category: r.category.as_str().into(),
        decision: r.decision.as_str().into(),
        enabled: r.enabled,
        archived: r.archived,
        description: r.description.clone(),
        conditions: serde_json::to_value(&r.conditions).unwrap_or(Value::Null),
    }
}

fn rule_context(r: &RuleRecord) -> EventContext {
    EventContext { project_id: Some(r.project_id), workspace_id: r.workspace_id, ..Default::default() }
}

/// Who asked and where, as Core's own records say.
#[derive(Debug, Clone)]
pub(crate) struct RequestOrigin {
    pub agent_id: Option<Uuid>,
    pub native_request_id: String,
    pub ctx: EventContext,
}

/// Result of [`evaluate_in_tx`].
pub(crate) struct Recorded {
    pub evaluation_id: Uuid,
    pub evaluation: Evaluation,
    pub drafts: Vec<EventDraft>,
}

fn target(input: &EvaluationInput) -> Option<String> {
    let op = &input.operation;
    let raw = op.command.clone().or_else(|| op.paths.first().cloned())?;
    Some(switchyard_agents::event::truncate(&raw, MAX_EVENT_TARGET).0)
}

/// Evaluates `input` against the rules current in `tx` and records the
/// evaluation (row + `agent.permission_evaluated`). Any storage error
/// aborts the caller's transaction; the caller must then deny.
pub(crate) fn evaluate_in_tx(
    tx: &Transaction<'_>,
    input: &EvaluationInput,
    origin: &RequestOrigin,
    trigger: &str,
    approval_id: Option<Uuid>,
    now: i64,
) -> Result<Recorded, StorageError> {
    let rules = repo::applicable_rules(tx, input.scope.project_id, input.scope.workspace_id)?;
    let revision = repo::current_revision(tx)?;
    // Auto mode comes from Core's records in this transaction, never from the
    // caller, and is recorded with the input.
    let mut input = input.clone();
    input.scope.auto_mode = repo::auto_mode(tx, input.scope.project_id)?;
    let input = &input;
    let evaluation = evaluate(input, &rules, revision);
    // Only `ask` creates an approval; other triggers settle an existing one.
    let linked =
        if trigger == "request" && evaluation.decision != Decision::Ask { None } else { approval_id };
    let id = new_id();
    let record = EvaluationRecord {
        id,
        evaluated_at_ms: now,
        trigger: trigger.into(),
        project_id: input.scope.project_id,
        workspace_id: input.scope.workspace_id,
        session_id: input.scope.session_id,
        agent_id: origin.agent_id,
        adapter: input.operation.adapter.clone(),
        tool_name: input.operation.tool.clone(),
        native_request_id: origin.native_request_id.clone(),
        category: input.operation.category,
        input: to_json(input)?,
        decision: evaluation.decision,
        reason: evaluation.reason.as_str().into(),
        explanation: evaluation.explanation.clone(),
        deciding_rule_id: evaluation.deciding_rule.as_ref().map(|r| r.id),
        deciding_rule_revision: evaluation.deciding_rule.as_ref().map(|r| r.revision),
        matched_rules: to_json(&evaluation.matched_rules)?,
        restrictions: to_json(&evaluation.restrictions)?,
        policy_revision: evaluation.policy_revision,
        system_version: evaluation.system_version,
        approval_id: linked,
    };
    repo::insert_evaluation(tx, &record)?;
    let event = DomainEvent::AgentPermissionEvaluated {
        evaluation_id: id,
        trigger: trigger.into(),
        tool_name: input.operation.tool.clone(),
        category: input.operation.category.as_str().into(),
        decision: evaluation.decision.as_str().into(),
        reason: evaluation.reason.as_str().into(),
        explanation: evaluation.explanation.clone(),
        rule_id: evaluation.deciding_rule.as_ref().map(|r| r.id),
        rule_revision: evaluation.deciding_rule.as_ref().map(|r| r.revision),
        policy_revision: evaluation.policy_revision,
        approval_id: linked,
        target: target(input),
    };
    Ok(Recorded { evaluation_id: id, evaluation, drafts: vec![draft(event, &origin.ctx)?] })
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, StorageError> {
    serde_json::to_value(value).map_err(|e| StorageError::Corrupt(format!("cannot encode evaluation: {e}")))
}

/// The stored input of an evaluation, for re-evaluation.
pub(crate) fn stored_input(record: &EvaluationRecord) -> Result<EvaluationInput, StorageError> {
    serde_json::from_value(record.input.clone())
        .map_err(|e| StorageError::Corrupt(format!("policy_evaluations.input: {e}")))
}

fn evaluation_to_wire(
    conn: &switchyard_storage::Connection,
    e: EvaluationRecord,
) -> Result<PolicyEvaluation, StorageError> {
    let corrupt = |what: &str, err: serde_json::Error| {
        StorageError::Corrupt(format!("policy_evaluations.{what}: {err}"))
    };
    let matched_rules: Vec<switchyard_policy::RuleRef> =
        serde_json::from_value(e.matched_rules).map_err(|err| corrupt("matched_rules", err))?;
    let restrictions = serde_json::from_value(e.restrictions).map_err(|err| corrupt("restrictions", err))?;
    let reason = serde_json::from_value(json!(e.reason)).map_err(|err| corrupt("reason", err))?;
    let deciding_rule = e.deciding_rule_id.and_then(|id| matched_rules.iter().find(|r| r.id == id).cloned());
    let approval_status = match e.approval_id {
        Some(id) => match switchyard_storage::approvals::get(conn, id) {
            Ok(a) => Some(a.status),
            Err(StorageError::NotFound(_)) => None,
            Err(err) => return Err(err),
        },
        None => None,
    };
    // Show only the operation and its path facts; the scope is in columns.
    let input = json!({
        "operation": e.input.get("operation").cloned().unwrap_or(Value::Null),
        "paths": e.input.get("paths").cloned().unwrap_or(Value::Null),
    });
    Ok(PolicyEvaluation {
        id: e.id,
        evaluated_at_ms: e.evaluated_at_ms,
        trigger: e.trigger,
        project_id: e.project_id,
        workspace_id: e.workspace_id,
        workspace_name: workspace_name(conn, e.workspace_id)?,
        session_id: e.session_id,
        adapter: e.adapter,
        tool_name: e.tool_name,
        category: e.category,
        input,
        decision: e.decision,
        reason,
        explanation: e.explanation,
        deciding_rule,
        matched_rules,
        restrictions,
        policy_revision: e.policy_revision,
        system_version: e.system_version,
        approval_id: e.approval_id,
        approval_status,
    })
}

/// Summary attached to an approval created by an `ask` evaluation.
pub(crate) fn approval_policy(
    conn: &switchyard_storage::Connection,
    evaluation_id: Option<Uuid>,
) -> Result<Option<switchyard_protocol::model::ApprovalPolicy>, StorageError> {
    let Some(id) = evaluation_id else { return Ok(None) };
    let Some(record) = repo::get_evaluation(conn, id)? else { return Ok(None) };
    let scope = stored_input(&record).ok().map(|input| approval_scope(&input));
    let wire = evaluation_to_wire(conn, record)?;
    Ok(Some(switchyard_protocol::model::ApprovalPolicy {
        evaluation_id: wire.id,
        category: wire.category,
        reason: wire.reason,
        explanation: wire.explanation,
        deciding_rule: wire.deciding_rule,
        policy_revision: wire.policy_revision,
        scope,
    }))
}

/// What approving this request would reach (shown before a person decides).
pub(crate) fn approval_scope(input: &EvaluationInput) -> switchyard_protocol::model::ApprovalScope {
    use switchyard_policy::{OperationCategory as C, PathFact};
    let op = &input.operation;
    let scope = |kind: &str, outside: Vec<String>, unresolved: Vec<String>, note: String| {
        switchyard_protocol::model::ApprovalScope {
            kind: kind.into(),
            outside_paths: outside,
            unresolved_paths: unresolved,
            note,
        }
    };
    if op.category == C::Shell {
        let mut note = "An approved command runs with your user account's permissions and can read, \
                        change or delete any file you can."
            .to_owned();
        if input.scope.review_session {
            note.push_str(
                " This is a review session: approving a command can still change the workspace under review; \
                 Switchyard checks the workspace for changes when the review ends.",
            );
        }
        if op.adapter == switchyard_agents::codex::ADAPTER {
            note.push_str(
                " Codex does not apply its own filesystem sandbox to approved commands (verified with codex-cli 0.166.0).",
            );
        }
        return scope("unrestricted", Vec::new(), Vec::new(), note);
    }
    if !op.category.is_path_category() {
        return scope(
            "unknown",
            Vec::new(),
            Vec::new(),
            "Switchyard cannot establish what this request would reach.".into(),
        );
    }
    let mut outside = Vec::new();
    let mut unresolved = Vec::new();
    for (i, fact) in input.paths.iter().enumerate() {
        let path = op.paths.get(i).cloned().unwrap_or_else(|| "the workspace".into());
        match fact {
            PathFact::Inside { .. } => {}
            PathFact::Outside => outside.push(path),
            PathFact::Unresolved { .. } => unresolved.push(path),
        }
    }
    if input.paths.is_empty() || input.paths.len() < op.paths.len() {
        return scope(
            "unknown",
            outside,
            unresolved,
            "Switchyard could not resolve every target of this request.".into(),
        );
    }
    if !outside.is_empty() {
        return scope(
            "outside_workspace",
            outside,
            unresolved,
            "Approving lets the agent change files beyond this workspace.".into(),
        );
    }
    if !unresolved.is_empty() {
        return scope(
            "unknown",
            outside,
            unresolved,
            "Switchyard could not establish where some targets point.".into(),
        );
    }
    scope(
        "workspace",
        outside,
        unresolved,
        "Every target resolves inside the workspace (after following symlinks, at the time of the request)."
            .into(),
    )
}

#[derive(Clone)]
pub(crate) struct PolicyService {
    storage: Storage,
    bus: EventBus,
}

/// What a rule change looks like.
enum Change {
    Update { decision: Option<Decision>, conditions: Option<Conditions>, description: Option<String> },
    SetEnabled(bool),
    Archive,
}

fn normalized_conditions(mut c: Conditions) -> Conditions {
    c.path_prefix = c.path_prefix.map(|p| normalize_path_prefix(&p));
    c
}

/// Validates that `workspace_id` is a usable rule scope of `project_id`.
fn check_scope(
    tx: &Transaction<'_>,
    project_id: Uuid,
    workspace_id: Option<Uuid>,
) -> Result<Result<(), ProtocolError>, StorageError> {
    match projects::get(tx, project_id) {
        Ok(_) => {}
        Err(StorageError::NotFound(_)) => {
            return Ok(Err(ProtocolError::new(
                ErrorCode::ProjectNotFound,
                format!("project {project_id} is not registered"),
            )));
        }
        Err(e) => return Err(e),
    }
    let Some(workspace_id) = workspace_id else { return Ok(Ok(())) };
    let ws = match workspaces::get(tx, workspace_id) {
        Ok(ws) => ws,
        Err(StorageError::NotFound(_)) => {
            return Ok(Err(ProtocolError::new(
                ErrorCode::WorkspaceNotFound,
                format!("workspace {workspace_id} not found"),
            )));
        }
        Err(e) => return Err(e),
    };
    if ws.project_id != project_id {
        return Ok(Err(ProtocolError::new(
            ErrorCode::WorkspaceProjectMismatch,
            format!("workspace {workspace_id} belongs to another project"),
        )
        .with_data(json!({ "workspace_id": workspace_id, "workspace_project_id": ws.project_id, "project_id": project_id }))));
    }
    if ws.kind != WorkspaceKind::Worktree || ws.status == WorkspaceStatus::Removed {
        return Ok(Err(ProtocolError::new(
            ErrorCode::InvalidPolicyScope,
            "rules can only be scoped to an isolated workspace that has not been removed",
        )));
    }
    Ok(Ok(()))
}

fn conflict_with(tx: &Transaction<'_>, candidate: &RuleRecord) -> Result<Option<RuleRecord>, StorageError> {
    let mine = candidate.to_rule();
    Ok(repo::list_rules(tx, candidate.project_id, false)?
        .into_iter()
        .find(|r| r.id != candidate.id && same_match(&r.to_rule(), &mine)))
}

fn conflict_error(existing: &RuleRecord) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::PolicyConflict,
        format!(
            "an active rule with the same scope, operation and conditions already exists (\u{201c}{}\u{201d}); edit it instead",
            existing.description
        ),
    )
    .with_data(json!({ "rule_id": existing.id, "revision": existing.revision }))
}

fn settings_to_wire(s: repo::ProjectPolicySettings) -> PolicySettings {
    PolicySettings {
        project_id: s.project_id,
        auto_mode: s.auto_mode,
        revision: s.revision,
        updated_at_ms: s.updated_at_ms,
        updated_by: s.updated_by,
    }
}

impl PolicyService {
    pub fn new(storage: Storage, bus: EventBus) -> Self {
        Self { storage, bus }
    }

    pub async fn settings(&self, project_id: Uuid) -> Result<PolicySettings, ProtocolError> {
        self.storage
            .call(move |c| {
                let tx = c.transaction()?;
                if let Err(e) = check_scope(&tx, project_id, None)? {
                    return Ok(Err(e));
                }
                Ok(Ok(settings_to_wire(repo::project_settings(&tx, project_id)?)))
            })
            .await
            .map_err(errors::storage)?
    }

    /// Turns auto mode on or off (ADR 0037). Pending approvals keep waiting
    /// for the person they were asked of.
    pub async fn set_auto_mode(
        &self,
        project_id: Uuid,
        enabled: bool,
        expected_revision: u32,
        changed_by: String,
    ) -> Result<(PolicySettings, i64), ProtocolError> {
        let now = now_ms();
        journal::commit_checked(&self.storage, &self.bus, move |tx| {
            if let Err(e) = check_scope(tx, project_id, None)? {
                return Ok(Err(e));
            }
            let current = repo::project_settings(tx, project_id)?;
            if current.revision != expected_revision {
                return Ok(Err(ProtocolError::new(
                    ErrorCode::StaleRevision,
                    format!(
                        "the policy settings changed since they were read (now revision {}, expected {expected_revision}); reload and try again",
                        current.revision
                    ),
                )
                .with_data(json!({ "project_id": project_id, "current_revision": current.revision }))));
            }
            if current.auto_mode == enabled {
                return Ok(Err(ProtocolError::new(
                    ErrorCode::InvalidParams,
                    format!("auto mode is already {}", if enabled { "on" } else { "off" }),
                )));
            }
            let (settings, policy_revision) =
                repo::set_auto_mode(tx, project_id, enabled, expected_revision, Some(&changed_by), now)?;
            let event = DomainEvent::PolicySettingsChanged {
                auto_mode: settings.auto_mode,
                revision: settings.revision,
                policy_revision,
            };
            let ctx = EventContext { project_id: Some(project_id), ..Default::default() };
            Ok(Ok(((settings_to_wire(settings), policy_revision), vec![draft(event, &ctx)?])))
        })
        .await
    }

    pub async fn create(
        &self,
        p: PoliciesCreateParams,
        changed_by: String,
    ) -> Result<(PolicyRule, i64), ProtocolError> {
        let conditions = normalized_conditions(p.conditions);
        let description = p.description.trim().to_owned();
        validate_rule(p.category, p.decision, &conditions, &description).map_err(rule_error)?;
        let now = now_ms();
        let record = RuleRecord {
            id: new_id(),
            project_id: p.project_id,
            workspace_id: p.workspace_id,
            category: p.category,
            decision: p.decision,
            conditions,
            description,
            enabled: p.enabled.unwrap_or(true),
            archived: false,
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            archived_at_ms: None,
        };
        journal::commit_checked(&self.storage, &self.bus, move |tx| {
            if let Err(e) = check_scope(tx, record.project_id, record.workspace_id)? {
                return Ok(Err(e));
            }
            if let Some(existing) = conflict_with(tx, &record)? {
                return Ok(Err(conflict_error(&existing)));
            }
            repo::insert_rule(tx, &record)?;
            let revision = repo::append_revision(tx, &record, "created", Some(&changed_by), now)?;
            let drafts = vec![draft(changed_event(&record, "created", revision), &rule_context(&record))?];
            let name = workspace_name(tx, record.workspace_id)?;
            Ok(Ok(((to_wire(record, name), revision), drafts)))
        })
        .await
    }

    async fn change(
        &self,
        rule_id: Uuid,
        expected_revision: u32,
        change: Change,
        changed_by: String,
    ) -> Result<(PolicyRule, i64), ProtocolError> {
        let now = now_ms();
        journal::commit_checked(&self.storage, &self.bus, move |tx| {
            let current = match repo::get_rule(tx, rule_id) {
                Ok(r) => r,
                Err(StorageError::NotFound(_)) => return Ok(Err(not_found(rule_id))),
                Err(e) => return Err(e),
            };
            if current.revision != expected_revision {
                return Ok(Err(ProtocolError::new(
                    ErrorCode::StaleRevision,
                    format!(
                        "the rule changed since it was read (now revision {}, expected {expected_revision}); reload and try again",
                        current.revision
                    ),
                )
                .with_data(json!({ "rule_id": rule_id, "current_revision": current.revision }))));
            }
            if current.archived {
                return Ok(Err(ProtocolError::new(ErrorCode::InvalidRule, "an archived rule cannot be changed")));
            }
            let mut next = current.clone();
            next.revision = current.revision + 1;
            next.updated_at_ms = now;
            let kind = match change {
                Change::Update { decision, conditions, description } => {
                    if decision.is_none() && conditions.is_none() && description.is_none() {
                        return Ok(Err(ProtocolError::new(ErrorCode::InvalidParams, "nothing to change")));
                    }
                    if let Some(d) = decision {
                        next.decision = d;
                    }
                    if let Some(c) = conditions {
                        next.conditions = normalized_conditions(c);
                    }
                    if let Some(d) = description {
                        next.description = d.trim().to_owned();
                    }
                    if let Err(e) = validate_rule(next.category, next.decision, &next.conditions, &next.description) {
                        return Ok(Err(rule_error(e)));
                    }
                    if let Some(existing) = conflict_with(tx, &next)? {
                        return Ok(Err(conflict_error(&existing)));
                    }
                    "updated"
                }
                Change::SetEnabled(enabled) => {
                    if current.enabled == enabled {
                        return Ok(Err(ProtocolError::new(
                            ErrorCode::InvalidParams,
                            format!("the rule is already {}", if enabled { "enabled" } else { "disabled" }),
                        )));
                    }
                    next.enabled = enabled;
                    if enabled { "enabled" } else { "disabled" }
                }
                Change::Archive => {
                    next.archived = true;
                    next.archived_at_ms = Some(now);
                    "archived"
                }
            };
            repo::update_rule(tx, &next, expected_revision)?;
            let revision = repo::append_revision(tx, &next, kind, Some(&changed_by), now)?;
            let drafts = vec![draft(changed_event(&next, kind, revision), &rule_context(&next))?];
            let name = workspace_name(tx, next.workspace_id)?;
            Ok(Ok(((to_wire(next, name), revision), drafts)))
        })
        .await
    }

    pub async fn update(
        &self,
        p: PoliciesUpdateParams,
        by: String,
    ) -> Result<(PolicyRule, i64), ProtocolError> {
        let change =
            Change::Update { decision: p.decision, conditions: p.conditions, description: p.description };
        self.change(p.rule_id, p.expected_revision, change, by).await
    }

    pub async fn set_enabled(
        &self,
        rule_id: Uuid,
        expected_revision: u32,
        enabled: bool,
        by: String,
    ) -> Result<(PolicyRule, i64), ProtocolError> {
        self.change(rule_id, expected_revision, Change::SetEnabled(enabled), by).await
    }

    pub async fn archive(
        &self,
        rule_id: Uuid,
        expected_revision: u32,
        by: String,
    ) -> Result<(PolicyRule, i64), ProtocolError> {
        self.change(rule_id, expected_revision, Change::Archive, by).await
    }

    pub async fn list(
        &self,
        project_id: Uuid,
        include_archived: bool,
    ) -> Result<(Vec<PolicyRule>, i64), ProtocolError> {
        self.storage
            .call(move |c| {
                if let Err(StorageError::NotFound(_)) = projects::get(c, project_id) {
                    return Ok(Err(ProtocolError::new(
                        ErrorCode::ProjectNotFound,
                        format!("project {project_id} is not registered"),
                    )));
                }
                let rules = repo::list_rules(c, project_id, include_archived)?
                    .into_iter()
                    .map(|r| {
                        let name = workspace_name(c, r.workspace_id)?;
                        Ok(to_wire(r, name))
                    })
                    .collect::<Result<Vec<_>, StorageError>>()?;
                Ok(Ok((rules, repo::current_revision(c)?)))
            })
            .await
            .map_err(errors::storage)?
    }

    pub async fn get(&self, rule_id: Uuid) -> Result<(PolicyRule, Vec<PolicyRuleRevision>), ProtocolError> {
        self.storage
            .call(move |c| {
                let rule = match repo::get_rule(c, rule_id) {
                    Ok(r) => r,
                    Err(StorageError::NotFound(_)) => return Ok(Err(not_found(rule_id))),
                    Err(e) => return Err(e),
                };
                let name = workspace_name(c, rule.workspace_id)?;
                let history = repo::list_revisions(c, rule_id)?
                    .into_iter()
                    .map(|h| PolicyRuleRevision {
                        revision: h.revision,
                        policy_revision: h.policy_revision,
                        change: h.change,
                        rule: to_wire(h.rule, name.clone()),
                        changed_at_ms: h.changed_at_ms,
                        changed_by: h.changed_by,
                    })
                    .collect();
                Ok(Ok((to_wire(rule, name), history)))
            })
            .await
            .map_err(errors::storage)?
    }

    pub async fn effective(
        &self,
        project_id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<EffectivePolicy, ProtocolError> {
        self.storage
            .call(move |c| {
                let tx = c.transaction()?;
                if let Err(e) = check_scope(&tx, project_id, workspace_id)? {
                    // A removed workspace may still be inspected.
                    if e.code != ErrorCode::InvalidPolicyScope {
                        return Ok(Err(e));
                    }
                }
                let all = repo::list_rules(&tx, project_id, false)?;
                let mut project_rules = Vec::new();
                let mut workspace_rules = Vec::new();
                for r in all {
                    match r.workspace_id {
                        None => project_rules.push(to_wire(r, None)),
                        Some(w) if Some(w) == workspace_id => {
                            let name = workspace_name(&tx, Some(w))?;
                            workspace_rules.push(to_wire(r, name));
                        }
                        Some(_) => {}
                    }
                }
                let categories = OperationCategory::ALL
                    .iter()
                    .map(|&category| PolicyCategoryInfo {
                        category,
                        default_decision: category.default_decision(),
                        configurable: category.configurable(),
                        allow_supported: category.allow_supported(),
                        path_condition: category.is_path_category(),
                        command_condition: category == OperationCategory::Shell,
                    })
                    .collect();
                Ok(Ok(EffectivePolicy {
                    project_id,
                    workspace_id,
                    policy_revision: repo::current_revision(&tx)?,
                    system_version: SYSTEM_POLICY_VERSION,
                    system_restrictions: system_restrictions(),
                    categories,
                    project_rules,
                    workspace_rules,
                }))
            })
            .await
            .map_err(errors::storage)?
    }

    pub async fn evaluations(
        &self,
        p: PoliciesEvaluationsParams,
        limit: u32,
    ) -> Result<Vec<PolicyEvaluation>, ProtocolError> {
        let filter = EvaluationFilter {
            project_id: p.project_id,
            workspace_id: p.workspace_id,
            session_id: p.session_id,
            rule_id: p.rule_id,
            decision: p.decision,
        };
        self.storage
            .call(move |c| {
                repo::list_evaluations(c, &filter, limit)?
                    .into_iter()
                    .map(|e| evaluation_to_wire(c, e))
                    .collect::<Result<Vec<_>, _>>()
            })
            .await
            .map_err(errors::storage)
    }
}

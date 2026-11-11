use std::path::Path;

use switchyard_events::{DomainEvent, EventContext, EventDraft, new_id, now_ms};
use switchyard_protocol::methods::{
    ProjectIdParams, ProjectResult, ProjectsAddParams, ProjectsListResult, ProjectsRemoveResult,
};
use switchyard_protocol::model::{GitStatusReport, Project};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::{ProjectRecord, StorageError, projects};

use super::errors;
use crate::state::CoreState;

async fn to_project(record: ProjectRecord) -> Project {
    let accessible = tokio::fs::metadata(&record.root_path).await.is_ok_and(|m| m.is_dir());
    Project {
        id: record.id,
        name: record.name,
        root_path: record.root_path,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
        accessible,
    }
}

pub(crate) async fn list(state: &CoreState) -> Result<ProjectsListResult, ProtocolError> {
    let records = state.storage.call(|c| projects::list(c)).await.map_err(errors::storage)?;
    let mut result = Vec::with_capacity(records.len());
    for record in records {
        result.push(to_project(record).await);
    }
    Ok(ProjectsListResult { projects: result })
}

pub(crate) async fn get(state: &CoreState, params: ProjectIdParams) -> Result<ProjectResult, ProtocolError> {
    let id = params.project_id;
    let record = state.storage.call(move |c| projects::get(c, id)).await.map_err(errors::storage)?;
    Ok(ProjectResult { project: to_project(record).await })
}

pub(crate) async fn add(
    state: &CoreState,
    params: ProjectsAddParams,
) -> Result<ProjectResult, ProtocolError> {
    let requested = params.path.trim();
    if requested.is_empty() {
        return Err(ProtocolError::new(ErrorCode::InvalidPath, "path must not be empty"));
    }
    let root =
        state.git.resolve_repository_root(Path::new(requested)).await.map_err(errors::git_registration)?;
    let root_path = root
        .path
        .to_str()
        .ok_or_else(|| ProtocolError::new(ErrorCode::InvalidPath, "repository path is not valid UTF-8"))?
        .to_owned();

    let now = now_ms();
    let record =
        ProjectRecord { id: new_id(), name: root.name, root_path, created_at_ms: now, updated_at_ms: now };
    let draft = EventDraft::new(
        DomainEvent::ProjectRegistered {
            project_id: record.id,
            name: record.name.clone(),
            root_path: record.root_path.clone(),
        },
        EventContext { project_id: Some(record.id), ..Default::default() },
    )
    .map_err(|e| errors::internal("cannot encode event", e))?;

    let storage = state.storage.clone();
    let (record, _event) = state
        .bus
        .publish_with(move || {
            storage.with_conn(|c| {
                if let Some(existing) = projects::find_by_path(c, &record.root_path)? {
                    return Err(StorageError::Conflict(format!(
                        "{} is already registered as project {}",
                        existing.root_path, existing.id
                    )));
                }
                let event = projects::insert_with_event(c, &record, draft)?;
                Ok((record, event))
            })
        })
        .await
        .map_err(errors::publish)?
        .map_err(errors::storage)?;
    tracing::info!(project_id = %record.id, path = %record.root_path, "project registered");
    Ok(ProjectResult { project: to_project(record).await })
}

/// Removes the registration only. Files on disk are never touched.
pub(crate) async fn remove(
    state: &CoreState,
    params: ProjectIdParams,
) -> Result<ProjectsRemoveResult, ProtocolError> {
    let id = params.project_id;
    if let Some(session) = state.agents.has_attached_for_project(id) {
        return Err(ProtocolError::new(
            ErrorCode::Conflict,
            format!("project has a running agent session ({session}); stop it before removing the project"),
        ));
    }
    let storage = state.storage.clone();
    let (record, _event) = state
        .bus
        .publish_with(move || {
            storage.with_conn(|c| {
                projects::delete_with_event(c, id, |record| {
                    EventDraft::new(
                        DomainEvent::ProjectRemoved {
                            project_id: record.id,
                            root_path: record.root_path.clone(),
                        },
                        EventContext { project_id: Some(record.id), ..Default::default() },
                    )
                    .map_err(|e| StorageError::Corrupt(format!("cannot encode event: {e}")))
                })
            })
        })
        .await
        .map_err(errors::publish)?
        .map_err(errors::storage)?;
    tracing::info!(project_id = %record.id, path = %record.root_path, "project registration removed");
    Ok(ProjectsRemoveResult { project_id: record.id })
}

pub(crate) async fn git_status(
    state: &CoreState,
    params: ProjectIdParams,
) -> Result<GitStatusReport, ProtocolError> {
    let id = params.project_id;
    let record = state.storage.call(move |c| projects::get(c, id)).await.map_err(errors::storage)?;
    let status = state.git.status(Path::new(&record.root_path)).await.map_err(errors::git_registered)?;
    Ok(GitStatusReport {
        project_id: record.id,
        clean: status.is_clean(),
        branch: status.branch,
        head_oid: status.head_oid,
        detached: status.detached,
        upstream: status.upstream,
        ahead: status.ahead,
        behind: status.behind,
        staged: status.staged,
        unstaged: status.unstaged,
        untracked: status.untracked,
        conflicted: status.conflicted,
        checked_at_ms: now_ms(),
    })
}

//! Filesystem observation for projects with live agent sessions.
//!
//! What the filesystem reports is recorded as `files.changed`. That is a
//! separate fact from an agent's tool call (intent) and from `git status`
//! (repository state); nothing here infers changes from tool calls.
//!
//! Raw notifications are debounced into batches, `.git/` and Git-ignored
//! paths are dropped, and each path is classified by checking the disk when
//! the batch is flushed. Batches are bounded; overflow is counted, not kept.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use switchyard_events::{DomainEvent, EventBus, EventContext, FileChange};
use switchyard_git::Git;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Raw notification queue per project.
const RAW_QUEUE: usize = 4096;
/// Changes reported per `files.changed` event; the rest are counted.
const MAX_BATCH: usize = 200;
/// Upper bound for one batch window, even if changes keep arriving.
const MAX_WINDOW: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Created,
    Modified,
    Removed,
}

struct RawChange {
    path: PathBuf,
    seen: Seen,
    /// Rename source when the backend reports both ends.
    from: Option<PathBuf>,
}

struct ProjectWatch {
    _watcher: RecommendedWatcher,
    task: JoinHandle<()>,
    sessions: Vec<Uuid>,
    context: Arc<Mutex<EventContext>>,
}

/// Starts and stops watchers as sessions come and go (reference counted per
/// project).
#[derive(Clone)]
pub(crate) struct WatcherHub {
    bus: EventBus,
    git: Git,
    debounce: Duration,
    projects: Arc<Mutex<HashMap<Uuid, ProjectWatch>>>,
}

impl WatcherHub {
    pub fn new(bus: EventBus, git: Git, debounce: Duration) -> Self {
        Self { bus, git, debounce, projects: Arc::default() }
    }

    /// Begins (or joins) watching `root` for `session`.
    pub fn attach(&self, project_id: Uuid, root: &Path, session: Uuid, workspace_id: Option<Uuid>) {
        let Ok(mut projects) = self.projects.lock() else { return };
        if let Some(watch) = projects.get_mut(&project_id) {
            watch.sessions.push(session);
            if let Ok(mut ctx) = watch.context.lock() {
                ctx.session_id = Some(session);
            }
            return;
        }
        let (tx, rx) = mpsc::channel(RAW_QUEUE);
        let dropped = Arc::new(Mutex::new(0u32));
        let dropped_cb = Arc::clone(&dropped);
        let handler = move |res: notify::Result<notify::Event>| match res {
            Ok(event) => {
                for change in classify(&event) {
                    if tx.try_send(change).is_err()
                        && let Ok(mut d) = dropped_cb.lock()
                    {
                        *d = d.saturating_add(1);
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "filesystem watcher error"),
        };
        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(w) => w,
            Err(err) => {
                tracing::error!(%err, %project_id, "cannot create filesystem watcher");
                return;
            }
        };
        if let Err(err) = watcher.watch(root, RecursiveMode::Recursive) {
            tracing::error!(%err, %project_id, "cannot watch project directory");
            return;
        }
        let context = Arc::new(Mutex::new(EventContext {
            project_id: Some(project_id),
            workspace_id,
            session_id: Some(session),
            ..Default::default()
        }));
        let task = tokio::spawn(batch_loop(
            rx,
            root.to_path_buf(),
            self.bus.clone(),
            self.git.clone(),
            self.debounce,
            Arc::clone(&context),
            dropped,
        ));
        tracing::info!(%project_id, root = %root.display(), "watching project files");
        projects
            .insert(project_id, ProjectWatch { _watcher: watcher, task, sessions: vec![session], context });
    }

    /// Stops watching for `session`; the watcher ends with its last session.
    /// The pending batch is flushed first.
    pub fn detach(&self, project_id: Uuid, session: Uuid) {
        let Ok(mut projects) = self.projects.lock() else { return };
        let Some(watch) = projects.get_mut(&project_id) else { return };
        watch.sessions.retain(|s| *s != session);
        if watch.sessions.is_empty()
            && let Some(watch) = projects.remove(&project_id)
        {
            // Dropping the watcher closes the raw channel; the batch loop
            // flushes what it has and exits.
            let ProjectWatch { _watcher, task, .. } = watch;
            drop(_watcher);
            tokio::spawn(async move {
                if tokio::time::timeout(Duration::from_secs(5), task).await.is_err() {
                    tracing::warn!(%project_id, "file batch task did not finish");
                }
            });
        }
    }
}

fn classify(event: &notify::Event) -> Vec<RawChange> {
    let seen = match event.kind {
        EventKind::Create(_) => Seen::Created,
        EventKind::Remove(_) => Seen::Removed,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() == 2 => {
            return vec![RawChange {
                path: event.paths[1].clone(),
                seen: Seen::Created,
                from: Some(event.paths[0].clone()),
            }];
        }
        EventKind::Modify(ModifyKind::Metadata(_)) | EventKind::Access(_) => return Vec::new(),
        EventKind::Modify(_) | EventKind::Any | EventKind::Other => Seen::Modified,
    };
    event.paths.iter().map(|p| RawChange { path: p.clone(), seen, from: None }).collect()
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    if rel.as_os_str().is_empty() || rel.components().any(|c| c.as_os_str() == ".git") {
        return None;
    }
    rel.to_str().map(str::to_owned)
}

async fn batch_loop(
    mut rx: mpsc::Receiver<RawChange>,
    root: PathBuf,
    bus: EventBus,
    git: Git,
    debounce: Duration,
    context: Arc<Mutex<EventContext>>,
    dropped: Arc<Mutex<u32>>,
) {
    // macOS reports canonical paths (e.g. /private/var/...).
    let root = tokio::fs::canonicalize(&root).await.unwrap_or(root);
    loop {
        let Some(first) = rx.recv().await else { return };
        let mut batch: BTreeMap<String, (Seen, Option<String>)> = BTreeMap::new();
        let absorb = |change: RawChange, batch: &mut BTreeMap<String, (Seen, Option<String>)>| {
            let Some(path) = relative(&root, &change.path) else { return };
            let from = change.from.as_deref().and_then(|f| relative(&root, f));
            let entry = batch.entry(path).or_insert((change.seen, None));
            // Creation within the window wins over later modifications.
            if entry.0 != Seen::Created || change.seen == Seen::Removed {
                entry.0 = change.seen;
            }
            if from.is_some() {
                entry.1 = from;
            }
        };
        absorb(first, &mut batch);
        let window = tokio::time::Instant::now() + MAX_WINDOW;
        let mut closed = false;
        loop {
            let deadline = (tokio::time::Instant::now() + debounce).min(window);
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(change)) => absorb(change, &mut batch),
                Ok(None) => {
                    closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
        flush(&root, &bus, &git, &context, &dropped, batch).await;
        if closed {
            return;
        }
    }
}

async fn flush(
    root: &Path,
    bus: &EventBus,
    git: &Git,
    context: &Arc<Mutex<EventContext>>,
    dropped: &Arc<Mutex<u32>>,
    batch: BTreeMap<String, (Seen, Option<String>)>,
) {
    if batch.is_empty() {
        return;
    }
    let paths: Vec<String> = batch.keys().cloned().collect();
    let ignored = match git.ignored(root, &paths).await {
        Ok(set) => set,
        Err(err) => {
            tracing::debug!(%err, "git check-ignore failed; reporting all paths");
            Default::default()
        }
    };
    let mut changes = Vec::new();
    let mut overflow =
        std::mem::take(&mut *dropped.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    for (path, (seen, from)) in batch {
        if ignored.contains(&path) {
            continue;
        }
        let exists = tokio::fs::symlink_metadata(root.join(&path)).await.is_ok();
        let kind = match (exists, seen, &from) {
            (true, _, Some(_)) => "renamed",
            (true, Seen::Created, None) => "created",
            (true, _, None) => "modified",
            (false, Seen::Created, _) => continue, // created and removed within the window
            (false, _, _) => "deleted",
        };
        // Directories are noise next to the files inside them.
        if exists && tokio::fs::metadata(root.join(&path)).await.is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if changes.len() >= MAX_BATCH {
            overflow = overflow.saturating_add(1);
            continue;
        }
        changes.push(FileChange { path, kind: kind.to_owned(), from });
    }
    if changes.is_empty() && overflow == 0 {
        return;
    }
    let ctx = context.lock().map(|c| c.clone()).unwrap_or_default();
    if let Err(err) = bus.publish(DomainEvent::FilesChanged { changes, dropped: overflow }, ctx).await {
        tracing::error!(%err, "cannot record file changes");
    }
}

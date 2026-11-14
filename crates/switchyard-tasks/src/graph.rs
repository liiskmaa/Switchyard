//! Task dependencies (ADR 0024): an edge `(task, depends_on)` means `task`
//! may only start once `depends_on` is completed.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use crate::model::TaskStatus;

/// Whether adding `task → depends_on` would close a cycle (including a task
/// depending on itself), given the existing edges.
pub fn would_create_cycle<T: Copy + Eq + Hash>(edges: &[(T, T)], task: T, depends_on: T) -> bool {
    if task == depends_on {
        return true;
    }
    let mut next: HashMap<T, Vec<T>> = HashMap::new();
    for (from, to) in edges {
        next.entry(*from).or_default().push(*to);
    }
    // A cycle exists if `task` is reachable from `depends_on`.
    let mut stack = vec![depends_on];
    let mut seen = HashSet::new();
    while let Some(n) = stack.pop() {
        if n == task {
            return true;
        }
        if seen.insert(n) {
            stack.extend(next.get(&n).into_iter().flatten().copied());
        }
    }
    false
}

/// Whether a task's dependencies allow it to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness<T> {
    /// Every dependency is completed (or there are none).
    Satisfied,
    /// Some dependencies are still open.
    Waiting(Vec<T>),
    /// A dependency failed or was cancelled; the task cannot proceed until the
    /// operator retries it or removes the dependency.
    Broken(Vec<T>),
}

/// Readiness from the dependencies' current statuses.
pub fn readiness<T: Copy>(dependencies: &[(T, TaskStatus)]) -> Readiness<T> {
    let broken: Vec<T> = dependencies
        .iter()
        .filter(|(_, s)| matches!(s, TaskStatus::Failed | TaskStatus::Cancelled))
        .map(|(id, _)| *id)
        .collect();
    if !broken.is_empty() {
        return Readiness::Broken(broken);
    }
    let waiting: Vec<T> =
        dependencies.iter().filter(|(_, s)| !s.satisfies_dependents()).map(|(id, _)| *id).collect();
    if waiting.is_empty() { Readiness::Satisfied } else { Readiness::Waiting(waiting) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_are_detected() {
        let edges = [(1, 2), (2, 3)];
        assert!(would_create_cycle(&edges, 3, 1), "3 → 1 closes 1 → 2 → 3");
        assert!(would_create_cycle(&edges, 1, 1), "self dependency");
        assert!(!would_create_cycle(&edges, 1, 3), "a shortcut is not a cycle");
        assert!(!would_create_cycle(&edges, 4, 1));
        let diamond = [(4, 2), (4, 3), (2, 1), (3, 1)];
        assert!(!would_create_cycle(&diamond, 4, 1));
        assert!(would_create_cycle(&diamond, 1, 4));
    }

    #[test]
    fn readiness_follows_dependency_statuses() {
        use TaskStatus::*;
        assert_eq!(readiness::<u8>(&[]), Readiness::Satisfied);
        assert_eq!(readiness(&[(1, Completed), (2, Completed)]), Readiness::Satisfied);
        assert_eq!(readiness(&[(1, Completed), (2, AwaitingReview)]), Readiness::Waiting(vec![2]));
        assert_eq!(readiness(&[(1, Failed), (2, Ready)]), Readiness::Broken(vec![1]));
        assert_eq!(readiness(&[(1, Cancelled)]), Readiness::Broken(vec![1]));
    }
}

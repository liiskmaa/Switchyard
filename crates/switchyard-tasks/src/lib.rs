//! Task orchestration domain for Switchyard (milestone 6), without I/O.
//!
//! * [`model`]: task, assignment, message, finding and proposal vocabulary,
//!   and the validated task state machine.
//! * [`graph`]: dependency cycle detection and readiness.
//! * [`report`]: the `switchyard-report` block an agent ends its turn with,
//!   parsed and validated. A report is a claim or a request, never an
//!   authorization.
//! * [`context`]: the versioned task context Core gives an agent.
//! * [`workflow`]: the built-in `engineering@1` workflow as a pure decision function.
//! * [`organization`]: dynamic agent organizations (`organization@1`) as a pure decision function.
//! * [`supervisor`]: the task supervisor's prompt and reply parser (ADR 0041).
//!
//! Nothing here knows about Claude, Codex or any wire protocol.

pub mod context;
pub mod graph;
pub mod model;
pub mod organization;
pub mod report;
pub mod supervisor;
pub mod workflow;

pub use context::{CONTEXT_VERSION, ContextDependency, ContextFinding, TaskContext, render};
pub use graph::{Readiness, readiness, would_create_cycle};
pub use model::{
    AssignmentStatus, MessageKind, Priority, ProposalStatus, ReviewResult, Role, Severity, TaskKind,
    TaskStatus, TransitionError, UnknownValue,
};
pub use report::{
    AcceptanceCheck, MAX_REPORT_BYTES, ProposedSubtask, Report, ReportError, ReportFinding, ReportKind,
    SubtaskDependency, parse_report,
};

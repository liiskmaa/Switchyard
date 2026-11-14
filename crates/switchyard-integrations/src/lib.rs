//! Change integration (Milestone 7, ADRs 0028–0031), without I/O.
//!
//! An *integration* is the proposed delivery of one workspace's changes into
//! a target branch. It is bound to an exact source commit and an exact target
//! commit; Core gathers [`Facts`] about the repository from Git, and
//! [`blockers`] decides what still prevents integration. Only the operator can
//! approve, and an approval names both commits.

mod model;
mod preflight;
mod validation;

pub use model::{IntegrationStatus, TransitionError, UnknownValue, merge_message};
pub use preflight::{Blocker, Facts, MergeFact, ReviewFact, TargetCheckoutFact, blockers};
pub use validation::{
    MAX_OUTPUT_BYTES, RunStatus, StepStatus, ValidationCommand, ValidationError, command_line,
    validate_command,
};

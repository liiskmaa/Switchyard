//! Approval policies: the vocabulary of normalized agent operations, the
//! rule model, rule validation and the deterministic evaluator.
//!
//! This crate performs no I/O. Core establishes the request's scope and path
//! facts, loads the rules and records the result (ADRs 0016–0018).
//!
//! Policies are an authorization and workflow mechanism, **not** a sandbox:
//! an allowed command runs with the user's privileges (ADR 0019).

mod evaluate;
mod model;
mod validate;

pub use evaluate::{Evaluation, ReasonCode, Restriction, RuleRef, evaluate, system_restrictions};
pub use model::{
    Conditions, Decision, EvaluationInput, Operation, OperationCategory, PathFact, RequestScope, Rule,
    RuleScope, SYSTEM_POLICY_VERSION, UnknownValue,
};
pub use validate::{
    MAX_COMMAND_BYTES, MAX_DESCRIPTION_CHARS, MAX_PATH_PREFIX_BYTES, RuleError, normalize_path_prefix,
    same_match, validate_command, validate_path_prefix, validate_rule,
};

//! Provider and model capability profiles, and capability-aware selection
//! (M8.3, ADR 0035).
//!
//! A profile says what a provider/model can do, and *how Switchyard knows*:
//!
//! * **verified**: runtime behaviour Switchyard verified against the installed
//!   CLI (the adapter's [`Capabilities`](crate::Capabilities): permission
//!   routing, resumption, interruption, steering, streaming, tool activity);
//! * **reported**: what the CLI itself reports (its model list and default);
//! * **configured**: the user's estimates of engineering skills (coding,
//!   review, documentation, …), cost and speed.
//!
//! Switchyard ships **no skill estimates**: without the user's configuration
//! every model is "unknown" for every skill, and selection says so instead of
//! inventing a ranking. No provider is assumed to be a supervisor or an
//! executor. Adding a provider means adding an adapter; this module only sees
//! [`AgentProvider`], its [`Capabilities`](crate::Capabilities) and its
//! [`ModelCatalog`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{AgentProvider, Capabilities, Support};
use crate::models::ModelCatalog;

/// Engineering skills a supervisor can ask for.
pub const SKILLS: &[&str] =
    &["reasoning", "coding", "research", "review", "documentation", "testing", "long-context"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Unknown,
    Unsupported,
    Basic,
    Good,
    Strong,
}

impl Level {
    fn score(self) -> i32 {
        match self {
            Self::Strong => 3,
            Self::Good => 2,
            Self::Basic => 1,
            Self::Unknown => 0,
            Self::Unsupported => -10,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Unsupported => "unsupported",
            Self::Basic => "basic",
            Self::Good => "good",
            Self::Strong => "strong",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Verified,
    Reported,
    Configured,
    /// Nothing is known.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub level: Level,
    pub basis: Basis,
}

/// The user's estimate for a provider (all its models) or one model.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Estimate {
    pub provider: String,
    /// `None`: every model of the provider (a model-specific estimate wins).
    pub model: Option<String>,
    pub skills: BTreeMap<String, Level>,
    /// 1 (cheapest) … 5.
    pub relative_cost: Option<u8>,
    /// 1 (slowest) … 5 (fastest).
    pub relative_speed: Option<u8>,
}

/// The user's fixed choice for agents that need a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    pub skill: String,
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub provider: AgentProvider,
    /// `None`: the CLI's default model.
    pub model: Option<String>,
    pub display: String,
    pub runtime: BTreeMap<String, Fact>,
    pub skills: BTreeMap<String, Fact>,
    pub relative_cost: Option<u8>,
    pub relative_speed: Option<u8>,
}

fn support(s: Support) -> Fact {
    match s {
        Support::Supported => Fact { level: Level::Good, basis: Basis::Verified },
        Support::Unsupported => Fact { level: Level::Unsupported, basis: Basis::Verified },
        Support::Unknown => Fact { level: Level::Unknown, basis: Basis::None },
    }
}

fn runtime_facts(c: &Capabilities) -> BTreeMap<String, Fact> {
    [
        ("permission_routing", c.permission_requests),
        ("resume", c.resume_session),
        ("interrupt", c.interrupt_turn),
        ("queue_message", c.queue_message_during_turn),
        ("steer_running_turn", c.steer_running_turn),
        ("streaming", c.streaming_text),
        ("tool_activity", c.tool_activity),
        ("usage", c.usage_tokens),
        ("cost", c.cost),
    ]
    .into_iter()
    .map(|(k, s)| (k.to_owned(), support(s)))
    .collect()
}

/// Profiles of every model the installed CLIs offer (the CLI default
/// included), with the user's estimates applied.
pub fn profiles(catalogs: &[ModelCatalog], estimates: &[Estimate]) -> Vec<Profile> {
    let mut out = Vec::new();
    for cat in catalogs {
        let caps = crate::capabilities(cat.provider);
        let mut models: Vec<(Option<String>, String)> = vec![(None, "CLI default".into())];
        for m in &cat.models {
            models.push((Some(m.id.clone()), m.display_name.clone()));
        }
        for (model, label) in models {
            let est = |pick_model: bool| {
                estimates.iter().find(|e| {
                    e.provider == cat.provider.as_str()
                        && if pick_model { e.model.is_some() && e.model == model } else { e.model.is_none() }
                })
            };
            let (specific, general) = (est(true), est(false));
            let mut skills = BTreeMap::new();
            for s in SKILLS {
                let level = specific
                    .and_then(|e| e.skills.get(*s))
                    .or_else(|| general.and_then(|e| e.skills.get(*s)))
                    .copied();
                skills.insert(
                    (*s).to_owned(),
                    match level {
                        Some(l) => Fact { level: l, basis: Basis::Configured },
                        None => Fact { level: Level::Unknown, basis: Basis::None },
                    },
                );
            }
            out.push(Profile {
                provider: cat.provider,
                model: model.clone(),
                display: format!("{} · {label}", crate::display_name(cat.provider)),
                runtime: runtime_facts(&caps),
                skills,
                relative_cost: specific
                    .and_then(|e| e.relative_cost)
                    .or(general.and_then(|e| e.relative_cost)),
                relative_speed: specific
                    .and_then(|e| e.relative_speed)
                    .or(general.and_then(|e| e.relative_speed)),
            });
        }
    }
    out
}

impl Profile {
    /// One line for a supervisor's prompt.
    pub fn describe(&self) -> String {
        let skills: Vec<String> = self
            .skills
            .iter()
            .filter(|(_, f)| f.basis == Basis::Configured)
            .map(|(k, f)| format!("{k} {}", f.level.as_str()))
            .collect();
        let runtime: Vec<&str> = self
            .runtime
            .iter()
            .filter(|(_, f)| f.basis == Basis::Verified && f.level != Level::Unsupported)
            .map(|(k, _)| k.as_str())
            .collect();
        format!(
            "provider {} model {} — verified: {}; skills (user's estimates): {}{}",
            self.provider.as_str(),
            self.model.as_deref().unwrap_or("(CLI default)"),
            runtime.join(", "),
            if skills.is_empty() { "none configured".to_owned() } else { skills.join(", ") },
            self.relative_cost.map(|c| format!("; relative cost {c}/5")).unwrap_or_default(),
        )
    }

    fn runtime_ok(&self, key: &str) -> bool {
        self.runtime.get(key).is_some_and(|f| f.basis == Basis::Verified && f.level != Level::Unsupported)
    }
}

/// What an agent needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Requirement {
    pub skills: Vec<String>,
    /// A supervisor is resumed at every checkpoint.
    pub supervisor: bool,
    /// The supervisor's preference (validated, never trusted).
    pub requested_provider: Option<String>,
    pub requested_model: Option<String>,
    /// Prefer a different provider than this one (independent review).
    pub independent_of: Option<AgentProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub provider: AgentProvider,
    pub model: Option<String>,
    pub rationale: String,
}

fn eligible(p: &Profile, r: &Requirement) -> Result<(), String> {
    if !p.runtime_ok("permission_routing") {
        return Err(format!("{} does not route permission requests to Switchyard", p.display));
    }
    if r.supervisor && !p.runtime_ok("resume") {
        return Err(format!("{} cannot resume a session, which a supervisor needs", p.display));
    }
    if let Some(s) = r.skills.iter().find(|s| p.skills.get(*s).is_some_and(|f| f.level == Level::Unsupported))
    {
        return Err(format!("{} is configured as unsuitable for {s}", p.display));
    }
    Ok(())
}

/// Chooses a provider and model for an agent, and says why.
///
/// Order: the user's override for a required skill; else the supervisor's
/// request (if available and eligible); else the best score on the user's
/// estimates for the required skills, preferring independence and lower
/// cost; ties keep the catalog order. Ineligible profiles are never chosen.
pub fn select(profiles: &[Profile], r: &Requirement, overrides: &[Override]) -> Result<Choice, String> {
    let ok: Vec<&Profile> = profiles.iter().filter(|p| eligible(p, r).is_ok()).collect();
    if ok.is_empty() {
        let why: Vec<String> = profiles.iter().filter_map(|p| eligible(p, r).err()).take(3).collect();
        return Err(format!("no installed provider can take this role ({})", why.join("; ")));
    }
    let find = |provider: &str, model: Option<&str>| {
        ok.iter().find(|p| p.provider.as_str() == provider && p.model.as_deref() == model).copied()
    };
    for o in overrides {
        if r.skills.contains(&o.skill) {
            return match find(&o.provider, o.model.as_deref()) {
                Some(p) => Ok(Choice {
                    provider: p.provider,
                    model: p.model.clone(),
                    rationale: format!("your override for {}: {}", o.skill, p.display),
                }),
                None => Err(format!(
                    "your override for {} ({} {}) is not available or cannot take this role",
                    o.skill,
                    o.provider,
                    o.model.as_deref().unwrap_or("CLI default")
                )),
            };
        }
    }
    if let Some(provider) = &r.requested_provider {
        let model = r.requested_model.as_deref();
        match find(provider, model) {
            Some(p) => {
                return Ok(Choice {
                    provider: p.provider,
                    model: p.model.clone(),
                    rationale: format!("requested by its supervisor; available and eligible ({})", p.display),
                });
            }
            None => {
                return Err(format!(
                    "the requested {provider} {} is not installed, not offered by its CLI, or cannot take this role",
                    model.unwrap_or("(CLI default)")
                ));
            }
        }
    }
    let score = |p: &Profile| -> (i32, i32, i32) {
        let skill: i32 = r.skills.iter().map(|s| p.skills.get(s).map_or(0, |f| f.level.score())).sum();
        let independent = i32::from(r.independent_of.is_some_and(|x| x != p.provider));
        let cheap = -i32::from(p.relative_cost.unwrap_or(3));
        (skill, independent, cheap)
    };
    let best = ok
        .iter()
        .enumerate()
        .max_by(|(ia, a), (ib, b)| score(a).cmp(&score(b)).then(ib.cmp(ia)))
        .map(|(_, p)| *p)
        .ok_or("no candidate")?;
    let (skill, independent, _) = score(best);
    let known: Vec<String> = r
        .skills
        .iter()
        .filter_map(|s| {
            best.skills
                .get(s)
                .filter(|f| f.basis == Basis::Configured)
                .map(|f| format!("{s} {}", f.level.as_str()))
        })
        .collect();
    let mut why = if known.is_empty() {
        format!(
            "no skill estimates are configured for {}; chose {} as the first eligible model (verified permission routing{})",
            if r.skills.is_empty() { "this role".to_owned() } else { r.skills.join(", ") },
            best.display,
            if r.supervisor { " and resumption" } else { "" }
        )
    } else {
        format!("best match on your estimates ({}, score {skill}): {}", known.join(", "), best.display)
    };
    if independent == 1 {
        why.push_str("; a different provider than the work it checks");
    }
    Ok(Choice { provider: best.provider, model: best.model.clone(), rationale: why })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ModelOption;

    fn catalogs() -> Vec<ModelCatalog> {
        vec![
            ModelCatalog {
                provider: AgentProvider::ClaudeCode,
                models: vec![ModelOption {
                    id: "opus".into(),
                    display_name: "opus".into(),
                    is_default: false,
                }],
                accepts_full_names: true,
                source: "test".into(),
                error: None,
            },
            ModelCatalog {
                provider: AgentProvider::Codex,
                models: vec![ModelOption {
                    id: "gpt-x".into(),
                    display_name: "GPT X".into(),
                    is_default: true,
                }],
                accepts_full_names: false,
                source: "test".into(),
                error: None,
            },
        ]
    }

    fn req(skills: &[&str]) -> Requirement {
        Requirement { skills: skills.iter().map(|s| (*s).to_owned()).collect(), ..Default::default() }
    }

    #[test]
    fn without_estimates_nothing_is_invented() {
        let p = profiles(&catalogs(), &[]);
        assert_eq!(p.len(), 4, "the CLI default and each listed model");
        assert!(p.iter().all(|p| p.skills.values().all(|f| f.basis == Basis::None)));
        assert!(p.iter().all(|p| p.runtime["permission_routing"].basis == Basis::Verified));
        let c = select(&p, &req(&["coding"]), &[]).unwrap();
        assert!(c.rationale.contains("no skill estimates"), "{}", c.rationale);
    }

    #[test]
    fn estimates_overrides_requests_and_independence() {
        let est = vec![
            Estimate {
                provider: "codex".into(),
                skills: [("coding".into(), Level::Strong)].into(),
                ..Default::default()
            },
            Estimate {
                provider: "claude_code".into(),
                model: Some("opus".into()),
                skills: [("review".into(), Level::Strong), ("coding".into(), Level::Good)].into(),
                ..Default::default()
            },
        ];
        let p = profiles(&catalogs(), &est);
        let c = select(&p, &req(&["coding"]), &[]).unwrap();
        assert_eq!(c.provider, AgentProvider::Codex);
        assert!(c.rationale.contains("coding strong"));
        let c = select(&p, &req(&["review"]), &[]).unwrap();
        assert_eq!((c.provider, c.model.as_deref()), (AgentProvider::ClaudeCode, Some("opus")));
        // Independence breaks a tie.
        let mut r = req(&[]);
        r.independent_of = Some(AgentProvider::ClaudeCode);
        assert_eq!(select(&p, &r, &[]).unwrap().provider, AgentProvider::Codex);
        // A request is honoured only when available.
        let mut r = req(&["coding"]);
        r.requested_provider = Some("claude_code".into());
        assert_eq!(select(&p, &r, &[]).unwrap().provider, AgentProvider::ClaudeCode);
        r.requested_model = Some("nonexistent".into());
        assert!(select(&p, &r, &[]).is_err());
        r.requested_provider = Some("gemini".into());
        assert!(select(&p, &r, &[]).unwrap_err().contains("not installed"));
        // The user's override wins.
        let o = vec![Override { skill: "coding".into(), provider: "claude_code".into(), model: None }];
        let c = select(&p, &req(&["coding"]), &o).unwrap();
        assert_eq!((c.provider, c.model), (AgentProvider::ClaudeCode, None));
        assert!(c.rationale.contains("override"));
        // A model configured as unsuitable is never chosen.
        let bad = vec![Estimate {
            provider: "codex".into(),
            skills: [("review".into(), Level::Unsupported)].into(),
            ..Default::default()
        }];
        let p = profiles(&catalogs(), &bad);
        let c = select(&p, &req(&["review"]), &[]).unwrap();
        assert_eq!(c.provider, AgentProvider::ClaudeCode);
        assert!(p[0].describe().contains("verified: "));
    }
}

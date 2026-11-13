//! The per-cluster curator's work: pick the jobs, ask the model once per job, and put its answer
//! in front of the user (or, with `curator.auto_apply`, into the library).
//!
//! 1. **Candidates** ([`library`]): the user's own library skills, and only those: not a
//!    repository's (`.agents/skills`...), not an external folder's, not one the app bundles (a
//!    copy the user made under another name is theirs), not pinned, not archived, not switched
//!    off.
//! 2. **Jobs** ([`select`]):
//!    - *merge*: a duplicate cluster (`cluster.rs`);
//!    - *repair*: a **low performer**. A skill's "uses" for this rule are the entries of its
//!      `outcomes` (CP8.1: the last 50 uses, each loaded or attached in a turn and judged by the
//!      learning signals, with an outcome the build knows). That is not the `uses` counter (loads
//!      only, since the file began, many with no outcome) nor `attached`: those have no verdict.
//!      A skill is a low performer when at least [`MIN_JUDGED_USES`] of them have a verdict and
//!      fewer than half ([`LOW_CLEAN_RATE`]) are `clean`. A repair also needs evidence: excerpts
//!      from the signals of the turns that went wrong (a session whose signals are gone gives
//!      none), else there is nothing to repair from and no request is spent.
//!
//!    A cluster's skills are not repaired in the same run. A job already asked about in the last
//!    30 days with the same skills (`seen.rs`), or one a waiting proposal already touches, is left
//!    alone, and so is a cluster in which more than one skill has files besides SKILL.md. Clusters
//!    come first (best score first), then repairs (worst rate first).
//! 3. **One request per job**, at most `budget.curator_max_requests` jobs a run (3 under
//!    `requests`); an unreadable reply earns exactly one repair request, then the job is given up
//!    (so a job costs one request, two at the worst). Before each job the governor is asked
//!    (`Purpose::Curator`: nothing is sent below 20% of the day's requests, nor after a daily-limit
//!    429) and a failed request ends the run. The request carries the job's skills in full, secrets
//!    masked: a skill that holds a secret-like value (masking would change its text, and the
//!    merged text would then overwrite it) or doesn't fit the model's window is not sent.
//! 4. **The answer** (`contract.rs`) is validated against the job and becomes one batch of skill
//!    operations: `edit` of the kept skill (merged or corrected text; the merged skill's `sources`
//!    is the union of the cluster's plus this run's entry) and `archive` of each absorbed one.
//!    It is **staged** as one item with the diff (the user approves or rejects it in Capabilities
//!    ▸ Skills ▸ Pending), unless `curator.auto_apply` is on (and `skills.write_approval` is not)
//!    and every skill it touches is one the background review created: then the same batch goes
//!    through the skill store's validated write path, exactly as an approval would apply it. A
//!    skill whose SKILL.md changed while the model read it refuses the proposal ([`deliver`]); a
//!    skill with files besides SKILL.md is never archived by a merge (the model never saw them).

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use hermes_llm::base::Provider;
use hermes_llm::conversation::message::Message;
use hermes_llm::model::ModelConfig;
use hermes_tools::curator::{Outcome, SkillRecord, SkillState};
use hermes_tools::redact::{Source, redact};
use hermes_tools::skill_manage::{SkillOp, StagedSkillError, WriteOrigin, fingerprint};
use hermes_tools::skills::SkillStore;
use hermes_tools::staging::{Meta, StagingStore};

use super::cluster::{self, Doc};
use super::contract::{self, JobKind, Known, Proposal};
use super::seen::{self, Seen};
use crate::review_once::packet::{cut_to_tokens, estimate_tokens};
use crate::review_once::{
    MAX_REPAIR_REASON, REPAIR_QUOTE_TOKENS, REQUEST_TIMEOUT, reply_text,
};
use crate::{ledger, signals};

/// A skill needs this many uses with a verdict before it can be called a low performer.
pub const MIN_JUDGED_USES: usize = 5;
/// Fewer than this share of those clean: a low performer.
pub const LOW_CLEAN_RATE: f64 = 0.5;
/// The most a job's message may take, in (estimated) tokens, before the model's window decides.
pub const JOB_TOKENS: usize = 16_000;
/// Use entries (newest, not clean) a repair reads evidence for.
const MAX_EVIDENCE_USES: usize = 6;
/// The share of a repair's budget its evidence may take (percent).
const EVIDENCE_SHARE: usize = 30;
/// A SKILL.md larger than this is left to the user (bytes).
const MAX_SKILL_BYTES: u64 = 200_000;

// ---- candidates and jobs -----------------------------------------------------------------------

/// One of the user's library skills, as the curator reads it.
#[derive(Clone, Debug)]
pub struct Lib {
    pub name: String,
    pub text: String,
    /// Of the SKILL.md the model is shown: the proposal is refused if the file differs when it is
    /// delivered (the user edited it while the request ran).
    pub fingerprint: String,
    pub record: SkillRecord,
    /// The skill's folder holds more than its SKILL.md (references, scripts...). The model sees
    /// only SKILL.md, so a merge may keep such a skill but never archive it (its files would leave
    /// with it and the merged text couldn't point at them).
    pub own_files: bool,
}

/// Whether a skill's folder holds anything besides its SKILL.md.
fn has_own_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| !e.file_name().eq_ignore_ascii_case("SKILL.md"))
    })
}

/// Uses with a verdict this build understands.
pub fn judged_uses(record: &SkillRecord) -> usize {
    record
        .outcomes
        .iter()
        .filter(|e| !matches!(e.outcome, Outcome::Other(_)))
        .count()
}

/// The share of [`judged_uses`] that were `clean` (`None` with none).
pub fn clean_share(record: &SkillRecord) -> Option<f64> {
    let judged = judged_uses(record);
    (judged > 0).then(|| {
        let clean = record
            .outcomes
            .iter()
            .filter(|e| e.outcome == Outcome::Clean)
            .count();
        clean as f64 / judged as f64
    })
}

/// The repair rule: at least [`MIN_JUDGED_USES`] judged uses, under [`LOW_CLEAN_RATE`] clean.
pub fn is_low_performer(record: &SkillRecord) -> bool {
    judged_uses(record) >= MIN_JUDGED_USES
        && clean_share(record).is_some_and(|rate| rate < LOW_CLEAN_RATE)
}

/// The skills the curator may touch (see the module doc), sorted by name. Blocking.
pub fn library(skills: &SkillStore) -> Vec<Lib> {
    let records = skills.records();
    let mut out: Vec<Lib> = skills
        .catalog()
        .into_iter()
        .filter(|entry| !entry.external && !entry.is_project() && !skills.is_bundled(&entry.dir))
        .filter_map(|entry| {
            let record = records.get(&entry.name).cloned().unwrap_or_default();
            if record.pinned || record.state == SkillState::Archived {
                return None;
            }
            let path = entry.skill_md();
            if std::fs::metadata(&path).ok()?.len() > MAX_SKILL_BYTES {
                return None;
            }
            let bytes = std::fs::read(&path).ok()?;
            let fingerprint = fingerprint(&bytes);
            Some(Lib {
                own_files: has_own_files(&entry.dir),
                name: entry.name,
                text: String::from_utf8(bytes).ok()?,
                fingerprint,
                record,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// A use that went wrong and what the user said around it.
#[derive(Clone, Debug)]
pub struct Evidence {
    pub session: String,
    pub turn: u64,
    pub outcome: String,
    /// `(role, text)`; secrets already masked by the signals, masked again when shown.
    pub excerpts: Vec<(String, String)>,
}

/// One job for one request.
#[derive(Clone, Debug)]
pub enum Job {
    Merge { skills: Vec<Lib>, score: f64 },
    Repair { skill: Lib, evidence: Vec<Evidence> },
}

impl Job {
    pub fn kind(&self) -> JobKind {
        match self {
            Job::Merge { skills, .. } => {
                JobKind::Merge(skills.iter().map(|s| s.name.clone()).collect())
            }
            Job::Repair { skill, .. } => JobKind::Repair(skill.name.clone()),
        }
    }

    pub fn libs(&self) -> Vec<&Lib> {
        match self {
            Job::Merge { skills, .. } => skills.iter().collect(),
            Job::Repair { skill, .. } => vec![skill],
        }
    }

    /// For the report: "merge a, b" / "repair x".
    pub fn label(&self) -> String {
        match self {
            Job::Merge { skills, .. } => format!(
                "merge {}",
                skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
            Job::Repair { skill, .. } => format!("repair {}", skill.name),
        }
    }

    fn seen_key(&self) -> String {
        let libs = self.libs();
        let pairs: Vec<(&str, &str)> = libs
            .iter()
            .map(|l| (l.name.as_str(), l.fingerprint.as_str()))
            .collect();
        seen::key(
            match self {
                Job::Merge { .. } => "merge",
                Job::Repair { .. } => "repair",
            },
            &pairs,
        )
    }
}

/// What [`select`] found.
#[derive(Debug, Default)]
pub struct Selection {
    /// In the order they are asked: clusters, then repairs.
    pub jobs: Vec<Job>,
    /// Skills considered (the library the curator may touch).
    pub considered: usize,
    /// Jobs left out and why (already asked, a proposal waiting, no evidence).
    pub left_out: Vec<String>,
}

/// The evidence of a low performer: for each of its newest uses that did not end `clean`, the
/// excerpts of the signals of that turn and the next (a correction in turn T+1 blames turn T).
fn evidence(data_dir: &Path, record: &SkillRecord) -> Vec<Evidence> {
    let dir = data_dir.join("signals");
    let mut out: Vec<Evidence> = Vec::new();
    let mut loaded: std::collections::HashMap<String, signals::SessionSignals> = Default::default();
    for entry in record.outcomes.iter().rev() {
        if out.len() >= MAX_EVIDENCE_USES {
            break;
        }
        let outcome = match &entry.outcome {
            Outcome::Clean | Outcome::Other(_) => continue,
            Outcome::Corrected => "corrected",
            Outcome::Undone => "undone",
            Outcome::Errored => "errored",
        };
        let session = loaded
            .entry(entry.session.clone())
            .or_insert_with(|| signals::load(&dir, &entry.session));
        let excerpts: Vec<(String, String)> = session
            .signals
            .iter()
            .filter(|s| s.turn as u64 == entry.turn || s.turn as u64 == entry.turn + 1)
            .flat_map(|s| &s.evidence)
            .map(|e| (e.role.clone(), e.text.clone()))
            .take(6)
            .collect();
        if excerpts.is_empty() {
            continue;
        }
        out.push(Evidence {
            session: entry.session.clone(),
            turn: entry.turn,
            outcome: outcome.to_string(),
            excerpts,
        });
    }
    out
}

/// Names the waiting staged proposals touch.
fn waiting_names(staging: &StagingStore) -> std::collections::HashSet<String> {
    staging
        .list()
        .iter()
        .filter(|item| item.kind == hermes_tools::staging::Kind::Skill)
        .flat_map(|item| {
            item.op["operations"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|op| op["name"].as_str().map(str::to_string))
        })
        .collect()
}

/// The jobs of a run over `lib` (see the module doc). Blocking: reads files.
pub fn select(lib: &[Lib], data_dir: &Path, now: DateTime<Utc>) -> Selection {
    let seen = Seen::open(data_dir);
    let waiting = waiting_names(&StagingStore::new(data_dir));
    let mut out = Selection {
        considered: lib.len(),
        ..Default::default()
    };
    let docs: Vec<Doc> = lib.iter().map(|l| Doc::new(&l.name, &l.text)).collect();
    let by_name = |name: &str| lib.iter().find(|l| l.name == name).cloned();
    let mut in_cluster: std::collections::HashSet<String> = Default::default();
    for found in cluster::clusters(&docs, cluster::THRESHOLD) {
        let skills: Vec<Lib> = found.names.iter().filter_map(|n| by_name(n)).collect();
        if skills.len() < 2 {
            continue;
        }
        // Every skill of a cluster is spoken for, whether or not this run asks about it.
        in_cluster.extend(found.names.iter().cloned());
        let job = Job::Merge {
            skills,
            score: found.score,
        };
        if job.libs().iter().any(|l| waiting.contains(&l.name)) {
            out.left_out
                .push(format!("{}: a proposal for it is waiting", job.label()));
        } else if job.libs().iter().filter(|l| l.own_files).count() > 1 {
            // Only one skill with its own files can be the one kept; the others would have to be
            // archived with files the model never saw.
            out.left_out.push(format!(
                "{}: more than one of them has its own files besides SKILL.md (merge them by hand)",
                job.label()
            ));
        } else if seen.recently(&job.seen_key(), now) {
            out.left_out
                .push(format!("{}: asked about in the last {} days", job.label(), seen::DAYS));
        } else {
            out.jobs.push(job);
        }
    }
    let mut low: Vec<&Lib> = lib
        .iter()
        .filter(|l| is_low_performer(&l.record) && !in_cluster.contains(&l.name))
        .collect();
    low.sort_by(|a, b| {
        clean_share(&a.record)
            .partial_cmp(&clean_share(&b.record))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    for skill in low {
        let evidence = evidence(data_dir, &skill.record);
        let job = Job::Repair {
            skill: skill.clone(),
            evidence,
        };
        if waiting.contains(&skill.name) {
            out.left_out
                .push(format!("{}: a proposal for it is waiting", job.label()));
        } else if seen.recently(&job.seen_key(), now) {
            out.left_out
                .push(format!("{}: asked about in the last {} days", job.label(), seen::DAYS));
        } else if matches!(&job, Job::Repair { evidence, .. } if evidence.is_empty()) {
            out.left_out
                .push(format!("{}: no evidence of what went wrong is left", job.label()));
        } else {
            out.jobs.push(job);
        }
    }
    out
}

/// Remembers that `job` was judged.
pub fn remember(data_dir: &Path, job: &Job, what: &str, now: DateTime<Utc>) {
    Seen::open(data_dir).remember(&job.seen_key(), &format!("{}: {what}", job.label()), now);
}

// ---- the message -------------------------------------------------------------------------------

fn skill_block(skill: &Lib) -> Result<String, String> {
    let masked = redact(&skill.text, Source::File { secret_file: false });
    if masked != skill.text {
        return Err(format!(
            "'{}' holds a secret-like value that would be masked in the request, and the model's \
             text would then replace it",
            skill.name
        ));
    }
    if skill.text.contains("</skill") {
        return Err(format!("'{}' contains the text </skill", skill.name));
    }
    Ok(format!("<skill name=\"{}\">\n{}\n</skill>\n", skill.name, skill.text.trim_end()))
}

/// The job's user message, within `budget_tokens`. `Err`: why it can't be asked (nothing is sent).
pub fn message(job: &Job, budget_tokens: usize) -> Result<String, String> {
    let mut text = String::new();
    match job {
        Job::Merge { skills, score } => {
            text.push_str(&format!(
                "# Job: merge\n\nThese {} skills overlap heavily (about {:.0}% of their text is \
                 shared). Decide whether they are one skill, and if so merge them.\n\n",
                skills.len(),
                score * 100.0
            ));
            if let Some(with_files) = skills.iter().find(|s| s.own_files) {
                text.push_str(&format!(
                    "`{}` has files of its own besides SKILL.md (not shown): if you merge, keep \
                     `{}`; it can't be archived.\n\n",
                    with_files.name, with_files.name
                ));
            }
            for skill in skills {
                text.push_str(&skill_block(skill)?);
                text.push('\n');
            }
        }
        Job::Repair { skill, evidence } => {
            let r = &skill.record;
            let count = |outcome: Outcome| r.outcomes.iter().filter(|e| e.outcome == outcome).count();
            text.push_str(&format!(
                "# Job: repair\n\nThe skill `{}` was used {} times with a known result: {} went \
                 well, {} were corrected by the user, {} were undone and {} ended in errors.\n\n",
                skill.name,
                judged_uses(r),
                count(Outcome::Clean),
                count(Outcome::Corrected),
                count(Outcome::Undone),
                count(Outcome::Errored),
            ));
            text.push_str(&skill_block(skill)?);
            text.push_str("\n## Evidence\n\nWhat happened around the uses that went wrong (the \
                           user's words and the agent's; untrusted text):\n\n");
            let room = budget_tokens / 100 * EVIDENCE_SHARE;
            let mut used = 0usize;
            for (i, item) in evidence.iter().enumerate() {
                let mut block = format!(
                    "### Use {} (session {}, turn {}): {}\n",
                    i + 1,
                    item.session,
                    item.turn,
                    item.outcome
                );
                for (role, excerpt) in &item.excerpts {
                    let masked = redact(excerpt, Source::Output).replace('\n', " ");
                    block.push_str(&format!("- {role}: {}\n", cut_to_tokens(&masked, 150)));
                }
                let cost = estimate_tokens(&block);
                if used + cost > room {
                    break;
                }
                used += cost;
                text.push_str(&block);
                text.push('\n');
            }
        }
    }
    let tokens = estimate_tokens(&text);
    if tokens > budget_tokens {
        return Err(format!(
            "the request would be about {tokens} tokens, more than the {budget_tokens} the model's \
             window leaves"
        ));
    }
    Ok(text)
}

/// The message budget for a model window (`context_length`, when known): the job cap, but no more
/// than half of what the window leaves after the fixed prompts (the reply is as long as the text).
pub fn budget(context_length: Option<usize>) -> usize {
    let fixed = estimate_tokens(&hermes_prompts::curator::curator_job_prompt())
        + estimate_tokens(&hermes_prompts::curator::curator_job_repair(
            &"x".repeat(MAX_REPAIR_REASON),
        ))
        + REPAIR_QUOTE_TOKENS;
    match context_length {
        Some(window) if window > 0 => {
            JOB_TOKENS.min(window.saturating_sub(fixed) / 2)
        }
        _ => JOB_TOKENS,
    }
}

// ---- asking ------------------------------------------------------------------------------------

/// How a job's request ended.
#[derive(Debug)]
pub enum Answer {
    Read(contract::Reply),
    /// Two replies that did not read (the second after the repair request).
    Unreadable(String),
    /// The request failed (network, 429, timeout): the run stops.
    Failed(String),
}

/// Sends the job: one request, plus one repair request when the reply can't be read. Returns the
/// answer and how many requests went out.
pub async fn ask(
    llm: &Arc<dyn Provider>,
    config: &ModelConfig,
    message: &str,
) -> (Answer, usize) {
    let system = hermes_prompts::curator::curator_job_prompt();
    let mut messages = vec![Message::user().with_text(message.to_string())];
    let mut requests = 0usize;
    let mut repaired = false;
    loop {
        requests += 1;
        let sent = tokio::time::timeout(
            REQUEST_TIMEOUT,
            llm.complete(config, &system, &messages, &[]),
        )
        .await;
        let reply = match sent {
            Ok(Ok((reply, _usage))) => reply,
            Ok(Err(error)) => return (Answer::Failed(error.to_string()), requests),
            Err(_) => {
                return (
                    Answer::Failed(format!(
                        "no answer within {} minutes",
                        REQUEST_TIMEOUT.as_secs() / 60
                    )),
                    requests,
                );
            }
        };
        let text = reply_text(&reply);
        match contract::parse(&text) {
            Ok(parsed) => return (Answer::Read(parsed), requests),
            Err(why) if !repaired => {
                tracing::info!("curator: reply unreadable ({why}); one repair request");
                repaired = true;
                let shown = cut_to_tokens(&text, REPAIR_QUOTE_TOKENS);
                messages.push(Message::assistant().with_text(if shown.trim().is_empty() {
                    "(empty reply)".to_string()
                } else {
                    shown.to_string()
                }));
                let why: String = why.chars().take(MAX_REPAIR_REASON).collect();
                messages.push(
                    Message::user().with_text(hermes_prompts::curator::curator_job_repair(&why)),
                );
            }
            Err(why) => {
                tracing::warn!("curator: reply unreadable after the repair request ({why}); giving up");
                return (Answer::Unreadable(why), requests);
            }
        }
    }
}

/// A daily-limit 429 in a failed request's text teaches the ledger the provider's limit.
pub fn learn_limit(ledger: &ledger::Ledger, provider_id: &str, error: &str) {
    if hermes_llm::retry::is_usage_limit(error)
        && let Some(limit) = ledger::daily_limit_in(error)
    {
        ledger.learn_limit(provider_id, limit);
    }
}

// ---- delivering --------------------------------------------------------------------------------

/// What became of a proposal.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Waiting in the staging area for the user.
    Staged,
    /// Written (`curator.auto_apply`).
    Applied(String),
    /// Refused by the skill store's checks (validation, threat scan, limits, a pin, staleness).
    Refused(String),
}

/// Puts `proposal` (the answer to `job`) in front of the user, or applies it when `auto_apply`.
/// Blocking.
///
/// The model read the job's skills when the job was selected; a skill the proposal touches whose
/// SKILL.md changed since (the user edited it while the request ran) refuses the proposal, so the
/// model's text never replaces an edit it didn't see. From here on the staged item's own bases
/// keep it so ([`SkillStore::apply_staged`]: an edit after staging makes the item stale).
///
/// `auto_apply` writes directly only when every skill the proposal touches is one the background
/// review created (`created_by: agent`, the skills the loop curator manages too): a skill the user
/// or a chat wrote waits in Pending even then, since undoing a merge means restoring the archived
/// skill and the kept skill's old text (`skills` history) by hand.
pub fn deliver(
    skills: &SkillStore,
    data_dir: &Path,
    job: &Job,
    proposal: &Proposal,
    auto_apply: bool,
) -> Delivery {
    let ops: Vec<SkillOp> = proposal.ops.clone();
    if ops.is_empty() {
        return Delivery::Refused("there is nothing to apply".into());
    }
    let plan = match skills.plan(&ops) {
        Ok(plan) => plan,
        Err(error) => return Delivery::Refused(error),
    };
    for lib in job.libs() {
        let touched = ops.iter().any(|op| op.name == lib.name);
        let now = plan
            .bases
            .iter()
            .find(|b| b.skill == lib.name && b.file == "SKILL.md")
            .and_then(|b| b.fingerprint.as_deref());
        if touched && now != Some(lib.fingerprint.as_str()) {
            return Delivery::Refused(format!(
                "'{}' changed while the model was reading it; asked again at the next run",
                lib.name
            ));
        }
    }
    let reason = reason(job);
    let reason = reason.as_str();
    let managed = ops
        .iter()
        .all(|op| skills.record(&op.name).curator_managed());
    // The agent's skill writes wait for approval when `skills.write_approval` is on; the curator's
    // are agent writes too.
    if auto_apply && managed && !skills.config().write_approval {
        return match skills.apply_staged(&ops, WriteOrigin::Curator, &plan.bases) {
            Ok(value) if value["success"] == serde_json::json!(true) => Delivery::Applied(
                value["message"]
                    .as_str()
                    .unwrap_or("applied")
                    .chars()
                    .take(200)
                    .collect(),
            ),
            Ok(value) => Delivery::Refused(
                value["error"]
                    .as_str()
                    .unwrap_or("the write failed")
                    .chars()
                    .take(500)
                    .collect(),
            ),
            Err(StagedSkillError::Stale(message)) => Delivery::Refused(message),
        };
    }
    let meta = Meta {
        session_id: "curator".into(),
        sources: Vec::new(),
        reason: reason.to_string(),
    };
    let value = StagingStore::new(data_dir).stage_skill_ops(skills, ops, WriteOrigin::Curator, &meta);
    if value["success"] == serde_json::json!(true) {
        Delivery::Staged
    } else {
        Delivery::Refused(
            value["error"]
                .as_str()
                .unwrap_or("it could not be staged")
                .chars()
                .take(500)
                .collect(),
        )
    }
}

/// The skills of `job` with their current text, for the merged skill's provenance.
pub fn known(job: &Job) -> Vec<Known<'_>> {
    job.libs()
        .into_iter()
        .map(|l| Known {
            name: &l.name,
            skill_md: &l.text,
            own_files: l.own_files,
        })
        .collect()
}

/// Why a proposal waits for the user.
pub fn reason(job: &Job) -> String {
    match job {
        Job::Merge { skills, .. } => format!(
            "The curator found near-duplicate skills: {}",
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
        ),
        Job::Repair { skill, .. } => format!(
            "The curator proposes repairing '{}': fewer than half of its recent uses went well",
            skill.name
        ),
    }
}

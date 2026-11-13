//! The per-cluster curator run (`budget.curator_mode = per_cluster`, the `requests` profile's):
//! `propose.rs` has the procedure, this drives it for one run. Zero requests when there is nothing
//! to merge or repair, when the governor says no, or while the curator's own cap or back-off
//! holds; at most `curator_max_requests` jobs otherwise.

use std::sync::atomic::Ordering;
use std::time::Instant;

use chrono::Utc;
use hermes_llm::model::ModelConfig;
use hermes_llm::observer::Purpose;

use super::propose::{self, Answer, Delivery, Job};
use super::{CuratorModel, contract};
use crate::{Runtime, effort_request_params, governor};

/// What the pass did: the line for the summary, and the lines for the report.
pub(super) struct Pass {
    pub summary: String,
    pub detail: String,
}

impl Pass {
    fn only(summary: impl Into<String>) -> Self {
        let summary = summary.into();
        Self {
            detail: format!("{summary}\n"),
            summary,
        }
    }
}

impl Runtime {
    pub(super) async fn per_cluster(&self, model: &CuratorModel) -> Pass {
        let budget = self.budget();
        let auto_apply = self.curator_auto_apply.load(Ordering::Relaxed);
        let (skills, data_dir) = (self.skills.clone(), self.config.data_dir.clone());
        let selection = {
            let (skills, data_dir) = (skills.clone(), data_dir.clone());
            let now = Utc::now();
            tokio::task::spawn_blocking(move || {
                let library = propose::library(&skills);
                propose::select(&library, &data_dir, now)
            })
            .await
        };
        let Ok(selection) = selection else {
            return Pass::only("error (the library could not be read)");
        };
        let mut lines: Vec<String> = selection
            .left_out
            .iter()
            .map(|line| format!("- left out: {line}"))
            .collect();
        let merges = selection
            .jobs
            .iter()
            .filter(|job| matches!(job, Job::Merge { .. }))
            .count();
        let repairs = selection.jobs.len() - merges;
        let found = format!(
            "{} skill(s) looked at, {merges} duplicate cluster(s) and {repairs} repair(s) found",
            selection.considered
        );
        if selection.jobs.is_empty() {
            return Pass {
                summary: format!("per-cluster: nothing to merge or repair; no request ({found})"),
                detail: format!("{found}; no request.\n\n{}\n", lines.join("\n")),
            };
        }
        let provider_id = model.provider.id.clone();
        if let governor::Verdict::Deny(why) = self.governor().check(&provider_id, Purpose::Curator)
        {
            return Pass {
                summary: "skipped (request quota is low)".into(),
                detail: format!("{found}.\n\nSkipped: {why}\n"),
            };
        }
        if let Err(why) = self.curator_gate.lock().admit(Instant::now()) {
            return Pass {
                summary: format!("skipped ({why})"),
                detail: format!("{found}.\n\nSkipped: {why}\n"),
            };
        }
        let llm = match model.provider.one_shot_provider(budget.http_retries) {
            Ok(llm) => llm,
            Err(error) => {
                self.curator_gate.lock().finished(Instant::now(), false);
                return Pass::only(format!("error ({error:#})"));
            }
        };
        let mut config =
            ModelConfig::new(&model.model).with_context_limit(model.options.context_length);
        if let Some(effort) = model.options.effort.as_deref() {
            config = config.with_merged_request_params(effort_request_params(
                &model.provider.base_url,
                effort,
            ));
        }
        let message_budget = propose::budget(model.options.context_length);
        let max_jobs = budget.curator_max_requests as usize;
        let (mut asked, mut requests, mut failed) = (0usize, 0usize, false);
        let (mut staged, mut applied, mut unchanged, mut refused, mut unsent) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        for job in selection.jobs {
            let label = job.label();
            if asked >= max_jobs {
                lines.push(format!(
                    "- {label}: not asked, {max_jobs} job(s) a run is the limit (next run)"
                ));
                continue;
            }
            // Asked before every job: a run that crosses the line stops there.
            if let governor::Verdict::Deny(why) =
                self.governor().check(&provider_id, Purpose::Curator)
            {
                lines.push(format!("- {label}: not asked: {why}"));
                unsent += 1;
                break;
            }
            let text = match propose::message(&job, message_budget) {
                Ok(text) => text,
                Err(why) => {
                    lines.push(format!("- {label}: not asked: {why}"));
                    unsent += 1;
                    continue;
                }
            };
            asked += 1;
            let (answer, sent) = propose::ask(&llm, &config, &text).await;
            requests += sent;
            let reply = match answer {
                Answer::Failed(error) => {
                    propose::learn_limit(&self.ledger, &provider_id, &error);
                    lines.push(format!("- {label}: the request failed: {error}"));
                    failed = true;
                    break;
                }
                Answer::Unreadable(why) => {
                    propose::remember(&data_dir, &job, "reply unreadable", Utc::now());
                    lines.push(format!(
                        "- {label}: the reply was unreadable twice ({why}); given up"
                    ));
                    refused += 1;
                    continue;
                }
                Answer::Read(reply) => reply,
            };
            let proposal = contract::judge(&reply, &job.kind(), &propose::known(&job));
            for why in &proposal.rejected {
                lines.push(format!("- {label}: refused: {why}"));
            }
            if proposal.is_empty() {
                propose::remember(&data_dir, &job, "no change", Utc::now());
                if proposal.rejected.is_empty() {
                    lines.push(format!("- {label}: the model sees nothing to change"));
                    unchanged += 1;
                } else {
                    refused += 1;
                }
                continue;
            }
            let delivery = {
                let (skills, data_dir) = (skills.clone(), data_dir.clone());
                let (proposal, job) = (proposal.clone(), job.clone());
                tokio::task::spawn_blocking(move || {
                    propose::deliver(&skills, &data_dir, &job, &proposal, auto_apply)
                })
                .await
                .unwrap_or_else(|e| Delivery::Refused(format!("stopped: {e}")))
            };
            let archived = if proposal.archived.is_empty() {
                String::new()
            } else {
                format!(", archiving {}", proposal.archived.join(", "))
            };
            let keep = proposal.keep.as_deref().unwrap_or("?");
            match delivery {
                Delivery::Staged => {
                    propose::remember(&data_dir, &job, "proposed", Utc::now());
                    lines.push(format!(
                        "- {label}: proposal for '{keep}'{archived} waits in Pending"
                    ));
                    staged += 1;
                }
                Delivery::Applied(message) => {
                    propose::remember(&data_dir, &job, "applied", Utc::now());
                    lines.push(format!("- {label}: applied ({message})"));
                    applied += 1;
                }
                Delivery::Refused(why) => {
                    propose::remember(&data_dir, &job, "refused by the store", Utc::now());
                    lines.push(format!("- {label}: the skill store refused it: {why}"));
                    refused += 1;
                }
            }
        }
        self.curator_gate.lock().finished(Instant::now(), failed);
        let summary = format!(
            "per-cluster: {asked} job(s) asked ({requests} request(s)): {staged} proposal(s) \
             staged, {applied} applied, {unchanged} no change, {refused} refused{}{}",
            if unsent > 0 {
                format!(", {unsent} not sent")
            } else {
                String::new()
            },
            if failed { "; a request failed" } else { "" },
        );
        Pass {
            detail: format!("{summary}\n\n{found}.\n\n{}\n", lines.join("\n")),
            summary,
        }
    }
}

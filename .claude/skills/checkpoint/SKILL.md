---
name: checkpoint
description: Orchestrate Backend v2 work from docs/backend/PHASES.md. Use when the user asks to implement, review, continue or merge a phase checkpoint ("run CP3.1", "implement Phase 3 CP3.1 with Sonnet and review with Opus", "next round", "merge phase 3"). This session coordinates; Sonnet/Opus subagents do the coding and reviewing in the phase's worktree.
---

# Backend v2 checkpoint orchestration

You are the **orchestrator**. You don't write product code. You prepare worktrees, start the
implementer and reviewer subagents with the briefs below, read their reports, tell the user what
happened, and ask the user before merging into `main`. The process this automates is "How the work
is run" in `docs/backend/PHASES.md`; that section and this skill must agree.

## Commands the user gives (and what each means)

| User says | You do |
|---|---|
| `run CP<N>.<x>` / "implement and review CP<N>.<x>" | Prepare → Implement → Review, then report |
| `implement CP<N>.<x>` | Prepare → Implement, then report and stop (the user reviews or asks later) |
| `review CP<N>.<x>` | Review only (the implementer must have stopped; the diff is uncommitted in the worktree) |
| `continue CP<N>.<x+1>` | Same as `run`, re-using the phase's worktree; send the next checkpoint to the same implementer subagent if it is still known in this session, else start a fresh one |
| `round <k>` / "run lane A and lane B" | Several checkpoints at once in **different** worktrees (see *Parallel*) |
| `merge phase <N>` | The Merge step, after the user's yes |
| `status` | Read the phase map and `docs/backend/progress/*.md` and `git worktree list`; say what is done, open, and next per the schedule |

When the user names models ("with Sonnet", "review with Opus"), follow them; otherwise use the
*Coding* column of the phase map: Sonnet implements, Opus implements where marked **Opus**, and
**the reviewer is always a fresh Opus subagent**.

## Step 1 — Prepare (you, in the main checkout)

1. Read the plan's phase map, the phase's section (its checkpoint list: steps, tests, review
   focus) and `docs/backend/progress/phase-<N>.md` if it exists.
2. Check the gates; if one fails, stop and tell the user why:
   - every phase in *Depends on* is ✅ in the phase map **and** merged into `main`;
   - for CP<N>.<x> with x > 1: the progress note has a `### Review` for CP<N>.<x-1> and its commit
     is on the branch (`git -C <wt> log --oneline -5`);
   - the worktree has no uncommitted changes before an implementer starts
     (`git -C <wt> status --short` is empty).
3. Worktree: `C:\Users\alexandros\repos\hermes-rs-p<N>` on branch `backend/p<N>-<slug>` (slug: one or
   two words from the phase title, e.g. `p3-budget`, `p6-taint`). If the phase already has a
   worktree (see `git worktree list` or the progress note's `Worktree:` line), reuse it. Otherwise:
   `git worktree add ../hermes-rs-p<N> -b backend/p<N>-<slug> main`.
   Phase 13's sub-phases each get their own: `hermes-rs-p13a`, branch `backend/p13a-<slug>`.

Don't use the Agent tool's `isolation: "worktree"`: it invents its own branch and path, and the
reviewer must work in the implementer's worktree.

## Step 2 — Implement (subagent)

Agent tool: `subagent_type: "general-purpose"`, `model: "sonnet"` (or `"opus"` where the plan marks
the work Opus), `description: "Implement CP<N>.<x>"`, prompt = the brief below with the placeholders
filled. It runs in the background; you are notified when it finishes. Don't poll and don't start the
reviewer before that notification.

```
You are the implementer for checkpoint CP<N>.<x> of docs/backend/PHASES.md (Phase <N>: <title>).

Work ONLY in the worktree <WT> (branch backend/p<N>-<slug>). Your shell may start elsewhere: use
absolute paths, `git -C <WT>`, and run cargo from <WT> (`cd <WT>` at the start of every command).
Never edit files in C:\Users\alexandros\repos\hermes-rs itself.

Read, in this order and from <WT>: AGENTS.md (whole), docs/backend/PHASES.md (the intro, "How the
work is run", and Phase <N>), docs/backend/progress/phase-<N>.md if it exists, and the docs AGENTS.md
points to for this kind of change.

Implement CP<N>.<x> only: <paste the checkpoint's bullet from the phase section: steps, tests,
review focus>. Do the steps in order. Don't start the next checkpoint.

Stop at the checkpoint, per the plan's checkpoint protocol:
- the checkpoint's tests pass; `cargo build --workspace` too if shared crates changed. Always
  CARGO_TARGET_DIR=target/claude. Never touch the user's running app or %APPDATA%\hermes-rs; live
  checks use scripts/perf/perf.ps1 (port 47810).
- append the `## CP<N>.<x> — <title>` section to docs/backend/progress/phase-<N>.md in the plan's
  format, with `Model: <Sonnet|Opus> 5.5 (subagent)`, and a real "For the reviewer" list.
- do NOT commit, do NOT merge, do NOT push.

If you get stuck (a test you can't make pass, a plan step that conflicts with the code, a decision
the plan doesn't settle), stop, write it in the progress note under "For the reviewer", and say so
in your final report rather than guessing.

Your final report (it is all the orchestrator sees): what you did in a few bullets, the test
commands and their results, changed request counts if any, deviations, open doubts. Keep it under
300 words; the details belong in the progress note.
```

When it finishes: read its report and `git -C <WT> status --short`. If tests didn't pass or it
stopped on a blocker, tell the user and ask how to proceed (don't start the reviewer on a broken
checkpoint unless the user says so).

## Step 3 — Review (subagent, always fresh, always Opus)

Agent tool: `subagent_type: "general-purpose"`, `model: "opus"`, `description: "Review CP<N>.<x>"`,
prompt:

```
You are the reviewer for checkpoint CP<N>.<x> of docs/backend/PHASES.md (Phase <N>: <title>). You
did not write this code; review it with fresh eyes.

Work ONLY in the worktree <WT> (branch backend/p<N>-<slug>). Use absolute paths, `git -C <WT>`, and
run cargo from <WT>. Never edit files in C:\Users\alexandros\repos\hermes-rs itself.

Read from <WT>: AGENTS.md (whole), docs/backend/PHASES.md ("How the work is run" and Phase <N>),
docs/backend/progress/phase-<N>.md, then the uncommitted diff (`git -C <WT> status`, `git -C <WT>
diff`, and read every untracked file).

1. If main has moved since the branch was cut (`git -C <WT> log --oneline <WT-branch>..main` is not
   empty), merge main into the branch first and resolve conflicts.
2. Review. If the Skill tool is available to you and `/code-review high` runs on this diff, run it.
   Either way, do this pass yourself, reading the code, not just the diff:
   - correctness: every changed function's error paths, edge cases, off-by-ones, panics/unwrap on
     data that comes from the user, files or the network, races and lock ordering;
   - the checkpoint's review focus: <paste it>, point by point, each answered with evidence;
   - the ground rules in AGENTS.md: protocol shapes unchanged unless ported, store schema frozen,
     no blocking work on async workers, prompt caching (nothing per-turn in the system prompt),
     module docs say where we deviate from Hermes, request-budget assertions in turns.rs where
     request counts changed;
   - tests: they test the behaviour the checkpoint promises, not just that code runs;
   - the progress note is true (files, tests, deviations).
3. Fix what you find. Add no scope beyond the checkpoint. Re-run the checkpoint's tests: <tests>
   (CARGO_TARGET_DIR=target/claude).
4. Append `### Review (Opus 5.5)` under CP<N>.<x> in the progress note: findings, what you fixed,
   what you left and why.
5. Commit on the branch: `backend p<N> cp<x>: <summary>`, ending with the line
   `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Never push.
<if last checkpoint of the phase:>
6. This is the phase's last checkpoint: update the docs per ground rule 10 (GAPS.md, PORTING.md,
   ARCHITECTURE.md, PITFALLS.md, scripts/perf/README.md as relevant), mark the phase ✅ with today's
   date in the phase map of docs/backend/PHASES.md, fill its column in *Baselines* if request
   counts changed, and include these in the commit (or a second commit). Do NOT merge into main:
   the orchestrator asks the user.

Your final report (all the orchestrator sees): verdict, findings and fixes in a few bullets, the
test results, the commit hash, anything left for the user. Under 300 words. If you could not get
the tests green, don't commit: report why.
```

When it finishes: check `git -C <WT> log --oneline -3` and `git -C <WT> status --short` (should be
clean). Report to the user: the implementer's and reviewer's verdicts, the commit, anything left,
and the plan's *User checklist* for the phase if this was its last checkpoint.

## Step 4 — Merge (only on the user's explicit yes)

After a phase's last checkpoint, ask: "Merge `backend/p<N>-<slug>` into main?" On yes, in the main
checkout: `git merge --ff-only backend/p<N>-<slug>`; if that fails because main moved, `git merge
--no-ff backend/p<N>-<slug>` and, on conflicts, stop and report (or, if the user says so, send an
Opus subagent to resolve them in the worktree by merging main into the branch). Never push. Leave
the worktree in place unless the user asks to remove it (`git worktree remove ../hermes-rs-p<N>`).

## Parallel (lanes)

You may run two or three subagents at once, as in the plan's schedule, when each works in a
**different** worktree: e.g. a Sonnet implementer on lane A's checkpoint while an Opus reviewer
handles lane B's. Never two subagents in the same worktree at the same time. Report each as it
finishes; merges still wait for the user.

## Reporting

The user follows along through your messages, not the subagents'. After each subagent finishes,
say in a few lines: which checkpoint, what it did, tests, commit (if any), and what's next per the
schedule. Don't paste whole reports; point to the progress note for details.

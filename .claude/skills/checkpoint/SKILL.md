---
name: checkpoint
description: Orchestrate phased work from docs/backend/PHASES.md (Backend v2) or docs/native-ui/PHASES.md (the native UI, v0.5). Use when the user asks to implement, review, continue or merge a phase checkpoint ("run CP3.1", "run native CP0.1", "implement Phase 3 CP3.1 with Sonnet and review with Opus", "next round", "merge phase 3"). This session coordinates on Sonnet; Haiku/Sonnet/Opus subagents do the coding, visual checks and reviewing in the phase's worktree.
---

# Checkpoint orchestration

You are the **orchestrator** (run this session on Sonnet 5.5). You don't write product code. You
prepare worktrees, start the implementer, visual-check and reviewer subagents with the briefs below,
read their reports, tell the user what happened, and ask the user before merging into `main`. The
process this automates is "How the work is run" in each plan; those sections and this skill must
agree.

Keep your own context small: read reports, progress notes and `git status`, never diffs or source
files. That is what makes this setup cheap.

## The two plans

| | Backend (`backend`) | Native UI (`native`) |
|---|---|---|
| Plan | `docs/backend/PHASES.md` | `docs/native-ui/PHASES.md` (and its `README.md`) |
| Worktree `<WT>` | `C:\Users\alexandros\repos\hermes-rs-p<N>` | `C:\Users\alexandros\repos\hermes-rs-n<N>` |
| Branch | `backend/p<N>-<slug>` | `native/p<N>-<slug>` |
| Progress note | `docs/backend/progress/phase-<N>.md` | `docs/native-ui/progress/phase-<N>.md` |
| Commit prefix | `backend p<N> cp<x>:` | `native p<N> cp<x>:` |
| Models | phase map's *Coding* column; reviewer Opus | the *Roles and models* table (Implement and Review columns) |
| Extra checks | request-budget assertions in `turns.rs` | `scripts/native/check.ps1` (from CP0.2 on); visual check step |
| Live instance | `scripts/perf/perf.ps1` (port 47810) | `HERMES_RS_HOME=target/native-test` |

Which plan: the one the user names ("native CP0.1", "backend CP3.1"). If they don't name one, use
the plan in which that checkpoint is still open; if it is open in both, ask. Below, `<PLAN>`,
`<WT>`, `<BRANCH>`, `<NOTE>` and `<PREFIX>` are that plan's values.

## Commands the user gives (and what each means)

| User says | You do |
|---|---|
| `run CP<N>.<x>` / "implement and review CP<N>.<x>" | Prepare → Implement → (Visual check) → Review, then report |
| `implement CP<N>.<x>` | Prepare → Implement, then report and stop (the user reviews or asks later) |
| `review CP<N>.<x>` | (Visual check) → Review only (the implementer must have stopped; the diff is uncommitted in the worktree) |
| `continue CP<N>.<x+1>` | Same as `run`, re-using the phase's worktree; send the next checkpoint to the same implementer subagent if it is still known in this session and on the right model, else start a fresh one |
| `round <k>` / "run lane A and lane B" | Several checkpoints at once in **different** worktrees (see *Parallel*) |
| `merge phase <N>` | The Merge step, after the user's yes |
| `status` | Read the plan's phase map, its progress notes and `git worktree list`; say what is done, open, and next |

## Models

When the user names models ("with Sonnet", "review with Opus"), follow them. Otherwise:

- **Backend:** Sonnet implements, Opus where the phase map marks **Opus**; the reviewer is always
  a fresh Opus.
- **Native:** the plan's *Roles and models* table gives Implement and Review per checkpoint
  (`haiku`, `sonnet` or `opus`). The visual check is Haiku, and Sonnet at a phase's last checkpoint.

Agent tool `model` values: `"haiku"`, `"sonnet"`, `"opus"`. The commit trailer names the reviewer's
model: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` (or `Claude Sonnet 5.5`).

## Step 1 — Prepare (you, in the main checkout)

1. Read the plan's phase map, the phase's section (its checkpoint: steps, tests, review focus or
   done criteria) and `<NOTE>` if it exists. For native, also the *Roles and models* row.
2. Check the gates; if one fails, stop and tell the user why:
   - every phase in *Depends on* is ✅ in the phase map **and** merged into `main`;
   - for CP<N>.<x> with x > 1: the progress note has a `### Review` for CP<N>.<x-1> and its commit
     is on the branch (`git -C <WT> log --oneline -5`);
   - the worktree has no uncommitted changes before an implementer starts
     (`git -C <WT> status --short` is empty).
3. Worktree: `<WT>` on branch `<BRANCH>` (slug: one or two words from the phase title, e.g.
   `p3-budget`, `p4-thread`). If the phase already has a worktree (see `git worktree list` or the
   progress note's `Worktree:` line), reuse it. Otherwise:
   `git worktree add ../hermes-rs-p<N> -b backend/p<N>-<slug> main` (backend) or
   `git worktree add ../hermes-rs-n<N> -b native/p<N>-<slug> main` (native).
   Backend Phase 13's sub-phases each get their own: `hermes-rs-p13a`, branch `backend/p13a-<slug>`.

Don't use the Agent tool's `isolation: "worktree"`: it invents its own branch and path, and the
reviewer must work in the implementer's worktree.

## Step 2 — Implement (subagent)

Agent tool: `subagent_type: "general-purpose"`, `model:` per *Models*, `description: "Implement
CP<N>.<x>"`, prompt = the brief below with the placeholders filled. It runs in the background; you
are notified when it finishes. Don't poll and don't start the next step before that notification.

```
You are the implementer for checkpoint CP<N>.<x> of <PLAN> (Phase <N>: <title>).

Work ONLY in the worktree <WT> (branch <BRANCH>). Your shell may start elsewhere: use absolute
paths, `git -C <WT>`, and run cargo from <WT> (`cd <WT>` at the start of every command). Never edit
files in C:\Users\alexandros\repos\hermes-rs itself.

Read, in this order and from <WT>: AGENTS.md (whole), <PLAN> (the intro, "How the work is run",
and Phase <N>), <native only: docs/native-ui/README.md>, <NOTE> if it exists, and the docs
AGENTS.md points to for this kind of change.

Implement CP<N>.<x> only: <paste the checkpoint from the phase section: steps, tests, review focus
or done criteria>. Do the steps in order. Don't start the next checkpoint.

Stop at the checkpoint, per the plan's checkpoint protocol:
- the checkpoint's tests pass; `cargo build --workspace` too if shared crates changed. Always
  CARGO_TARGET_DIR=target/claude. Never touch the user's running app or %APPDATA%\hermes-rs.
  <backend: live checks use scripts/perf/perf.ps1 (port 47810).>
  <native: run scripts/native/check.ps1 (once it exists) and list in the progress note every
  snapshot difference it reports with the paths of its .diff.png/.actual.png files. Our instance
  runs with HERMES_RS_HOME=target/native-test. Don't look at images yourself unless a test needs
  it; a separate check does that. Cite the t3code source in the header of every module that copies
  a layout. Don't accept new snapshot baselines (UPDATE_SNAPSHOTS) for screens you changed: list
  them, the visual check and the reviewer decide.>
- append the `## CP<N>.<x> — <title>` section to <NOTE> in the plan's format, with
  `Model: <Haiku|Sonnet|Opus> 5.5 (subagent)`, and a real "For the reviewer" list.
- do NOT commit, do NOT merge, do NOT push.

If you get stuck (a test you can't make pass, a plan step that conflicts with the code, a decision
the plan doesn't settle), stop, write it in the progress note under "For the reviewer", and say so
in your final report rather than guessing.

Your final report (it is all the orchestrator sees): what you did in a few bullets, the test
commands and their results, <backend: changed request counts if any,> <native: snapshot
differences (count and paths),> deviations, open doubts. Keep it under 300 words; the details
belong in the progress note.
```

When it finishes: read its report and `git -C <WT> status --short`. If tests didn't pass or it
stopped on a blocker, tell the user and ask how to proceed (don't start the reviewer on a broken
checkpoint unless the user says so).

## Step 2b — Visual check (native only, subagent)

Run it when the implementer reported snapshot differences, and always at a phase's last
checkpoint. Agent tool: `subagent_type: "general-purpose"`, `model: "haiku"` (`"sonnet"` at a
phase's last checkpoint), `description: "Visual check CP<N>.<x>"`:

```
You are the visual checker for checkpoint CP<N>.<x> of docs/native-ui/PHASES.md. You change no
code and accept no snapshots.

From <WT>: read the CP<N>.<x> section of <NOTE> (what changed and why, and the list of snapshot
differences). Then look at each listed image with the Read tool: the .diff.png first, then the
.actual.png and the approved baseline in crates/native/snapshots/ only when the diff isn't clear.
<last checkpoint: also look once at each new screen's gallery capture, light and dark, and compare
with its t3code reference named in the module header (read the t3code component, don't run it):
spacing, alignment, sizes, colours by token, states.>

For each image, one line: `<name>: expected | unexpected — <what you see, with position>`
("expected: the hover colour changed as the note says", "unexpected: the badge overlaps the
title by ~3px at the right edge"). Append them under CP<N>.<x> in <NOTE> as
`### Visual check (<Haiku|Sonnet> 5.5)`. Your final report: the count of expected and unexpected,
and the unexpected lines. Under 150 words.
```

## Step 3 — Review (subagent, always fresh)

Agent tool: `subagent_type: "general-purpose"`, `model:` per *Models* (Opus unless the plan says
Sonnet), `description: "Review CP<N>.<x>"`, prompt:

```
You are the reviewer for checkpoint CP<N>.<x> of <PLAN> (Phase <N>: <title>). You did not write
this code; review it with fresh eyes.

Work ONLY in the worktree <WT> (branch <BRANCH>). Use absolute paths, `git -C <WT>`, and run cargo
from <WT>. Never edit files in C:\Users\alexandros\repos\hermes-rs itself.

Read from <WT>: AGENTS.md (whole), <PLAN> ("How the work is run" and Phase <N>), <native only:
docs/native-ui/README.md>, <NOTE>, then the uncommitted diff (`git -C <WT> status`, `git -C <WT>
diff`, and read every untracked file).

1. If main has moved since the branch was cut (`git -C <WT> log --oneline <BRANCH>..main` is not
   empty), merge main into the branch first and resolve conflicts.
2. Review. If the Skill tool is available to you and `/code-review high` runs on this diff, run it.
   Either way, do this pass yourself, reading the code, not just the diff:
   - correctness: every changed function's error paths, edge cases, off-by-ones, panics/unwrap on
     data that comes from the user, files or the network, races and lock ordering;
   - the checkpoint's review focus or done criteria: <paste it>, point by point, each answered
     with evidence;
   - the ground rules in AGENTS.md: protocol shapes unchanged unless ported, store schema frozen,
     no blocking work on async workers, prompt caching (nothing per-turn in the system prompt),
     module docs say where we deviate from Hermes;
   - <backend: request-budget assertions in turns.rs where request counts changed;>
   - <native: views never call the runtime (only hermes_core through the app's models); no Zed
     code outside gpui-kit; t3code cited in module headers; new UI has layout assertions, gallery
     stories and snapshots; the Tauri app's tests and the ACP tests still pass; every
     "unexpected" line of the visual check is fixed or explained; accept snapshot baselines
     (UPDATE_SNAPSHOTS=1) only for differences the visual check called expected or you fixed;>
   - tests: they test the behaviour the checkpoint promises, not just that code runs;
   - the progress note is true (files, tests, deviations).
3. Fix what you find. Add no scope beyond the checkpoint. Re-run the checkpoint's tests: <tests>
   (CARGO_TARGET_DIR=target/claude)<native: and scripts/native/check.ps1>.
4. Append `### Review (<Opus|Sonnet> 5.5)` under CP<N>.<x> in the progress note: findings, what
   you fixed, what you left and why.
5. Commit on the branch: `<PREFIX> <summary>`, ending with the line
   `Co-Authored-By: Claude <Opus|Sonnet> 5.5 <noreply@anthropic.com>`. Never push.
<if last checkpoint of the phase:>
6. This is the phase's last checkpoint: update the docs per ground rule 10 (GAPS.md, PORTING.md,
   ARCHITECTURE.md, PITFALLS.md, TESTING.md, scripts/perf/README.md as relevant), mark the phase ✅
   with today's date in the phase map of <PLAN>, <backend: fill its column in *Baselines* if
   request counts changed,> write the phase's numbered user checklist into <NOTE> if the plan
   doesn't have one, and include these in the commit (or a second commit). Do NOT merge into main:
   the orchestrator asks the user.

Your final report (all the orchestrator sees): verdict, findings and fixes in a few bullets, the
test results, the commit hash, anything left for the user. Under 300 words. If you could not get
the tests green, don't commit: report why.
```

When it finishes: check `git -C <WT> log --oneline -3` and `git -C <WT> status --short` (should be
clean). Report to the user: the implementer's, visual check's and reviewer's verdicts, the commit,
anything left, and the phase's user checklist if this was its last checkpoint.

## Step 4 — Merge (only on the user's explicit yes)

After a phase's last checkpoint, ask: "Merge `<BRANCH>` into main?" On yes, in the main checkout:
`git merge --ff-only <BRANCH>`; if that fails because main moved, `git merge --no-ff <BRANCH>` and,
on conflicts, stop and report (or, if the user says so, send an Opus subagent to resolve them in the
worktree by merging main into the branch). Never push. Leave the worktree in place unless the user
asks to remove it (`git worktree remove <WT>`).

## Parallel (lanes)

You may run two or three subagents at once when each works in a **different** worktree: e.g. a
Sonnet implementer on one phase's checkpoint while an Opus reviewer handles another's (backend:
the plan's lanes and schedule; native: Phases 0 and 1 together, then 6 and 7 together). Never two
subagents in the same worktree at the same time. Report each as it finishes; merges still wait for
the user.

## Reporting

The user follows along through your messages, not the subagents'. After each subagent finishes,
say in a few lines: which checkpoint, what it did, tests, commit (if any), and what's next. Don't
paste whole reports; point to the progress note for details.

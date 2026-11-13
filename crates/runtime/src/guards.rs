//! Hermes's turn-loop guards as pure logic: tool-name repair (`agent_runtime_helpers.repair_tool_call`),
//! the tool-call loop guardrails (`agent/tool_guardrails.py`) and the repetition detector
//! (`agent/repetition_guard.py`). Ported from Hermes (MIT); the runtime decides what each verdict does.
//!
//! The desktop app is an attended surface, so like Hermes's CLI/desktop the guardrails only warn:
//! hard stops are Hermes's opt-in for interactive platforms and stay off here.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

use serde_json::Value;

// ---- tool-name repair -------------------------------------------------------------------------

/// Repairs a mismatched tool name (case, separators, CamelCase, `_tool` suffixes, then a fuzzy
/// match at 0.7 similarity). `None` when nothing in `valid` is close enough.
pub fn repair_tool_name(name: &str, valid: &[&str]) -> Option<String> {
    // Some providers leak XML attribute fragments into the name (`terminal" parameter="command"`):
    // cut at the first quote or angle bracket, but never at whitespace ("write file" is repairable).
    let mut name = name;
    for separator in ['"', '\'', '<', '>'] {
        if let Some(index) = name.find(separator).filter(|&index| index > 0) {
            name = &name[..index];
        }
    }
    if name.is_empty() {
        return None;
    }
    let is_valid = |candidate: &str| valid.contains(&candidate);
    let norm = |s: &str| s.to_lowercase().replace(['-', ' '], "_");
    let camel_snake = |s: &str| {
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && c.is_uppercase() {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        }
        out
    };
    let strip_tool_suffix = |s: &str| -> Option<String> {
        let lower = s.to_lowercase();
        ["_tool", "-tool", "tool"]
            .iter()
            .find(|suffix| lower.ends_with(*suffix))
            .map(|suffix| {
                s[..s.len() - suffix.len()]
                    .trim_end_matches(['_', '-'])
                    .to_string()
            })
    };

    let lowered = name.to_lowercase();
    if is_valid(&lowered) {
        return Some(lowered);
    }
    let normalized = norm(name);
    if is_valid(&normalized) {
        return Some(normalized);
    }
    let mut candidates: HashSet<String> = [
        name.to_string(),
        lowered.clone(),
        normalized,
        camel_snake(name),
    ]
    .into();
    for _ in 0..2 {
        let extra: Vec<String> = candidates
            .iter()
            .filter_map(|c| strip_tool_suffix(c))
            .flat_map(|stripped| [norm(&stripped), camel_snake(&stripped), stripped])
            .collect();
        candidates.extend(extra);
    }
    if let Some(found) = valid.iter().find(|v| candidates.contains(**v)) {
        return Some(found.to_string());
    }
    valid
        .iter()
        .map(|v| (similarity(&lowered, v), *v))
        .filter(|(ratio, _)| *ratio >= 0.7)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, v)| v.to_string())
}

/// Python's `difflib.SequenceMatcher.ratio()`: 2·matches / (len(a) + len(b)), where matches come
/// from recursively taking the longest common block.
fn similarity(a: &str, b: &str) -> f64 {
    fn matches(a: &[char], b: &[char]) -> usize {
        let (mut best, mut best_i, mut best_j) = (0, 0, 0);
        for i in 0..a.len() {
            for j in 0..b.len() {
                let mut k = 0;
                while i + k < a.len() && j + k < b.len() && a[i + k] == b[j + k] {
                    k += 1;
                }
                if k > best {
                    (best, best_i, best_j) = (k, i, j);
                }
            }
        }
        if best == 0 {
            return 0;
        }
        best + matches(&a[..best_i], &b[..best_j])
            + matches(&a[best_i + best..], &b[best_j + best..])
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    2.0 * matches(&a, &b) as f64 / (a.len() + b.len()) as f64
}

// ---- repetition detection ---------------------------------------------------------------------

/// Below this length the check doesn't run: short truncations legitimately repeat tokens.
const MIN_FRAGMENT_LENGTH: usize = 400;
const REPEAT_WINDOW: usize = 60;
const MIN_REPEAT_COUNT: usize = 5;
const DOMINANCE_RATIO: f64 = 0.5;
const MAX_ANCHOR_SAMPLES: usize = 32;
const MAX_ANCHOR_MATCHES: usize = 8;

/// True when a contiguous run of at least five exact repetitions covers at least half of `text`:
/// a model stuck in a loop. Conservative: only long verbatim repeats trip it.
pub fn is_repetition_dominated(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n < MIN_FRAGMENT_LENGTH {
        return false;
    }
    line_repetition_dominated(text, n)
        || window_count_dominated(&chars)
        || periodic_run_dominated(&chars)
}

fn line_repetition_dominated(text: &str, n: usize) -> bool {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        *counts.entry(line).or_default() += 1;
    }
    counts.iter().any(|(line, &count)| {
        count >= MIN_REPEAT_COUNT
            && (count * line.chars().count()) as f64 >= n as f64 * DOMINANCE_RATIO
    })
}

fn window_count_dominated(chars: &[char]) -> bool {
    let n = chars.len();
    let needed =
        MIN_REPEAT_COUNT.max((n as f64 * DOMINANCE_RATIO / REPEAT_WINDOW as f64).ceil() as usize);
    let mut counts: HashMap<&[char], usize> = HashMap::new();
    for window in chars.windows(REPEAT_WINDOW) {
        let count = counts.entry(window).or_default();
        *count += 1;
        if *count >= needed {
            return true;
        }
    }
    false
}

fn periodic_run_dominated(chars: &[char]) -> bool {
    let n = chars.len();
    let Some(max_start) = n.checked_sub(REPEAT_WINDOW).filter(|&m| m >= 1) else {
        return false;
    };
    let step = ((max_start + MAX_ANCHOR_SAMPLES - 2) / (MAX_ANCHOR_SAMPLES - 1)).max(1);
    let mut starts: Vec<usize> = (0..=max_start).step_by(step).collect();
    if starts.last() != Some(&max_start) {
        starts.push(max_start);
    }
    let find = |anchor: &[char], from: usize| {
        (from..=n.saturating_sub(anchor.len())).find(|&i| &chars[i..i + anchor.len()] == anchor)
    };
    let mut rejected: Vec<(usize, usize, usize)> = Vec::new();
    for start in starts {
        let anchor = &chars[start..start + REPEAT_WINDOW];
        let mut search_from = start + 1;
        for _ in 0..MAX_ANCHOR_MATCHES {
            let Some(found) = find(anchor, search_from) else {
                break;
            };
            let period = found - start;
            search_from = found + 1;
            if rejected
                .iter()
                .any(|&(lo, hi, p)| lo <= start && start < hi && period % p == 0)
            {
                continue;
            }
            let (mut left, mut right) = (start, start + REPEAT_WINDOW);
            while left > 0 && chars[left - 1] == chars[left - 1 + period] {
                left -= 1;
            }
            while right + period < n && chars[right] == chars[right + period] {
                right += 1;
            }
            let right = right + period;
            let length = right - left;
            if length >= MIN_REPEAT_COUNT * period && length as f64 >= n as f64 * DOMINANCE_RATIO {
                return true;
            }
            rejected.push((left, right, period));
        }
    }
    false
}

// ---- tool-call loop guardrails ----------------------------------------------------------------

/// Read-only tools: the same result twice means no progress.
const IDEMPOTENT_TOOLS: &[&str] = &["read_file", "search_files"];
/// Pollers are legitimately re-invoked with identical args; the identical-call notice skips them.
const REPEATABLE_TOOLS: &[&str] = &["process_manage"];
/// A success from one of these is progress for every failing call this turn.
const PROGRESS_RESET_TOOLS: &[&str] = &[
    "write_file",
    "patch",
    "terminal",
    "run_script",
    "process_manage",
    "todo_list",
];

const EXACT_FAILURE_WARN_AFTER: usize = 2;
const SAME_TOOL_FAILURE_WARN_AFTER: usize = 3;
const NO_PROGRESS_WARN_AFTER: usize = 2;
const IDENTICAL_CALL_THRESHOLD: usize = 3;
const MAX_CYCLE_PERIOD: usize = 4;
const CYCLE_HISTORY: usize = 64;
const IDENTICAL_RESULT_STUB_MIN_CHARS: usize = 512;
const STUB_ARGS_PREVIEW_CHARS: usize = 120;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Signature {
    tool: u64,
    args: u64,
}

fn hash_of(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Sorted compact JSON (serde_json's maps are sorted unless `preserve_order` is on, so sort
/// explicitly).
fn canonical_json(value: &Value) -> String {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k.clone(), sort(v)))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&sort(value)).unwrap_or_default()
}

fn result_hash(result: &str) -> u64 {
    match serde_json::from_str::<Value>(result) {
        Ok(value) => hash_of(&canonical_json(&value)),
        Err(_) => hash_of(result),
    }
}

/// Hermes's fallback failure classifier: a terminal call fails on a nonzero exit code; a JSON
/// object fails on `success: false` or an `error` field; anything else on an error flag.
pub fn classify_failure(tool: &str, result: &str, is_error: bool) -> bool {
    let parsed: Option<Value> = serde_json::from_str(result).ok();
    if tool == "terminal" || tool == "run_script" {
        return parsed
            .as_ref()
            .and_then(|v| v.get("exit_code"))
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
            || is_error;
    }
    match parsed {
        Some(Value::Object(map)) => {
            let failed = map.get("success") == Some(&Value::Bool(false));
            let error = map
                .get("error")
                .or_else(|| map.get("message"))
                .is_some_and(|e| !e.is_null() && e != "");
            (error && (failed || map.contains_key("error"))) || is_error
        }
        _ => is_error,
    }
}

/// What the guardrails add to one tool result.
#[derive(Debug, Default, PartialEq)]
pub struct Observation {
    /// Replaces a byte-identical repeat of an earlier result (the tool still ran).
    pub stub: Option<String>,
    /// Appended after the result: loop warnings and the identical-call notice.
    pub notes: Vec<String>,
}

/// Per-turn tool-call loop detector (Hermes's `ToolCallGuardrailController`, warn-only).
#[derive(Default)]
pub struct ToolGuardrails {
    exact_failures: HashMap<Signature, usize>,
    same_tool_failures: HashMap<String, usize>,
    progress_since_failure: HashSet<Signature>,
    no_progress: HashMap<Signature, (u64, usize)>,
    streak: Option<(Signature, u64)>,
    streak_count: usize,
    streak_first_call: String,
    history: VecDeque<(Signature, u64, bool)>,
}

impl ToolGuardrails {
    /// Records one finished call and returns what to add to its result.
    pub fn observe(
        &mut self,
        tool: &str,
        args: &Value,
        call_id: &str,
        result: &str,
        failed: bool,
    ) -> Observation {
        let args_json = canonical_json(args);
        let signature = Signature {
            tool: hash_of(tool),
            args: hash_of(&args_json),
        };
        let mut observation = Observation::default();
        if let Some(warning) = self.after_call(tool, signature, result, failed) {
            observation.notes.push(warning);
        }

        // Identical-call streak: consecutive identical (tool, args, result) calls.
        let hash = result_hash(result);
        if self.streak == Some((signature, hash)) {
            self.streak_count += 1;
        } else {
            self.streak = Some((signature, hash));
            self.streak_count = 1;
            self.streak_first_call = call_id.to_string();
        }
        let count = self.streak_count;
        let repeatable = REPEATABLE_TOOLS.contains(&tool);
        let mut notice = None;
        if !repeatable && count >= IDENTICAL_CALL_THRESHOLD {
            notice = Some(format!(
                "[hermes note: this is the {} consecutive identical call to {tool} with identical \
                 arguments returning the same result. Do not repeat it — change arguments, use a \
                 different tool, or proceed with what you have.]",
                ordinal(count)
            ));
        }
        // Repeating multi-call cycles (A,B,A,B,…) reset the streak above on every alternation.
        if self.history.len() == CYCLE_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back((signature, hash, repeatable));
        if notice.is_none() {
            if let Some((period, laps)) = self.identical_cycle() {
                notice = Some(format!(
                    "[hermes note: the last {laps} rounds repeated the same cycle of {period} tool \
                     calls (ending with {tool}) with identical arguments and identical results. Do \
                     not repeat the batch — change arguments, use a different tool, or proceed \
                     with what you have.]"
                ));
            }
        }
        observation.notes.extend(notice);

        if count >= 2 && !failed && result.chars().count() >= IDENTICAL_RESULT_STUB_MIN_CHARS {
            let mut preview: String = args_json.chars().take(STUB_ARGS_PREVIEW_CHARS).collect();
            if args_json.chars().count() > STUB_ARGS_PREVIEW_CHARS {
                preview.push('…');
            }
            let reference = if self.streak_first_call.is_empty() {
                String::new()
            } else {
                format!(" (tool_call_id {})", self.streak_first_call)
            };
            observation.stub = Some(format!(
                "[hermes note: this result is byte-identical to the {tool} result earlier this \
                 turn{reference}. Refer to that result; it has not changed. Args: {preview}]"
            ));
        }
        observation
    }

    fn after_call(
        &mut self,
        tool: &str,
        signature: Signature,
        result: &str,
        failed: bool,
    ) -> Option<String> {
        if failed {
            // An identical failing call is only a replay if nothing landed in between.
            if self.progress_since_failure.remove(&signature) {
                self.exact_failures.remove(&signature);
            }
            let exact = self.exact_failures.entry(signature).or_default();
            *exact += 1;
            let exact = *exact;
            let same = self.same_tool_failures.entry(tool.to_string()).or_default();
            *same += 1;
            let same = *same;
            self.no_progress.remove(&signature);
            if exact >= EXACT_FAILURE_WARN_AFTER {
                return Some(guidance(
                    "repeated_exact_failure_warning",
                    exact,
                    &format!(
                        "{tool} has failed {exact} times with identical arguments. This looks like \
                         a loop; inspect the error and change strategy instead of retrying it \
                         unchanged."
                    ),
                ));
            }
            if same >= SAME_TOOL_FAILURE_WARN_AFTER {
                return Some(guidance(
                    "same_tool_failure_warning",
                    same,
                    &failure_recovery_hint(tool, same),
                ));
            }
            return None;
        }
        self.exact_failures.remove(&signature);
        self.same_tool_failures.remove(tool);
        if PROGRESS_RESET_TOOLS.contains(&tool) {
            self.progress_since_failure
                .extend(self.exact_failures.keys().copied());
            self.same_tool_failures.clear();
        }
        if !IDEMPOTENT_TOOLS.contains(&tool) {
            self.no_progress.remove(&signature);
            return None;
        }
        let hash = result_hash(result);
        let repeats = match self.no_progress.get(&signature) {
            Some(&(previous, count)) if previous == hash => count + 1,
            _ => 1,
        };
        self.no_progress.insert(signature, (hash, repeats));
        (repeats >= NO_PROGRESS_WARN_AFTER).then(|| {
            guidance(
                "idempotent_no_progress_warning",
                repeats,
                &format!(
                    "{tool} returned the same result {repeats} times. Use the result already \
                     provided or change the query instead of repeating it unchanged."
                ),
            )
        })
    }

    /// The smallest period 2..=4 whose trailing laps (same signature and result per position)
    /// reach the notice threshold. A cycle made only of pollers is legitimate waiting.
    fn identical_cycle(&self) -> Option<(usize, usize)> {
        let history = &self.history;
        let len = history.len();
        for period in 2..=MAX_CYCLE_PERIOD {
            if len < period * IDENTICAL_CALL_THRESHOLD {
                continue;
            }
            let mut laps = 1;
            loop {
                let Some(base) = len.checked_sub(period * (laps + 1)) else {
                    break;
                };
                let equal = (0..period).all(|i| {
                    let (a, b) = (&history[base + i], &history[len - period + i]);
                    (a.0, a.1) == (b.0, b.1)
                });
                if !equal {
                    break;
                }
                laps += 1;
            }
            if laps >= IDENTICAL_CALL_THRESHOLD {
                if (0..period).all(|i| history[len - period + i].2) {
                    continue;
                }
                return Some((period, laps));
            }
        }
        None
    }
}

fn guidance(code: &str, count: usize, message: &str) -> String {
    format!("[Tool loop warning: {code}; count={count}; {message}]")
}

fn failure_recovery_hint(tool: &str, count: usize) -> String {
    let common = format!(
        "{tool} has failed {count} times this turn. This looks like a loop. Do not switch to \
         text-only replies; keep using tools, but diagnose before retrying. First inspect the \
         latest error/output and verify your assumptions. "
    );
    if tool == "terminal" {
        common
            + "For terminal failures, run a small diagnostic such as `pwd && ls -la` in the same \
               tool, then try an absolute path, a simpler command, a different working directory, \
               or a different tool such as read_file/write_file/patch."
    } else {
        common
            + "Try different arguments, a narrower query/path, an absolute path when relevant, or \
               a different tool that can make progress. If the blocker is external, report the \
               blocker after one diagnostic attempt instead of repeating the same failing path."
    }
}

fn ordinal(count: usize) -> String {
    let suffix = if (11..=13).contains(&(count % 100)) {
        "th"
    } else {
        match count % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{count}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOOLS: &[&str] = &[
        "terminal",
        "read_file",
        "write_file",
        "patch",
        "search_files",
        "todo_list",
    ];

    #[test]
    fn repairs_common_tool_name_mistakes() {
        assert_eq!(
            repair_tool_name("Terminal", TOOLS).as_deref(),
            Some("terminal")
        );
        assert_eq!(
            repair_tool_name("write-file", TOOLS).as_deref(),
            Some("write_file")
        );
        assert_eq!(
            repair_tool_name("write file", TOOLS).as_deref(),
            Some("write_file")
        );
        assert_eq!(
            repair_tool_name("ReadFile", TOOLS).as_deref(),
            Some("read_file")
        );
        assert_eq!(
            repair_tool_name("TodoListTool_tool", TOOLS).as_deref(),
            Some("todo_list")
        );
        assert_eq!(
            repair_tool_name("terminal\" parameter=\"command\"", TOOLS).as_deref(),
            Some("terminal")
        );
        assert_eq!(
            repair_tool_name("read_fil", TOOLS).as_deref(),
            Some("read_file")
        );
        assert_eq!(repair_tool_name("browser_navigate", TOOLS), None);
        assert_eq!(repair_tool_name("", TOOLS), None);
    }

    #[test]
    fn similarity_matches_difflib() {
        // difflib.SequenceMatcher(None, "read_fil", "read_file").ratio() == 0.9411…
        assert!((similarity("read_fil", "read_file") - 16.0 / 17.0).abs() < 1e-9);
        assert_eq!(similarity("abc", "xyz"), 0.0);
    }

    #[test]
    fn repetition_needs_long_dominating_repeats() {
        let looped = "The answer is forty-two and nothing else matters here at all.\n".repeat(20);
        assert!(is_repetition_dominated(&looped));
        let short = "same line\n".repeat(10);
        assert!(!is_repetition_dominated(&short));
        let prose: String = (0..80)
            .map(|i| format!("Sentence number {i} says something new. "))
            .collect();
        assert!(!is_repetition_dominated(&prose));
        // A periodic loop without newlines.
        let unit = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor ";
        let periodic = format!("{}{}", "intro text ".repeat(3), unit.repeat(10));
        assert!(is_repetition_dominated(&periodic));
    }

    #[test]
    fn identical_calls_get_a_notice_and_a_stub() {
        let mut guard = ToolGuardrails::default();
        let args = json!({"path": "a.txt"});
        let big = format!("{{\"content\":\"{}\"}}", "x".repeat(600));
        let first = guard.observe("read_file", &args, "c1", &big, false);
        assert_eq!(first, Observation::default());
        let second = guard.observe("read_file", &args, "c2", &big, false);
        assert!(second.stub.as_deref().unwrap().contains("tool_call_id c1"));
        assert!(second.notes[0].contains("idempotent_no_progress_warning"));
        let third = guard.observe("read_file", &args, "c3", &big, false);
        assert!(
            third
                .notes
                .iter()
                .any(|n| n.contains("3rd consecutive identical call"))
        );
    }

    #[test]
    fn repeated_failures_warn_and_progress_resets_them() {
        let mut guard = ToolGuardrails::default();
        let args = json!({"command": "make"});
        let failed = r#"{"exit_code":2,"output":"boom"}"#;
        assert!(classify_failure("terminal", failed, false));
        assert!(
            guard
                .observe("terminal", &args, "1", failed, true)
                .notes
                .is_empty()
        );
        let warn = guard.observe("terminal", &args, "2", failed, true);
        assert!(warn.notes[0].contains("repeated_exact_failure_warning; count=2"));
        // An edit in between makes the next identical failure a new experiment.
        guard.observe(
            "patch",
            &json!({"path": "x"}),
            "3",
            r#"{"success":true}"#,
            false,
        );
        let after = guard.observe("terminal", &args, "4", failed, true);
        assert!(
            after
                .notes
                .iter()
                .all(|n| !n.contains("repeated_exact_failure"))
        );
    }

    #[test]
    fn cycles_of_identical_batches_are_flagged() {
        let mut guard = ToolGuardrails::default();
        let mut last = Observation::default();
        for lap in 0..3 {
            guard.observe(
                "search_files",
                &json!({"q": "a"}),
                &format!("a{lap}"),
                "A",
                false,
            );
            last = guard.observe(
                "terminal",
                &json!({"command": "ls"}),
                &format!("b{lap}"),
                r#"{"exit_code":0}"#,
                false,
            );
        }
        assert!(
            last.notes
                .iter()
                .any(|n| n.contains("cycle of 2 tool calls"))
        );
    }

    #[test]
    fn pollers_never_get_the_identical_call_notice() {
        let mut guard = ToolGuardrails::default();
        let args = json!({"action": "poll", "id": "p1"});
        for i in 0..5 {
            let obs = guard.observe(
                "process_manage",
                &args,
                &i.to_string(),
                r#"{"status":"running"}"#,
                false,
            );
            assert!(obs.notes.is_empty());
        }
    }
}

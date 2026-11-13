//! Duplicate clusters: which of the user's skills say (nearly) the same thing.
//!
//! **Method: Jaccard similarity of 5-word shingles.** A skill's text (front matter left out) is
//! lowercased and cut into words (runs of letters and digits); every run of five consecutive words
//! is one shingle, hashed (FNV-1a, 64 bits). Two skills' similarity is `|A ∩ B| / |A ∪ B|` over
//! their shingle sets. Why this and not the index's BM25 cross-scores: it needs nothing but the
//! text (no index, no ranking state), it is exact and deterministic (the same library always gives
//! the same clusters, in the same order), and it measures "the same sentences" rather than "the
//! same topic": two deploy skills for different services share a vocabulary, not five-word runs,
//! while a skill copied and edited shares most of them. Skills are clustered by single linkage
//! (a pair at or above [`THRESHOLD`] joins two skills; a group is a cluster) and a cluster holds at
//! most [`MAX_CLUSTER`] skills (a bigger group is taken in parts over several runs).
//!
//! **The threshold** ([`THRESHOLD`], 0.35) was tuned on the 39 skills the app bundles (the "37" of
//! the plan, plus two nested ones): the most similar *pair* among them scores 0.13
//! (`bundled_skills_do_not_cluster` pins a margin of 2x). Over Hermes's whole set (`../hermes-agent`
//! `skills` and `optional-skills`, 193 skills; informational, run only when that clone is present)
//! exactly one pair is at or above it, the same one anywhere from 0.3 to 0.55: two manuals made from
//! one template (`mlops/pytorch-fsdp` and `mlops/training/unsloth`, 0.56; both have `references/`,
//! so the curator would leave them to the user). Distinct skills sit under 0.2. On the other side,
//! Jaccard is strict: two skills that share a fraction `s` of their text score about `s / (2 - s)`,
//! so the realistic duplicate (a copy with a new title and a few steps of its own each, 60% shared)
//! scores 0.43 (`two_skills_sharing_about_sixty_percent_of_their_steps_cluster`), and 0.5 would
//! need two-thirds shared; 0.35 catches about half shared. A false pair costs one request answered
//! "none", not asked again for 30 days (`seen.rs`). Normalisation: front matter left out, case
//! folded, markdown and punctuation are separators, code is words like the rest. Skills with fewer than
//! [`MIN_SHINGLES`] shingles (a stub) are never compared: two short skills share a few phrases by
//! accident.
//!
//! **Cost.** Exact pairwise comparison is O(n²) merges of sorted hash lists; a pair whose sizes
//! differ too much to reach the threshold is skipped without comparing. At most [`MAX_SKILLS`]
//! skills are compared per run (the first by name; the rest are not compared until the library
//! is smaller: a guard, the libraries this is for hold tens to hundreds), and a
//! skill's text counts up to [`MAX_TEXT_CHARS`] characters, so a library of hundreds costs
//! milliseconds and one of thousands costs the same as [`MAX_SKILLS`].

use std::collections::BTreeSet;

/// Words per shingle.
pub const SHINGLE_WORDS: usize = 5;
/// Fewer shingles than this and a skill is too short to judge.
pub const MIN_SHINGLES: usize = 30;
/// A pair at or above this similarity is a pair of duplicates.
pub const THRESHOLD: f64 = 0.35;
/// Skills one run compares (a library of thousands is cut here).
pub const MAX_SKILLS: usize = 400;
/// Skills in one cluster (one request shows all their bodies).
pub const MAX_CLUSTER: usize = 4;
/// How much of a skill's text is shingled.
pub const MAX_TEXT_CHARS: usize = 60_000;

/// A skill as the comparison sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub name: String,
    /// Sorted, distinct shingle hashes.
    shingles: Vec<u64>,
}

/// A group of near-duplicates: their names (sorted) and the highest pair similarity in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    pub names: Vec<String>,
    pub score: f64,
}

fn fnv(words: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            hash ^= u64::from(b' ');
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        for byte in word.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// The words of `text`: lowercase runs of letters and digits.
fn words(text: &str) -> Vec<String> {
    let lower: String = text.chars().take(MAX_TEXT_CHARS).collect::<String>().to_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

impl Doc {
    /// `skill_md` is the whole SKILL.md; its front matter is not part of the comparison.
    pub fn new(name: &str, skill_md: &str) -> Self {
        let (_, body) = hermes_tools::skills::parse_frontmatter(skill_md);
        let words = words(&body);
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        let mut shingles: Vec<u64> = refs.windows(SHINGLE_WORDS).map(fnv).collect();
        shingles.sort_unstable();
        shingles.dedup();
        Self {
            name: name.to_string(),
            shingles,
        }
    }

    /// Whether the skill is long enough to be compared.
    pub fn comparable(&self) -> bool {
        self.shingles.len() >= MIN_SHINGLES
    }

}

/// `|A ∩ B| / |A ∪ B|`: 0 for two empty sets.
pub fn similarity(a: &Doc, b: &Doc) -> f64 {
    let (x, y) = (&a.shingles, &b.shingles);
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut common) = (0, 0, 0usize);
    while i < x.len() && j < y.len() {
        match x[i].cmp(&y[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
            }
        }
    }
    common as f64 / (x.len() + y.len() - common) as f64
}

/// Every pair at or above `threshold`: `(i, j, similarity)` with `i < j`, over comparable docs.
fn pairs(docs: &[Doc], threshold: f64) -> Vec<(usize, usize, f64)> {
    let mut out = Vec::new();
    for i in 0..docs.len() {
        if !docs[i].comparable() {
            continue;
        }
        for j in i + 1..docs.len() {
            if !docs[j].comparable() {
                continue;
            }
            let (small, large) = {
                let (a, b) = (docs[i].shingles.len(), docs[j].shingles.len());
                (a.min(b), a.max(b))
            };
            // The intersection can't pass the smaller set, nor the union go under the larger.
            if (small as f64) < threshold * large as f64 {
                continue;
            }
            let score = similarity(&docs[i], &docs[j]);
            if score >= threshold {
                out.push((i, j, score));
            }
        }
    }
    out
}

#[cfg(test)]
/// The most similar pair of comparable docs: `(similarity, a, b)`; for tuning and tests.
pub fn most_similar(docs: &[Doc]) -> Option<(f64, String, String)> {
    let mut best: Option<(f64, String, String)> = None;
    for (i, a) in docs.iter().enumerate().filter(|(_, d)| d.comparable()) {
        for b in docs[i + 1..].iter().filter(|d| d.comparable()) {
            let score = similarity(a, b);
            if best.as_ref().is_none_or(|(top, ..)| score > *top) {
                best = Some((score, a.name.clone(), b.name.clone()));
            }
        }
    }
    best
}

/// The clusters among `docs` at `threshold`: best score first, then by name. Docs beyond
/// [`MAX_SKILLS`] (after sorting by name) are left out of this run.
pub fn clusters(docs: &[Doc], threshold: f64) -> Vec<Cluster> {
    let mut docs: Vec<Doc> = docs.to_vec();
    docs.sort_by(|a, b| a.name.cmp(&b.name));
    docs.dedup_by(|a, b| a.name == b.name);
    docs.truncate(MAX_SKILLS);
    let pairs = pairs(&docs, threshold);
    // Single linkage by union-find.
    let mut parent: Vec<usize> = (0..docs.len()).collect();
    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for &(i, j, _) in &pairs {
        let (a, b) = (root(&mut parent, i), root(&mut parent, j));
        if a != b {
            parent[a.max(b)] = a.min(b);
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for &(i, j, _) in &pairs {
        let r = root(&mut parent, i);
        let members = groups.entry(r).or_default();
        for k in [i, j] {
            if !members.contains(&k) {
                members.push(k);
            }
        }
    }
    let mut out: Vec<Cluster> = groups
        .into_values()
        .map(|mut members| {
            // A group over the cap keeps the members most similar to the others (ties by name).
            let weight = |k: usize| -> f64 {
                pairs
                    .iter()
                    .filter(|(i, j, _)| *i == k || *j == k)
                    .map(|(_, _, s)| s)
                    .sum()
            };
            if members.len() > MAX_CLUSTER {
                members.sort_by(|&a, &b| {
                    weight(b)
                        .partial_cmp(&weight(a))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.cmp(&b))
                });
                members.truncate(MAX_CLUSTER);
            }
            let kept: BTreeSet<usize> = members.iter().copied().collect();
            let score = pairs
                .iter()
                .filter(|(i, j, _)| kept.contains(i) && kept.contains(j))
                .map(|(_, _, s)| *s)
                .fold(0.0, f64::max);
            Cluster {
                names: kept.iter().map(|&k| docs[k].name.clone()).collect(),
                score,
            }
        })
        .filter(|cluster| cluster.score >= threshold)
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.names.cmp(&b.names))
    });
    out
}

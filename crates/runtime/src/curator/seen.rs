//! What the curator has already asked the model about: `curator-seen.json` in the data folder (a
//! side file: the store schema is frozen).
//!
//! A cluster or a low performer the model has judged (it proposed a merge, or said "none", or its
//! reply was refused) is not asked about again for [`DAYS`] days **unless the skills changed**: the
//! key is a hash of the kind of job and of each skill's name and file. Without this, a weekly run
//! would spend its requests every week on the same proposals the user has left waiting or
//! rejected. A job whose request failed is not remembered (the back-off in `gate.rs` paces it).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// How long a judged job is left alone.
pub const DAYS: i64 = 30;
/// Entries kept (the oldest go).
const MAX_ENTRIES: usize = 300;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    key: String,
    at: String,
    what: String,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    entries: Vec<Entry>,
}

pub struct Seen {
    path: PathBuf,
}

/// The key of a job: its kind and each skill's name and the hash of its file.
pub fn key(kind: &str, skills: &[(&str, &str)]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |text: &str| {
        for byte in text.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(kind);
    for (name, fingerprint) in skills {
        feed(name);
        feed(fingerprint);
    }
    format!("{hash:016x}")
}

impl Seen {
    pub fn open(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("curator-seen.json"),
        }
    }

    fn load(&self) -> File {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Whether the job `key` was judged within the last [`DAYS`] days.
    pub fn recently(&self, key: &str, now: DateTime<Utc>) -> bool {
        self.load().entries.iter().any(|e| {
            e.key == key
                && DateTime::parse_from_rfc3339(&e.at)
                    .is_ok_and(|at| now - at.with_timezone(&Utc) < Duration::days(DAYS))
        })
    }

    /// Remembers that the job `key` was judged (`what`: a few words for whoever reads the file).
    pub fn remember(&self, key: &str, what: &str, now: DateTime<Utc>) {
        let mut file = self.load();
        file.entries.retain(|e| {
            e.key != key
                && DateTime::parse_from_rfc3339(&e.at)
                    .is_ok_and(|at| now - at.with_timezone(&Utc) < Duration::days(DAYS * 2))
        });
        file.entries.push(Entry {
            key: key.to_string(),
            at: now.to_rfc3339(),
            what: what.chars().take(200).collect(),
        });
        let excess = file.entries.len().saturating_sub(MAX_ENTRIES);
        file.entries.drain(..excess);
        let Ok(bytes) = serde_json::to_vec_pretty(&file) else {
            return;
        };
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &self.path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

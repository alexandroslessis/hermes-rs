//! Secret redaction for tool output, ported from Hermes's `agent/redact.py` (MIT): API keys with
//! known vendor prefixes, private keys, auth headers, JWTs and credentials in connection strings
//! are masked before the model (and the transcript) sees them. `KEY=value` assignments are masked
//! only where they are expected to be secrets: environment dumps and secret files (`.env`, shell
//! profiles), never ordinary source code (`MAX_TOKENS=100`).
//!
//! Masks follow Hermes: short tokens become `***`, longer ones keep their first 6 and last 4
//! characters (`sk-pro...7890`); file contents get a sentinel instead (`«redacted:ghp_…»`), which
//! can't be mistaken for a real key and written back into a config file.

use std::path::Path;
use std::sync::LazyLock;

use fancy_regex::{Captures, Regex};

/// Where the text comes from, which decides the passes and the mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Command output or other text: prefixed keys, headers, private keys, JWTs, URLs.
    Output,
    /// Output that is a list of `KEY=value` pairs (`env`, `printenv`, reading a `.env`):
    /// assignments with secret-like names are masked too.
    Assignments,
    /// File contents for the model (`read_file`, `search_files`): prefixed keys become a
    /// sentinel; assignments are masked only when the file holds secrets.
    File { secret_file: bool },
}

/// Hermes's `_PREFIX_PATTERNS`: credentials with a known vendor prefix.
const PREFIX_PATTERNS: &[&str] = &[
    r"sk-[A-Za-z0-9_-](?:\.?[A-Za-z0-9_-]){9,}",
    r"ctx7sk-[A-Za-z0-9_-]{10,}",
    r"ghp_[A-Za-z0-9]{10,}",
    r"github_pat_[A-Za-z0-9_]{10,}",
    r"gho_[A-Za-z0-9]{10,}",
    r"ghu_[A-Za-z0-9]{10,}",
    r"ghs_[A-Za-z0-9]{10,}",
    r"ghr_[A-Za-z0-9]{10,}",
    r"xapp-\d+-[A-Za-z0-9-]{10,}",
    r"xox[baprs]-[A-Za-z0-9-]{10,}",
    r"AIza[A-Za-z0-9_-]{30,}",
    r"pplx-[A-Za-z0-9]{10,}",
    r"fal_[A-Za-z0-9_-]{10,}",
    r"fc-[A-Za-z0-9]{10,}",
    r"bb_live_[A-Za-z0-9_-]{10,}",
    r"gAAAA[A-Za-z0-9_=-]{20,}",
    r"AKIA[A-Z0-9]{16}",
    r"sk_live_[A-Za-z0-9]{10,}",
    r"sk_test_[A-Za-z0-9]{10,}",
    r"rk_live_[A-Za-z0-9]{10,}",
    r"SG\.[A-Za-z0-9_-]{10,}",
    r"hf_[A-Za-z0-9]{10,}",
    r"r8_[A-Za-z0-9]{10,}",
    r"npm_[A-Za-z0-9]{10,}",
    r"pypi-[A-Za-z0-9_-]{10,}",
    r"dop_v1_[A-Za-z0-9]{10,}",
    r"doo_v1_[A-Za-z0-9]{10,}",
    r"am_(?:org_)?[A-Za-z0-9]{20,}",
    r"sk_[A-Za-z0-9_]{10,}",
    r"tvly-[A-Za-z0-9]{10,}",
    r"exa_[A-Za-z0-9]{10,}",
    r"gsk_[A-Za-z0-9]{10,}",
    r"syt_[A-Za-z0-9]{10,}",
    r"xai-[A-Za-z0-9]{30,}",
    r"ntn_[A-Za-z0-9]{10,}",
    r"fw-[A-Za-z0-9]{30,}",
    r"fw_[A-Za-z0-9]{30,}",
    r"fpk_[A-Za-z0-9]{30,}",
    r"glpat-[A-Za-z0-9_\-]{10,}",
    r"gloas-[A-Za-z0-9_\-]{10,}",
    r"gldt-[A-Za-z0-9_\-]{10,}",
    r"glrt-[A-Za-z0-9_.\-]{10,}",
    r"glcbt-[A-Za-z0-9_\-]{10,}",
    r"glptt-[A-Za-z0-9_\-]{10,}",
    r"glft-[A-Za-z0-9_\-]{10,}",
    r"pk-lf-[A-Za-z0-9\-]{8,}",
];

/// The literal start of each prefix pattern (for the sentinel's label).
fn literal_prefix(pattern: &str) -> &str {
    let end = pattern
        .find(|c: char| "[(\\.?*+|{^$".contains(c))
        .unwrap_or(pattern.len());
    &pattern[..end]
}

static PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?<![A-Za-z0-9_-])((?:{}))",
        PREFIX_PATTERNS.join("|")
    ))
    .unwrap()
});
static AUTH_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)((?:Proxy-)?Authorization:\s*)([A-Za-z][\w.+-]*\s+)?([^\s"']+)"#).unwrap()
});
static SECRET_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)((?:x-api-key|x-goog-api-key|api-key|apikey|x-api-token|x-auth-token|x-access-token)\s*:\s*)(\S+)").unwrap()
});
static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN[A-Z ]*PRIVATE KEY-----[\s\S]*?-----END[A-Z ]*PRIVATE KEY-----").unwrap()
});
static DB_CONNSTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)((?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|amqp)://[^:\s]+:)([^@\s]+)(@)",
    )
    .unwrap()
});
static URL_BARE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)((?:https?|wss?|git|ssh|ftp|ftps|sftp)://)([^\s:@/]{8,})(@[^\s]+)").unwrap()
});
static JWT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"eyJ[A-Za-z0-9_-]{10,}(?:\.[A-Za-z0-9_=-]{4,}){0,2}").unwrap());
/// `KEY=value` with a secret-like name (Hermes's `_ENV_ASSIGN_RE` and its lowercase twin).
static ENV_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?<![A-Za-z0-9_])([A-Za-z0-9_]{1,100})\s*=\s*(['"]?)(\S+?)\2(?=\s|$)"#).unwrap()
});
/// `"apiKey": "…"`-style JSON fields.
static JSON_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)("(?:[a-z0-9_]*(?:api_?key|token|secret|password|passwd|credential)s?|auth)")\s*:\s*"([^"]+)""#).unwrap()
});
/// `password: value` lines of YAML / INI secret files.
static YAML_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r##"(?im)^(\s*[\w.-]*(?:api[_-]?key|token|secret|passwd|password|credential)[\w.-]*\s*:\s*)(['"]?)([^\s'"#]+)\2"##).unwrap()
});

const SECRET_WORDS: &[&str] = &[
    "API",
    "APIKEY",
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASS",
    "PW",
    "CREDENTIAL",
    "CREDENTIALS",
    "AUTH",
];

/// Whether an assignment's name is a secret (`OPENAI_API_KEY`, `db_password`), not merely
/// containing the letters (`KEYBOARD`, `MAX_TOKENS` is still a TOKEN-word name, as in Hermes).
fn secret_name(name: &str) -> bool {
    name.split('_')
        .any(|part| SECRET_WORDS.contains(&part.to_uppercase().as_str()))
        && !name.eq_ignore_ascii_case("pass")
}

/// Hermes's `_mask_token`: `***` below 18 characters, else the first 6 and last 4.
pub fn mask(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < 18 {
        return "***".into();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}...{tail}")
}

/// Hermes's `_mask_token_nonreusable`: the vendor label only.
fn sentinel(token: &str) -> String {
    PREFIX_PATTERNS
        .iter()
        .map(|p| literal_prefix(p))
        .filter(|prefix| !prefix.is_empty() && token.starts_with(prefix))
        .max_by_key(|prefix| prefix.len())
        .map(|prefix| format!("«redacted:{prefix}…»"))
        .unwrap_or_else(|| "«redacted-secret»".into())
}

fn already_masked(value: &str) -> bool {
    value == "***" || value.contains("...") && value.len() <= 13 || value.starts_with("«redacted")
}

fn replace(regex: &Regex, text: &str, render: impl Fn(&Captures) -> String) -> String {
    regex
        .replace_all(text, |caps: &Captures| render(caps))
        .into_owned()
}

/// Masks the secrets in `text`.
pub fn redact(text: &str, source: Source) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let file = matches!(source, Source::File { .. });
    let assignments = matches!(
        source,
        Source::Assignments | Source::File { secret_file: true }
    );
    let token_mask = |token: &str| if file { sentinel(token) } else { mask(token) };
    let value_mask = |value: &str| {
        if file {
            "«redacted-secret»".to_string()
        } else {
            mask(value)
        }
    };

    let mut text = replace(&PREFIX, text, |caps| token_mask(&caps[1]));
    if assignments {
        text = replace(&ENV_ASSIGN, &text, |caps| {
            let value = &caps[3];
            if !secret_name(&caps[1]) || value.starts_with('$') || already_masked(value) {
                return caps[0].to_string();
            }
            format!("{}={}{}{}", &caps[1], &caps[2], value_mask(value), &caps[2])
        });
        text = replace(&JSON_FIELD, &text, |caps| {
            if already_masked(&caps[2]) {
                return caps[0].to_string();
            }
            format!("{}: \"{}\"", &caps[1], value_mask(&caps[2]))
        });
        text = replace(&YAML_ASSIGN, &text, |caps| {
            if already_masked(&caps[3]) || caps[3].starts_with('$') {
                return caps[0].to_string();
            }
            format!(
                "{}{}{}{}",
                &caps[1],
                &caps[2],
                value_mask(&caps[3]),
                &caps[2]
            )
        });
    }
    if text.to_lowercase().contains("authorization") {
        text = replace(&AUTH_HEADER, &text, |caps| {
            let scheme = caps.get(2).map_or("", |m| m.as_str());
            format!("{}{scheme}{}", &caps[1], mask(&caps[3]))
        });
    }
    if text.contains(':') {
        text = replace(&SECRET_HEADER, &text, |caps| {
            format!("{}{}", &caps[1], mask(&caps[2]))
        });
    }
    if text.contains("BEGIN") && text.contains("-----") {
        text = PRIVATE_KEY
            .replace_all(&text, "[REDACTED PRIVATE KEY]")
            .into_owned();
    }
    if text.contains("://") {
        text = replace(&DB_CONNSTR, &text, |caps| {
            format!("{}***{}", &caps[1], &caps[3])
        });
        text = replace(&URL_BARE_TOKEN, &text, |caps| {
            format!("{}{}{}", &caps[1], mask(&caps[2]), &caps[3])
        });
    }
    if text.contains("eyJ") {
        text = replace(&JWT, &text, |caps| mask(&caps[0]));
    }
    text
}

/// Files whose contents are secrets by nature: `.env` files, shell profiles, credential files,
/// private keys.
pub fn is_secret_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == ".env"
        || (name.starts_with(".env.") && !name.ends_with(".example") && !name.ends_with(".sample"))
        || name.ends_with(".env")
        || matches!(
            name.as_str(),
            ".bashrc"
                | ".bash_profile"
                | ".profile"
                | ".zshrc"
                | ".zprofile"
                | ".netrc"
                | ".pgpass"
                | ".npmrc"
                | ".pypirc"
                | "credentials"
                | ".git-credentials"
                | "id_rsa"
                | "id_ed25519"
                | "id_ecdsa"
                | "id_dsa"
        )
        || name.ends_with(".pem")
        || name.ends_with(".key")
}

/// Hermes's `redact_terminal_output`: the assignment pass runs only for environment dumps and
/// commands that read a secret file.
pub fn redact_terminal_output(output: &str, command: &str) -> String {
    let source = if is_env_dump(command) || reads_secret_file(command) {
        Source::Assignments
    } else {
        Source::Output
    };
    redact(output, source)
}

fn first_word(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

fn is_env_dump(command: &str) -> bool {
    let lower = command.to_lowercase();
    matches!(
        first_word(command).as_str(),
        "env" | "printenv" | "set" | "export" | "declare"
    ) || lower.contains("env:")
}

fn reads_secret_file(command: &str) -> bool {
    const READERS: &[&str] = &[
        "cat",
        "head",
        "tail",
        "type",
        "bat",
        "less",
        "more",
        "nl",
        "grep",
        "awk",
        "sed",
        "get-content",
        "gc",
    ];
    READERS.contains(&first_word(command).as_str())
        && command
            .split_whitespace()
            .skip(1)
            .any(|word| is_secret_file(Path::new(word.trim_matches(['"', '\'']))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_keys_are_masked() {
        let out = redact("key: sk-proj-abcdefghij1234567890XYZ end", Source::Output);
        assert_eq!(out, "key: sk-pro...0XYZ end");
        assert_eq!(redact("token ghp_short12345", Source::Output), "token ***");
        let file = redact(
            "OPENROUTER=sk-or-v1-abcdef1234567890abcdef",
            Source::File { secret_file: false },
        );
        assert_eq!(file, "OPENROUTER=«redacted:sk-…»");
    }

    #[test]
    fn assignments_are_masked_only_where_secrets_live() {
        let dump =
            "OPENAI_API_KEY=abc123abc123abc123abc\nMAX_TOKENS=100\nPATH=/usr/bin\nKEYBOARD=us";
        let out = redact(dump, Source::Assignments);
        assert!(out.contains("OPENAI_API_KEY=abc123...3abc"), "{out}");
        assert!(
            out.contains("PATH=/usr/bin") && out.contains("KEYBOARD=us"),
            "{out}"
        );
        // Source code keeps its constants.
        assert_eq!(redact("MAX_TOKENS=100", Source::Output), "MAX_TOKENS=100");
        let env = redact("DB_PASSWORD=hunter2\n", Source::File { secret_file: true });
        assert_eq!(env, "DB_PASSWORD=«redacted-secret»\n");
    }

    #[test]
    fn headers_keys_urls_and_jwts_are_masked() {
        let out = redact(
            "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz' https://x",
            Source::Output,
        );
        assert!(out.contains("Authorization: Bearer abcdef...wxyz"), "{out}");
        let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----";
        assert_eq!(redact(key, Source::Output), "[REDACTED PRIVATE KEY]");
        assert_eq!(
            redact("postgres://app:s3cret@db:5432/x", Source::Output),
            "postgres://app:***@db:5432/x"
        );
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJlLXZhbHVl";
        assert!(!redact(jwt, Source::Output).contains("eyJzdWIi"));
    }

    #[test]
    fn terminal_output_follows_the_command() {
        let out = "SECRET_TOKEN=abcdefghijklmnopqrstuv";
        assert!(redact_terminal_output(out, "printenv").contains("abcdef...stuv"));
        assert!(redact_terminal_output(out, "cat .env").contains("abcdef...stuv"));
        assert_eq!(redact_terminal_output(out, "cat config.py"), out);
        assert!(is_secret_file(Path::new("app/.env.local")));
        assert!(!is_secret_file(Path::new(".env.example")));
    }
}

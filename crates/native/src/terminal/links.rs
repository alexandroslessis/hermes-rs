//! Links in terminal text, with no GPUI in it: web addresses (`http://`, `https://` only) and
//! file paths with an optional line (`src/main.rs:12`, `src\main.rs:12:5`, `main.rs(12,5)`,
//! `C:\work\a.txt`). A Ctrl+click opens a web address in the browser and a file in the Files tab
//! (which applies its own rules: inside the thread's folder, no credentials); nothing is ever run.

/// A link found in a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// An `http(s)` address.
    Url(String),
    /// A file, as written (relative to the thread's folder when relative), and its line.
    File { path: String, line: Option<u32> },
}

/// Characters that end a word a link can be in.
fn is_stop(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|' | '{' | '}' | '\u{0}')
}

/// The link in `text` that covers character index `at` (an index into `text.chars()`), with the
/// character range it spans.
pub fn link_at(text: &str, at: usize) -> Option<(Link, std::ops::Range<usize>)> {
    let chars: Vec<char> = text.chars().collect();
    if at >= chars.len() || is_stop(chars[at]) {
        return None;
    }
    let mut start = at;
    while start > 0 && !is_stop(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && !is_stop(chars[end]) {
        end += 1;
    }
    let word: String = chars[start..end].iter().collect();
    // A web address may start inside the word (`(see:https://x)`).
    if let Some(found) = ["https://", "http://"]
        .iter()
        .filter_map(|scheme| word.find(scheme))
        .min()
    {
        let offset = word[..found].chars().count();
        let url = trim_url(&word[found..]);
        let (s, e) = (start + offset, start + offset + url.chars().count());
        if url.len() > "https://".len() && (s..e).contains(&at) {
            return Some((Link::Url(url), s..e));
        }
        return None;
    }
    let (path, line, consumed) = file_of(&word)?;
    let lead = word.chars().take_while(|c| matches!(c, '(' | '[')).count();
    let (s, e) = (start + lead, start + lead + consumed);
    (s..e)
        .contains(&at)
        .then_some((Link::File { path, line }, s..e))
}

/// A web address without the punctuation that usually follows one in prose (`.`, `,`, a closing
/// bracket that was not opened in it).
fn trim_url(url: &str) -> String {
    let mut url = url.to_string();
    loop {
        let Some(last) = url.chars().last() else {
            break;
        };
        let unbalanced = |open: char, close: char| {
            last == close && url.matches(open).count() < url.matches(close).count()
        };
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
        {
            url.pop();
        } else {
            break;
        }
    }
    url
}

/// `(path, line, characters used)` when `word` looks like a file: it has a path separator or a
/// file extension, and may end in `:line`, `:line:col` or `(line,col)`.
fn file_of(word: &str) -> Option<(String, Option<u32>, usize)> {
    let word = word.trim_start_matches(['(', '[']);
    let word = word.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    // `name(12,5)` (compilers on Windows).
    let paren = word
        .rfind('(')
        .filter(|_| word.ends_with(')'))
        .and_then(|open| {
            let inner = &word[open + 1..word.len() - 1];
            let line: u32 = inner.split(',').next()?.trim().parse().ok()?;
            Some((&word[..open], Some(line)))
        });
    let word = if paren.is_some() {
        word
    } else {
        word.trim_end_matches([')', ']', '.', ',', ';', ':'])
    };
    let (body, line) = if let Some(found) = paren {
        found
    } else {
        // `name:12` or `name:12:5`, but not a drive letter's colon.
        let mut body = word;
        let mut line = None;
        let parts: Vec<&str> = word.rsplitn(3, ':').collect();
        let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if parts.len() == 3 && numeric(parts[0]) && numeric(parts[1]) && parts[2].len() > 1 {
            body = parts[2];
            line = parts[1].parse().ok();
        } else if parts.len() >= 2 && numeric(parts[0]) && word.len() > parts[0].len() + 2 {
            body = &word[..word.len() - parts[0].len() - 1];
            line = parts[0].parse().ok();
        }
        (body, line)
    };
    let body = body.trim_end_matches(':');
    if body.is_empty() || body.contains("://") {
        return None;
    }
    let name = body.rsplit(['/', '\\']).next().unwrap_or(body);
    let has_separator = body.contains('/') || body.contains('\\');
    let has_extension = name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=8).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
    });
    // A path needs letters in it (not `1/2` or `../`), and either a separator or an extension.
    let has_letters = body.chars().any(char::is_alphabetic);
    if !has_letters || !(has_separator || has_extension) {
        return None;
    }
    // A version number or a time is not a file (`v1.2`, `3.14`).
    if !has_separator && name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let used = word.chars().count();
    Some((body.to_string(), line, used))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, needle: &str) -> Option<Link> {
        let ix = text.find(needle).unwrap();
        link_at(text, text[..ix].chars().count()).map(|(l, _)| l)
    }

    #[test]
    fn web_addresses_without_trailing_punctuation() {
        assert_eq!(
            at("see https://example.com/a?b=1. next", "example"),
            Some(Link::Url("https://example.com/a?b=1".into()))
        );
        assert_eq!(
            at("(docs: http://x.org/wiki/A_(b))", "x.org"),
            Some(Link::Url("http://x.org/wiki/A_(b)".into()))
        );
        assert_eq!(
            at("<https://example.com>", "https"),
            Some(Link::Url("https://example.com".into()))
        );
        // Other schemes are not links.
        assert_eq!(at("file:///c:/x run", "file"), None);
        assert_eq!(at("ftp://host/x", "ftp"), None);
    }

    #[test]
    fn file_paths_with_lines() {
        let file = |path: &str, line: Option<u32>| {
            Some(Link::File {
                path: path.into(),
                line,
            })
        };
        assert_eq!(
            at("error at src/main.rs:12:5: bad", "main"),
            file("src/main.rs", Some(12))
        );
        assert_eq!(
            at("  --> crates\\native\\src\\lib.rs:40", "lib"),
            file("crates\\native\\src\\lib.rs", Some(40))
        );
        assert_eq!(
            at(r"C:\work\a.txt(7,3): warning", "work"),
            file(r"C:\work\a.txt", Some(7))
        );
        assert_eq!(at("open README.md.", "README"), file("README.md", None));
        assert_eq!(at("see ./docs/guide", "docs"), file("./docs/guide", None));
        assert_eq!(
            at("PS C:\\Users\\me> ", "Users"),
            file("C:\\Users\\me", None)
        );
        // Not files: numbers, versions, prose.
        assert_eq!(at("took 3.14 s", "3.14"), None);
        assert_eq!(at("ratio 1/2 done", "1/2"), None);
        assert_eq!(at("hello world", "world"), None);
        assert_eq!(at("v1.2.3", "v1"), None);
    }
}

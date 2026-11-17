use unicode_normalization::UnicodeNormalization;

fn is_in_unicode_tag_range(c: char) -> bool {
    matches!(c, '\u{E0000}'..='\u{E007F}')
}

pub fn sanitize_unicode_tags(text: &str) -> String {
    let normalized: String = text.nfc().collect();
    strip_unicode_tags(&normalized)
}

pub fn strip_unicode_tags(text: &str) -> String {
    text.chars()
        .filter(|&c| !is_in_unicode_tag_range(c))
        .collect()
}

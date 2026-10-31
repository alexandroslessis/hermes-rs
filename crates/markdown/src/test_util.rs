//! Test helper copied from Zed's `util::test::marked_text` (GPL-3.0-or-later).

use std::ops::Range;

#[track_caller]
pub fn marked_text_ranges(
    marked_text: &str,
    ranges_are_directed: bool,
) -> (String, Vec<Range<usize>>) {
    let mut unmarked_text = String::with_capacity(marked_text.len());
    let mut ranges = Vec::new();
    let mut prev_marked_ix = 0;
    let mut current_range_start = None;
    let mut current_range_cursor = None;

    let marked_text = marked_text.replace('•', " ");
    for (marked_ix, marker) in marked_text.match_indices(&['«', '»', 'ˇ']) {
        unmarked_text.push_str(&marked_text[prev_marked_ix..marked_ix]);
        let unmarked_len = unmarked_text.len();
        let len = marker.len();
        prev_marked_ix = marked_ix + len;

        match marker {
            "ˇ" => {
                if current_range_start.is_some() {
                    if current_range_cursor.is_some() {
                        panic!("duplicate point marker 'ˇ' at index {marked_ix}");
                    }

                    current_range_cursor = Some(unmarked_len);
                } else {
                    ranges.push(unmarked_len..unmarked_len);
                }
            }
            "«" => {
                if current_range_start.is_some() {
                    panic!("unexpected range start marker '«' at index {marked_ix}");
                }
                current_range_start = Some(unmarked_len);
            }
            "»" => {
                let current_range_start = if let Some(start) = current_range_start.take() {
                    start
                } else {
                    panic!("unexpected range end marker '»' at index {marked_ix}");
                };

                let mut reversed = false;
                if let Some(current_range_cursor) = current_range_cursor.take() {
                    if current_range_cursor == current_range_start {
                        reversed = true;
                    } else if current_range_cursor != unmarked_len {
                        panic!("unexpected 'ˇ' marker in the middle of a range");
                    }
                } else if ranges_are_directed {
                    panic!("missing 'ˇ' marker to indicate range direction");
                }

                ranges.push(if reversed {
                    unmarked_len..current_range_start
                } else {
                    current_range_start..unmarked_len
                });
            }
            _ => unreachable!(),
        }
    }

    unmarked_text.push_str(&marked_text[prev_marked_ix..]);
    (unmarked_text, ranges)
}

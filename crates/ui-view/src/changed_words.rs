//! Changed words within modified lines (`open` → `open_with_flags`): each
//! removed line of a change is paired with the added line most like it, and
//! the words that differ between the two are marked. Lines with no partner,
//! a line written from nothing, carry no marks: the whole line is the
//! change.
//!
//! Worked out here, from a hunk's lines, by a longest common subsequence
//! over words, rather than with a diff crate: the shared view layer keeps
//! its dependencies as they are, since every client builds it.

use std::ops::Range;

use crate::review::LineKind;

/// The least likeness, from 0 to 1, a removed and an added line need to be
/// read as one line edited rather than one line replaced by another: below
/// it, marking words would mark nearly all of both.
const PAIRED_AT: f64 = 0.5;

/// Lines with more words than this are not compared: the comparison's cost
/// grows with the product of two lines' lengths, and a line that long is
/// generated or minified, where marked words help nobody.
const MOST_WORDS: usize = 500;

/// A change with more removed lines times added lines than this looks only
/// at the few added lines after the last pairing for each removed one, so a
/// rewritten file's one large change stays cheap to compare.
const FULL_SEARCH: usize = 256;

/// How many added lines a removed one is compared with when the change is
/// too large to compare every pair.
const WINDOW: usize = 8;

/// For each of a hunk's lines, in order, the stretches of its text that
/// changed, in UTF-8 bytes from the line's start: on a removed line the
/// words its partner no longer has, on an added line the words its partner
/// did not have. Context lines and lines with no partner have none.
///
/// A change is a run of lines that are not context; its removed lines are
/// paired in order with its added lines, each with the most alike of those
/// after the last one paired, when they are alike enough.
pub fn changed_words(lines: &[(LineKind, &str)]) -> Vec<Vec<Range<usize>>> {
    let mut out = vec![Vec::new(); lines.len()];
    let mut at = 0;
    while at < lines.len() {
        if lines[at].0 == LineKind::Context {
            at += 1;
            continue;
        }
        let end = lines[at..]
            .iter()
            .position(|(kind, _)| *kind == LineKind::Context)
            .map_or(lines.len(), |length| at + length);
        let removed: Vec<usize> = (at..end)
            .filter(|&i| lines[i].0 == LineKind::Removed)
            .collect();
        let added: Vec<usize> = (at..end)
            .filter(|&i| lines[i].0 == LineKind::Added)
            .collect();
        for (old, new) in pairs(lines, &removed, &added) {
            let (old_spans, new_spans) = word_changes(lines[old].1, lines[new].1);
            out[old] = old_spans;
            out[new] = new_spans;
        }
        at = end;
    }
    out
}

/// The removed and added lines read as edits of each other, by index into
/// `lines`, in order.
fn pairs(lines: &[(LineKind, &str)], removed: &[usize], added: &[usize]) -> Vec<(usize, usize)> {
    let full = removed.len() * added.len() <= FULL_SEARCH;
    let added_words: Vec<Vec<Word>> = added.iter().map(|&i| words(lines[i].1)).collect();
    let mut out = Vec::new();
    let mut next = 0;
    for &old in removed {
        let old_words = words(lines[old].1);
        let last = if full {
            added.len()
        } else {
            (next + WINDOW).min(added.len())
        };
        let mut best: Option<(usize, f64)> = None;
        for candidate in next..last {
            let likeness = likeness(
                lines[old].1,
                &old_words,
                lines[added[candidate]].1,
                &added_words[candidate],
            );
            if likeness >= PAIRED_AT && best.is_none_or(|(_, most)| likeness > most) {
                best = Some((candidate, likeness));
            }
        }
        if let Some((candidate, _)) = best {
            out.push((old, added[candidate]));
            next = candidate + 1;
        }
    }
    out
}

/// The changed stretches of an edited line, on its old text and its new.
fn word_changes(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let old_words = words(old);
    let new_words = words(new);
    if old_words.len() > MOST_WORDS || new_words.len() > MOST_WORDS {
        return (Vec::new(), Vec::new());
    }
    let (old_kept, new_kept) = common(old, &old_words, new, &new_words);
    (
        spans(old, &old_words, &old_kept),
        spans(new, &new_words, &new_kept),
    )
}

/// How alike two lines are, from 0 to 1: twice the bytes of the words they
/// have in common, in order, over the bytes of both, leaving out spaces.
fn likeness(old: &str, old_words: &[Word], new: &str, new_words: &[Word]) -> f64 {
    if old_words.len() > MOST_WORDS || new_words.len() > MOST_WORDS {
        return 0.0;
    }
    let ink = |words: &[Word]| -> usize {
        words
            .iter()
            .filter(|word| !word.space)
            .map(|word| word.range.len())
            .sum()
    };
    let total = ink(old_words) + ink(new_words);
    if total == 0 {
        return 0.0;
    }
    let (kept, _) = common(old, old_words, new, new_words);
    let shared: usize = old_words
        .iter()
        .zip(&kept)
        .filter(|(word, kept)| **kept && !word.space)
        .map(|(word, _)| word.range.len())
        .sum();
    (2 * shared) as f64 / total as f64
}

/// Which words of each line belong to their longest common subsequence.
fn common(old: &str, old_words: &[Word], new: &str, new_words: &[Word]) -> (Vec<bool>, Vec<bool>) {
    let (n, m) = (old_words.len(), new_words.len());
    let same =
        |i: usize, j: usize| old[old_words[i].range.clone()] == new[new_words[j].range.clone()];
    // `table[i][j]` is the length of the longest common subsequence of the
    // words from i and from j on.
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if same(i, j) {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut old_kept = vec![false; n];
    let mut new_kept = vec![false; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if same(i, j) {
            old_kept[i] = true;
            new_kept[j] = true;
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (old_kept, new_kept)
}

/// The stretches the words not kept cover, joined across the spaces between
/// them; a stretch of spaces alone is no change worth marking.
fn spans(text: &str, words: &[Word], kept: &[bool]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (word, kept) in words.iter().zip(kept) {
        if *kept || word.space {
            continue;
        }
        match out.last_mut() {
            Some(last) if text[last.end..word.range.start].trim().is_empty() => {
                last.end = word.range.end;
            }
            _ => out.push(word.range.clone()),
        }
    }
    out
}

/// A word of a line: a run of letters, digits and underscores, a run of
/// spaces, or any other single character.
struct Word {
    range: Range<usize>,
    space: bool,
}

fn words(text: &str) -> Vec<Word> {
    #[derive(PartialEq)]
    enum Class {
        Name,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Name
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut out: Vec<Word> = Vec::new();
    let mut last: Option<Class> = None;
    for (at, c) in text.char_indices() {
        let this = class(c);
        let joins = this != Class::Other && last.as_ref() == Some(&this);
        if joins && let Some(word) = out.last_mut() {
            word.range.end = at + c.len_utf8();
        } else {
            out.push(Word {
                range: at..at + c.len_utf8(),
                space: this == Class::Space,
            });
        }
        last = Some(this);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked<'a>(text: &'a str, spans: &[Range<usize>]) -> Vec<&'a str> {
        spans.iter().map(|span| &text[span.clone()]).collect()
    }

    #[test]
    fn a_renamed_call_marks_the_name_and_the_new_argument() {
        let old = "    let file = open(&path)?;";
        let new = "    let file = open_with_flags(&path, flags)?;";
        let spans = changed_words(&[(LineKind::Removed, old), (LineKind::Added, new)]);
        assert_eq!(marked(old, &spans[0]), ["open"]);
        assert_eq!(marked(new, &spans[1]), ["open_with_flags", ", flags"]);
    }

    #[test]
    fn unlike_lines_and_lines_with_no_partner_are_not_marked() {
        let lines = [
            (LineKind::Context, "fn a() {"),
            (LineKind::Removed, "    return compute_total(items);"),
            (LineKind::Added, "    // nothing alike here at all"),
            (LineKind::Added, "    let extra = 1;"),
            (LineKind::Context, "}"),
        ];
        assert!(changed_words(&lines).iter().all(Vec::is_empty));
    }

    #[test]
    fn each_removed_line_takes_the_most_alike_added_line_in_order() {
        let lines = [
            (LineKind::Removed, "let a = 1;"),
            (LineKind::Removed, "let b = 2;"),
            (LineKind::Added, "let inserted = 0;"),
            (LineKind::Added, "let a = 10;"),
            (LineKind::Added, "let b = 20;"),
        ];
        let spans = changed_words(&lines);
        assert_eq!(marked(lines[0].1, &spans[0]), ["1"]);
        assert_eq!(marked(lines[1].1, &spans[1]), ["2"]);
        assert!(spans[2].is_empty(), "the inserted line has no partner");
        assert_eq!(marked(lines[3].1, &spans[3]), ["10"]);
        assert_eq!(marked(lines[4].1, &spans[4]), ["20"]);
    }

    #[test]
    fn spans_are_bytes_of_text_past_ascii() {
        let old = "say(\"héllo\")";
        let new = "say(\"héllo wörld\")";
        let spans = changed_words(&[(LineKind::Removed, old), (LineKind::Added, new)]);
        assert!(spans[0].is_empty());
        assert_eq!(marked(new, &spans[1]), ["wörld"]);
    }

    #[test]
    fn a_change_of_spacing_alone_marks_nothing() {
        let spans = changed_words(&[(LineKind::Removed, "a  +  b"), (LineKind::Added, "a + b")]);
        assert!(spans.iter().all(Vec::is_empty));
    }
}

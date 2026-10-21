//! Edit a PDF ▸ Find and replace: every match of a phrase in the pages' text, rewritten in its
//! paragraph (its font, size and colour kept, rewrapped in its box), as one change.

use std::ops::Range;

use pdfcraft_cos::Document;

use crate::EditError;
use crate::text::{BlockStyle, rewrite_block, text_blocks};

/// More than this many replacements in one go is refused (each rewrites a paragraph).
pub const MAX_REPLACEMENTS: usize = 10_000;
/// The longest phrase looked for, in characters.
const MAX_FIND_CHARS: usize = 1_000;

/// How a phrase is matched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchOptions {
    /// Upper and lower case differ.
    pub match_case: bool,
    /// Only where the phrase is whole words (no letter or digit just before or after it).
    pub whole_words: bool,
}

fn same(a: char, b: char, case: bool) -> bool {
    a == b || (!case && a.to_lowercase().eq(b.to_lowercase()))
}

/// Where `find` reads in `text`, as byte ranges, none overlapping. A space in `find` matches any
/// run of white space (a paragraph's line breaks included).
pub fn find_matches(text: &str, find: &str, options: MatchOptions) -> Vec<Range<usize>> {
    let wanted: Vec<char> = find.trim().chars().collect();
    if wanted.is_empty() || wanted.len() > MAX_FIND_CHARS {
        return Vec::new();
    }
    let hay: Vec<(usize, char)> = text.char_indices().collect();
    let end_of = |i: usize| hay.get(i).map_or(text.len(), |(at, _)| *at);
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut start = 0;
    while start < hay.len() {
        // Try to match the phrase from `start`; `k` walks the text, `w` the phrase.
        let (mut k, mut w) = (start, 0);
        let mut ok = true;
        while w < wanted.len() {
            let Some(&want) = wanted.get(w) else { break };
            if want.is_whitespace() {
                let from = k;
                while hay.get(k).is_some_and(|(_, c)| c.is_whitespace()) {
                    k += 1;
                }
                while wanted.get(w).is_some_and(|c| c.is_whitespace()) {
                    w += 1;
                }
                if k == from {
                    ok = false;
                    break;
                }
                continue;
            }
            match hay.get(k) {
                Some((_, c)) if same(*c, want, options.match_case) => {
                    k += 1;
                    w += 1;
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && options.whole_words {
            let before = start.checked_sub(1).and_then(|i| hay.get(i)).is_some_and(|(_, c)| word(*c));
            let after = hay.get(k).is_some_and(|(_, c)| word(*c));
            ok = !before && !after;
        }
        if ok && k > start {
            out.push(end_of(start)..end_of(k));
            start = k;
        } else {
            start += 1;
        }
    }
    out
}

/// How many matches of `find` the paragraphs of `pages` hold.
pub fn count_matches(doc: &Document, pages: &[usize], find: &str, options: MatchOptions) -> Result<usize, EditError> {
    let mut n = 0usize;
    for &page in pages {
        for block in text_blocks(doc, page)? {
            n = n.saturating_add(find_matches(&block.editor_text, find, options).len());
        }
    }
    Ok(n)
}

/// Replace every match of `find` on `pages` with `replace`, each paragraph rewritten once. All
/// or nothing: a paragraph whose fonts cannot show the replacement refuses the whole change.
/// Returns how many matches were replaced.
pub fn find_replace(doc: &mut Document, pages: &[usize], find: &str, replace: &str, options: MatchOptions) -> Result<usize, EditError> {
    if find.trim().is_empty() {
        return Err(EditError::Invalid("type the text to find".into()));
    }
    if find.trim().chars().count() > MAX_FIND_CHARS || replace.chars().count() > MAX_FIND_CHARS * 10 {
        return Err(EditError::Invalid("the text to find or its replacement is too long".into()));
    }
    let total = count_matches(doc, pages, find, options)?;
    if total > MAX_REPLACEMENTS {
        return Err(EditError::Invalid(format!("{total} matches is more than one change may replace ({MAX_REPLACEMENTS})")));
    }
    let mut work = doc.clone();
    let mut done = 0usize;
    for &page in pages {
        let blocks = text_blocks(&work, page)?;
        // Later paragraphs first: a rewrite never renumbers the ones before it.
        for (index, block) in blocks.iter().enumerate().rev() {
            let found = find_matches(&block.editor_text, find, options);
            if found.is_empty() {
                continue;
            }
            let mut text = block.editor_text.clone();
            for range in found.iter().rev() {
                if text.is_char_boundary(range.start) && text.is_char_boundary(range.end) && range.start <= range.end {
                    text.replace_range(range.clone(), replace);
                }
            }
            rewrite_block(&mut work, page, index, Some(&text), &BlockStyle::default())?;
            done = done.saturating_add(found.len());
        }
    }
    if done == 0 {
        return Err(EditError::Invalid(format!("{find:?} was not found")));
    }
    *doc = work;
    Ok(done)
}

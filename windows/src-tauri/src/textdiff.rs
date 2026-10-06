// Word-level diff, used to turn a model's corrected text into Word tracked
// changes, to count corrections, and to refuse answers that rewrote the text
// instead of correcting it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Equal,
    Delete,
    Insert,
}

/// Splits into words, whitespace runs and single punctuation marks, so a
/// fixed accent or comma changes one small token and not a whole sentence.
pub fn tokenize(text: &str) -> Vec<&str> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Punct,
    }
    let class = |c: char| {
        if c.is_alphanumeric() {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Punct
        }
    };
    let mut out = Vec::new();
    let mut start = 0;
    let mut current: Option<Class> = None;
    for (i, c) in text.char_indices() {
        let k = class(c);
        let split = match &current {
            None => false,
            Some(Class::Punct) => true, // every mark is its own token
            Some(prev) => *prev != k,
        };
        if split {
            out.push(&text[start..i]);
            start = i;
        }
        current = Some(k);
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Above this many LCS cells the diff gives up and reports a full replace.
const MAX_CELLS: usize = 9_000_000;

/// Edit script from `a` to `b`, consecutive operations merged.
pub fn diff(a: &str, b: &str) -> Vec<(Op, String)> {
    let ta = tokenize(a);
    let tb = tokenize(b);

    // Common prefix / suffix first: corrections are sparse, so this usually
    // leaves only small islands for the quadratic part.
    let mut pre = 0;
    while pre < ta.len() && pre < tb.len() && ta[pre] == tb[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < ta.len() - pre && suf < tb.len() - pre && ta[ta.len() - 1 - suf] == tb[tb.len() - 1 - suf] {
        suf += 1;
    }
    let ma = &ta[pre..ta.len() - suf];
    let mb = &tb[pre..tb.len() - suf];

    let mut ops: Vec<(Op, &str)> = Vec::new();
    for t in &ta[..pre] {
        ops.push((Op::Equal, t));
    }

    if ma.len().saturating_mul(mb.len()) > MAX_CELLS {
        for t in ma {
            ops.push((Op::Delete, t));
        }
        for t in mb {
            ops.push((Op::Insert, t));
        }
    } else {
        let (n, m) = (ma.len(), mb.len());
        // lcs[i][j] = LCS length of ma[i..] and mb[j..]
        let w = m + 1;
        let mut lcs = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i * w + j] = if ma[i] == mb[j] {
                    lcs[(i + 1) * w + j + 1] + 1
                } else {
                    lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if ma[i] == mb[j] {
                ops.push((Op::Equal, ma[i]));
                i += 1;
                j += 1;
            } else if lcs[(i + 1) * w + j] >= lcs[i * w + j + 1] {
                ops.push((Op::Delete, ma[i]));
                i += 1;
            } else {
                ops.push((Op::Insert, mb[j]));
                j += 1;
            }
        }
        for t in &ma[i..] {
            ops.push((Op::Delete, t));
        }
        for t in &mb[j..] {
            ops.push((Op::Insert, t));
        }
    }

    for t in &ta[ta.len() - suf..] {
        ops.push((Op::Equal, t));
    }
    merge(ops)
}

/// Merges runs, and folds a whitespace-only Equal squeezed between two edits
/// into them ("teh cat" → "the  dog" reads better as one change than three).
fn merge(ops: Vec<(Op, &str)>) -> Vec<(Op, String)> {
    let mut out: Vec<(Op, String)> = Vec::new();
    for (op, t) in ops {
        match out.last_mut() {
            Some((last, s)) if *last == op => s.push_str(t),
            _ => out.push((op, t.to_string())),
        }
    }
    // Reorder Delete/Insert pairs so deletions always come first: that is what
    // Word shows as a replacement.
    let mut i = 0;
    while i + 1 < out.len() {
        if out[i].0 == Op::Insert && out[i + 1].0 == Op::Delete {
            out.swap(i, i + 1);
        }
        i += 1;
    }
    out
}

/// Number of separate corrections (a delete followed by an insert is one).
pub fn change_count(ops: &[(Op, String)]) -> usize {
    let mut n = 0;
    let mut in_change = false;
    for (op, _) in ops {
        if *op == Op::Equal {
            in_change = false;
        } else if !in_change {
            n += 1;
            in_change = true;
        }
    }
    n
}

/// Share of the original's characters kept unchanged (0..1). A proofreading
/// answer keeps most of the text; a rewrite or a hallucination does not.
pub fn similarity(a: &str, ops: &[(Op, String)]) -> f64 {
    let total = a.chars().filter(|c| !c.is_whitespace()).count().max(1);
    let kept: usize = ops
        .iter()
        .filter(|(op, _)| *op == Op::Equal)
        .map(|(_, s)| s.chars().filter(|c| !c.is_whitespace()).count())
        .sum();
    kept as f64 / total as f64
}

/// True when `corrected` looks like a correction of `original` and not a
/// rewrite, a translation or an answer to the text.
pub fn looks_like_correction(original: &str, corrected: &str) -> bool {
    let (lo, lc) = (original.chars().count() as f64, corrected.chars().count() as f64);
    if lc == 0.0 {
        return false;
    }
    let ratio = lc / lo.max(1.0);
    if !(0.6..=1.6).contains(&ratio) && lo > 20.0 {
        return false;
    }
    let ops = diff(original, corrected);
    // Very short texts ("Ca va?" → "Ça va ?") legitimately change a lot.
    let floor = if lo < 40.0 { 0.3 } else { 0.55 };
    similarity(original, &ops) >= floor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(ops: &[(Op, String)], old: bool) -> String {
        ops.iter()
            .filter(|(op, _)| *op == Op::Equal || (*op == Op::Delete) == old)
            .map(|(_, s)| s.as_str())
            .collect()
    }

    #[test]
    fn diff_round_trips() {
        let a = "Bonjour, ceci est un tst avec des fotes d'ortographe.";
        let b = "Bonjour, ceci est un test avec des fautes d'orthographe.";
        let ops = diff(a, b);
        assert_eq!(apply(&ops, true), a);
        assert_eq!(apply(&ops, false), b);
        assert_eq!(change_count(&ops), 3);
        assert!(looks_like_correction(a, b));
    }

    #[test]
    fn unchanged_text_has_no_changes() {
        let ops = diff("rien à changer", "rien à changer");
        assert_eq!(change_count(&ops), 0);
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn rewrites_are_refused() {
        let a = "Il sont venu hier soir pour manger avec nous a la maison.";
        let b = "Here is the corrected text: they came yesterday evening to eat with us at home.";
        assert!(!looks_like_correction(a, b));
        assert!(!looks_like_correction(a, ""));
    }

    #[test]
    fn deletes_come_before_inserts() {
        let ops = diff("un chat noir", "un chien noir");
        let kinds: Vec<Op> = ops.iter().map(|(o, _)| *o).collect();
        assert_eq!(kinds, vec![Op::Equal, Op::Delete, Op::Insert, Op::Equal]);
    }
}

//! Random texts and patterns, compared with a naive oracle: split the text into lines and ask the
//! `regex` crate about each line separately. If the whole-buffer search or the line rewriting ever
//! lets a match cross a line break, or mislocates a line, the selected lines differ.

use gx::matcher::{Matcher, PatternOptions};
use gx::search::{search, SearchOptions};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

fn random_text(r: &mut Rng) -> String {
    let alphabet = ['a', 'b', 'c', ' ', '\t', 'a', 'b', '_', '1'];
    let lines = r.below(14);
    let mut t = String::new();
    for _ in 0..lines {
        for _ in 0..r.below(12) {
            t.push(alphabet[r.below(alphabet.len())]);
        }
        t.push('\n');
    }
    if r.chance(25) && !t.is_empty() {
        t.pop(); // no final newline
    }
    t
}

fn random_pattern(r: &mut Rng) -> String {
    let atoms = ["a", "b", "c", "ab", " ", ".", "[ab]", "[^a]", r"\s", r"\w", r"\W", r"\d", "(a|b)", "(ab|c)", r"\S", "[a-c]", "_"];
    let quant = ["", "", "", "*", "+", "?", "{1,2}", "*?"];
    let mut p = String::new();
    if r.chance(15) {
        p.push('^');
    } else if r.chance(8) {
        p.push_str(r"\A");
    }
    for _ in 0..1 + r.below(4) {
        p.push_str(atoms[r.below(atoms.len())]);
        p.push_str(quant[r.below(quant.len())]);
        if r.chance(8) {
            p.push_str(r"\b");
        }
    }
    if r.chance(15) {
        p.push('$');
    } else if r.chance(8) {
        p.push_str(r"\z");
    }
    if r.chance(10) {
        p = format!("{p}|{}", atoms[r.below(atoms.len())]);
    }
    p
}

fn oracle_lines(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') || text.is_empty() {
        v.pop();
    }
    v
}

fn oracle_selected(text: &str, pattern: &str, ci: bool, word: bool, line: bool, invert: bool) -> Vec<usize> {
    let mut p = format!("(?:{pattern})");
    if word {
        p = format!(r"\b{p}\b");
    }
    if line {
        p = format!("^{p}$");
    }
    let flags = if ci { "(?mi)" } else { "(?m)" };
    let re = regex::Regex::new(&format!("{flags}{p}")).unwrap();
    oracle_lines(text).iter().enumerate().filter(|(_, l)| re.is_match(l) != invert).map(|(i, _)| i).collect()
}

/// Context output built the naive way: mark every line to show, then print them in order.
fn oracle_context(text: &str, selected: &[usize], before: usize, after: usize) -> String {
    let lines = oracle_lines(text);
    let mut show = vec![false; lines.len()];
    let mut is_sel = vec![false; lines.len()];
    for &i in selected {
        is_sel[i] = true;
        let (from, to) = (i.saturating_sub(before), (i + after).min(lines.len().saturating_sub(1)));
        show[from..=to].iter_mut().for_each(|s| *s = true);
    }
    let mut out = String::new();
    let mut last: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if !show[i] {
            continue;
        }
        if let Some(prev) = last {
            if i > prev + 1 && (before > 0 || after > 0) {
                out.push_str("--\n");
            }
        }
        out.push_str(&format!("{}{}{l}\n", i + 1, if is_sel[i] { ':' } else { '-' }));
        last = Some(i);
    }
    out
}

#[test]
fn selected_lines_and_context_match_the_oracle() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut compared = 0;
    let mut with_matches = 0;
    for case in 0..6000 {
        let text = random_text(&mut rng);
        let pattern = random_pattern(&mut rng);
        let (ci, word, line, invert) = (rng.chance(25), rng.chance(20), rng.chance(10), rng.chance(25));
        let (before, after) = if rng.chance(40) { (rng.below(3), rng.below(3)) } else { (0, 0) };

        let m = Matcher::new(std::slice::from_ref(&pattern), &PatternOptions { ignore_case: ci, word, line, ..Default::default() }).unwrap();
        let o = SearchOptions { line_numbers: true, invert, before, after, ..Default::default() };
        let mut out = Vec::new();
        let got = search("f", text.as_bytes(), &m, &o, &mut out);

        let want = oracle_selected(&text, &pattern, ci, word, line, invert);
        let ctx = format!("case {case}: text={text:?} pattern={pattern:?} ci={ci} word={word} line={line} invert={invert} ctx={before}/{after}");
        assert_eq!(got.selected as usize, want.len(), "count differs, {ctx}");
        assert_eq!(String::from_utf8(out).unwrap(), oracle_context(&text, &want, before, after), "output differs, {ctx}");
        compared += 1;
        if !want.is_empty() {
            with_matches += 1;
        }
    }
    // The generator must produce a healthy mix of hits and misses, or the test proves little.
    assert_eq!(compared, 6000);
    assert!((1500..5500).contains(&with_matches), "{with_matches} of {compared} cases had a selected line");
}

#[test]
fn the_literal_fast_path_agrees_with_the_oracle() {
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    for case in 0..2000 {
        let text = random_text(&mut rng);
        let needle: String = (0..1 + rng.below(3)).map(|_| ['a', 'b', 'c', ' '][rng.below(4)]).collect();
        let m = Matcher::new(std::slice::from_ref(&needle), &PatternOptions { fixed: true, ..Default::default() }).unwrap();
        assert!(m.is_literal());
        let o = SearchOptions { line_numbers: true, ..Default::default() };
        let mut out = Vec::new();
        let got = search("f", text.as_bytes(), &m, &o, &mut out);
        let want: Vec<usize> = oracle_lines(&text).iter().enumerate().filter(|(_, l)| l.contains(&needle)).map(|(i, _)| i).collect();
        assert_eq!(got.selected as usize, want.len(), "case {case}: text={text:?} needle={needle:?}");
        assert_eq!(String::from_utf8(out).unwrap(), oracle_context(&text, &want, 0, 0), "case {case}");
    }
}

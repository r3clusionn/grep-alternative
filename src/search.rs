//! Searching one buffer and formatting the result.

use memchr::{memchr, memchr_iter, memrchr};

use crate::matcher::Matcher;

#[derive(Clone, Debug, Default)]
pub struct SearchOptions {
    /// Select the lines that do not match.
    pub invert: bool,
    /// Only count the selected lines.
    pub count: bool,
    /// Stop at the first selected line (for `-l` and `-q`).
    pub first_only: bool,
    /// Print only the matching parts of each selected line.
    pub only_matching: bool,
    pub line_numbers: bool,
    pub before: usize,
    pub after: usize,
    /// Stop after this many selected lines.
    pub max_count: Option<u64>,
    pub color: bool,
    /// Prefix every output line with the file label.
    pub with_filename: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Lines selected (matching lines, or non-matching with `invert`).
    pub selected: u64,
    /// Separate matches printed with `only_matching`.
    pub matches: u64,
}

const PATH: &[u8] = b"\x1b[35m";
const LINENO: &[u8] = b"\x1b[32m";
const SEP: &[u8] = b"\x1b[36m";
const MATCH: &[u8] = b"\x1b[1;31m";
const RESET: &[u8] = b"\x1b[0m";

/// Yields the selected lines of a buffer in order, as (start, end) with `end` at the newline.
struct Selector<'a> {
    buf: &'a [u8],
    m: &'a Matcher,
    invert: bool,
    pos: usize,
    /// For `invert`: the next matching line at or after `pos`, once it has been looked up.
    next_match: Option<Option<(usize, usize)>>,
}

impl<'a> Selector<'a> {
    fn new(buf: &'a [u8], m: &'a Matcher, invert: bool) -> Self {
        Selector { buf, m, invert, pos: 0, next_match: None }
    }

    fn line_end(&self, from: usize) -> usize {
        memchr(b'\n', &self.buf[from..]).map_or(self.buf.len(), |i| from + i)
    }

    /// The first line at or after `from` (a line start) that the matcher matches.
    fn find_line(&self, from: usize) -> Option<(usize, usize)> {
        let (s, e) = self.m.find_at(self.buf, from)?;
        // `^` matches after a final newline too, but there is no line there.
        if s >= self.buf.len() && self.buf.last() == Some(&b'\n') {
            return None;
        }
        let ls = memrchr(b'\n', &self.buf[from..s]).map_or(from, |i| from + i + 1);
        Some((ls, self.line_end(e)))
    }

    fn next(&mut self) -> Option<(usize, usize)> {
        let len = self.buf.len();
        if !self.invert {
            if self.pos >= len {
                return None;
            }
            return match self.find_line(self.pos) {
                Some((ls, le)) => {
                    self.pos = le + 1;
                    Some((ls, le))
                }
                None => {
                    self.pos = len;
                    None
                }
            };
        }
        loop {
            if self.pos >= len {
                return None;
            }
            let nm = match self.next_match {
                Some(nm) => nm,
                None => {
                    let nm = self.find_line(self.pos);
                    self.next_match = Some(nm);
                    nm
                }
            };
            match nm {
                Some((ms, me)) if self.pos >= ms => {
                    // This line matches, so it is not selected.
                    self.pos = me + 1;
                    self.next_match = None;
                }
                _ => {
                    let le = self.line_end(self.pos);
                    let ls = self.pos;
                    self.pos = le + 1;
                    return Some((ls, le));
                }
            }
        }
    }
}

struct LineCounter {
    off: usize,
    line: u64,
}

impl LineCounter {
    /// The 1-based number of the line starting at `target`. Targets must not decrease.
    fn line_at(&mut self, buf: &[u8], target: usize) -> u64 {
        if target > self.off {
            self.line += memchr_iter(b'\n', &buf[self.off..target]).count() as u64;
            self.off = target;
        }
        self.line
    }
}

struct Printer<'a> {
    buf: &'a [u8],
    label: &'a [u8],
    o: &'a SearchOptions,
    m: &'a Matcher,
    out: &'a mut Vec<u8>,
    lines: LineCounter,
    /// Offset where the next unprinted line starts.
    last_end: usize,
    printed_any: bool,
}

impl Printer<'_> {
    fn colored(&mut self, color: &[u8], text: &[u8]) {
        if self.o.color {
            self.out.extend_from_slice(color);
            self.out.extend_from_slice(text);
            self.out.extend_from_slice(RESET);
        } else {
            self.out.extend_from_slice(text);
        }
    }

    fn prefix(&mut self, start: usize, sep: &'static [u8]) {
        if self.o.with_filename {
            let label = self.label;
            self.colored(PATH, label);
            self.colored(SEP, sep);
        }
        if self.o.line_numbers {
            let n = self.lines.line_at(self.buf, start);
            self.colored(LINENO, n.to_string().as_bytes());
            self.colored(SEP, sep);
        }
    }

    fn trim_cr(&self, start: usize, end: usize) -> (usize, usize) {
        if end > start && self.buf[end - 1] == b'\r' {
            (start, end - 1)
        } else {
            (start, end)
        }
    }

    /// Prints one whole line, as a selected line (`:`) or a context line (`-`).
    fn line(&mut self, start: usize, end: usize, selected: bool) {
        if (self.o.before > 0 || self.o.after > 0) && self.printed_any && start > self.last_end {
            self.colored(SEP, b"--");
            self.out.push(b'\n');
        }
        self.prefix(start, if selected { b":" } else { b"-" });
        let (s, e) = self.trim_cr(start, end);
        let text = &self.buf[s..e];
        if self.o.color && selected && !self.o.invert {
            let mut at = 0;
            for (ms, me) in self.m.find_all_in_line(text) {
                self.out.extend_from_slice(&text[at..ms]);
                self.colored(MATCH, &text[ms..me]);
                at = me;
            }
            self.out.extend_from_slice(&text[at..]);
        } else {
            self.out.extend_from_slice(text);
        }
        self.out.push(b'\n');
        self.last_end = end + 1;
        self.printed_any = true;
    }

    /// Prints each match of a selected line on its own line.
    fn matches_only(&mut self, start: usize, end: usize) -> u64 {
        let (s, e) = self.trim_cr(start, end);
        let text = &self.buf[s..e];
        let found = self.m.find_all_in_line(text);
        for &(ms, me) in &found {
            self.prefix(start, b":");
            self.colored(MATCH, &text[ms..me]);
            self.out.push(b'\n');
        }
        found.len() as u64
    }

    fn line_end(&self, from: usize) -> usize {
        memchr(b'\n', &self.buf[from..]).map_or(self.buf.len(), |i| from + i)
    }

    /// The start of the line that is `n` lines above the line starting at `ls`, but not above `floor`.
    fn back_lines(&self, ls: usize, n: usize, floor: usize) -> usize {
        let mut p = ls;
        for _ in 0..n {
            if p <= floor {
                break;
            }
            // `p - 1` is the newline that ends the previous line.
            p = memrchr(b'\n', &self.buf[floor..p - 1]).map_or(floor, |i| floor + i + 1);
        }
        p
    }

    /// Prints up to `n` context lines starting at `from`, stopping before `limit`.
    fn trailing(&mut self, from: usize, n: usize, limit: usize) {
        let mut p = from;
        let mut done = 0;
        while done < n && p < limit {
            let e = self.line_end(p);
            self.line(p, e, false);
            p = e + 1;
            done += 1;
        }
    }
}

/// Searches `buf` and appends the output to `out`. `label` is the file name shown in front of lines.
pub fn search(label: &str, buf: &[u8], m: &Matcher, o: &SearchOptions, out: &mut Vec<u8>) -> Outcome {
    let mut outcome = Outcome::default();
    let mut sel = Selector::new(buf, m, o.invert);
    let printing = !o.count && !o.first_only;
    let max = o.max_count.unwrap_or(u64::MAX);
    let mut p = Printer { buf, label: label.as_bytes(), o, m, out, lines: LineCounter { off: 0, line: 1 }, last_end: 0, printed_any: false };

    while outcome.selected < max {
        let Some((ls, le)) = sel.next() else { break };
        outcome.selected += 1;
        if o.first_only {
            break;
        }
        if !printing {
            continue;
        }
        if o.only_matching {
            if !o.invert {
                outcome.matches += p.matches_only(ls, le);
            }
            continue;
        }
        let before_start = p.back_lines(ls, o.before, p.last_end);
        if o.after > 0 && p.printed_any {
            p.trailing(p.last_end, o.after, before_start);
        }
        let mut c = before_start;
        while c < ls {
            let e = p.line_end(c);
            p.line(c, e, false);
            c = e + 1;
        }
        p.line(ls, le, true);
    }
    if printing && !o.only_matching && o.after > 0 && p.printed_any {
        p.trailing(p.last_end, o.after, buf.len());
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::PatternOptions;

    fn run(text: &str, pat: &str, tweak: impl Fn(&mut SearchOptions)) -> (String, Outcome) {
        let m = Matcher::new(&[pat.to_string()], &PatternOptions::default()).unwrap();
        let mut o = SearchOptions { line_numbers: true, ..Default::default() };
        tweak(&mut o);
        let mut out = Vec::new();
        let r = search("f", text.as_bytes(), &m, &o, &mut out);
        (String::from_utf8(out).unwrap(), r)
    }

    #[test]
    fn prints_matching_lines_with_numbers() {
        let (out, r) = run("one\ntwo\nthree\ntwo again\n", "two", |_| {});
        assert_eq!(out, "2:two\n4:two again\n");
        assert_eq!(r.selected, 2);
    }

    #[test]
    fn file_label_and_missing_final_newline() {
        let (out, _) = run("a\nb", "b", |o| o.with_filename = true);
        assert_eq!(out, "f:2:b\n");
    }

    #[test]
    fn invert_selects_the_other_lines() {
        let (out, r) = run("a\nb\nc\nb\n", "b", |o| o.invert = true);
        assert_eq!(out, "1:a\n3:c\n");
        assert_eq!(r.selected, 2);
        let (out, _) = run("b\nb\n", "b", |o| o.invert = true);
        assert_eq!(out, "");
    }

    #[test]
    fn context_lines_use_a_dash_and_groups_are_separated() {
        let text = "a\nb\nMATCH\nc\nd\ne\nf\ng\nMATCH\nh\n";
        let (out, _) = run(text, "MATCH", |o| {
            o.before = 1;
            o.after = 1
        });
        assert_eq!(out, "2-b\n3:MATCH\n4-c\n--\n8-g\n9:MATCH\n10-h\n");
        // Overlapping context merges into one group.
        let (out, _) = run("x\nM\ny\nM\nz\n", "M", |o| {
            o.before = 1;
            o.after = 1
        });
        assert_eq!(out, "1-x\n2:M\n3-y\n4:M\n5-z\n");
    }

    #[test]
    fn count_and_first_only_do_not_print() {
        let (out, r) = run("a\na\nb\n", "a", |o| o.count = true);
        assert_eq!((out.as_str(), r.selected), ("", 2));
        let (_, r) = run("a\na\nb\n", "a", |o| o.first_only = true);
        assert_eq!(r.selected, 1);
    }

    #[test]
    fn max_count_stops_early_but_keeps_trailing_context() {
        let (out, r) = run("a\nb\na\nc\na\n", "a", |o| {
            o.max_count = Some(2);
            o.after = 1
        });
        assert_eq!(out, "1:a\n2-b\n3:a\n4-c\n");
        assert_eq!(r.selected, 2);
    }

    #[test]
    fn only_matching_prints_each_match() {
        let (out, r) = run("ab12cd345\nnone\n", r"\d+", |o| o.only_matching = true);
        assert_eq!(out, "1:12\n1:345\n");
        assert_eq!(r, Outcome { selected: 1, matches: 2 });
    }

    #[test]
    fn empty_matches_and_the_phantom_last_line() {
        // `^$` finds the one empty line, not a line after the final newline.
        let (out, r) = run("a\n\nb\n", "^$", |_| {});
        assert_eq!(out, "2:\n");
        assert_eq!(r.selected, 1);
        let (_, r) = run("a\nb\n", "", |_| {});
        assert_eq!(r.selected, 2);
        let (_, r) = run("", "", |_| {});
        assert_eq!(r.selected, 0);
    }

    #[test]
    fn carriage_returns_are_not_printed() {
        let (out, _) = run("a\r\nb\r\n", "a", |_| {});
        assert_eq!(out, "1:a\n");
    }

    #[test]
    fn colour_marks_the_file_number_and_match() {
        let (out, _) = run("a needle b\n", "needle", |o| {
            o.color = true;
            o.with_filename = true
        });
        assert_eq!(out, "\x1b[35mf\x1b[0m\x1b[36m:\x1b[0m\x1b[32m1\x1b[0m\x1b[36m:\x1b[0ma \x1b[1;31mneedle\x1b[0m b\n");
    }
}

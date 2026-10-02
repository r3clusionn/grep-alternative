//! Turning patterns into something that can search a whole file buffer at once.
//!
//! Searching a buffer in one call (instead of line by line) is what makes the regex engine's
//! literal prefilters and SIMD scans pay off, but it is only correct if no match can cross a line
//! break. So the pattern is parsed to its syntax tree, every class has the newline taken out of
//! it, a literal newline is rejected, and `\A` and `\z` are turned into line anchors. A pattern
//! that is a plain literal skips the regex engine and goes straight to a SIMD substring search.

use memchr::memmem;
use regex_automata::meta;
use regex_automata::Input;
use regex_syntax::hir::{Capture, Class, ClassBytes, ClassBytesRange, ClassUnicode, ClassUnicodeRange, Hir, HirKind, Look, Repetition};
use regex_syntax::ParserBuilder;

#[derive(Clone, Debug, Default)]
pub struct PatternOptions {
    /// Treat every pattern as a literal string.
    pub fixed: bool,
    pub ignore_case: bool,
    /// Ignore case unless a pattern contains an uppercase letter.
    pub smart_case: bool,
    /// Wrap the pattern in `\b ... \b`.
    pub word: bool,
    /// The pattern must match the whole line.
    pub line: bool,
    /// `$` also matches before `\r\n`.
    pub crlf: bool,
}

enum Kind {
    Literal(Box<memmem::Finder<'static>>),
    Regex(meta::Regex),
}

pub struct Matcher {
    kind: Kind,
}

impl Matcher {
    /// Builds a matcher for the union of `patterns` (a line matches if any pattern matches it).
    pub fn new(patterns: &[String], opts: &PatternOptions) -> Result<Matcher, String> {
        if patterns.is_empty() {
            return Err("no pattern given".to_string());
        }
        let sources: Vec<String> = patterns.iter().map(|p| if opts.fixed { regex_syntax::escape(p) } else { p.clone() }).collect();
        let parse = |ci: bool| -> Result<Vec<Hir>, String> {
            sources
                .iter()
                .zip(patterns)
                .map(|(s, orig)| {
                    // A parser can only be used once.
                    let mut parser = ParserBuilder::new().case_insensitive(ci).multi_line(true).crlf(opts.crlf).utf8(false).build();
                    parser.parse(s).map_err(|e| format!("invalid pattern '{orig}': {e}"))
                })
                .collect()
        };
        let mut hirs = parse(opts.ignore_case)?;
        if !opts.ignore_case && opts.smart_case && !hirs.iter().any(has_uppercase_literal) {
            hirs = parse(true)?;
        }

        // A single plain literal needs no regex engine.
        if hirs.len() == 1 && !opts.word && !opts.line {
            if let HirKind::Literal(lit) = hirs[0].kind() {
                if lit.0.contains(&b'\n') {
                    return Err(NEWLINE_ERROR.to_string());
                }
                if !lit.0.is_empty() {
                    return Ok(Matcher { kind: Kind::Literal(Box::new(memmem::Finder::new(&lit.0).into_owned())) });
                }
            }
        }

        let mut hir = if hirs.len() == 1 { hirs.pop().unwrap() } else { Hir::alternation(hirs) };
        hir = line_safe(&hir)?;
        if opts.word {
            hir = Hir::concat(vec![Hir::look(Look::WordUnicode), hir, Hir::look(Look::WordUnicode)]);
        }
        if opts.line {
            let (start, end) = if opts.crlf { (Look::StartCRLF, Look::EndCRLF) } else { (Look::StartLF, Look::EndLF) };
            hir = Hir::concat(vec![Hir::look(start), hir, Hir::look(end)]);
        }
        let re = meta::Builder::new()
            .configure(meta::Config::new().utf8_empty(false))
            .build_from_hir(&hir)
            .map_err(|e| format!("cannot build the matcher: {e}"))?;
        Ok(Matcher { kind: Kind::Regex(re) })
    }

    /// True when searches use the SIMD substring finder instead of the regex engine.
    pub fn is_literal(&self) -> bool {
        matches!(self.kind, Kind::Literal(_))
    }

    /// The first match that starts at or after `at`. `at` must be the start of a line (or 0), so
    /// that `^` and `\b` see the byte before it correctly.
    pub fn find_at(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        match &self.kind {
            Kind::Literal(f) => f.find(&hay[at..]).map(|i| (at + i, at + i + f.needle().len())),
            Kind::Regex(re) => re.find(Input::new(hay).span(at..hay.len())).map(|m| (m.start(), m.end())),
        }
    }

    /// All non-empty matches inside one line, for `--only-matching` and colouring.
    pub fn find_all_in_line(&self, line: &[u8]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at <= line.len() {
            let Some((s, e)) = (match &self.kind {
                Kind::Literal(f) => f.find(&line[at..]).map(|i| (at + i, at + i + f.needle().len())),
                Kind::Regex(re) => re.find(Input::new(line).span(at..line.len())).map(|m| (m.start(), m.end())),
            }) else {
                break;
            };
            if e > s {
                out.push((s, e));
                at = e;
            } else {
                at = e + 1;
            }
        }
        out
    }
}

const NEWLINE_ERROR: &str = "the pattern can only match across lines (it contains a literal newline), which a line-based search cannot do";

fn has_uppercase_literal(h: &Hir) -> bool {
    match h.kind() {
        HirKind::Literal(l) => String::from_utf8_lossy(&l.0).chars().any(char::is_uppercase),
        HirKind::Repetition(r) => has_uppercase_literal(&r.sub),
        HirKind::Capture(c) => has_uppercase_literal(&c.sub),
        HirKind::Concat(v) | HirKind::Alternation(v) => v.iter().any(has_uppercase_literal),
        _ => false,
    }
}

/// Rewrites the tree so that no match can contain a newline.
pub fn line_safe(h: &Hir) -> Result<Hir, String> {
    Ok(match h.kind() {
        HirKind::Empty => Hir::empty(),
        HirKind::Literal(l) => {
            if l.0.contains(&b'\n') {
                return Err(NEWLINE_ERROR.to_string());
            }
            Hir::literal(l.0.clone())
        }
        HirKind::Class(Class::Unicode(c)) => {
            let mut c: ClassUnicode = c.clone();
            c.difference(&ClassUnicode::new([ClassUnicodeRange::new('\n', '\n')]));
            Hir::class(Class::Unicode(c))
        }
        HirKind::Class(Class::Bytes(c)) => {
            let mut c: ClassBytes = c.clone();
            c.difference(&ClassBytes::new([ClassBytesRange::new(b'\n', b'\n')]));
            Hir::class(Class::Bytes(c))
        }
        // Whole-buffer anchors become line anchors: "start of the text" means "start of the line".
        HirKind::Look(Look::Start) => Hir::look(Look::StartLF),
        HirKind::Look(Look::End) => Hir::look(Look::EndLF),
        HirKind::Look(l) => Hir::look(*l),
        HirKind::Repetition(r) => Hir::repetition(Repetition { min: r.min, max: r.max, greedy: r.greedy, sub: Box::new(line_safe(&r.sub)?) }),
        HirKind::Capture(c) => Hir::capture(Capture { index: c.index, name: c.name.clone(), sub: Box::new(line_safe(&c.sub)?) }),
        HirKind::Concat(v) => Hir::concat(v.iter().map(line_safe).collect::<Result<_, _>>()?),
        HirKind::Alternation(v) => Hir::alternation(v.iter().map(line_safe).collect::<Result<_, _>>()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str) -> Matcher {
        Matcher::new(&[p.to_string()], &PatternOptions::default()).unwrap()
    }

    #[test]
    fn plain_literals_use_the_simd_finder() {
        assert!(m("needle").is_literal());
        assert!(!m("ne+dle").is_literal());
        let fixed = Matcher::new(&["a.b".into()], &PatternOptions { fixed: true, ..Default::default() }).unwrap();
        assert!(fixed.is_literal());
        assert_eq!(fixed.find_at(b"axb a.b", 0), Some((4, 7)));
        let ci = Matcher::new(&["abc".into()], &PatternOptions { ignore_case: true, ..Default::default() }).unwrap();
        assert!(!ci.is_literal());
        assert_eq!(ci.find_at(b"xxABC", 0), Some((2, 5)));
    }

    #[test]
    fn a_match_never_spans_a_line_break() {
        // `\s`, `[^x]` and `(?s).` all contain \n in a plain regex.
        for p in [r"a\sb", "a[^x]b", "(?s)a.b", r"a\W+b", r"a\Db"] {
            assert_eq!(m(p).find_at(b"a\nb\n", 0), None, "{p}");
        }
        assert_eq!(m(r"a\sb").find_at(b"a b", 0), Some((0, 3)));
    }

    #[test]
    fn a_newline_in_the_pattern_is_an_error() {
        for p in ["a\nb", r"a\nb", "x|a\nb"] {
            assert!(Matcher::new(&[p.to_string()], &PatternOptions::default()).is_err(), "{p:?}");
        }
    }

    #[test]
    fn text_anchors_become_line_anchors() {
        assert_eq!(m(r"\Ab").find_at(b"a\nb\n", 0), Some((2, 3)));
        assert_eq!(m(r"a\z").find_at(b"ab\na\nc", 0), Some((3, 4)));
        assert_eq!(m("^b$").find_at(b"ab\nb\n", 0), Some((3, 4)));
    }

    #[test]
    fn find_at_sees_the_context_before_the_start() {
        // Searching from offset 2 (a line start) must still know that offset 1 was a newline.
        assert_eq!(m("^x").find_at(b"x\nx", 2), Some((2, 3)));
        assert_eq!(m("^x").find_at(b"x\ny\nx", 2), Some((4, 5)));
    }

    #[test]
    fn word_line_and_smart_case_options() {
        let w = Matcher::new(&["cat".into()], &PatternOptions { word: true, ..Default::default() }).unwrap();
        assert_eq!(w.find_at(b"concat cat", 0), Some((7, 10)));
        let x = Matcher::new(&["cat".into()], &PatternOptions { line: true, ..Default::default() }).unwrap();
        assert_eq!(x.find_at(b"cat dog\ncat\n", 0), Some((8, 11)));
        let sc = PatternOptions { smart_case: true, ..Default::default() };
        let lower = Matcher::new(&["cat".into()], &sc).unwrap();
        assert!(lower.find_at(b"CAT", 0).is_some());
        let mixed = Matcher::new(&["Cat".into()], &sc).unwrap();
        assert!(mixed.find_at(b"cat", 0).is_none() && mixed.find_at(b"Cat", 0).is_some());
        let escaped = Matcher::new(&[r"\S+".into()], &sc).unwrap();
        assert!(escaped.find_at(b"abc", 0).is_some());
    }

    #[test]
    fn several_patterns_are_a_union() {
        let both = Matcher::new(&["foo".into(), "ba+r".into()], &PatternOptions::default()).unwrap();
        assert_eq!(both.find_at(b"x baar foo", 0), Some((2, 6)));
        assert_eq!(both.find_all_in_line(b"foo baar foo"), vec![(0, 3), (4, 8), (9, 12)]);
    }

    #[test]
    fn find_all_skips_empty_matches() {
        let star = m("a*");
        assert_eq!(star.find_all_in_line(b"baab a"), vec![(1, 3), (5, 6)]);
    }
}

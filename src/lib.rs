//! `gx`: a parallel recursive grep.
//!
//! The library has two parts. [`matcher`] turns patterns into a matcher that can search a whole
//! file buffer at once without ever matching across a line break. [`search`] runs a matcher over a
//! buffer and formats the selected lines (line numbers, context, counts, colours). The binary
//! adds the directory walk, file reading and output ordering.
//!
//! ```
//! use gx::matcher::{Matcher, PatternOptions};
//! use gx::search::{search, SearchOptions};
//!
//! let m = Matcher::new(&["wor.d".to_string()], &PatternOptions::default()).unwrap();
//! let mut out = Vec::new();
//! let opts = SearchOptions { line_numbers: true, ..Default::default() };
//! let outcome = search("demo", b"hello\nworld\n", &m, &opts, &mut out);
//! assert_eq!(outcome.selected, 1);
//! assert_eq!(out, b"2:world\n");
//! ```

pub mod matcher;
pub mod search;

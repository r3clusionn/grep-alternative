use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Instant;

use clap::{Parser, ValueEnum};
use gx::matcher::{Matcher, PatternOptions};
use gx::search::{search, SearchOptions};
use ignore::overrides::OverrideBuilder;
use ignore::types::TypesBuilder;
use ignore::{WalkBuilder, WalkState};
use memmap2::Mmap;

/// Files at least this large are memory-mapped instead of read.
const MMAP_THRESHOLD: u64 = 1 << 20;
/// A file is binary if a NUL byte shows up in its first 8 KiB.
const BINARY_PROBE: usize = 8192;

#[derive(Parser)]
#[command(
    name = "gx",
    version,
    disable_help_flag = true,
    about = "Parallel recursive grep: regex search that respects .gitignore, skips binary files and uses SIMD for literals",
    override_usage = "gx [OPTIONS] PATTERN [PATH...]\n       gx [OPTIONS] -e PATTERN... [PATH...]"
)]
struct Cli {
    /// The pattern (unless -e or -f is used), then the files and directories to search (default: the current directory; `-` is standard input)
    #[arg(value_name = "PATTERN] [PATH")]
    args: Vec<String>,
    /// A pattern; repeat to match any of several
    #[arg(short = 'e', long = "regexp", value_name = "PATTERN")]
    patterns: Vec<String>,
    /// Read patterns from a file, one per line
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    pattern_files: Vec<PathBuf>,
    /// Treat patterns as literal strings
    #[arg(short = 'F', long)]
    fixed_strings: bool,
    /// Ignore case
    #[arg(short = 'i', long)]
    ignore_case: bool,
    /// Ignore case unless the pattern has an uppercase letter
    #[arg(short = 'S', long)]
    smart_case: bool,
    /// Match whole words only
    #[arg(short = 'w', long)]
    word_regexp: bool,
    /// Match whole lines only
    #[arg(short = 'x', long)]
    line_regexp: bool,
    /// Show the lines that do not match
    #[arg(short = 'v', long)]
    invert_match: bool,
    /// Print help (`-h` means --no-filename, as in grep)
    #[arg(long, action = clap::ArgAction::Help)]
    help: Option<bool>,
    /// Print only the number of matching lines per file
    #[arg(short = 'c', long)]
    count: bool,
    /// With -c, also print files that have no matching line
    #[arg(long)]
    include_zero: bool,
    /// Print only the names of files with a match
    #[arg(short = 'l', long)]
    files_with_matches: bool,
    /// Print only the names of files without a match
    #[arg(long)]
    files_without_match: bool,
    /// Print only the matching parts of lines
    #[arg(short = 'o', long)]
    only_matching: bool,
    /// Do not print line numbers
    #[arg(short = 'N', long)]
    no_line_number: bool,
    /// Always print file names
    #[arg(short = 'H', long, conflicts_with = "no_filename")]
    with_filename: bool,
    /// Never print file names
    #[arg(short = 'h', long)]
    no_filename: bool,
    /// Lines of context after each match
    #[arg(short = 'A', long, value_name = "N")]
    after_context: Option<usize>,
    /// Lines of context before each match
    #[arg(short = 'B', long, value_name = "N")]
    before_context: Option<usize>,
    /// Lines of context before and after each match
    #[arg(short = 'C', long, value_name = "N")]
    context: Option<usize>,
    /// Stop reading a file after N matching lines
    #[arg(short = 'm', long, value_name = "N")]
    max_count: Option<u64>,
    /// Search binary files as if they were text
    #[arg(short = 'a', long)]
    text: bool,
    /// Search hidden files and directories
    #[arg(long)]
    hidden: bool,
    /// Do not respect .gitignore and .ignore files
    #[arg(long)]
    no_ignore: bool,
    /// Include or exclude paths by glob; prefix with ! to exclude (repeatable)
    #[arg(short = 'g', long, value_name = "GLOB")]
    glob: Vec<String>,
    /// Only search files of this type (see --type-list)
    #[arg(short = 't', long = "type", value_name = "TYPE")]
    types: Vec<String>,
    /// Do not search files of this type
    #[arg(short = 'T', long = "type-not", value_name = "TYPE")]
    types_not: Vec<String>,
    /// List the known file types and exit
    #[arg(long)]
    type_list: bool,
    /// Follow symbolic links
    #[arg(long)]
    follow: bool,
    /// Descend at most this many directory levels
    #[arg(long, value_name = "N")]
    max_depth: Option<usize>,
    /// Number of search threads (default: one per logical CPU)
    #[arg(short = 'j', long, value_name = "N")]
    threads: Option<usize>,
    /// Read every file instead of memory-mapping large ones
    #[arg(long)]
    no_mmap: bool,
    /// Print results ordered by path (collects all output first)
    #[arg(long)]
    sort: bool,
    /// When to use colours
    #[arg(long, value_enum, default_value_t = ColorWhen::Auto)]
    color: ColorWhen,
    /// `$` also matches before \r\n
    #[arg(long)]
    crlf: bool,
    /// Print a summary to standard error
    #[arg(long)]
    stats: bool,
    /// Print nothing; exit 0 as soon as anything matches
    #[arg(short = 'q', long)]
    quiet: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum ColorWhen {
    Auto,
    Always,
    Never,
}

#[derive(Default)]
struct Stats {
    files_searched: AtomicU64,
    files_matched: AtomicU64,
    bytes_searched: AtomicU64,
    lines_matched: AtomicU64,
    binary_skipped: AtomicU64,
    errors: AtomicU64,
}

/// Per-file output collected for `--sort`: (path, text).
type SortedOutput = Mutex<Vec<(PathBuf, Vec<u8>)>>;

struct Ctx<'a> {
    matcher: &'a Matcher,
    opts: SearchOptions,
    /// How a file's result is reported.
    mode: Mode,
    text: bool,
    include_zero: bool,
    mmap: bool,
    quiet: bool,
    stats: &'a Stats,
    done: &'a AtomicBool,
    sorted: Option<&'a SortedOutput>,
}

/// Standard output, remembering whether anything was written (for the `--` between files).
struct Sink {
    out: io::Stdout,
    wrote: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Lines,
    Count,
    FilesWith,
    FilesWithout,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("gx: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let cli = Cli::parse();
    if cli.type_list {
        let mut b = TypesBuilder::new();
        b.add_defaults();
        for d in b.definitions() {
            println!("{}: {}", d.name(), d.globs().join(", "));
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut args = cli.args.clone();
    let mut patterns = cli.patterns.clone();
    for f in &cli.pattern_files {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        patterns.extend(text.lines().map(str::to_string));
    }
    if patterns.is_empty() {
        if args.is_empty() {
            return Err("no pattern given (try `gx --help`)".to_string());
        }
        patterns.push(args.remove(0));
    }
    let matcher = Matcher::new(
        &patterns,
        &PatternOptions {
            fixed: cli.fixed_strings,
            ignore_case: cli.ignore_case,
            smart_case: cli.smart_case,
            word: cli.word_regexp,
            line: cli.line_regexp,
            crlf: cli.crlf,
        },
    )?;

    let explicit_paths: Vec<PathBuf> = args.iter().map(PathBuf::from).collect();
    let roots: Vec<PathBuf> = if explicit_paths.is_empty() { vec![PathBuf::from(".")] } else { explicit_paths.clone() };
    let searches_dirs = explicit_paths.is_empty() || explicit_paths.iter().any(|p| p.is_dir());
    let with_filename = cli.with_filename || (!cli.no_filename && (searches_dirs || roots.len() > 1));

    let color = match cli.color {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => io::stdout().is_terminal(),
    };
    let (before, after) = (cli.before_context.or(cli.context).unwrap_or(0), cli.after_context.or(cli.context).unwrap_or(0));
    let mode = if cli.files_with_matches {
        Mode::FilesWith
    } else if cli.files_without_match {
        Mode::FilesWithout
    } else if cli.count {
        Mode::Count
    } else {
        Mode::Lines
    };
    let opts = SearchOptions {
        invert: cli.invert_match,
        count: mode != Mode::Lines,
        first_only: matches!(mode, Mode::FilesWith | Mode::FilesWithout) || cli.quiet,
        only_matching: cli.only_matching,
        line_numbers: !cli.no_line_number,
        before,
        after,
        max_count: cli.max_count,
        color,
        with_filename,
    };

    let stats = Stats::default();
    let done = AtomicBool::new(false);
    let sorted = cli.sort.then(|| Mutex::new(Vec::new()));
    let stdout = Mutex::new(Sink { out: io::stdout(), wrote: false });
    let ctx = Ctx { matcher: &matcher, opts, mode, text: cli.text, include_zero: cli.include_zero, mmap: !cli.no_mmap, quiet: cli.quiet, stats: &stats, done: &done, sorted: sorted.as_ref() };
    let started = Instant::now();

    // Standard input is only searched when asked for with `-`.
    let (stdin_paths, fs_roots): (Vec<_>, Vec<_>) = roots.into_iter().partition(|p| p.as_os_str() == "-");
    for _ in &stdin_paths {
        let mut data = Vec::new();
        io::stdin().lock().read_to_end(&mut data).map_err(|e| format!("standard input: {e}"))?;
        report(&ctx, Path::new("(standard input)"), &data, true, &stdout);
    }

    if !fs_roots.is_empty() {
        let mut wb = WalkBuilder::new(&fs_roots[0]);
        for r in &fs_roots[1..] {
            wb.add(r);
        }
        wb.hidden(!cli.hidden).follow_links(cli.follow).max_depth(cli.max_depth).threads(cli.threads.unwrap_or(0));
        if cli.no_ignore {
            wb.ignore(false).git_ignore(false).git_global(false).git_exclude(false).parents(false);
        }
        if !cli.glob.is_empty() {
            let mut ob = OverrideBuilder::new(std::env::current_dir().map_err(|e| e.to_string())?);
            for g in &cli.glob {
                ob.add(g).map_err(|e| format!("bad glob '{g}': {e}"))?;
            }
            wb.overrides(ob.build().map_err(|e| e.to_string())?);
        }
        if !cli.types.is_empty() || !cli.types_not.is_empty() {
            let mut tb = TypesBuilder::new();
            tb.add_defaults();
            for t in &cli.types {
                tb.select(t);
            }
            for t in &cli.types_not {
                tb.negate(t);
            }
            wb.types(tb.build().map_err(|e| e.to_string())?);
        }
        wb.build_parallel().run(|| {
            let mut buf: Vec<u8> = Vec::new();
            let (ctx, stdout) = (&ctx, &stdout);
            Box::new(move |entry| {
                if ctx.done.load(Relaxed) {
                    return WalkState::Quit;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        ctx.stats.errors.fetch_add(1, Relaxed);
                        eprintln!("gx: {e}");
                        return WalkState::Continue;
                    }
                };
                let explicit = entry.depth() == 0;
                if !entry.file_type().is_some_and(|t| t.is_file()) && !(explicit && entry.file_type().is_none()) {
                    return WalkState::Continue;
                }
                let path = entry.path();
                match with_contents(path, ctx.mmap, &mut buf, |data| report(ctx, path, data, explicit, stdout)) {
                    Ok(()) => {}
                    Err(e) => {
                        ctx.stats.errors.fetch_add(1, Relaxed);
                        eprintln!("gx: {}: {e}", path.display());
                    }
                }
                if ctx.done.load(Relaxed) {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            })
        });
    }

    if let Some(s) = &sorted {
        let mut all = s.lock().unwrap();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, text) in all.iter() {
            if !write_file_output(&ctx, &stdout, text) {
                break;
            }
        }
    }
    let _ = stdout.lock().unwrap().out.flush();

    if cli.stats {
        let secs = started.elapsed().as_secs_f64();
        let bytes = stats.bytes_searched.load(Relaxed);
        eprintln!(
            "\n{} matching lines\n{} files contained matches\n{} files searched\n{} binary files skipped\n{:.1} MiB searched\n{:.3} s ({:.2} GB/s)",
            stats.lines_matched.load(Relaxed),
            stats.files_matched.load(Relaxed),
            stats.files_searched.load(Relaxed),
            stats.binary_skipped.load(Relaxed),
            bytes as f64 / 1048576.0,
            secs,
            bytes as f64 / secs.max(1e-9) / 1e9
        );
    }

    let matched = stats.files_matched.load(Relaxed) > 0;
    Ok(if cli.quiet && matched {
        ExitCode::SUCCESS
    } else if stats.errors.load(Relaxed) > 0 {
        ExitCode::from(2)
    } else if matched {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Hands `f` the contents of the file: memory-mapped when large, otherwise read into `buf`.
fn with_contents<R>(path: &Path, allow_mmap: bool, buf: &mut Vec<u8>, f: impl FnOnce(&[u8]) -> R) -> io::Result<R> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if allow_mmap && len >= MMAP_THRESHOLD {
        // SAFETY: the map is read-only and dropped before this function returns. If another
        // process truncates the file while it is mapped, reading it can fault; `--no-mmap`
        // avoids that at the cost of reading the whole file into memory.
        if let Ok(map) = unsafe { Mmap::map(&file) } {
            return Ok(f(&map));
        }
    }
    buf.clear();
    buf.reserve(len as usize + 1);
    file.read_to_end(buf)?;
    Ok(f(buf))
}

/// Searches one file's contents and writes or stores its output.
fn report(ctx: &Ctx, path: &Path, data: &[u8], explicit: bool, stdout: &Mutex<Sink>) {
    let stats = ctx.stats;
    let shown = path.strip_prefix(".").unwrap_or(path);
    let label = shown.to_string_lossy();
    let mut out: Vec<u8> = Vec::new();

    let binary = !ctx.text && memchr::memchr(0, &data[..data.len().min(BINARY_PROBE)]).is_some();
    let selected = if binary {
        // Binary files found by the walk are skipped. One named on the command line is only
        // probed, so the user hears that it matches without getting garbage on the terminal.
        stats.binary_skipped.fetch_add(1, Relaxed);
        if !explicit || ctx.opts.invert {
            return;
        }
        let hit = ctx.matcher.find_at(data, 0).is_some();
        if hit {
            match ctx.mode {
                Mode::Lines if !ctx.quiet => out.extend_from_slice(format!("{label}: binary file matches
").as_bytes()),
                Mode::FilesWith => out.extend_from_slice(format!("{label}
").as_bytes()),
                _ => {}
            }
        }
        u64::from(hit)
    } else {
        stats.files_searched.fetch_add(1, Relaxed);
        stats.bytes_searched.fetch_add(data.len() as u64, Relaxed);
        let outcome = search(&label, data, ctx.matcher, &ctx.opts, &mut out);
        match ctx.mode {
            Mode::Count if outcome.selected == 0 && !ctx.include_zero => {}
            Mode::Count if ctx.opts.with_filename => out.extend_from_slice(format!("{label}:{}
", outcome.selected).as_bytes()),
            Mode::Count => out.extend_from_slice(format!("{}
", outcome.selected).as_bytes()),
            Mode::FilesWith if outcome.selected > 0 => out.extend_from_slice(format!("{label}
").as_bytes()),
            Mode::FilesWithout if outcome.selected == 0 => out.extend_from_slice(format!("{label}
").as_bytes()),
            _ => {}
        }
        outcome.selected
    };
    if selected > 0 {
        stats.files_matched.fetch_add(1, Relaxed);
        stats.lines_matched.fetch_add(selected, Relaxed);
        if ctx.quiet {
            ctx.done.store(true, Relaxed);
        }
    }
    if out.is_empty() || ctx.quiet {
        return;
    }
    match ctx.sorted {
        Some(s) => s.lock().unwrap().push((path.to_path_buf(), out)),
        None => {
            if !write_file_output(ctx, stdout, &out) {
                // The reader went away (for example `gx ... | head`): stop searching.
                ctx.done.store(true, Relaxed);
            }
        }
    }
}

/// Writes one file's output. With context lines on, groups from different files are separated by
/// `--` like the groups within a file. Returns false when standard output is closed.
fn write_file_output(ctx: &Ctx, stdout: &Mutex<Sink>, text: &[u8]) -> bool {
    let mut sink = stdout.lock().unwrap();
    let separate = (ctx.opts.before > 0 || ctx.opts.after > 0) && ctx.mode == Mode::Lines && sink.wrote;
    let ok = (!separate || sink.out.write_all(if ctx.opts.color { b"[36m--[0m
" } else { b"--
" }).is_ok()) && sink.out.write_all(text).is_ok();
    sink.wrote = true;
    ok
}

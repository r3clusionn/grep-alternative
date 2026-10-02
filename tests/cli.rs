use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn gx(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gx")).current_dir(dir).args(args).output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

fn lines(o: &Output) -> Vec<String> {
    let mut v: Vec<String> = out(o).lines().map(|l| l.replace('\\', "/")).collect();
    v.sort();
    v
}

fn write(dir: &Path, rel: &str, content: impl AsRef<[u8]>) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

/// A small tree: a git repo with an ignore file, hidden files and a binary file.
fn tree() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    fs::create_dir(r.join(".git")).unwrap(); // the walker only honours .gitignore inside a repository
    write(r, ".gitignore", "ignored/\n*.log\n");
    write(r, "a.txt", "needle one\nplain\nNeedle two\n");
    write(r, "src/main.rs", "fn main() {\n    let needle = 1;\n}\n");
    write(r, "src/lib.py", "needle = 2\n");
    write(r, "ignored/x.txt", "needle in ignored dir\n");
    write(r, "debug.log", "needle in a log\n");
    write(r, ".hidden/h.txt", "needle in hidden\n");
    write(r, "bin.dat", b"needle\0binary\n");
    d
}

#[test]
fn recursive_search_respects_gitignore_hidden_and_binary() {
    let t = tree();
    let o = gx(t.path(), &["needle"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(lines(&o), ["a.txt:1:needle one", "src/lib.py:1:needle = 2", "src/main.rs:2:    let needle = 1;"]);

    let o = gx(t.path(), &["needle", "--hidden"]);
    assert!(out(&o).contains(".hidden"), "{}", out(&o));
    assert!(!out(&o).contains("ignored/"));

    let o = gx(t.path(), &["needle", "--no-ignore", "--hidden"]);
    let text = out(&o).replace('\\', "/");
    assert!(text.contains("ignored/x.txt") && text.contains("debug.log") && text.contains(".hidden/h.txt"), "{text}");
    // Binary files stay skipped when they are only found by the walk.
    assert!(!text.contains("bin.dat"), "{text}");
}

#[test]
fn a_binary_file_named_on_the_command_line_reports_a_match() {
    let t = tree();
    let o = gx(t.path(), &["needle", "bin.dat"]);
    assert_eq!(out(&o), "bin.dat: binary file matches\n");
    assert_eq!(o.status.code(), Some(0));
    let o = gx(t.path(), &["nothing-like-this", "bin.dat"]);
    assert_eq!((out(&o).as_str(), o.status.code()), ("", Some(1)));
    // -a searches it as text.
    let o = gx(t.path(), &["-a", "needle", "bin.dat"]);
    assert!(out(&o).starts_with("1:needle"), "{}", out(&o));
}

#[test]
fn exit_codes_are_0_for_a_match_1_for_none_and_2_for_errors() {
    let t = tree();
    assert_eq!(gx(t.path(), &["needle", "a.txt"]).status.code(), Some(0));
    assert_eq!(gx(t.path(), &["absent", "a.txt"]).status.code(), Some(1));
    let missing = gx(t.path(), &["needle", "no-such-file"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no-such-file"));
    let bad = gx(t.path(), &["(unclosed", "a.txt"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("invalid pattern"));
    let nl = gx(t.path(), &["a\\nb", "a.txt"]);
    assert_eq!(nl.status.code(), Some(2));
    assert_eq!(gx(t.path(), &[]).status.code(), Some(2));
}

#[test]
fn quiet_prints_nothing() {
    let t = tree();
    let o = gx(t.path(), &["-q", "needle"]);
    assert_eq!((out(&o).as_str(), o.status.code()), ("", Some(0)));
    assert_eq!(gx(t.path(), &["-q", "absent"]).status.code(), Some(1));
}

#[test]
fn single_file_has_no_filename_prefix_and_flags_change_the_output() {
    let t = tree();
    assert_eq!(out(&gx(t.path(), &["needle", "a.txt"])), "1:needle one\n");
    assert_eq!(out(&gx(t.path(), &["-N", "needle", "a.txt"])), "needle one\n");
    assert_eq!(out(&gx(t.path(), &["-H", "-N", "needle", "a.txt"])), "a.txt:needle one\n");
    assert_eq!(out(&gx(t.path(), &["-i", "needle", "a.txt"])), "1:needle one\n3:Needle two\n");
    assert_eq!(out(&gx(t.path(), &["-S", "Needle", "a.txt"])), "3:Needle two\n");
    assert_eq!(out(&gx(t.path(), &["-S", "needle", "a.txt"])), "1:needle one\n3:Needle two\n");
    assert_eq!(out(&gx(t.path(), &["-v", "needle", "a.txt"])), "2:plain\n3:Needle two\n");
    assert_eq!(out(&gx(t.path(), &["-c", "-i", "needle", "a.txt"])), "2\n");
    assert_eq!(out(&gx(t.path(), &["-o", "-i", "ne+dle", "a.txt"])), "1:needle\n3:Needle\n");
    assert_eq!(out(&gx(t.path(), &["-w", "one", "a.txt"])), "1:needle one\n");
    assert_eq!(out(&gx(t.path(), &["-w", "need", "a.txt"])), "");
    assert_eq!(out(&gx(t.path(), &["-x", "plain", "a.txt"])), "2:plain\n");
    assert_eq!(out(&gx(t.path(), &["-F", "-e", "n.edle", "-e", "plain", "a.txt"])), "2:plain\n");
    assert_eq!(out(&gx(t.path(), &["-e", "plain", "-e", "two", "a.txt"])), "2:plain\n3:Needle two\n");
    assert_eq!(out(&gx(t.path(), &["-m", "1", "-i", "needle", "a.txt"])), "1:needle one\n");
}

#[test]
fn context_lines_and_separators() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "f.txt", "1\n2\nHIT\n4\n5\n6\n7\nHIT\n9\n");
    assert_eq!(out(&gx(d.path(), &["-C", "1", "HIT", "f.txt"])), "2-2\n3:HIT\n4-4\n--\n7-7\n8:HIT\n9-9\n");
    assert_eq!(out(&gx(d.path(), &["-A", "1", "-N", "HIT", "f.txt"])), "HIT\n4\n--\nHIT\n9\n");
    assert_eq!(out(&gx(d.path(), &["-B", "2", "HIT", "f.txt"])), "1-1\n2-2\n3:HIT\n--\n6-6\n7-7\n8:HIT\n");
}

#[test]
fn file_lists_counts_and_sorted_output() {
    let t = tree();
    assert_eq!(out(&gx(t.path(), &["-l", "--sort", "needle"])).replace('\\', "/"), "a.txt\nsrc/lib.py\nsrc/main.rs\n");
    assert_eq!(out(&gx(t.path(), &["-c", "--sort", "-i", "needle"])).replace('\\', "/"), "a.txt:2\nsrc/lib.py:1\nsrc/main.rs:1\n");
    let without = out(&gx(t.path(), &["--files-without-match", "--sort", "needle"])).replace('\\', "/");
    assert_eq!(without, "");
    let without = out(&gx(t.path(), &["--files-without-match", "--sort", "plain"])).replace('\\', "/");
    assert_eq!(without, "src/lib.py\nsrc/main.rs\n");
    // Order is by path with --sort, whatever the thread count.
    for j in ["1", "8"] {
        let o = gx(t.path(), &["--sort", "-j", j, "-l", "needle"]);
        assert_eq!(out(&o).replace('\\', "/"), "a.txt\nsrc/lib.py\nsrc/main.rs\n");
    }
}

#[test]
fn globs_and_types_select_files() {
    let t = tree();
    assert_eq!(lines(&gx(t.path(), &["-l", "-g", "*.rs", "needle"])), ["src/main.rs"]);
    assert_eq!(lines(&gx(t.path(), &["-l", "-g", "!*.rs", "needle"])), ["a.txt", "src/lib.py"]);
    assert_eq!(lines(&gx(t.path(), &["-l", "-t", "py", "needle"])), ["src/lib.py"]);
    assert_eq!(lines(&gx(t.path(), &["-l", "-T", "py", "-T", "rust", "needle"])), ["a.txt"]);
    let list = out(&gx(t.path(), &["--type-list"]));
    assert!(list.contains("rust: ") && list.contains("py: "), "{list}");
}

#[test]
fn patterns_from_a_file_and_standard_input() {
    let t = tree();
    write(t.path(), "pats.txt", "plain\ntwo\n");
    assert_eq!(out(&gx(t.path(), &["-f", "pats.txt", "a.txt"])), "2:plain\n3:Needle two\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_gx"))
        .current_dir(t.path())
        .args(["b+", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child.stdin.take().unwrap().write_all(b"a\nbb\nc\n").unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(out(&o), "2:bb\n");
}

#[test]
fn crlf_files_and_the_crlf_flag() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "w.txt", "foo\r\nbar\r\n");
    // Without --crlf `$` sees the \r; with it, the line ends before \r\n.
    assert_eq!(gx(d.path(), &["foo$", "w.txt"]).status.code(), Some(1));
    assert_eq!(out(&gx(d.path(), &["--crlf", "foo$", "w.txt"])), "1:foo\n");
    assert_eq!(out(&gx(d.path(), &["foo", "w.txt"])), "1:foo\n");
}

#[test]
fn large_files_search_the_same_mapped_or_read() {
    let d = tempfile::tempdir().unwrap();
    let mut text = String::new();
    for i in 0..120_000 {
        text.push_str(&format!("line {i} lorem ipsum {}\n", if i % 997 == 0 { "NEEDLE" } else { "dolor" }));
    }
    assert!(text.len() > 3 << 20);
    write(d.path(), "big.txt", &text);
    let a = gx(d.path(), &["NEEDLE", "big.txt"]);
    let b = gx(d.path(), &["--no-mmap", "NEEDLE", "big.txt"]);
    assert_eq!(out(&a), out(&b));
    assert_eq!(out(&a).lines().count(), 121); // i = 0, 997, ... 119_548 (121 values)
    assert!(out(&a).starts_with("1:line 0 lorem ipsum NEEDLE\n"));
    assert_eq!(out(&gx(d.path(), &["-c", "-v", "NEEDLE", "big.txt"])), "119879\n");
}

#[test]
fn thread_count_does_not_change_the_result() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..200 {
        write(d.path(), &format!("d{}/f{i}.txt", i % 7), format!("row {i}\nneedle {i}\n"));
    }
    let one = lines(&gx(d.path(), &["-j", "1", "needle"]));
    let many = lines(&gx(d.path(), &["-j", "8", "needle"]));
    assert_eq!(one.len(), 200);
    assert_eq!(one, many);
}

#[test]
fn stats_go_to_standard_error() {
    let t = tree();
    let o = gx(t.path(), &["--stats", "needle"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("3 matching lines") && err.contains("3 files contained matches") && err.contains("binary files skipped"), "{err}");
}

#[test]
fn count_skips_files_without_matches_unless_asked() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "has.txt", "x\nx\ny\n");
    write(d.path(), "none.txt", "y\n");
    assert_eq!(out(&gx(d.path(), &["-c", "--sort", "x"])), "has.txt:2\n");
    assert_eq!(out(&gx(d.path(), &["-c", "--sort", "--include-zero", "x"])), "has.txt:2\nnone.txt:0\n");
}

#[test]
fn context_groups_from_different_files_are_separated() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "a.txt", "1\nHIT\n3\n");
    write(d.path(), "b.txt", "x\nHIT\nz\n");
    let o = out(&gx(d.path(), &["--sort", "-C", "1", "HIT"]));
    assert_eq!(o, "a.txt-1-1\na.txt:2:HIT\na.txt-3-3\n--\nb.txt-1-x\nb.txt:2:HIT\nb.txt-3-z\n");
    // Without context lines there are no separators.
    assert_eq!(out(&gx(d.path(), &["--sort", "HIT"])), "a.txt:2:HIT\nb.txt:2:HIT\n");
}

#[test]
fn short_h_hides_file_names_and_help_is_long_only() {
    let t = tree();
    assert_eq!(out(&gx(t.path(), &["-h", "-N", "Needle", "a.txt", "src/lib.py"])), "Needle two\n");
    let help = out(&gx(t.path(), &["--help"]));
    assert!(help.contains("--no-filename") && help.contains("Usage:"), "{help}");
}

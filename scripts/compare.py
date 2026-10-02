"""Compares gx with ripgrep and GNU grep on real files: do they print the same lines, and how long
do they take?

    python scripts/compare.py check             # diff gx against rg on the cargo registry sources
    python scripts/compare.py bench             # timings on the tree and on two large single files
    python scripts/compare.py bench tree|files  # only one part
    python scripts/compare.py bench --runs 7

Corpora (the registry sources are the crates you have already downloaded with cargo):
    TREE   ~/.cargo/registry/src
    BIG1   target/bench/rust-all.txt   every .rs file of TREE concatenated (see README)
    BIG2   target/bench/access.log     python ../11-log-analyzer/scripts/gen_log.py target/bench/access.log 2000000
"""
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GX = str(ROOT / "target" / "release" / "gx.exe")
RG = "rg"
GREP = r"C:\Program Files\Git\usr\bin\grep.exe"
TREE = Path(os.path.expanduser("~/.cargo/registry/src"))
BIG1 = ROOT / "target" / "bench" / "rust-all.txt"
BIG2 = ROOT / "target" / "bench" / "access.log"


def run(cmd, cwd=None):
    return subprocess.run(cmd, cwd=cwd, capture_output=True)


# ripgrep ships a test file with a NUL byte after its first 8 KiB. rg notices it at any depth and
# stops searching the file; gx probes only the first 8 KiB, so it searches the whole file. This is a
# documented difference, so those lines are left out of the comparison and counted separately.
KNOWN = "sherlock-nul.txt"
known_skipped = 0


def norm(b):
    global known_skipped
    lines = [l.replace("\\", "/") for l in b.decode("utf-8", "replace").replace("\r\n", "\n").split("\n") if l]
    kept = [l for l in lines if KNOWN not in l]
    known_skipped += len(lines) - len(kept)
    return sorted(kept)


# name, gx args, rg args (rg needs -n --no-heading to print path:line:text like gx)
CHECKS = [
    ("literal", ["TODO"], ["TODO"]),
    ("literal common", ["unsafe"], ["unsafe"]),
    ("literal with space", ["fn main"], ["fn main"]),
    ("ignore case", ["-i", "todo"], ["-i", "todo"]),
    ("word", ["-w", "Result"], ["-w", "Result"]),
    ("regex fn signature", [r"fn\s+\w+\s*\("], [r"fn\s+\w+\s*\("]),
    ("anchored", ["^use std::"], ["^use std::"]),
    ("char classes", ["[A-Z]{4,}_[A-Z]+"], ["[A-Z]{4,}_[A-Z]+"]),
    ("alternation", ["(foo|bar)_baz|impl<T>"], ["(foo|bar)_baz|impl<T>"]),
    ("fixed string", ["-F", ".unwrap()"], ["-F", ".unwrap()"]),
    ("smart case lower", ["-S", "rust"], ["-S", "rust"]),
    ("smart case upper", ["-S", "Rust"], ["-S", "Rust"]),
    ("only matching", ["-o", "https?://[a-z.]+"], ["-o", "https?://[a-z.]+"]),
    ("context", ["-C", "1", "panic!"], ["-C", "1", "panic!"]),
    ("files with matches", ["-l", "extern crate"], ["-l", "extern crate"]),
    ("count", ["-c", "pub fn"], ["-c", "pub fn"]),
    ("glob", ["-g", "*.toml", "version"], ["-g", "*.toml", "version"]),
    ("type", ["-t", "rust", "Box<dyn"], ["-t", "rust", "Box<dyn"]),
    ("comment todo", [r"^\s*//.*TODO"], [r"^\s*//.*TODO"]),
    ("several -e", ["-e", "foo", "-e", "bar", "-e", "baz"], ["-e", "foo", "-e", "bar", "-e", "baz"]),
    ("hidden", ["--hidden", "TODO"], ["--hidden", "TODO"]),
    ("whole line", ["-x", "}"], ["-x", "}"]),
    ("invert count", ["-v", "-c", "e"], ["-v", "-c", "e"]),
    ("max count", ["-m", "2", "use "], ["-m", "2", "use "]),
]


def check():
    bad = 0
    for name, gx_args, rg_args in CHECKS:
        a = run([GX, "--color", "never", *gx_args], cwd=TREE)
        b = run([RG, "--color", "never", "-n", "--no-heading", *rg_args], cwd=TREE)
        la, lb = norm(a.stdout), norm(b.stdout)
        same = la == lb
        print(f"{'ok  ' if same else 'DIFF'} {name:<22} gx {len(la):>7} lines   rg {len(lb):>7} lines")
        if not same:
            bad += 1
            for l in sorted(set(la) - set(lb))[:3]:
                print("      only gx:", l[:150])
            for l in sorted(set(lb) - set(la))[:3]:
                print("      only rg:", l[:150])
    print(f"\n{len(CHECKS) - bad} of {len(CHECKS)} searches print exactly the same lines as rg")
    print(f"({known_skipped} output lines about {KNOWN}, which has a late NUL byte, were left out of the comparison)")
    return bad


def timeit(cmd, cwd, runs, capture=False):
    """Median wall time. `capture` reads stdout through a pipe instead of discarding it: GNU grep
    stops at the first match when its output is the null device, which would make it look instant."""
    out = subprocess.PIPE if capture else subprocess.DEVNULL

    def once():
        s = time.perf_counter()
        subprocess.run(cmd, cwd=cwd, stdout=out, stderr=subprocess.DEVNULL)
        return time.perf_counter() - s

    once()  # warm-up
    return statistics.median(once() for _ in range(runs))


def bench_tree(runs):
    cases = [
        ("literal, rare", ["XYZZY_NOT_PRESENT"]),
        ("literal, common", ["unsafe"]),
        ("literal, ignore case", ["-i", "todo"]),
        ("regex", [r"fn\s+\w+\s*\("]),
        ("alternation of literals", ["-e", "unsafe", "-e", "TODO", "-e", "FIXME", "-e", "panic!"]),
        ("whole word", ["-w", "Result"]),
        ("count, every line", ["-c", "e"]),
    ]
    print(f"== directory tree: {TREE}")
    print(f"{'search':<26}{'gx':>9}{'gx -j1':>9}{'rg':>9}{'rg -j1':>9}")
    for name, args in cases:
        gx = timeit([GX, "--color", "never", *args], TREE, runs)
        gx1 = timeit([GX, "--color", "never", "-j", "1", *args], TREE, runs)
        rg = timeit([RG, "--color", "never", "-n", "--no-heading", *args], TREE, runs)
        rg1 = timeit([RG, "--color", "never", "-n", "--no-heading", "-j", "1", *args], TREE, runs)
        print(f"{name:<26}{gx*1e3:>7.0f}ms{gx1*1e3:>7.0f}ms{rg*1e3:>7.0f}ms{rg1*1e3:>7.0f}ms")


# Patterns avoid backslashes so all three tools receive exactly the same text: the MSYS build of GNU
# grep rewrites backslashes in arguments it is given by a native Windows program.
SINGLE_FILES = [
    (BIG1, "rust sources", [
        ("literal, rare", ["XYZZY_NOT_PRESENT"], False),
        ("literal, common", ["unsafe"], False),
        ("literal, ignore case", ["-i", "todo"], False),
        ("regex", ["fn +[A-Za-z0-9_]+ *[(]"], True),
        ("alternation of literals", ["unsafe|TODO|FIXME|panic!"], True),
        ("whole word", ["-w", "Result"], False),
        ("inverted", ["-v", "e"], False),
    ]),
    (BIG2, "access log", [
        ("literal, rare", ["XYZZY_NOT_PRESENT"], False),
        ("literal", ["POST /api/orders"], False),
        ("literal, ignore case", ["-i", "curl"], False),
        ("regex", ['" (404|500|503) '], True),
        ("regex with wildcards", ["Mozilla.*Win64.*0[.]1"], True),
        ("whole word", ["-w", "healthz"], False),
    ]),
]


def bench_files(runs):
    for path, label, cases in SINGLE_FILES:
        size = path.stat().st_size / 1e6
        print(f"\n== one file, {label}, {size:.0f} MB, `-c` (number of matching lines)")
        print(f"{'search':<26}{'gx':>9}{'rg':>9}{'GNU grep':>10}   matching lines (gx / rg / grep)")
        for name, args, extended in cases:
            tools = [(GX, ["--color", "never"], args), (RG, ["--color", "never"], args), (GREP, ["-E"] if extended else [], args)]
            counts, times = [], []
            for tool, extra, targs in tools:
                cmd = [tool, *extra, "-c", *targs, str(path)]
                # gx and rg print nothing for a file with no match; grep prints 0.
                counts.append(run(cmd).stdout.decode().strip() or "0")
                times.append(timeit(cmd, None, runs, capture=True))
            verdict = "same" if len(set(counts)) == 1 else "DIFFERENT"
            print(f"{name:<26}{times[0]*1e3:>7.0f}ms{times[1]*1e3:>7.0f}ms{times[2]*1e3:>8.0f}ms   {counts[0]} / {counts[1]} / {counts[2]}  {verdict}")


if __name__ == "__main__":
    args = sys.argv[1:]
    cmd = args[0] if args else "check"
    if cmd == "check":
        sys.exit(1 if check() else 0)
    runs = int(args[args.index("--runs") + 1]) if "--runs" in args else 5
    part = args[1] if len(args) > 1 and args[1] in ("tree", "files") else "all"
    print(f"median of {runs} runs after a warm-up run; files already in the OS cache\n")
    if part in ("tree", "all"):
        bench_tree(runs)
    if part in ("files", "all"):
        bench_files(runs)

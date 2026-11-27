//! Unified diffs as `gh pr diff` and `glab mr diff --raw` print them.

use serde::Serialize;

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileDiff {
    /// The path after the change; for a deleted file, the path it had.
    pub path: String,
    /// The path before a rename; `None` when the path did not change.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    /// Lockfiles, generated code and snapshots: folded and not counted as reading work.
    pub generated: bool,
    /// Stable across runs and Rust versions: a viewed mark is kept only while this matches.
    pub hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hunk {
    /// The whole `@@ -a,b +c,d @@ context` line.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line number before the change; `None` on an added line.
    pub old: Option<u32>,
    /// Line number after the change; `None` on a removed line.
    pub new: Option<u32>,
    /// The line without its leading ` `, `+` or `-`.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// Splits a multi-file unified diff into files. Never fails: text it cannot read is skipped.
pub fn parse_unified(text: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut start: Option<usize> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            if let Some(from) = start {
                files.extend(text.get(from..offset).and_then(parse_file));
            }
            start = Some(offset);
        }
        offset += line.len();
    }
    if let Some(from) = start {
        files.extend(text.get(from..).and_then(parse_file));
    }
    files
}

/// Whether `path` is a lockfile, generated code or a snapshot.
pub fn is_generated(path: &str) -> bool {
    const NAMES: [&str; 8] =
        ["Cargo.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "go.sum", "poetry.lock", "Gemfile.lock", "composer.lock"];
    const SUFFIXES: [&str; 5] = [".pb.go", "_pb2.py", ".min.js", ".min.css", ".snap"];
    let name = path.rsplit('/').next().unwrap_or(path);
    NAMES.contains(&name)
        || SUFFIXES.iter().any(|suffix| path.ends_with(suffix))
        || path.contains(".generated.")
        || path.split('/').rev().skip(1).any(|dir| dir == "__snapshots__")
}

/// FNV-1a 64-bit, written out so the hash does not change with the Rust version.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// What the header lines of one file say, before its first hunk.
#[derive(Default)]
struct Header {
    minus: Option<String>,
    plus: Option<String>,
    minus_null: bool,
    plus_null: bool,
    rename_from: Option<String>,
    rename_to: Option<String>,
    new_file: bool,
    deleted_file: bool,
    binary: bool,
}

impl Header {
    fn read(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("--- ") {
            self.minus_null = rest == "/dev/null";
            self.minus = (!self.minus_null).then(|| header_path(rest));
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            self.plus_null = rest == "/dev/null";
            self.plus = (!self.plus_null).then(|| header_path(rest));
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            self.rename_from = Some(unquote(rest));
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            self.rename_to = Some(unquote(rest));
        } else if line.starts_with("new file mode") {
            self.new_file = true;
        } else if line.starts_with("deleted file mode") {
            self.deleted_file = true;
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            self.binary = true;
        }
    }
}

/// A path as git prints it. Git C-quotes names with non-ASCII bytes or specials: `"caf\303\251.go"`.
fn unquote(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else { return path.to_string() };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut rest = inner.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        rest = tail;
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match rest {
            [a @ b'0'..=b'3', b @ b'0'..=b'7', c @ b'0'..=b'7', tail @ ..] => {
                bytes.push((a - b'0') * 64 + (b - b'0') * 8 + (c - b'0'));
                rest = tail;
            }
            [e, tail @ ..] => {
                bytes.push(match e {
                    b'n' => b'\n',
                    b't' => b'\t',
                    other => *other,
                });
                rest = tail;
            }
            [] => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `a/X` or `"a/X"` as `---`/`+++` print it (git adds a tab after names with spaces).
fn header_path(raw: &str) -> String {
    let path = unquote(raw.trim_end_matches('\t'));
    path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).map(str::to_string).unwrap_or(path)
}

/// The two paths on `diff --git a/X b/Y`; ambiguous when a name has spaces, so only a fallback.
fn git_line_paths(rest: &str) -> (String, String) {
    if rest.starts_with('"')
        && let Some(split) = rest.find("\" ")
    {
        let (a, b) = rest.split_at(split + 1);
        return (header_path(a), header_path(b.trim_start()));
    }
    let candidates = rest.match_indices(" b/").map(|(i, _)| i);
    let split = candidates
        .clone()
        .find(|&i| rest.get(..i).zip(rest.get(i + 1..)).is_some_and(|(a, b)| a.get(2..) == b.get(2..)))
        .or(candidates.into_iter().next());
    match split.and_then(|i| rest.get(..i).zip(rest.get(i + 1..))) {
        Some((a, b)) => (header_path(a), header_path(b)),
        None => (header_path(rest), header_path(rest)),
    }
}

fn parse_file(patch: &str) -> Option<FileDiff> {
    let mut lines = patch.lines();
    let rest = lines.next()?.strip_prefix("diff --git ")?;
    let (git_old, git_new) = git_line_paths(rest);
    let mut header = Header::default();
    let mut hunks: Vec<Hunk> = Vec::new();
    // `None` while the current `@@` line is unreadable: its body is skipped.
    let mut cursor: Option<Cursor> = None;
    let mut in_hunk = false;
    for raw in patch.split_inclusive('\n').skip(1) {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        if line.starts_with("@@") {
            in_hunk = true;
            cursor = parse_hunk_header(line);
            if cursor.is_some() {
                hunks.push(Hunk { header: line.trim_end_matches('\r').to_string(), lines: Vec::new() });
            }
        } else if !in_hunk {
            header.read(line.trim_end_matches('\r'));
            if header.binary {
                break;
            }
        } else if let (Some(cursor), Some(hunk)) = (cursor.as_mut(), hunks.last_mut()) {
            hunk.lines.extend(cursor.advance(line));
        }
    }
    Some(build(header, hunks, (git_old, git_new), fnv1a(patch.as_bytes())))
}

fn build(header: Header, hunks: Vec<Hunk>, (git_old, git_new): (String, String), hash: u64) -> FileDiff {
    let renamed = header.rename_from.is_some() && header.rename_to.is_some();
    let status = if header.new_file || header.minus_null {
        FileStatus::Added
    } else if header.deleted_file || header.plus_null {
        FileStatus::Deleted
    } else if renamed {
        FileStatus::Renamed
    } else {
        FileStatus::Modified
    };
    let old = header.rename_from.or(header.minus).unwrap_or(git_old);
    let new = header.rename_to.or(header.plus).unwrap_or(git_new);
    let path = if status == FileStatus::Deleted { old.clone() } else { new };
    let count = |kind| hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == kind).count() as u32;
    FileDiff {
        old_path: (status == FileStatus::Renamed).then_some(old),
        status,
        additions: count(LineKind::Added),
        deletions: count(LineKind::Removed),
        binary: header.binary,
        generated: is_generated(&path),
        path,
        hunks,
        hash,
    }
}

/// The next line numbers inside a hunk.
struct Cursor {
    old: u32,
    new: u32,
}

impl Cursor {
    fn advance(&mut self, line: &str) -> Option<DiffLine> {
        let mut chars = line.chars();
        let kind = match chars.next()? {
            ' ' => LineKind::Context,
            '+' => LineKind::Added,
            '-' => LineKind::Removed,
            _ => return None,
        };
        let old = (kind != LineKind::Added).then_some(self.old);
        let new = (kind != LineKind::Removed).then_some(self.new);
        self.old = self.old.saturating_add(u32::from(old.is_some()));
        self.new = self.new.saturating_add(u32::from(new.is_some()));
        Some(DiffLine { kind, old, new, text: chars.as_str().to_string() })
    }
}

/// Reads `@@ -a[,b] +c[,d] @@ ctx` into the starting line numbers.
fn parse_hunk_header(line: &str) -> Option<Cursor> {
    let (old, rest) = line.strip_prefix("@@ -")?.split_once(' ')?;
    let (new, rest) = rest.strip_prefix('+')?.split_once(' ')?;
    rest.starts_with("@@").then_some(())?;
    Some(Cursor { old: range_start(old)?, new: range_start(new)? })
}

/// `12,5` or `12`: the first number; the count must still be a number.
fn range_start(range: &str) -> Option<u32> {
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    count.parse::<u32>().ok()?;
    start.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Invented data shaped like `gh pr diff` output: a rename with edits, an add, a delete, a binary.
    const MULTI: &str = "\
diff --git a/billing/old.go b/billing/invoicer.go
similarity index 80%
rename from billing/old.go
rename to billing/invoicer.go
index 1111111..2222222 100644
--- a/billing/old.go
+++ b/billing/invoicer.go
@@ -1,4 +1,4 @@
 package billing
-func Old() {}
+func Invoice() {}
 // keep
 // keep too
@@ -20,3 +20,4 @@ func Tail() {
 a
+b
 c
 d
diff --git a/docs/new.md b/docs/new.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1,2 @@
+# Title
+body
diff --git a/old/gone.txt b/old/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/old/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
\\ No newline at end of file
diff --git a/img/logo.png b/img/logo.png
index 5555555..6666666 100644
Binary files a/img/logo.png and b/img/logo.png differ
";

    #[test]
    fn splits_files_and_reads_statuses() {
        let files = parse_unified(MULTI);
        let summary: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.status, f.old_path.as_deref(), f.binary)).collect();
        assert_eq!(
            summary,
            [
                ("billing/invoicer.go", FileStatus::Renamed, Some("billing/old.go"), false),
                ("docs/new.md", FileStatus::Added, None, false),
                ("old/gone.txt", FileStatus::Deleted, None, false),
                ("img/logo.png", FileStatus::Modified, None, true),
            ]
        );
        assert!(files.last().is_some_and(|f| f.hunks.is_empty()));
    }

    #[test]
    fn counts_and_numbers_lines_across_two_hunks() {
        let files = parse_unified(MULTI);
        let file = &files[0];
        assert_eq!((file.additions, file.deletions), (2, 1));
        assert_eq!(file.hunks.len(), 2);
        assert_eq!(file.hunks[0].header, "@@ -1,4 +1,4 @@");
        let numbers: Vec<_> = file.hunks[0].lines.iter().map(|l| (l.kind, l.old, l.new)).collect();
        assert_eq!(
            numbers,
            [
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Removed, Some(2), None),
                (LineKind::Added, None, Some(2)),
                (LineKind::Context, Some(3), Some(3)),
                (LineKind::Context, Some(4), Some(4)),
            ]
        );
        assert_eq!(file.hunks[0].lines[1].text, "func Old() {}");
        assert_eq!(file.hunks[1].header, "@@ -20,3 +20,4 @@ func Tail() {");
        let second: Vec<_> = file.hunks[1].lines.iter().map(|l| (l.old, l.new)).collect();
        assert_eq!(second, [(Some(20), Some(20)), (None, Some(21)), (Some(21), Some(22)), (Some(22), Some(23))]);
    }

    #[test]
    fn no_newline_marker_is_not_a_line() {
        let files = parse_unified(MULTI);
        let gone = &files[2];
        assert_eq!(gone.hunks[0].lines.len(), 1);
        assert_eq!((gone.additions, gone.deletions), (0, 1));
    }

    #[test]
    fn added_file_starts_at_line_one_and_hunk_count_defaults_to_one() {
        let files = parse_unified(MULTI);
        let new = &files[1].hunks[0].lines;
        assert_eq!(new.iter().map(|l| l.new).collect::<Vec<_>>(), [Some(1), Some(2)]);
        assert_eq!(files[2].hunks[0].lines[0].old, Some(1));
    }

    #[test]
    fn path_with_spaces_comes_from_the_plus_lines() {
        // Invented; git appends a tab after a name that contains spaces.
        let text =
            "diff --git a/my docs/read me.md b/my docs/read me.md\n--- a/my docs/read me.md\t\n+++ b/my docs/read me.md\t\n@@ -1 +1 @@\n-a\n+b\n";
        let files = parse_unified(text);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "my docs/read me.md");
        assert_eq!(files[0].status, FileStatus::Modified);
    }

    #[test]
    fn binary_file_with_spaces_falls_back_to_the_git_line() {
        let text = "diff --git a/my img/logo 2.png b/my img/logo 2.png\nnew file mode 100644\nindex 0000000..1111111\nGIT binary patch\nliteral 3\nKcmZQz00000\n\nliteral 0\nHcmV?d00001\n\n";
        let files = parse_unified(text);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "my img/logo 2.png");
        assert_eq!(files[0].status, FileStatus::Added);
        assert!(files[0].binary);
        assert!(files[0].hunks.is_empty());
    }

    #[test]
    fn pure_rename_has_no_hunks_and_quoted_paths_lose_their_quotes() {
        let text = "diff --git \"a/caf\\303\\251 old.txt\" \"b/caf\\303\\251 new.txt\"\nsimilarity index 100%\nrename from \"caf\\303\\251 old.txt\"\nrename to \"caf\\303\\251 new.txt\"\n";
        let files = parse_unified(text);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Renamed);
        assert_eq!(files[0].path, "café new.txt", "git's octal escapes decode to the real name");
        assert_eq!(files[0].old_path.as_deref(), Some("café old.txt"));
        assert!(files[0].hunks.is_empty());
    }

    #[test]
    fn removed_line_that_looks_like_a_header_stays_a_line() {
        let text = "diff --git a/a.sql b/a.sql\n--- a/a.sql\n+++ b/a.sql\n@@ -1,2 +1,1 @@\n--- comment\n keep\n";
        let files = parse_unified(text);
        assert_eq!(files[0].deletions, 1);
        assert_eq!(files[0].path, "a.sql");
    }

    #[test]
    fn hash_is_stable_and_changes_with_any_line() {
        let one = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        let two = one.replace("+b", "+c");
        let hash = |t: &str| parse_unified(t)[0].hash;
        assert_eq!(hash(one), hash(one));
        assert_ne!(hash(one), hash(&two));
        // Pinned: FNV-1a must not drift with the toolchain.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn a_file_hash_ignores_its_neighbours() {
        let files = parse_unified(MULTI);
        let alone = parse_unified(&MULTI[MULTI.find("diff --git a/docs").unwrap_or(0)..]);
        assert_eq!(files[1].hash, alone[0].hash);
    }

    #[test]
    fn garbage_and_empty_input_give_nothing() {
        assert!(parse_unified("").is_empty());
        assert!(parse_unified("not a diff\n@@ -1 +1 @@\n+x\n").is_empty());
    }

    #[test]
    fn malformed_hunk_header_skips_only_that_hunk() {
        let text = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -oops +1 @@\n-lost\n@@ -5 +5 @@\n-a\n+b\n";
        let files = parse_unified(text);
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!((files[0].additions, files[0].deletions), (1, 1));
        assert_eq!(files[0].hunks[0].lines[0].old, Some(5));
    }

    #[test]
    fn generated_files_are_flagged() {
        for path in [
            "Cargo.lock",
            "web/package-lock.json",
            "go.sum",
            "api/user.pb.go",
            "x_pb2.py",
            "dist/app.min.js",
            "a.min.css",
            "ui/a.snap",
            "src/schema.generated.ts",
            "tests/__snapshots__/a.txt",
        ] {
            assert!(is_generated(path), "{path}");
        }
        for path in ["src/main.rs", "Cargo.toml", "lock.rs", "__snapshots__", "snap.rs"] {
            assert!(!is_generated(path), "{path}");
        }
        let files = parse_unified("diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n@@ -1 +1 @@\n-a\n+b\n");
        assert!(files[0].generated);
    }
}

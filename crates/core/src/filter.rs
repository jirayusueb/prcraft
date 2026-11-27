//! Include and exclude filename patterns for a change's files (`internal/**`, `!*_test.go`).

/// Whether `path` stays visible under `patterns`.
///
/// A pattern starting with `!` excludes; any other pattern includes. With no include pattern every
/// file is included. A file is visible when it matches an include (or there is none) and no exclude.
/// `*` matches within one path segment, `**` matches any number of segments, `?` one character.
/// A pattern without `/` matches the file name in any folder.
pub fn visible(patterns: &[String], path: &str) -> bool {
    let mut has_include = false;
    let mut included = false;
    for raw in patterns {
        let raw = raw.trim();
        let (exclude, pattern) = match raw.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, raw),
        };
        if pattern.is_empty() {
            continue;
        }
        let hit = matches(pattern, path);
        if exclude {
            if hit {
                return false;
            }
        } else {
            has_include = true;
            included |= hit;
        }
    }
    included || !has_include
}

fn matches(pattern: &str, path: &str) -> bool {
    let subject = if pattern.contains('/') { path } else { path.rsplit('/').next().unwrap_or(path) };
    let pattern: Vec<&str> = pattern.split('/').collect();
    let subject: Vec<&str> = subject.split('/').collect();
    match_segments(&pattern, &subject)
}

fn match_segments(pattern: &[&str], subject: &[&str]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return subject.is_empty();
    };
    if *first == "**" {
        return (0..=subject.len()).any(|skip| subject.get(skip..).is_some_and(|tail| match_segments(rest, tail)));
    }
    subject.split_first().is_some_and(|(segment, tail)| {
        let p: Vec<char> = first.chars().collect();
        let s: Vec<char> = segment.chars().collect();
        match_chars(&p, &s) && match_segments(rest, tail)
    })
}

/// One path segment: `*` is any run of characters, `?` exactly one. Greedy with one backtrack
/// point, so a pattern full of `*` stays linear while the user types it.
fn match_chars(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while let Some(&c) = text.get(t) {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&want) if want == '?' || want == c => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    star = Some((star_p, star_t + 1));
                    p = star_p + 1;
                    t = star_t + 1;
                }
                None => return false,
            },
        }
    }
    pattern.get(p..).is_some_and(|rest| rest.iter().all(|&c| c == '*'))
}

#[cfg(test)]
mod tests {
    use super::visible;

    fn pats(list: &[&str]) -> Vec<String> {
        list.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn no_patterns_shows_everything() {
        assert!(visible(&[], "any/path.rs"));
        assert!(visible(&pats(&["", "  ", "!"]), "any/path.rs"));
    }

    #[test]
    fn folder_include_matches_files_below_it_only() {
        let p = pats(&["internal/**"]);
        assert!(visible(&p, "internal/billing/invoicer.go"));
        assert!(visible(&p, "internal/x.go"));
        assert!(!visible(&p, "cmd/main.go"));
        assert!(!visible(&p, "pkg/internal/x.go"));
    }

    #[test]
    fn bare_exclude_matches_file_name_in_any_folder() {
        let p = pats(&["!*_test.go"]);
        assert!(!visible(&p, "billing/invoicer_test.go"));
        assert!(!visible(&p, "invoicer_test.go"));
        assert!(visible(&p, "billing/invoicer.go"));
    }

    #[test]
    fn include_and_exclude_together() {
        let p = pats(&["internal/**", "!*_test.go"]);
        assert!(visible(&p, "internal/billing/invoicer.go"));
        assert!(!visible(&p, "internal/billing/invoicer_test.go"));
        assert!(!visible(&p, "cmd/main.go"));
    }

    #[test]
    fn double_star_in_the_middle_spans_zero_or_more_folders() {
        let p = pats(&["src/**/mod.rs"]);
        assert!(visible(&p, "src/mod.rs"));
        assert!(visible(&p, "src/a/b/mod.rs"));
        assert!(!visible(&p, "src/a/b/lib.rs"));
        assert!(!visible(&p, "other/mod.rs"));
    }

    #[test]
    fn single_star_does_not_cross_slash_and_question_mark_is_one_char() {
        let p = pats(&["src/*.rs"]);
        assert!(visible(&p, "src/lib.rs"));
        assert!(!visible(&p, "src/a/lib.rs"));
        let q = pats(&["?.md"]);
        assert!(visible(&q, "docs/a.md"));
        assert!(!visible(&q, "docs/ab.md"));
    }

    #[test]
    fn many_stars_on_a_miss_finish() {
        let p = pats(&["*a*a*a*a*a*a*b"]);
        assert!(!visible(&p, &"a".repeat(24)));
    }
}

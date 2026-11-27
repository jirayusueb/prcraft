//! Names a forge or a clone hands us that git will later read as a ref or an option. No I/O.
//!
//! The rules are `git check-ref-format --branch`, plus one of ours: a name may never start with `-`, so it cannot be read as
//! a flag wherever a command line takes it.

/// Why `name` cannot be a branch or remote name, or `Ok` when git accepts it.
pub fn check_branch(name: &str) -> Result<(), String> {
    check_name(name)?;
    if name == "@" || name == "HEAD" {
        return Err(format!("`{name}` is not a branch name git accepts"));
    }
    Ok(())
}

/// [`check_branch`] without the two names only a branch may not take; a remote may be called anything else a ref may.
pub fn check_name(name: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("`{}` is not a name git accepts: {why}", name.escape_debug()));
    if name.is_empty() {
        return bad("it is empty");
    }
    if name.len() > 255 {
        return bad("it is longer than 255 bytes");
    }
    if name.starts_with('-') {
        return bad("it starts with `-`, which a command would read as an option");
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return bad("it has an empty path part");
    }
    if name.ends_with('.') || name.contains("..") || name.contains("@{") {
        return bad("it has `..`, `@{` or a trailing `.`");
    }
    if name.chars().any(|c| c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')) {
        return bad("it has a space, a control character or one of ~ ^ : ? * [ \\");
    }
    if name.split('/').any(|part| part.starts_with('.') || part.ends_with(".lock")) {
        return bad("a part starts with `.` or ends with `.lock`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_could_be_an_option_or_a_hostile_ref_is_refused() {
        for bad in [
            "",
            "-x",
            "--upload-pack=sh",
            "a..b",
            "a b",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[0]",
            "a\\b",
            "a/",
            "/a",
            "a//b",
            ".hidden",
            "a/.b",
            "x.lock",
            "a/x.lock",
            "a.",
            "a@{1}",
            "@",
            "HEAD",
            "tab\there",
            "nul\0",
        ] {
            assert!(check_branch(bad).is_err(), "{bad:?} must be refused");
        }
        for good in ["main", "tokafor/finalize-tx", "release-1.2", "feature/ünï", "a@b", "x.locked"] {
            assert!(check_branch(good).is_ok(), "{good:?} must be accepted");
        }
        assert!(check_name("HEAD").is_ok(), "a remote may be called HEAD; only a branch may not");
    }
}

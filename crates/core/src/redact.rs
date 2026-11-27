//! What the AI engine may read (Settings → AI engine → what is sent): the read scope, files that never leave this Mac,
//! and secret redaction. No I/O.
//!
//! The redactor works on the shapes of secret, not on the values: vendor token prefixes, private key blocks, credentials
//! inside URLs, `Basic` and `Bearer` headers, and the value assigned to a name that ends in `password`, `secret`, `token`,
//! `api key`, `private key` and the like. It leans towards masking: a false positive costs a word of the diff, a leak costs a
//! credential. Code that only mentions such a name (a call, a type annotation, a variable on the right-hand side) stays
//! readable. The never-send patterns stay the stronger guard: a file that matches one is not sent at all.

use std::collections::BTreeMap;
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::filter;

/// What replaces a secret in text the engine will see.
pub const MASK: &str = "[redacted]";

/// What the UI says next to the read rules (frame 101) and on the AI review sheet. It is the documented limit of the
/// redactor, not a promise about every secret.
pub const REDACTION_NOTICE: &str = "Secret redaction is best effort: it catches common key, token and password shapes. Files matching never-send patterns are never sent. Add a pattern for anything that must not leave this Mac. Preview exactly what is sent before you start.";

/// How much of the repository an agent run may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadScope {
    /// Only the diff text. The one scope that is built: PR Craft keeps no checkout to read other files from.
    #[default]
    DiffOnly,
    DiffAndChangedFiles,
    WholeCheckout,
}

/// The rules every prompt passes before it is built (frame 101). A repository may replace them whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadRules {
    pub scope: ReadScope,
    /// Files matching any of these patterns are dropped whole ([`filter`] syntax, without `!`; case does not matter).
    pub never_send: Vec<String>,
    /// Strings that look like keys or tokens become `[redacted]`.
    pub redact_secrets: bool,
}

/// The never-send patterns a new install starts with: environment files, key and certificate files, secrets folders at
/// any depth, package registry and netrc credentials, and Terraform state and variables.
pub const DEFAULT_NEVER_SEND: [&str; 17] = [
    "**/.env*",
    "**/*.pem",
    "**/*.key",
    "**/*.p12",
    "**/*.pfx",
    "**/*.jks",
    "**/*.keystore",
    "**/id_rsa*",
    "**/id_ed25519*",
    "**/id_ecdsa*",
    "**/.npmrc",
    "**/.netrc",
    "**/*.tfstate*",
    "**/*.tfvars",
    "**/secrets/**",
    "**/secret/**",
    "**/credentials*",
];

impl Default for ReadRules {
    fn default() -> Self {
        ReadRules { scope: ReadScope::DiffOnly, never_send: DEFAULT_NEVER_SEND.map(String::from).into(), redact_secrets: true }
    }
}

impl ReadRules {
    /// Whether the file at `path` must never be sent. Case does not matter: `certs/TLS.PEM` is a certificate.
    pub fn withheld(&self, path: &str) -> bool {
        let patterns: Vec<String> =
            self.never_send.iter().filter(|p| !p.trim().is_empty() && !p.starts_with('!')).map(|p| p.trim().to_lowercase()).collect();
        !patterns.is_empty() && filter::visible(&patterns, &path.to_lowercase())
    }
}

/// Text with its secrets masked, and how many were found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub found: usize,
}

/// A private key block with no END marker ends here, so a stray BEGIN line cannot swallow a whole file.
const MAX_KEY_LINES: usize = 300;

/// Masks the secrets in `text`, line by line; each input line gives exactly one output line. A private key block is masked
/// whole, however it is written (raw lines, string literals, escaped `\n`), up to its END marker.
pub fn redact(text: &str) -> Redacted {
    let mut out = String::with_capacity(text.len());
    let mut found = 0;
    let mut in_key: Option<usize> = None;
    let raws: Vec<&str> = text.split_inclusive('\n').collect();
    for (at, raw) in raws.iter().enumerate() {
        let body = raw.trim_end_matches(['\r', '\n']);
        let next = raws.get(at + 1).map(|next| next.trim_end_matches(['\r', '\n']));
        let (line, n) = redact_line(body, next, &mut in_key);
        out.push_str(&line);
        out.push_str(raw.get(body.len()..).unwrap_or_default());
        found += n;
    }
    Redacted { text: out, found }
}

/// Whether `line` reads as the first line of a key body: base64 (alone or as a string literal with quotes, `\n`, `+` and
/// commas around it), or one of the armor headers of an encrypted or PGP key.
fn looks_like_key_body(line: &str, pgp: bool) -> bool {
    // A commented-out key: `# MIIE…`, ` * MIIE…`, `> MIIE…`, `-- MIIE…`, ` | MIIE…`.
    let trimmed = line.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '#' | '*' | '>' | '-' | '|' | ';' | '/' | '"' | '\'' | '`'));
    if ["Proc-Type:", "DEK-Info:", "Version:", "Comment:", "Charset:", "Hash:"].iter().any(|header| trimmed.starts_with(header)) {
        return true;
    }
    if trimmed.trim().is_empty() {
        return pgp;
    }
    let bare: String = trimmed
        .replace("\\n", "")
        .replace("\\r", "")
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '`' | '+' | ',' | '\\' | ' ' | '\t' | ';' | ')'))
        .collect();
    let long_run = line.split(|c: char| !is_base64(c)).any(|part| part.len() >= 40);
    (bare.len() >= 16 && bare.chars().all(is_base64)) || long_run
}

/// The base64 run that follows a BEGIN marker on its line (after `\n` escapes and blanks), if it is long enough for a key.
fn key_body_after(line: &str, from: usize) -> Option<Range<usize>> {
    let mut at = from;
    while let Some(rest) = line.get(at..) {
        if rest.starts_with("\\n") || rest.starts_with("\\r") {
            at += 2;
        } else if rest.starts_with([' ', '\t', '\n', '\r']) {
            at += 1;
        } else {
            break;
        }
    }
    let len = run(line, at, is_base64);
    (len >= 16).then_some(at..at + len)
}

/// The span of a `-----BEGIN …-----` or `-----END …-----` marker whose label holds `PRIVATE KEY`, at or after `from`.
fn key_marker(line: &str, word: &str, from: usize) -> Option<Range<usize>> {
    let mut at = from;
    loop {
        let start = at + line.get(at..)?.find(word)?;
        let label_from = start + word.len();
        if let Some(close) = line.get(label_from..).and_then(|rest| rest.find("-----"))
            && line.get(label_from..label_from + close).is_some_and(|label| label.contains("PRIVATE KEY"))
        {
            return Some(start..label_from + close + "-----".len());
        }
        at = label_from;
    }
}

fn redact_line(body: &str, next: Option<&str>, in_key: &mut Option<usize>) -> (String, usize) {
    if let Some(lines) = in_key {
        if let Some(end) = key_marker(body, "-----END ", 0) {
            *in_key = None;
            let (tail, found) = redact_line(body.get(end.end..).unwrap_or_default(), None, in_key);
            return (format!("{MASK}{tail}"), found);
        }
        *lines += 1;
        if *lines <= MAX_KEY_LINES {
            return (MASK.into(), 0);
        }
        *in_key = None;
    }
    let mut line = body.to_string();
    let mut found = 0;
    if let Some(begin) = key_marker(&line, "-----BEGIN ", 0) {
        match key_marker(&line, "-----END ", begin.end) {
            Some(end) => {
                // `"-----BEGIN PRIVATE KEY-----", "-----END PRIVATE KEY-----"` names the markers; it holds no key.
                if line.get(begin.end..end.start).is_some_and(between_is_key) {
                    line.replace_range(begin.start..end.end, MASK);
                    found += 1;
                }
            }
            None => {
                let pgp = line.get(begin.clone()).is_some_and(|marker| marker.contains("BLOCK"));
                if next.is_some_and(|next| looks_like_key_body(next, pgp)) {
                    line.truncate(begin.start);
                    line.push_str(MASK);
                    *in_key = Some(0);
                    found += 1;
                } else if let Some(body) = key_body_after(&line, begin.end) {
                    // A lone mention of the marker (a string in a key parser) is not a key, but a body run right after it is.
                    line.replace_range(body, MASK);
                    found += 1;
                }
            }
        }
    }
    let (line, n) = mask_secrets(&line);
    (line, found + n)
}

fn mask_secrets(line: &str) -> (String, usize) {
    let mut ranges = token_ranges(line);
    ranges.extend(dotted_ranges(line));
    ranges.extend(slack_webhook_ranges(line));
    ranges.extend(url_password_ranges(line));
    ranges.extend(basic_ranges(line));
    ranges.extend(bearer_ranges(line));
    ranges.extend(assignment_ranges(line));
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut out = String::with_capacity(line.len());
    let (mut cursor, mut found) = (0, 0);
    for range in ranges {
        if range.start < cursor {
            continue;
        }
        out.push_str(line.get(cursor..range.start).unwrap_or_default());
        out.push_str(MASK);
        cursor = range.end;
        found += 1;
    }
    out.push_str(line.get(cursor..).unwrap_or_default());
    (out, found)
}

// Counts the characters the scans look at, in the unit tests, so a test can check that the work grows with the length
// of a line (a relation) instead of timing it. It costs nothing outside the tests.
#[cfg(test)]
thread_local! {
    static WORK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn tick(count: usize) {
    WORK.with(|work| work.set(work.get() + count as u64));
}

#[cfg(not(test))]
#[inline(always)]
fn tick(_count: usize) {}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_dash_word(c: char) -> bool {
    is_word(c) || c == '-'
}

fn is_upper_digit(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

fn is_base64(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')
}

fn word_before(line: &str, at: usize) -> bool {
    line.get(..at).and_then(|head| head.chars().next_back()).is_some_and(|c| c.is_ascii_alphanumeric())
}

/// How many characters from `at` satisfy `class`. The classes are ASCII, so characters and bytes count the same.
fn run(line: &str, at: usize, class: fn(char) -> bool) -> usize {
    let len = line.get(at..).unwrap_or_default().chars().take_while(|&c| class(c)).count();
    tick(len);
    len
}

type Shape = (&'static str, usize, fn(char) -> bool);

/// Tokens with a fixed prefix: the prefix, the fewest characters after it, and which characters those are.
const PREFIXED: [Shape; 25] = [
    ("ghp_", 36, is_word),
    ("gho_", 36, is_word),
    ("ghu_", 36, is_word),
    ("ghs_", 36, is_word),
    ("ghr_", 36, is_word),
    ("github_pat_", 22, is_word),
    ("glpat-", 20, is_dash_word),
    ("AKIA", 16, is_upper_digit),
    ("ASIA", 16, is_upper_digit),
    ("sk_live_", 16, is_word),
    ("sk_test_", 16, is_word),
    ("rk_live_", 16, is_word),
    ("rk_test_", 16, is_word),
    ("xoxb-", 10, is_dash_word),
    ("xoxa-", 10, is_dash_word),
    ("xoxp-", 10, is_dash_word),
    ("xoxr-", 10, is_dash_word),
    ("xoxs-", 10, is_dash_word),
    ("xapp-", 10, is_dash_word),
    ("AIza", 35, is_dash_word),
    ("sk-ant-", 20, is_dash_word),
    ("sk-", 32, is_dash_word),
    ("npm_", 36, is_word),
    ("shpat_", 32, is_word),
    ("hf_", 30, is_word),
];

fn token_ranges(line: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for (prefix, min, class) in PREFIXED {
        // A match inside the run of the previous one is the same token, and is not looked at again.
        let mut consumed = 0;
        for (at, _) in line.match_indices(prefix) {
            if at < consumed || word_before(line, at) {
                continue;
            }
            let len = run(line, at + prefix.len(), class);
            if len >= min {
                consumed = at + prefix.len() + len;
                out.push(at..consumed);
            }
        }
    }
    out
}

/// Tokens made of dot-separated parts: SendGrid keys (`SG.id.secret`) and JSON web tokens (`eyJ….eyJ….signature`).
fn dotted_ranges(line: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for (prefix, first, second) in [("SG.", 16, 16), ("eyJ", 8, 8)] {
        for (at, _) in line.match_indices(prefix) {
            if word_before(line, at) {
                continue;
            }
            let head = at + prefix.len();
            let one = run(line, head, is_dash_word);
            let dot = head + one;
            if one < first || line.get(dot..dot + 1) != Some(".") {
                continue;
            }
            let tail = dot + 1;
            let two = run(line, tail, is_dash_word);
            let valid = if prefix == "eyJ" { line.get(tail..tail + 3) == Some("eyJ") } else { true };
            if two >= second && valid {
                let sig = tail + two;
                let third = if line.get(sig..sig + 1) == Some(".") { run(line, sig + 1, is_dash_word) + 1 } else { 0 };
                out.push(at..sig + third);
            }
        }
    }
    out
}

fn slack_webhook_ranges(line: &str) -> Vec<Range<usize>> {
    const HOST: &str = "hooks.slack.com/services/";
    line.match_indices(HOST)
        .filter_map(|(at, _)| {
            let start = at + HOST.len();
            let len = run(line, start, |c| c.is_ascii_alphanumeric() || c == '/');
            (len >= 10).then_some(start..start + len)
        })
        .collect()
}

/// The password of `scheme://user:password@host`.
fn url_password_ranges(line: &str) -> Vec<Range<usize>> {
    line.match_indices("://")
        .filter_map(|(at, _)| {
            let start = at + 3;
            let len = line.get(start..)?.find(|c: char| c == '@' || c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\'' | '`'))?;
            let end = start + len;
            if line.get(end..end + 1) != Some("@") {
                return None;
            }
            let colon = start + line.get(start..end)?.find(':')? + 1;
            let password = line.get(colon..end)?;
            (!password.is_empty() && !password.starts_with(['$', '{', '<', '%'])).then_some(colon..end)
        })
        .collect()
}

/// `Authorization: Basic dXNlcjpwYXNz`: base64 that is more than a capitalised word.
fn basic_ranges(line: &str) -> Vec<Range<usize>> {
    let lower = line.to_ascii_lowercase();
    lower
        .match_indices("basic ")
        .filter_map(|(at, word)| {
            if word_before(line, at) {
                return None;
            }
            let start = at + word.len();
            let len = run(line, start, is_base64);
            let token = line.get(start..start + len)?;
            let uppers = token.chars().filter(char::is_ascii_uppercase).count();
            let marked = token.contains(|c: char| c.is_ascii_digit() || matches!(c, '+' | '/' | '='));
            (len >= 12 && (marked || uppers >= 2)).then_some(start..start + len)
        })
        .collect()
}

/// `Authorization: Bearer <token>` and `Authorization: Token <token>`: a long high-entropy run after the word.
fn bearer_ranges(line: &str) -> Vec<Range<usize>> {
    let lower = line.to_ascii_lowercase();
    ["bearer ", "token "]
        .iter()
        .flat_map(|word| lower.match_indices(word))
        .filter_map(|(at, word)| {
            if word == "token " {
                // Prose such as `the token authentication_middleware_handler_v2`: a real token has a digit and no underscore.
                let after = lower.get(at + word.len()..).unwrap_or_default();
                let first_run: String = after.chars().take_while(|c| !c.is_whitespace()).collect();
                let prose = line.get(..at).and_then(|head| head.chars().next_back()).is_some_and(is_word)
                    || first_run.contains('_')
                    || !first_run.contains(|c: char| c.is_ascii_digit());
                if prose {
                    return None;
                }
            }
            let start = at + word.len();
            let len = run(line, start, |c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '+' | '/' | '=' | '-'));
            let token = line.get(start..start + len)?;
            (len >= 20 && entropy(token) >= 3.5).then_some(start..start + len)
        })
        .collect()
}

/// Shannon entropy in bits per character.
fn entropy(text: &str) -> f64 {
    let mut bytes: Vec<u8> = text.bytes().collect();
    bytes.sort_unstable();
    let total = bytes.len() as f64;
    bytes
        .chunk_by(|a, b| a == b)
        .map(|run| {
            let p = run.len() as f64 / total;
            -p * p.log2()
        })
        .sum()
}

/// What a name must end with (in whole words) for the value assigned to it to be a secret.
const SECRET_NAMES: [&str; 28] = [
    "password",
    "passwd",
    "passphrase",
    "pwd",
    "secret",
    "secretkey",
    "secretkeybase",
    "secretaccesskey",
    "token",
    "accesstoken",
    "authtoken",
    "refreshtoken",
    "apikey",
    "accesskey",
    "privatekey",
    "signingkey",
    "encryptionkey",
    "masterkey",
    "authkey",
    "appkey",
    "credential",
    "credentials",
    "clientsecret",
    "sessionkey",
    "webhooksecret",
    "passwords",
    "secrets",
    "tokens",
];

/// Splits `secretAccessKey`, `SECRET_KEY` and `x-api-key` into lowercase words.
fn words(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        if c == '_' || c == '-' {
            prev = None;
            out.push(String::new());
            continue;
        }
        let boundary = c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit());
        if boundary || out.is_empty() {
            out.push(String::new());
        }
        if let Some(word) = out.last_mut() {
            word.push(c.to_ascii_lowercase());
        }
        prev = Some(c);
    }
    out.retain(|word| !word.is_empty());
    out
}

fn is_secret_name(name: &str) -> bool {
    let words = words(name);
    let listed = (1..=3).any(|n| words.len() >= n && words.get(words.len() - n..).is_some_and(|tail| SECRET_NAMES.contains(&tail.concat().as_str())));
    // `pass` and `pat` are ordinary words (a compiler pass, a pattern) unless written as an environment variable.
    let upper = name.chars().any(|c| c.is_ascii_uppercase()) && name.chars().all(|c| is_upper_digit(c) || matches!(c, '_' | '-'));
    listed || (upper && words.last().is_some_and(|word| word == "pass" || word == "pat"))
}

/// The text between a BEGIN and an END marker on one line holds a key when, with `\n` escapes (single or doubled), quotes,
/// `+` and commas read as blanks, a run of 24 or more base64 characters with a digit, `+`, `/` or `=` in it remains. An
/// identifier (`+ privateKeyContent +`) or a sentence between the markers is not a key.
fn between_is_key(between: &str) -> bool {
    let spaced: String = between
        .replace("\\\\n", " ")
        .replace("\\n", " ")
        .replace("\\r", " ")
        .chars()
        .map(|c| if matches!(c, '"' | '\'' | '`' | '+' | ',' | '\\') { ' ' } else { c })
        .collect();
    spaced
        .split_whitespace()
        .any(|part| part.len() >= 24 && part.chars().all(is_base64) && part.contains(|c: char| c.is_ascii_digit() || matches!(c, '+' | '/' | '=')))
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// The ranges found so far, kept as disjoint intervals, so that "does a name lie inside a secret already found" is a
/// lookup, not a scan.
#[derive(Default)]
struct Covered(BTreeMap<usize, usize>);

impl Covered {
    fn add(&mut self, range: Range<usize>) {
        let (mut start, mut end) = (range.start, range.end);
        if let Some((&before, &before_end)) = self.0.range(..=start).next_back()
            && before_end >= start
        {
            start = before;
            end = end.max(before_end);
        }
        let inside: Vec<usize> = self.0.range(start..=end).map(|(&at, _)| at).collect();
        for at in inside {
            if let Some(inside_end) = self.0.remove(&at) {
                end = end.max(inside_end);
            }
        }
        self.0.insert(start, end);
    }

    fn contains(&self, at: usize) -> bool {
        self.0.range(..=at).next_back().is_some_and(|(_, &end)| at < end)
    }
}

fn assignment_ranges(line: &str) -> Vec<Range<usize>> {
    let ctx = Ctx::new(line);
    tick(line.len());
    let mut out = Vec::new();
    // A name that lies inside a range already found is part of that secret, not a new one: this keeps hostile lines cheap.
    // A name between two ranges is still looked at.
    let mut covered = Covered::default();
    let mut start: Option<usize> = None;
    for (at, c) in line.char_indices().chain(std::iter::once((line.len(), ' '))) {
        match (start, is_ident(c)) {
            (None, true) => start = Some(at),
            (Some(from), false) => {
                start = None;
                if covered.contains(from) {
                    continue;
                }
                let name = line.get(from..at).unwrap_or_default();
                let found = if is_secret_name(name) {
                    secret_ranges(line, from..at, &ctx)
                } else if words(name).last().is_some_and(|word| word == "pass") {
                    // A lowercase `pass` is also a compiler pass; it counts only when its value is a quoted literal.
                    let quoted = |found: &Range<usize>| line.get(..found.start).is_some_and(|head| head.ends_with(['"', '\'', '`']));
                    secret_ranges(line, from..at, &ctx).into_iter().filter(quoted).collect()
                } else {
                    Vec::new()
                };
                for range in &found {
                    covered.add(range.clone());
                }
                out.extend(found);
            }
            _ => {}
        }
    }
    out.extend(xml_ranges(line, &ctx));
    out
}

/// `<password>text</password>`.
fn xml_ranges(line: &str, ctx: &Ctx) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for (open, _) in line.match_indices('<') {
        let name_from = open + 1;
        let name_len = run(line, name_from, is_ident);
        let after = name_from + name_len;
        if name_len == 0 || line.get(after..after + 1) != Some(">") || !line.get(name_from..after).is_some_and(is_secret_name) {
            continue;
        }
        let from = after + 1;
        let Some(&close) = ctx.closers.get(ctx.closers.partition_point(|&at| at < from)) else { continue };
        let len = close - from;
        let text = line.get(from..from + len).unwrap_or_default();
        if text.trim().chars().count() >= 3 && !text.trim_start().starts_with(['$', '{', '<', '%']) {
            out.push(from..from + len);
        }
    }
    out
}

/// Values that are not secrets whatever the name: type words, and words a config uses for "not set".
const PLAIN_WORDS: &[&str] = &[
    "string",
    "boolean",
    "integer",
    "number",
    "object",
    "unknown",
    "optional",
    "required",
    "nullable",
    "undefined",
    "secret",
    "password",
    "token",
    "bytes",
    "pointer",
    "redacted",
    "default",
    "disabled",
    "enabled",
    "masked",
    "hidden",
    "empty",
    "str",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "float",
    "float32",
    "float64",
    "double",
    "bool",
    "char",
    "byte",
    "list",
    "dict",
    "tuple",
    "set",
    "any",
    "long",
    "short",
    "u8",
    "u16",
    "u32",
    "u64",
    "usize",
    "i8",
    "i16",
    "i32",
    "i64",
    "isize",
    "f32",
    "f64",
    "void",
    "secretstr",
    "secretstring",
    "return",
    "import",
    "export",
    "public",
    "private",
    "static",
    "function",
    "struct",
    "extends",
    "await",
    "async",
    "typeof",
    "yield",
    "throw",
    "switch",
    "continue",
];

fn skip_blanks(line: &str, at: usize) -> usize {
    at + run(line, at, |c| c == ' ' || c == '\t')
}

/// Where a string literal starts in `value`: after at most two prefix letters (`b"…"`, `r'…'`, `f"…"`, `br#"…"#`) and
/// hashes. Gives the offset of the quote and the quote.
fn literal_start(value: &str) -> Option<(usize, char)> {
    let prefix = value.chars().take_while(|c| matches!(c, 'b' | 'B' | 'r' | 'R' | 'f' | 'F' | 'u' | 'U')).count().min(2);
    let hashes = if prefix > 0 { value.get(prefix..).unwrap_or_default().chars().take_while(|&c| c == '#').count() } else { 0 };
    let at = prefix + hashes;
    let quote = value.get(at..)?.chars().next().filter(|c| matches!(c, '"' | '\'' | '`'))?;
    Some((at, quote))
}

/// The string literal at `at`, if there is one there and it can be a secret.
fn literal_at(line: &str, at: usize, ctx: &Ctx) -> Option<Range<usize>> {
    let value = line.get(at..)?;
    let (offset, quote) = literal_start(value)?;
    let span = ctx.literal(line, at + offset, quote)?;
    usable(line.get(span.clone())?).then_some(span)
}

/// What the name at `name` makes secret on this line: its assigned value, its literal argument, or a literal paired with
/// it as `define('NAME', 'value')` or `env["NAME"] = "value"`.
fn secret_ranges(line: &str, name: Range<usize>, ctx: &Ctx) -> Vec<Range<usize>> {
    let Some(text) = line.get(name.clone()) else { return Vec::new() };
    let last = words(text).pop().unwrap_or_default();
    let subject = Subject {
        name: text,
        env_style: text.chars().all(|c| is_upper_digit(c) || matches!(c, '_' | '-')),
        plural: matches!(last.as_str(), "tokens" | "secrets" | "passwords"),
        credentials: matches!(last.as_str(), "credentials" | "credential"),
        ctx,
    };
    let mut at = name.end;
    let quoted_name = line.get(at..).is_some_and(|rest| rest.starts_with(['"', '\'', '`']));
    while line.get(at..).is_some_and(|rest| rest.starts_with(['"', '\'', '`'])) {
        at += 1;
    }
    if !quoted_name && line.get(at..).is_some_and(|rest| rest.starts_with('(')) {
        // `setPassword("x")`, `.password("x")`: a literal handed to a setter of a secret.
        return if is_secret_setter(line, name.start, text) {
            literal_at(line, skip_blanks(line, at + 1), ctx).into_iter().collect()
        } else {
            Vec::new()
        };
    }
    let spaced = line.get(at..).is_some_and(|rest| rest.starts_with([' ', '\t']));
    at = skip_blanks(line, at);
    if quoted_name && line.get(at..).is_some_and(|rest| rest.starts_with(']')) {
        at = skip_blanks(line, at + 1);
    }
    let Some(rest) = line.get(at..) else { return Vec::new() };
    if quoted_name && rest.starts_with(',') {
        return paired_literal(line, name.start, at + 1, &subject);
    }
    let colon = rest.starts_with(':') && !rest.starts_with("::") && !rest.starts_with(":=");
    let sep = if rest.starts_with(":=") || rest.starts_with("=>") {
        2
    } else if rest.starts_with("==") || rest.starts_with("=~") || rest.starts_with("::") {
        return Vec::new();
    } else if rest.starts_with(['=', ':']) {
        1
    } else if ctx.after_env_keyword(line, name.start) {
        0
    } else if !quoted_name
        && spaced
        && let Some(next) = typed_without_colon(line, at)
    {
        return value_ranges(line, next, &subject, false);
    } else {
        return Vec::new();
    };
    // `"Token:", token`, `"?token=" + t`, `"password = '" + pw`: the name is inside a string, and what looks like the start of
    // a value is the end of that string, or a literal that does not end inside it.
    let value_at = skip_blanks(line, at + sep);
    let quote = line.get(value_at..).and_then(|value| value.chars().next()).filter(|c| matches!(c, '"' | '\'' | '`'));
    if !quoted_name && let (Some(value_quote), Some((string_quote, close))) = (quote, ctx.enclosing(name.start)) {
        let inside = string_quote != value_quote && literal_at(line, value_at, ctx).is_some_and(|found| found.end < close);
        if !inside {
            return Vec::new();
        }
    }
    value_ranges(line, at + sep, &subject, colon)
}

struct Subject<'a> {
    name: &'a str,
    /// `DB_PASSWORD`: upper case, so a value after it is a literal whatever it looks like.
    env_style: bool,
    /// The name ends in a plural (`max_tokens`, `secrets`): a number after it is a count, not a secret.
    plural: bool,
    /// `credentials` and `credential`: `fetch(url, { credentials: "include" })` is a setting.
    credentials: bool,
    /// What was worked out once for the line.
    ctx: &'a Ctx,
}

/// A call that hands a literal to a secret: a `set*`/`with*` setter (`setPassword`, `set_password`, `withSecret`) or a
/// method that is the secret word alone (`.password("x")`, `.passphrase("x")`). Constructors, `peek_token("else")`,
/// `add_pass("inline")` and `Secret("db")` are not.
fn is_secret_setter(line: &str, name_start: usize, name: &str) -> bool {
    let parts = words(name);
    let setter = parts.len() >= 2 && parts.first().is_some_and(|word| word == "set" || word == "with");
    let method = parts.len() == 1
        && parts.first().is_some_and(|word| word == "password" || word == "passphrase")
        && line.get(..name_start).is_some_and(|head| head.ends_with('.'))
        && name.chars().next().is_some_and(char::is_lowercase);
    setter || method
}

/// Calls that store a value under a name given as a string: `define('DB_PASSWORD', 'x')`.
const SETTERS: [&str; 6] = ["define(", "setenv(", "putenv(", "setproperty(", "setdefault(", "add_string("];

/// `define('NAME', 'value')`: the string after a quoted secret name. Only for an upper-case name or right after a known
/// setter, so a list of field names (`["username", "password", "email"]`) stays readable; and not a lowercase word under a
/// lowercase name (`{"grant_type": "password", "username": …}`).
fn paired_literal(line: &str, name_start: usize, next: usize, subject: &Subject) -> Vec<Range<usize>> {
    let head = line.get(..name_start).unwrap_or_default().trim_end_matches(['"', '\'', '`']).trim_end();
    // Only the last few characters matter: the longest setter is 12 bytes.
    let mut from = head.len().saturating_sub(16);
    while head.get(from..).is_none() {
        from += 1;
    }
    let head = head.get(from..).unwrap_or_default().to_ascii_lowercase();
    if !subject.env_style && !SETTERS.iter().any(|setter| head.ends_with(setter)) {
        return Vec::new();
    }
    let Some(found) = literal_at(line, skip_blanks(line, next), subject.ctx) else { return Vec::new() };
    let text = line.get(found.clone()).unwrap_or_default();
    if !subject.env_style && text.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
        return Vec::new();
    }
    std::iter::once(found).collect()
}

/// Go and Kotlin put the type after the name: `var Password string = "x"`. Gives where the value starts.
fn typed_without_colon(line: &str, at: usize) -> Option<usize> {
    let len = run(line, at, |c| is_word(c) || matches!(c, '*' | '&'));
    if len == 0 {
        return None;
    }
    let eq = skip_blanks(line, at + len);
    let rest = line.get(eq..)?;
    (rest.starts_with('=') && !rest.starts_with("==") && !rest.starts_with("=>") && !rest.starts_with("=~")).then_some(eq + 1)
}

/// After `name: Type = value`, where the value starts. `None` when there is no `=` after a type-looking text, which also
/// keeps base64 padding (`token: abc=`) and `a: b == c` out.
fn type_annotation_end(line: &str, at: usize) -> Option<usize> {
    let rest = window(line, at, SCAN / 4);
    let mut apostrophes = 0;
    for (i, c) in rest.char_indices() {
        match c {
            '=' => {
                let before = rest.get(..i)?;
                let after = rest.get(i + 1..)?;
                let bad_before = before.ends_with(['=', '!', '<', '>']);
                let bad_after = after.starts_with(['=', '>', '~']);
                return (!bad_before && !bad_after && !before.trim().is_empty() && !after.trim().is_empty()).then_some(at + i + 1);
            }
            '\'' => {
                apostrophes += 1;
                if apostrophes >= 2 {
                    return None;
                }
            }
            c if c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '<' | '>' | '[' | ']' | ',' | ' ' | '\t' | '&' | '*' | '?' | '|') => {}
            _ => return None,
        }
    }
    None
}

/// The literals a value at `at` holds that are secrets.
fn value_ranges(line: &str, at: usize, subject: &Subject, colon: bool) -> Vec<Range<usize>> {
    let at = skip_blanks(line, at);
    let head = window(line, at, SCAN);
    if let Some(found) = literal_at(line, at, subject.ctx) {
        let text = line.get(found.clone()).unwrap_or_default();
        if subject.credentials && matches!(text, "include" | "omit" | "same-origin") {
            return Vec::new();
        }
        return std::iter::once(found).collect();
    }
    let Some(first) = head.chars().next() else { return Vec::new() };
    if first == '[' {
        return list_literals(line, at + 1, subject.ctx);
    }
    if colon && let Some(next) = type_annotation_end(line, at) {
        return value_ranges(line, next, subject, false);
    }
    let ends = |c: char| c.is_whitespace() || matches!(c, ',' | ';' | ')' | '}' | ']' | '"' | '\'' | '`' | '&');
    // The decision is taken on the first SCAN bytes of the token, so a name costs at most that when it is not a secret.
    let short = head.split(ends).next().unwrap_or_default();
    tick(short.len());
    if unquoted_is_secret(short, subject, colon) {
        // A secret is masked whole, however long: its end is found by reading on from where the decision was taken.
        let token = if short.len() == head.len() { line.get(at..).unwrap_or_default().split(ends).next().unwrap_or(short) } else { short };
        tick(token.len());
        return std::iter::once(at..at + token.len()).collect();
    }
    fallback_literals(line, at, short, subject.ctx)
}

/// `["a", "b"]`: each string in the list.
fn list_literals(line: &str, from: usize, ctx: &Ctx) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = skip_blanks(line, from);
    while let Some(found) = literal_at(line, at, ctx) {
        at = skip_blanks(line, found.end + 1);
        out.push(found);
        if line.get(at..).is_some_and(|rest| rest.starts_with(',')) {
            at = skip_blanks(line, at + 1);
        } else {
            break;
        }
    }
    out
}

/// A value that is code can still carry the secret as its fallback: `os.getenv("PW", "secret")`, `env.PW || "secret"`,
/// `env.PW ?? 'secret'`, `os.environ.get("PW") or "secret"`.
fn fallback_literals(line: &str, at: usize, token: &str, ctx: &Ctx) -> Vec<Range<usize>> {
    if let Some(found) = top_level_default(line, at, ctx) {
        return std::iter::once(found).collect();
    }
    let Some(open) = token.find('(') else { return Vec::new() };
    // Only a lookup with a default has the secret as an argument; `strings.TrimPrefix(header, "Bearer ")` does not.
    let callee = token.get(..open).unwrap_or_default().rsplit(['.', ':', '>']).next().unwrap_or_default().to_ascii_lowercase();
    if !LOOKUPS.contains(&callee.as_str()) {
        return Vec::new();
    }
    // Every string after a comma inside the call, up to its closing parenthesis: the arguments that are not the first. A
    // string after the call (`, secret: "x"`, a later `ENV.fetch("H", "h")`) is not a default of this lookup.
    let mut out = Vec::new();
    let start = at + open + 1;
    let (mut depth, mut skip_to) = (1_u32, start);
    for (i, c) in window(line, start, SCAN).char_indices() {
        let here = start + i;
        if here < skip_to {
            continue;
        }
        tick(1);
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            '"' | '\'' | '`' => {
                let Some(span) = ctx.literal(line, here, c) else { break };
                let before = line.get(..here).unwrap_or_default().trim_end();
                if before.ends_with(',') && usable(line.get(span.clone()).unwrap_or_default()) {
                    out.push(span.clone());
                }
                skip_to = span.end + 1;
            }
            _ => {}
        }
    }
    out
}

/// The callees whose later arguments are defaults: `os.getenv("PW", "x")`, `config.get("pw", "x")`, `opt.unwrap_or("x")`.
const LOOKUPS: [&str; 11] =
    ["getenv", "get", "environ", "fetch", "var", "unwrap_or", "unwrap_or_else", "or_else", "default", "getproperty", "lookup"];

/// How far past a name the rules look for syntax that is not a literal: a default after an expression, a type before `=`.
/// A value, a literal and a closing quote have no such limit.
const SCAN: usize = 1024;

/// `len` bytes of `text` from `from`, cut back to a character boundary.
fn window(text: &str, from: usize, len: usize) -> &str {
    let mut end = from.saturating_add(len).min(text.len());
    while end > from && text.get(from..end).is_none() {
        end -= 1;
    }
    text.get(from..end).unwrap_or_default()
}

/// One string of a line: its quote, where its text starts and where its closing delimiter starts (`None` when it never
/// closes, as with an apostrophe).
struct Str {
    quote: char,
    inner_from: usize,
    close: Option<usize>,
}

/// What is worked out once per line, so that a name does not rescan the line it stands in.
struct Ctx {
    strings: Vec<Str>,
    /// Where the leading `ENV` or `ARG` word of a Dockerfile line ends.
    env_end: Option<usize>,
    /// Where each `</` starts, so that `<password>…</password>` finds its end without scanning the line again.
    closers: Vec<usize>,
}

impl Ctx {
    fn new(line: &str) -> Ctx {
        let lead = line.len() - line.trim_start().len();
        let word = line.get(lead..lead + 3).filter(|word| word.eq_ignore_ascii_case("env") || word.eq_ignore_ascii_case("arg"));
        let closers = line.match_indices("</").map(|(at, _)| at).collect();
        Ctx { strings: string_spans(line), env_end: word.map(|_| lead + 3), closers }
    }

    /// The text between the quotes of the literal whose opening quote sits at `quote_at`. The closing quote is found
    /// through the strings of the line, however far away it is. A quote opened inside another string closes inside it. A
    /// literal that never closes is not a literal: `"Token:", token` must not read its tail as a value.
    fn literal(&self, line: &str, quote_at: usize, quote: char) -> Option<Range<usize>> {
        let inner_from = quote_at + quote.len_utf8();
        if let Ok(index) = self.strings.binary_search_by_key(&inner_from, |string| string.inner_from)
            && let Some(string) = self.strings.get(index)
            && string.quote == quote
        {
            tick(1);
            return string.close.map(|close| inner_from..close);
        }
        let limit = self.enclosing(quote_at).map_or(line.len(), |(_, close)| close);
        let mut escaped = false;
        for (i, c) in line.get(inner_from..limit)?.char_indices() {
            tick(1);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                return Some(inner_from..inner_from + i);
            }
        }
        None
    }

    /// The string the position `at` sits inside, as its quote and the position of its closing delimiter.
    fn enclosing(&self, at: usize) -> Option<(char, usize)> {
        let index = self.strings.partition_point(|string| string.inner_from <= at);
        let string = self.strings.get(index.checked_sub(1)?)?;
        string.close.filter(|&close| close >= at).map(|close| (string.quote, close))
    }

    /// `ENV DB_PASSWORD hunter2` in a Dockerfile: a space is the separator.
    fn after_env_keyword(&self, line: &str, name_start: usize) -> bool {
        self.env_end.is_some_and(|end| name_start > end && line.get(end..name_start).is_some_and(|gap| gap.chars().all(|c| c == ' ' || c == '\t')))
    }
}

/// Every string of the line in one pass. Triple quotes (`'''`, `"""`) count as one delimiter.
fn string_spans(line: &str) -> Vec<Str> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    tick(chars.len());
    let same = |at: usize, quote: char| chars.get(at).map(|&(_, c)| c) == Some(quote);
    let mut out = Vec::new();
    let mut open: Option<(char, bool, usize)> = None;
    let (mut i, mut escaped) = (0, false);
    while let Some(&(pos, c)) = chars.get(i) {
        match open {
            Some((quote, triple, inner_from)) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == quote && (!triple || (same(i + 1, quote) && same(i + 2, quote))) {
                    out.push(Str { quote, inner_from, close: Some(pos) });
                    open = None;
                    if triple {
                        i += 2;
                    }
                }
            }
            None if matches!(c, '"' | '\'' | '`') => {
                let triple = same(i + 1, c) && same(i + 2, c);
                let inner_from = if triple { chars.get(i + 2).map_or(line.len(), |&(at, last)| at + last.len_utf8()) } else { pos + c.len_utf8() };
                open = Some((c, triple, inner_from));
                if triple {
                    i += 2;
                }
            }
            None => {}
        }
        i += 1;
    }
    if let Some((quote, _, inner_from)) = open {
        out.push(Str { quote, inner_from, close: None });
    }
    out
}

/// The literal after the first top-level `||`, `??` or ` or ` of the value at `at`, with parentheses, brackets and quotes
/// tracked: `ENV["PW"] || "x"`, `os.environ.get("PW") or "x"`, `<%= ENV["PW"] || "x" %>`. The value ends at a top-level `,`
/// or `;`, and at most [`SCAN`] bytes are looked at.
fn top_level_default(line: &str, at: usize, ctx: &Ctx) -> Option<Range<usize>> {
    let (mut depth, mut quote, mut escaped) = (0_i32, None::<char>, false);
    for (i, c) in window(line, at, SCAN).char_indices() {
        tick(1);
        let here = at + i;
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            ';' | ',' if depth == 0 => return None,
            _ if depth == 0 => {
                let rest = line.get(here..)?;
                let skip = if rest.starts_with("||") || rest.starts_with("??") {
                    2
                } else if rest.starts_with(" or ") {
                    4
                } else {
                    continue;
                };
                return literal_at(line, skip_blanks(line, here + skip), ctx);
            }
            _ => {}
        }
    }
    None
}

/// Whether the text of a literal can be a secret: not a placeholder, and at least three characters.
fn usable(inner: &str) -> bool {
    inner.chars().take(3).count() >= 3 && !inner.starts_with(['$', '{', '<', '%']) && inner != MASK
}

/// Whether an unquoted value reads as a literal secret. Calls, indexing, dotted names, placeholders, type names and
/// variables do not.
fn unquoted_is_secret(token: &str, subject: &Subject, colon: bool) -> bool {
    let Some(first) = token.chars().next() else { return false };
    // A setting, not a secret: `max_tokens=1000`, `budget_tokens: 16000`, `credentials: "include"`.
    if subject.plural && token.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if subject.credentials && matches!(token.to_ascii_lowercase().as_str(), "include" | "omit" | "same-origin") {
        return false;
    }
    if matches!(first, '$' | '{' | '<' | '%' | '[' | '(' | '*' | '&' | '!' | '|' | '>' | '@' | '#') || token.starts_with(MASK) {
        return false;
    }
    if token.contains(['(', ')', '[', ']', '{', '}', '<', '>', '$']) || token.contains("::") {
        return false;
    }
    // An Objective-C selector part: `forProtectionSpace:`.
    if token.strip_suffix(':').is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic())) {
        return false;
    }
    // A macro call: `quote!`.
    if token.strip_suffix('!').is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')) {
        return false;
    }
    let plain = token.chars().all(|c| is_word(c) || c == '.');
    let has_digit = token.contains(|c: char| c.is_ascii_digit());
    if plain && token.contains('.') && !has_digit {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    if PLAIN_WORDS.contains(&lower.as_str()) || matches!(lower.as_str(), "true" | "false" | "null" | "none" | "nil") {
        return false;
    }
    if subject.env_style {
        return token.len() >= 3;
    }
    if has_digit || !plain {
        return token.len() >= 4;
    }
    // A bare word. It is a variable, not a literal, when it repeats the name (`tokens = tokens`, `private_key = private_key`)
    // or reads as an identifier (`hashedPassword`, `new_refresh_token`); after a colon a capitalised word is a type.
    let squash = |text: &str| text.chars().filter(|c| !matches!(c, '_' | '-')).collect::<String>().to_ascii_lowercase();
    if squash(token).ends_with(&squash(subject.name)) {
        return false;
    }
    if token.chars().skip(1).any(|c| c.is_ascii_uppercase()) || token.contains('_') {
        return false;
    }
    token.len() >= 6 && !(colon && first.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The characters the scans looked at while redacting `text`.
    fn work(text: &str) -> u64 {
        WORK.with(|work| work.set(0));
        let _ = redact(text);
        WORK.with(std::cell::Cell::get)
    }

    #[test]
    fn the_work_on_hostile_lines_grows_with_their_length() {
        type Hostile = (&'static str, fn(usize) -> String);
        let shapes: [Hostile; 11] = [
            ("quoted values", |n| "token:\"abc12345\",".repeat(n)),
            ("bare values", |n| "token:abcdef,".repeat(n)),
            ("environment assignments", |n| "PASSWORD=x ".repeat(n)),
            ("a vendor prefix repeated", |n| "sk-".repeat(n)),
            ("calls", |n| "token=f(".repeat(n)),
            ("dollars", |n| "token=$".repeat(n)),
            ("unclosed xml", |n| "<token>".repeat(n)),
            ("quotes opened inside a string", |n| "\"token='".repeat(n)),
            ("strings and secrets", |n| "{s:\"a, b c\"},password = \"secretvalue12\";".repeat(n)),
            ("defaults", |n| "token=a b c ".repeat(n)),
            ("names inside a value", |n| "token=x_token=x_".repeat(n)),
        ];
        for (what, shape) in shapes {
            let (small, big) = (work(&shape(4_000)), work(&shape(8_000)));
            // Twice the line, at most about twice the work (and never a square).
            assert!(big * 2 <= small * 5 + 50_000, "{what}: {small} for n and {big} for 2n");
            assert!(big <= 300 * shape(8_000).len() as u64, "{what}: {big} for {} bytes", shape(8_000).len());
        }
    }

    #[test]
    fn a_closing_quote_is_found_by_lookup_and_not_by_scanning() {
        let line = format!("password = \"{}\" // after", "x".repeat(100_000));
        let ctx = Ctx::new(&line);
        WORK.with(|work| work.set(0));
        let span = ctx.literal(&line, 11, '"');
        assert_eq!(span.map(|found| found.len()), Some(100_000));
        assert!(WORK.with(std::cell::Cell::get) <= 2, "the closing quote of a long literal costs a lookup");
    }
}

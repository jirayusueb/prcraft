//! Opening a file at a line in an editor, and the command line each editor takes. No I/O.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Editor {
    #[serde(rename = "vscode")]
    VsCode,
    Xcode,
    Zed,
    Terminal,
}

impl Editor {
    pub const ALL: [Editor; 4] = [Editor::VsCode, Editor::Xcode, Editor::Zed, Editor::Terminal];

    pub fn label(self) -> &'static str {
        match self {
            Editor::VsCode => "VS Code",
            Editor::Xcode => "Xcode",
            Editor::Zed => "Zed",
            Editor::Terminal => "Terminal",
        }
    }

    /// The command that opens it.
    pub fn tool(self) -> &'static str {
        match self {
            Editor::VsCode => "code",
            Editor::Xcode => "xed",
            Editor::Zed => "zed",
            Editor::Terminal => "open",
        }
    }

    /// Where the command lives when the app's own `PATH` (a Finder launch has a short one) does not list it.
    pub fn fallbacks(self) -> &'static [&'static str] {
        match self {
            Editor::VsCode => {
                &["/usr/local/bin/code", "/opt/homebrew/bin/code", "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"]
            }
            Editor::Xcode => &["/usr/bin/xed"],
            Editor::Zed => &["/usr/local/bin/zed", "/opt/homebrew/bin/zed", "/Applications/Zed.app/Contents/MacOS/cli"],
            Editor::Terminal => &["/usr/bin/open"],
        }
    }

    /// What to do when [`Editor::tool`] is nowhere to be found.
    pub fn fix(self) -> &'static str {
        match self {
            Editor::VsCode => "In VS Code, run \"Shell Command: Install 'code' command in PATH\" from the command palette.",
            Editor::Xcode => "Install Xcode, then run `xcode-select --install` so the `xed` command exists.",
            Editor::Zed => "In Zed, run \"cli: install\" from the command palette so the `zed` command exists.",
            Editor::Terminal => "macOS ships the `open` command; check that /usr/bin is on the PATH.",
        }
    }

    /// The command as the popover words it under the row.
    pub fn hint(self) -> &'static str {
        match self {
            Editor::VsCode => "code -g",
            Editor::Xcode => "xed -l",
            Editor::Zed => "zed",
            Editor::Terminal => "open -a Terminal",
        }
    }
}

/// The arguments after [`Editor::tool`]. `root` is the checkout, `file` an absolute path inside it, `line` counts from 1. Every
/// path is absolute, so none of them can be read as an option. Terminal opens the checkout itself.
pub fn args(editor: Editor, root: &Path, file: &Path, line: u32) -> Vec<OsString> {
    let at = |sep: &str| {
        let mut arg = file.as_os_str().to_os_string();
        arg.push(format!("{sep}{line}"));
        arg
    };
    match editor {
        Editor::VsCode => vec!["-g".into(), at(":")],
        Editor::Zed => vec![at(":")],
        Editor::Xcode => vec!["-l".into(), line.to_string().into(), file.as_os_str().to_os_string()],
        Editor::Terminal => vec!["-a".into(), "Terminal".into(), root.as_os_str().to_os_string()],
    }
}

/// `path` as a path inside a checkout, or why it cannot be one: it must be relative and have no `..`, no empty part and no NUL.
pub fn inside(path: &str) -> Result<PathBuf, String> {
    let plain = !path.contains('\0') && !path.starts_with('/') && path.split('/').all(|part| !matches!(part, "" | "." | ".."));
    if plain { Ok(PathBuf::from(path)) } else { Err(format!("`{}` is not a path inside the checkout", path.escape_debug())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn each_editor_gets_its_own_command_line_with_absolute_paths() {
        let (root, file) = (Path::new("/c/acme-api-7"), Path::new("/c/acme-api-7/src/main.rs"));
        assert_eq!(strings(args(Editor::VsCode, root, file, 41)), ["-g", "/c/acme-api-7/src/main.rs:41"]);
        assert_eq!(strings(args(Editor::Zed, root, file, 41)), ["/c/acme-api-7/src/main.rs:41"]);
        assert_eq!(strings(args(Editor::Xcode, root, file, 41)), ["-l", "41", "/c/acme-api-7/src/main.rs"]);
        assert_eq!(strings(args(Editor::Terminal, root, file, 41)), ["-a", "Terminal", "/c/acme-api-7"]);
    }

    #[test]
    fn a_path_that_leaves_the_checkout_or_names_nothing_is_refused() {
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a/./b", "a//b", "a\0b", "./a"] {
            assert!(inside(bad).is_err(), "{bad:?}");
        }
        assert_eq!(inside("src/main.rs"), Ok(PathBuf::from("src/main.rs")));
        assert_eq!(inside("-rf/--version.rs"), Ok(PathBuf::from("-rf/--version.rs")), "a leading dash is safe once the path is joined to the root");
    }
}

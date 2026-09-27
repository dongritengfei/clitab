//! Per-tab shell integration setup.
//!
//! The previous implementation typed `. /tmp/.clitab_integration.sh\n` into the
//! PTY 300 ms after spawning the shell. That shows up in the terminal, clobbers
//! whatever the user had already typed, races between tabs over one shared temp
//! file, and silently does nothing when the shell is busy at that moment.
//!
//! Instead we hand the shell an rc file that it reads *by itself* on startup:
//!
//!   * bash -> `bash --rcfile <wrapper>`
//!   * zsh  -> `ZDOTDIR=<wrapper dir>`
//!
//! The user's own `.zshenv`, `.zprofile` and `.zlogin` are linked into the zsh
//! wrapper directory, so zsh's startup order is preserved.
//!
//! Each tab gets its own directory under the system temp dir, removed when the
//! session is dropped. Unsupported shells are left completely untouched.

use std::fs;
use std::path::{Path, PathBuf};

/// Sourced after the user's own rc file. Reports the working directory on every
/// prompt (OSC 7) and, when the previous command looked like `claude`, signals
/// that the assistant's turn is over (OSC 9).
const ZSH_SNIPPET: &str = r#"# clitab shell integration
_clitab_claude_running=0
_clitab_preexec() {
    case "$1" in
        *[cC]laude*) _clitab_claude_running=1 ;;
    esac
}
_clitab_precmd() {
    printf '\e]7;file://%s%s\e\\' "$(hostname)" "$PWD"
    if [[ $_clitab_claude_running -eq 1 ]]; then
        _clitab_claude_running=0
        printf '\e]9;claude-done\e\\'
    fi
}
typeset -ga preexec_functions precmd_functions
preexec_functions+=(_clitab_preexec)
precmd_functions+=(_clitab_precmd)
"#;

const BASH_SNIPPET: &str = r#"# clitab shell integration
_clitab_claude_running=0
_clitab_preexec() {
    case "$BASH_COMMAND" in
        *[cC]laude*) _clitab_claude_running=1 ;;
    esac
}
_clitab_prompt() {
    printf '\e]7;file://%s%s\e\\' "$(hostname)" "$PWD"
    if [[ $_clitab_claude_running -eq 1 ]]; then
        _clitab_claude_running=0
        printf '\e]9;claude-done\e\\'
    fi
}
trap '_clitab_preexec' DEBUG
PROMPT_COMMAND="_clitab_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
"#;

/// Files zsh reads before `.zshrc`; linked into the wrapper dir so the user's
/// PATH / completions / prompt framework keep working.
const ZSH_PASSTHROUGH: [&str; 3] = [".zshenv", ".zprofile", ".zlogin"];

/// Everything the caller needs to spawn an integrated shell.
pub struct Prepared {
    pub shell: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Directory to delete once the session goes away.
    pub dir: Option<PathBuf>,
}

impl Prepared {
    pub fn cleanup(&self) {
        if let Some(dir) = &self.dir {
            let _ = fs::remove_dir_all(dir);
        }
    }

    fn plain(shell: &str) -> Self {
        Self {
            shell: shell.to_string(),
            args: Vec::new(),
            env: Vec::new(),
            dir: None,
        }
    }
}

/// Build the integration for `shell_path` (normally `$SHELL`).
pub fn prepare(tab_id: &str, shell_path: &str) -> Prepared {
    let name = Path::new(shell_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match name.as_str() {
        "zsh" => zsh(tab_id, shell_path),
        "bash" => bash(tab_id, shell_path),
        // fish / nu / dash / ...: never inject what we cannot express.
        _ => Prepared::plain(shell_path),
    }
}

fn wrapper_dir(tab_id: &str) -> Option<PathBuf> {
    // Sanitize rather than truncate: two tabs must never share a directory, or
    // closing one would delete the rc file the other is still reading.
    let slug: String = tab_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = std::env::temp_dir().join(format!("clitab-{slug}"));
    match fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        // Failing to create the wrapper dir must not stop a tab from opening.
        Err(_) => None,
    }
}

fn bash(tab_id: &str, shell_path: &str) -> Prepared {
    let Some(dir) = wrapper_dir(tab_id) else {
        return Prepared::plain(shell_path);
    };

    // An interactive non-login bash only reads ~/.bashrc, and `--rcfile`
    // overrides exactly that. Source the first rc file the user actually has.
    let contents = "\
# clitab shell integration wrapper
for __clitab_rc in \"$HOME/.bashrc\" \"$HOME/.bash_profile\" \"$HOME/.bash_login\" \"$HOME/.profile\"; do
    if [ -f \"$__clitab_rc\" ]; then
        . \"$__clitab_rc\"
        break
    fi
done
unset __clitab_rc
"
    .to_string()
        + BASH_SNIPPET;

    let rc_path = dir.join("bashrc");
    if fs::write(&rc_path, contents).is_err() {
        let _ = fs::remove_dir_all(&dir);
        return Prepared::plain(shell_path);
    }

    Prepared {
        shell: shell_path.to_string(),
        args: vec![String::from("--rcfile"), rc_path.display().to_string()],
        env: vec![(String::from("CLITAB_SHELL_INTEGRATION"), String::from("1"))],
        dir: Some(dir),
    }
}

fn zsh(tab_id: &str, shell_path: &str) -> Prepared {
    let real_zdotdir = std::env::var("ZDOTDIR")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "/".to_string());

    let Some(dir) = wrapper_dir(tab_id) else {
        return Prepared::plain(shell_path);
    };

    for file in ZSH_PASSTHROUGH {
        let source = Path::new(&real_zdotdir).join(file);
        if source.is_file() {
            let _ = link_file(&source, &dir.join(file));
        }
    }

    let contents = "\
# clitab shell integration wrapper
[[ -f \"${CLITAB_ZDOTDIR}/.zshrc\" ]] && source \"${CLITAB_ZDOTDIR}/.zshrc\"
"
    .to_string()
        + ZSH_SNIPPET;

    if fs::write(dir.join(".zshrc"), contents).is_err() {
        let _ = fs::remove_dir_all(&dir);
        return Prepared::plain(shell_path);
    }

    Prepared {
        shell: shell_path.to_string(),
        args: Vec::new(),
        env: vec![
            (String::from("CLITAB_ZDOTDIR"), real_zdotdir),
            (String::from("ZDOTDIR"), dir.display().to_string()),
            (String::from("CLITAB_SHELL_INTEGRATION"), String::from("1")),
        ],
        dir: Some(dir),
    }
}

/// Symlink where the platform supports it, otherwise copy the file.
fn link_file(source: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target)
    }
    #[cfg(not(unix))]
    {
        fs::copy(source, target).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_shell_is_left_alone() {
        let prepared = prepare("aaaa-bbbb", "/usr/bin/fish");
        assert!(prepared.args.is_empty());
        assert!(prepared.env.is_empty());
        assert!(prepared.dir.is_none());
        assert_eq!(prepared.shell, "/usr/bin/fish");
    }

    #[test]
    fn bash_wrapper_sources_user_rc_then_snippet() {
        let prepared = prepare("1111-2222", "/bin/bash");
        assert_eq!(prepared.args.first().map(String::as_str), Some("--rcfile"));
        assert!(prepared
            .env
            .iter()
            .any(|(k, v)| k == "CLITAB_SHELL_INTEGRATION" && v == "1"));

        let dir = prepared.dir.clone().expect("wrapper dir");
        let contents = fs::read_to_string(dir.join("bashrc")).expect("rc file");
        assert!(contents.contains(".bashrc"), "must source the user's rc");
        assert!(contents.contains("PROMPT_COMMAND"), "must install the hook");
        // The snippet must come *after* the user's rc.
        assert!(
            contents.find(".bashrc").unwrap() < contents.find("PROMPT_COMMAND").unwrap(),
            "user rc should be sourced first"
        );

        prepared.cleanup();
        assert!(!dir.exists(), "temp dir must be removed on cleanup");
    }

    #[test]
    fn zsh_wrapper_sets_zdotdir_and_keeps_user_rc() {
        let prepared = prepare("3333-4444", "/bin/zsh");
        let dir = prepared.dir.clone().expect("wrapper dir");
        assert!(prepared
            .env
            .iter()
            .any(|(k, v)| k == "ZDOTDIR" && Path::new(v) == dir));
        let expected_real = std::env::var("ZDOTDIR")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| "/".to_string());
        assert!(prepared
            .env
            .iter()
            .any(|(k, v)| k == "CLITAB_ZDOTDIR" && v == &expected_real));

        let contents = fs::read_to_string(dir.join(".zshrc")).expect("rc file");
        assert!(contents.contains("${CLITAB_ZDOTDIR}/.zshrc"));
        assert!(contents.contains("precmd_functions"));

        prepared.cleanup();
        assert!(!dir.exists());
    }

    #[test]
    fn wrapper_dirs_are_unique_per_tab() {
        // Ids that share their first segment must not share a directory: one
        // tab closing would otherwise delete the rc file another is reading.
        let a = prepare("1111-2222-3333", "/bin/bash");
        let b = prepare("1111-9999-8888", "/bin/bash");
        assert_ne!(a.dir, b.dir);
        a.cleanup();
        b.cleanup();
    }

    /// Spawn a real shell in a real PTY with the wrapper applied, run one
    /// command, and return everything the shell printed. Skipped when no PTY
    /// device is available (some sandboxes), since that is not our bug.
    #[cfg(unix)]
    fn probe(id: &str, shell_path: &str, command: &str) -> Option<String> {
        use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
        use std::io::{Read, Write};

        if !Path::new(shell_path).exists() {
            return None;
        }

        let pair = NativePtySystem::default()
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .ok()?;

        let prepared = prepare(id, shell_path);
        let mut cmd = CommandBuilder::new(&prepared.shell);
        for arg in &prepared.args {
            cmd.arg(arg);
        }
        for (key, value) in &prepared.env {
            cmd.env(key, value);
        }

        let mut child = pair.slave.spawn_command(cmd).ok()?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().ok()?;
        let mut writer = pair.master.take_writer().ok()?;

        writer
            .write_all(format!("{command}\nexit\n").as_bytes())
            .ok()?;
        writer.flush().ok()?;

        let mut output = String::new();
        let mut buf = [0u8; 4096];
        while output.len() < 256 * 1024 {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => output.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
        let _ = child.wait();
        prepared.cleanup();
        Some(output)
    }

    /// Run `script` in a shell that has sourced the generated wrapper, without
    /// needing a PTY device, so this works on any machine.
    #[cfg(unix)]
    fn source_wrapper(id: &str, shell_path: &str, rc_name: &str, script: &str) -> Option<String> {
        use std::process::{Command as OsCommand, Stdio};

        if !Path::new(shell_path).exists() {
            return None;
        }
        let prepared = prepare(id, shell_path);
        let dir = prepared.dir.clone()?;
        let rc = dir.join(rc_name);

        let output = OsCommand::new(shell_path)
            // -f / --norc: skip the user's own startup files, we source the
            // wrapper explicitly so the test stays deterministic.
            .arg(match shell_path {
                p if p.ends_with("zsh") => "-f",
                _ => "--norc",
            })
            .arg("-c")
            .arg(format!("source '{}'; {script}", rc.display()))
            .stdin(Stdio::null())
            .output()
            .ok()?;

        prepared.cleanup();
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[cfg(unix)]
    #[test]
    fn bash_wrapper_installs_working_hooks() {
        let output = source_wrapper(
            "probe-bash-hooks",
            "/bin/bash",
            "bashrc",
            r#"printf 'PC=[%s]\n' "$PROMPT_COMMAND"; trap -p DEBUG; _clitab_prompt"#,
        )
        .expect("bash unavailable");
        assert!(
            output.contains("PC=[_clitab_prompt"),
            "PROMPT_COMMAND was not installed:\n{output}"
        );
        assert!(
            output.contains("_clitab_preexec"),
            "DEBUG trap was not installed:\n{output}"
        );
        assert!(
            output.contains("\u{1b}]7;file://"),
            "the prompt hook emitted no OSC 7:\n{output}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn zsh_wrapper_appends_hooks_without_clobbering_the_user() {
        let output = source_wrapper(
            "probe-zsh-hooks",
            "/bin/zsh",
            ".zshrc",
            r#"print -r -- "PC[$precmd_functions]"; print -r -- "EX[$preexec_functions]"; _clitab_precmd"#,
        )
        .expect("zsh unavailable");
        assert!(
            output.contains("_clitab_precmd"),
            "precmd hook was not installed:\n{output}"
        );
        assert!(
            output.contains("_clitab_preexec"),
            "preexec hook was not installed:\n{output}"
        );
        assert!(
            output.contains("\u{1b}]7;file://"),
            "the prompt hook emitted no OSC 7:\n{output}"
        );
    }

    /// A claude invocation must raise the flag, and the next prompt must report
    /// the turn as done exactly once.
    #[cfg(unix)]
    #[test]
    fn claude_done_is_reported_once_per_turn() {
        let output = source_wrapper(
            "probe-bash-once",
            "/bin/bash",
            "bashrc",
            r#"_clitab_preexec 'claude -p hi'; _clitab_prompt; _clitab_prompt"#,
        )
        .expect("bash unavailable");
        assert_eq!(
            output.matches("\u{1b}]9;claude-done").count(),
            1,
            "expected exactly one done notification:\n{output}"
        );
        // Two prompts means two cwd reports as well.
        assert_eq!(
            output.matches("\u{1b}]7;file://").count(),
            2,
            "every prompt should report the cwd:\n{output}"
        );
    }

    /// The wrapper and the OSC parser were written independently, so nothing yet
    /// proves they agree: the shell must emit terminators and parameter shapes
    /// that `osc::OscParser` actually decodes. Pipe one side's real bytes
    /// through the other.
    #[cfg(unix)]
    #[test]
    fn parser_understands_what_the_wrapper_emits() {
        use crate::osc::{OscEvent, OscParser};

        // The prompt hook has a different name per shell; the preexec hook does
        // not, so the claude detection is spelled the same for both.
        for (shell, rc_name, prompt_fn) in [
            ("/bin/bash", "bashrc", "_clitab_prompt"),
            ("/bin/zsh", ".zshrc", "_clitab_precmd"),
        ] {
            let name = Path::new(shell)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let script = format!("_clitab_preexec 'claude -p hi'; {prompt_fn}; {prompt_fn}");
            let Some(output) =
                source_wrapper(&format!("probe-roundtrip-{name}"), shell, rc_name, &script)
            else {
                eprintln!("skipped: {shell} unavailable");
                continue;
            };

            // Pin the OSC 7 terminator itself: the snippets use ST (`ESC \`),
            // which is the harder one to get right when a read splits it in
            // half. ST must arrive before any BEL in that sequence.
            let osc7 = output
                .find("\u{1b}]7;file://")
                .unwrap_or_else(|| panic!("{shell} emitted no OSC 7 at all:\n{output:?}"));
            let rest = &output[osc7..];
            let st = rest.find("\u{1b}\\").unwrap_or(usize::MAX);
            let bel = rest.find('\u{7}').unwrap_or(usize::MAX);
            assert!(st < bel, "{shell}: OSC 7 is not ST-terminated:\n{output:?}");

            let bytes = output.into_bytes();
            let events = OscParser::new().parse(&bytes);

            let cwd = events.iter().find_map(|e| match e {
                OscEvent::CwdChanged(path) => Some(path.clone()),
                _ => None,
            });
            assert!(
                matches!(&cwd, Some(path) if path.starts_with('/') && path.len() > 1),
                "{shell}: OSC 7 did not decode to a path (got {cwd:?})"
            );
            assert!(
                events.iter().any(|e| matches!(e, OscEvent::PromptReady)),
                "{shell}: OSC 9 claude-done did not decode to PromptReady"
            );
            // Two prompts, and each one reports the directory.
            let reported = events
                .iter()
                .filter(|e| matches!(e, OscEvent::CwdChanged(_)))
                .count();
            assert!(
                reported >= 2,
                "{shell}: expected a cwd report per prompt, got {reported}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn bash_hook_is_live_in_a_real_pty() {
        let Some(output) = probe(
            "probe-bash-pty",
            "/bin/bash",
            r#"printf 'HOOK[%s]\n' "$PROMPT_COMMAND""#,
        ) else {
            eprintln!("skipped: no bash or no PTY device available");
            return;
        };
        assert!(
            output.contains("HOOK[_clitab_prompt"),
            "integration hook missing from PROMPT_COMMAND:\n{output}"
        );
        assert!(
            output.contains("\u{1b}]7;file://"),
            "no OSC 7 cwd report was emitted:\n{output}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn zsh_hook_is_live_in_a_real_pty() {
        let Some(output) = probe(
            "probe-zsh-pty",
            "/bin/zsh",
            r#"print -r -- "HOOK[$precmd_functions]""#,
        ) else {
            eprintln!("skipped: no zsh or no PTY device available");
            return;
        };
        assert!(
            output.contains("_clitab_precmd"),
            "precmd hook was not installed:\n{output}"
        );
        assert!(
            output.contains("\u{1b}]7;file://"),
            "no OSC 7 cwd report was emitted:\n{output}"
        );
    }
}

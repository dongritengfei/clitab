//! Historical user prompts from an agent session transcript (JSONL), parsed
//! after a resume so the timeline can show what the user submitted before
//! this app run. Streaming by design: transcripts reach 50 MB, and only the
//! tail (last `cap` prompts) is ever shown.

use crate::status::{AgentKind, SessionBinding};
use chrono::DateTime;
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Matches the timeline's renderer-side cap (`MAX_TIMELINE_EVENTS` in
/// `src/lib/timeline.ts`): history and live rows share one budget.
pub const HISTORY_CAP: usize = 500;

/// One historical user prompt. `at` is epoch ms from the transcript's own
/// timestamp — the renderer displays it like a live row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPrompt {
    pub at: u64,
    pub msg: String,
}

/// Wrappers the agents inject through the user-prompt pipeline: they ride
/// `type:"user"` records but are not the user's own input. Same verdict as
/// `status::is_injected_prompt` for the live path, extended to the other
/// transcript-only wrappers (spec D6 — v1 is strict).
const INJECTED_PREFIXES: [&str; 6] = [
    "<task-notification>",
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<command-name>",
    "<command-message>",
    "<cross-session-message",
];

/// Where the transcript for a binding lives: the hook-supplied path when
/// present, else derived from the agent's projects-dir convention
/// (`~/.claude/projects/<cwd with / as ->/<sid>.jsonl`; Qoder mirrors it).
pub fn transcript_for(binding: &SessionBinding, cwd: &str) -> Option<PathBuf> {
    if let Some(tp) = &binding.transcript {
        return Some(PathBuf::from(tp));
    }
    let home = std::env::var("HOME").ok()?;
    let base = match binding.agent {
        AgentKind::Claude => ".claude",
        AgentKind::Qoder => ".qoder",
    };
    Some(
        PathBuf::from(home)
            .join(base)
            .join("projects")
            .join(cwd.replace('/', "-"))
            .join(format!("{}.jsonl", binding.sid)),
    )
}

/// The real user prompts of a transcript, oldest first, capped to the last
/// `cap`. Tolerant line-by-line: a truncated tail line, a non-JSON line or an
/// unknown record shape is skipped, never an error (Review Focus 2).
pub fn user_prompts(path: &Path, cap: usize) -> Vec<HistoryPrompt> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut out: VecDeque<HistoryPrompt> = VecDeque::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let Some(prompt) = prompt_of(&line) else { continue };
        if out.len() == cap {
            out.pop_front();
        }
        out.push_back(prompt);
    }
    out.into()
}

fn prompt_of(line: &str) -> Option<HistoryPrompt> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "user" {
        return None;
    }
    if v.get("isSidechain").and_then(|s| s.as_bool()).unwrap_or(false) {
        return None;
    }
    let message = v.get("message")?;
    if message.get("role")?.as_str()? != "user" {
        return None;
    }
    // Strict v1 rule (spec D6): only plain-string content. Arrays are tool
    // results, skill injections or attachments — never a typed prompt.
    let text = message.get("content")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if INJECTED_PREFIXES.iter().any(|prefix| text.starts_with(prefix)) {
        return None;
    }
    let at = DateTime::parse_from_rfc3339(v.get("timestamp")?.as_str()?)
        .ok()?
        .timestamp_millis();
    if at < 0 {
        return None;
    }
    Some(HistoryPrompt { at: at as u64, msg: text.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{AgentKind, SessionBinding};
    use chrono::DateTime;
    use std::path::PathBuf;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("clitab-history-{tag}-{}.jsonl", uuid::Uuid::new_v4()))
    }

    fn user_line(text: &str, ts: &str) -> String {
        format!(r#"{{"type":"user","timestamp":"{ts}","message":{{"role":"user","content":"{text}"}}}}"#)
    }

    #[test]
    fn extracts_only_real_user_prompts() {
        let path = temp_path("extract");
        let ts = "2026-10-19T08:30:00.000Z";
        let expected_at = DateTime::parse_from_rfc3339(ts).unwrap().timestamp_millis() as u64;
        let lines = vec![
            user_line("fix the bug", ts),
            // tool_result: content is an array, never a typed prompt
            r#"{"type":"user","timestamp":"2026-10-19T08:31:00.000Z","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#.to_string(),
            // wrappers the agent injects through the user-prompt pipeline
            user_line("<task-notification>x</task-notification>", "2026-10-19T08:32:00.000Z"),
            user_line("<local-command-stdout>done</local-command-stdout>", "2026-10-19T08:33:00.000Z"),
            user_line("<command-name>/clear</command-name>", "2026-10-19T08:33:30.000Z"),
            // assistant record
            r#"{"type":"assistant","timestamp":"2026-10-19T08:34:00.000Z","message":{"role":"assistant","content":"hi"}}"#.to_string(),
            // sidechain (subagent) prompt
            r#"{"type":"user","isSidechain":true,"timestamp":"2026-10-19T08:35:00.000Z","message":{"role":"user","content":"agent prompt"}}"#.to_string(),
            // garbage and a truncated tail line (crash mid-write)
            "not json at all".to_string(),
            r#"{"type":"user","timest"#.to_string(),
            // whitespace-only prompt
            user_line("   ", "2026-10-19T08:36:00.000Z"),
            user_line("second real prompt", "2026-10-19T08:37:00.000Z"),
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let prompts = user_prompts(&path, HISTORY_CAP);
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].msg, "fix the bug");
        assert_eq!(prompts[0].at, expected_at);
        assert_eq!(prompts[1].msg, "second real prompt");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn keeps_only_the_last_cap_prompts_in_order() {
        let path = temp_path("cap");
        let lines: Vec<String> = (0..5)
            .map(|i| user_line(&format!("p{i}"), "2026-10-19T08:30:00.000Z"))
            .collect();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let prompts = user_prompts(&path, 2);
        assert_eq!(
            prompts.iter().map(|p| p.msg.as_str()).collect::<Vec<_>>(),
            ["p3", "p4"]
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_or_unreadable_file_is_empty() {
        assert!(user_prompts(Path::new("/clitab-no-such-transcript.jsonl"), 10).is_empty());
    }

    #[test]
    fn transcript_path_prefers_the_hook_then_derives() {
        let with_tp = SessionBinding {
            agent: AgentKind::Claude,
            sid: "s1".into(),
            transcript: Some("/custom/s1.jsonl".into()),
        };
        assert_eq!(
            transcript_for(&with_tp, "/proj").unwrap(),
            PathBuf::from("/custom/s1.jsonl")
        );

        let home = std::env::var("HOME").unwrap();
        let derived = SessionBinding { agent: AgentKind::Claude, sid: "s1".into(), transcript: None };
        assert_eq!(
            transcript_for(&derived, "/Users/x/proj").unwrap(),
            PathBuf::from(format!("{home}/.claude/projects/-Users-x-proj/s1.jsonl"))
        );
        let qoder = SessionBinding { agent: AgentKind::Qoder, sid: "s2".into(), transcript: None };
        assert_eq!(
            transcript_for(&qoder, "/Users/x/proj").unwrap(),
            PathBuf::from(format!("{home}/.qoder/projects/-Users-x-proj/s2.jsonl"))
        );
    }
}

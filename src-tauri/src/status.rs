//! The clitab hook protocol: OSC 7777 carrying a small JSON payload.
//!
//! Claude Code and Qoder CLI hooks (UserPromptSubmit / PreToolUse / Stop /
//! Notification) printf these sequences to the tab's PTY; the OSC parser
//! hands us the raw JSON text and this module gives it meaning. Everything
//! is deliberately tolerant: unknown event kinds, extra fields (v2 will add
//! token/cost) and malformed payloads decode to `None` and are silently
//! dropped — a terminal must never break because a hook emitted something
//! new.

use serde::{Deserialize, Serialize};

/// A decoded protocol event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusEvent {
    /// UserPromptSubmit: an assistant turn began. `msg` is the user's
    /// submitted text, optional because the hook degrades to a bare prompt
    /// when jq is unavailable.
    Prompt { msg: Option<String> },
    /// PreToolUse: a tool is about to run.
    Tool { name: String },
    /// Stop: the turn ended. Duration is computed by the receiver, not sent.
    Stop,
    /// Notification: the session wants attention. `msg` is optional because
    /// the hook degrades to a bare notify when jq is unavailable.
    Notify { msg: Option<String> },
    /// PostToolUse of an in-terminal question (AskUserQuestion): the user's
    /// choice, as the tool's result text.
    Answer { msg: String },
}

/// Wire shape. Unknown fields are ignored by serde, which is the forward
/// compatibility guarantee for v2 payloads.
#[derive(Debug, Deserialize)]
struct Wire {
    e: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    msg: Option<String>,
}

/// Decode one OSC 7777 payload. Returns `None` for anything we do not
/// understand; callers must treat that as "ignore", never as an error.
pub fn decode(json: &str) -> Option<StatusEvent> {
    let wire: Wire = serde_json::from_str(json).ok()?;
    match wire.e.as_str() {
        // An empty prompt text would render as a blank timeline row; treat it
        // like a missing one.
        "prompt" => Some(StatusEvent::Prompt {
            msg: wire.msg.filter(|msg| !msg.is_empty()),
        }),
        // An empty tool name would render as a blank dashboard cell; treat it
        // like a missing one.
        "tool" => Some(StatusEvent::Tool {
            name: wire.tool.filter(|name| !name.is_empty())?,
        }),
        "stop" => Some(StatusEvent::Stop),
        "notify" => Some(StatusEvent::Notify { msg: wire.msg }),
        "answer" => Some(StatusEvent::Answer {
            msg: wire.msg.filter(|msg| !msg.is_empty())?,
        }),
        _ => None,
    }
}

/// Which agent CLI a tab's session belongs to; `cmd` is the binary name the
/// resume injection types into the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Qoder,
}

impl AgentKind {
    pub fn cmd(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Qoder => "qoder",
        }
    }
}

/// The agent session a tab is currently talking to, learned from the prompt
/// hook's `sid`/`tp`/`a` fields. Persisted into the tab snapshot so the next
/// launch can `--resume` it.
///
/// The `sid` is interpolated into a shell command line at resume time
/// (`TabManager::resume_tab`) and into a transcript path at history load, so
/// it is validated on *every* serde ingress — not just the hook path — via
/// the custom `Deserialize` below. A hostile snapshot (tampered or corrupt)
/// degrades to "no binding", never to injected shell input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBinding {
    pub agent: AgentKind,
    pub sid: String,
    /// Hook-supplied transcript path; `None` when the hook could not provide
    /// it (history then derives the conventional projects-dir path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

/// The sid charset guard: `[A-Za-z0-9-]`, non-empty. The sid is interpolated
/// into a shell command line at resume time and into a transcript path at
/// history load, so this predicate is the single injection guard shared by
/// the hook decoder and the snapshot's serde ingress. Factored out so the
/// two call sites cannot drift.
pub fn is_valid_sid(sid: &str) -> bool {
    !sid.is_empty() && sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

impl<'de> Deserialize<'de> for SessionBinding {
    /// Custom deserialize: rejects unsafe sids with a serde error so the
    /// snapshot loader's `ok()?` turns a hostile binding into `session: None`
    /// while the rest of the tab (cwd, title, flags) survives — matches the
    /// file's existing "corrupt → nothing to restore" tolerance.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            agent: AgentKind,
            sid: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            transcript: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if !is_valid_sid(&raw.sid) {
            return Err(serde::de::Error::custom(
                "session binding sid contains unsafe characters",
            ));
        }
        Ok(SessionBinding {
            agent: raw.agent,
            sid: raw.sid,
            transcript: raw.transcript,
        })
    }
}

/// Decode the session-binding side of a payload (in practice only
/// UserPromptSubmit carries it). `None` when the hook did not send a usable
/// binding — old install, missing jq, unknown agent — and callers keep the
/// previous binding then. The sid charset guard is load-bearing: the id is
/// interpolated into a shell command line at resume time.
pub fn decode_session(json: &str) -> Option<SessionBinding> {
    #[derive(Deserialize)]
    struct SessionWire {
        #[serde(default)]
        sid: Option<String>,
        #[serde(default)]
        tp: Option<String>,
        #[serde(default)]
        a: Option<String>,
    }
    let wire: SessionWire = serde_json::from_str(json).ok()?;
    let agent = match wire.a?.as_str() {
        "claude" => AgentKind::Claude,
        "qoder" => AgentKind::Qoder,
        _ => return None,
    };
    let sid = wire.sid?;
    if !is_valid_sid(&sid) {
        return None;
    }
    Some(SessionBinding {
        agent,
        sid,
        transcript: wire.tp.filter(|tp| !tp.is_empty()),
    })
}

/// Classifies a prompt hook's text: Claude Code injects background-task
/// completion notices (`<task-notification>` XML) through the user prompt
/// pipeline — the UserPromptSubmit hook fires with the XML as the prompt.
/// Such a turn is real (Claude processes it) but is not user input, so it is
/// flagged for the renderer (`Thinking.system`), which keeps it out of the
/// timeline. A user-typed prompt literally starting with the tag shares the
/// fate — acceptably rare. This is the single extension point should other
/// injected wrappers ever need it; do not generalize speculatively.
pub fn is_injected_prompt(msg: &str) -> bool {
    msg.trim_start().starts_with("<task-notification>")
}

/// The turn state shown on a tab's status line. Timestamps are epoch
/// milliseconds (not `Instant`): the state crosses the IPC boundary and must
/// still make sense after a webview reload. "Idle" is represented as `None`
/// at the storage layer — a tab that never spoke the protocol has no status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TabStatus {
    /// Turn in flight, no tool reported yet. `msg` is the prompt text that
    /// started the turn, when the hook could supply it. The two flags are
    /// orthogonal — `auto` is about *who submitted*, `system` about *who
    /// authored*:
    /// - `auto`: a turn Claude Code started by itself — at a stop with a
    ///   non-empty prompt queue it auto-submits the head *without re-firing
    ///   the hook*, so the backend models the submission (registry
    ///   `begin_auto_turn`). The renderer must not add a timeline row for one
    ///   — the queued prompt's row already exists — it flips that row to
    ///   executing (`startQueuedTurn`), unless `system` (no row to flip).
    /// - `system`: the prompt was authored by Claude Code, not the user
    ///   (`is_injected_prompt`). The turn is real — timer and duration apply
    ///   — but the renderer adds no timeline row for it, fresh or popped
    ///   (`{auto:false, system:true}` and `{auto:true, system:true}`).
    Thinking { since: u64, msg: Option<String>, auto: bool, system: bool },
    Tool { name: String, since: u64 },
    /// Turn finished; `duration` is None when the start was never observed
    /// (partially installed hooks).
    Done { duration: Option<u64>, at: u64 },
}

/// A Notification-hook message awaiting the user. Orthogonal to `TabStatus`:
/// acknowledging it must reveal the turn state underneath, not lose it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub msg: Option<String>,
    pub at: u64,
}

/// The user's answer to an in-terminal question (AskUserQuestion). Unlike
/// `Notice` this is a point-in-time record, not pending state: it stays in
/// the payload so a reloaded webview re-renders the row, and the renderer
/// dedups it by `at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub msg: String,
    pub at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_event() {
        assert_eq!(decode(r#"{"e":"prompt"}"#), Some(StatusEvent::Prompt { msg: None }));
        assert_eq!(
            decode(r#"{"e":"tool","tool":"Bash"}"#),
            Some(StatusEvent::Tool { name: "Bash".into() })
        );
        assert_eq!(decode(r#"{"e":"stop"}"#), Some(StatusEvent::Stop));
        assert_eq!(
            decode(r#"{"e":"notify","msg":"needs permission"}"#),
            Some(StatusEvent::Notify { msg: Some("needs permission".into()) })
        );
    }

    /// The Notification hook degrades to no-msg when jq is missing; the flash
    /// must still work, so msg is optional.
    #[test]
    fn notify_without_msg_still_decodes() {
        assert_eq!(decode(r#"{"e":"notify"}"#), Some(StatusEvent::Notify { msg: None }));
        assert_eq!(
            decode(r#"{"e":"notify","msg":null}"#),
            Some(StatusEvent::Notify { msg: None })
        );
    }

    /// The prompt hook carries the user's submitted text; it degrades to no
    /// msg when jq is missing, and an empty msg is treated as a missing one.
    #[test]
    fn prompt_carries_submitted_text() {
        assert_eq!(
            decode(r#"{"e":"prompt","msg":"fix the bug"}"#),
            Some(StatusEvent::Prompt { msg: Some("fix the bug".into()) })
        );
        assert_eq!(
            decode(r#"{"e":"prompt","msg":""}"#),
            Some(StatusEvent::Prompt { msg: None })
        );
    }

    /// Background-task completion notices ride the user-prompt pipeline:
    /// only the exact injected wrapper classifies, and the tag must be
    /// complete and at the start.
    #[test]
    fn injected_prompt_classification() {
        assert!(is_injected_prompt(
            "<task-notification>\n<task-id>x</task-id>\n</task-notification>"
        ));
        assert!(is_injected_prompt("  \n<task-notification>x</task-notification>"));
        assert!(!is_injected_prompt("see <task-notification> below"));
        assert!(!is_injected_prompt("<task-notificationx>"));
        assert!(!is_injected_prompt("<task-notification"));
        assert!(!is_injected_prompt("fix the bug"));
    }

    /// The answer hook carries the user's choice from an in-terminal
    /// question; without msg there is nothing to show, so the event is
    /// ignored rather than rendered as an empty row.
    #[test]
    fn answer_carries_the_users_choice() {
        assert_eq!(
            decode(r#"{"e":"answer","msg":"\"Q\"=\"A\""}"#),
            Some(StatusEvent::Answer { msg: r#""Q"="A""#.into() })
        );
        assert_eq!(decode(r#"{"e":"answer"}"#), None);
        assert_eq!(decode(r#"{"e":"answer","msg":""}"#), None);
    }

    /// Forward compatibility: v2 fields (tokens, cost) must not break v1.
    #[test]
    fn unknown_extra_fields_are_ignored() {
        assert_eq!(
            decode(r#"{"e":"stop","session":"x","cost":{"usd":0.1}}"#),
            Some(StatusEvent::Stop)
        );
    }

    #[test]
    fn malformed_input_is_ignored() {
        assert_eq!(decode("not json"), None);
        assert_eq!(decode(""), None);
        assert_eq!(decode("{}"), None); // no event kind
        assert_eq!(decode(r#"{"e":"bogus"}"#), None); // unknown kind
        assert_eq!(decode(r#"{"e":"tool"}"#), None); // tool without name
        assert_eq!(decode(r#"{"e":"tool","tool":""}"#), None); // empty name
        assert_eq!(decode("\u{0}garbage\u{0}"), None); // binary junk
    }

    #[test]
    fn tab_status_serializes_camel_case() {
        let json = serde_json::to_value(TabStatus::Thinking {
            since: 5,
            msg: Some("hi".into()),
            auto: false,
            system: false,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "thinking", "since": 5, "msg": "hi", "auto": false, "system": false})
        );
        let json = serde_json::to_value(TabStatus::Tool { name: "Bash".into(), since: 42 }).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "tool", "name": "Bash", "since": 42}));
        let json = serde_json::to_value(TabStatus::Done { duration: None, at: 7 }).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "done", "duration": null, "at": 7}));
        let json = serde_json::to_value(Notice { msg: Some("hi".into()), at: 7 }).unwrap();
        assert_eq!(json, serde_json::json!({"msg": "hi", "at": 7}));
        let json = serde_json::to_value(Answer { msg: "A".into(), at: 9 }).unwrap();
        assert_eq!(json, serde_json::json!({"msg": "A", "at": 9}));
    }

    #[test]
    fn session_binding_decodes_from_the_prompt_payload() {
        let b = decode_session(
            r#"{"e":"prompt","msg":"hi","sid":"6f9a1b2c-0d3e","tp":"/Users/x/.claude/projects/-Users-x/6f9a1b2c.jsonl","a":"claude"}"#,
        )
        .unwrap();
        assert_eq!(b.agent, AgentKind::Claude);
        assert_eq!(b.agent.cmd(), "claude");
        assert_eq!(b.sid, "6f9a1b2c-0d3e");
        assert_eq!(
            b.transcript.as_deref(),
            Some("/Users/x/.claude/projects/-Users-x/6f9a1b2c.jsonl")
        );
        // The status event itself is unchanged by the extra fields.
        assert_eq!(
            decode(r#"{"e":"prompt","msg":"hi","sid":"6f9a1b2c-0d3e","a":"claude"}"#),
            Some(StatusEvent::Prompt { msg: Some("hi".into()) })
        );
    }

    #[test]
    fn qoder_binding_decodes_with_its_own_command() {
        let b = decode_session(r#"{"e":"prompt","sid":"abc123","a":"qoder"}"#).unwrap();
        assert_eq!(b.agent, AgentKind::Qoder);
        assert_eq!(b.agent.cmd(), "qoder");
        assert_eq!(b.transcript, None, "tp is optional; history derives a path");
    }

    /// The sid is interpolated into a shell command line at resume time, so the
    /// decoder is the injection guard: anything outside [A-Za-z0-9-] is rejected.
    #[test]
    fn session_binding_rejects_unusable_fields() {
        assert_eq!(decode_session(r#"{"e":"prompt","sid":"abc"}"#), None, "no agent kind");
        assert_eq!(decode_session(r#"{"e":"prompt","a":"claude"}"#), None, "no sid");
        assert_eq!(decode_session(r#"{"e":"prompt","a":"claude","sid":""}"#), None);
        assert_eq!(decode_session(r#"{"e":"prompt","a":"claude","sid":"a; rm -rf /"}"#), None);
        assert_eq!(decode_session(r#"{"e":"prompt","a":"claude","sid":"$(whoami)"}"#), None);
        assert_eq!(decode_session(r#"{"e":"prompt","a":"cursor","sid":"abc"}"#), None, "unknown agent");
        assert_eq!(decode_session("not json"), None);
    }

    #[test]
    fn empty_transcript_path_is_normalized_to_none() {
        let b = decode_session(r#"{"e":"prompt","a":"claude","sid":"abc","tp":""}"#).unwrap();
        assert_eq!(b.transcript, None);
    }

    /// Payloads from old hook installs (no sid/tp/a) keep decoding as before —
    /// session restore degrades to plain-shell tabs, never to an error.
    #[test]
    fn legacy_payloads_have_no_binding_but_keep_working() {
        assert_eq!(decode_session(r#"{"e":"prompt","msg":"hi"}"#), None);
        assert_eq!(
            decode(r#"{"e":"prompt","msg":"hi"}"#),
            Some(StatusEvent::Prompt { msg: Some("hi".into()) })
        );
    }

    /// The snapshot file round-trips bindings through serde.
    #[test]
    fn session_binding_round_trips_through_json() {
        let b = SessionBinding {
            agent: AgentKind::Qoder,
            sid: "s-1".into(),
            transcript: Some("/tp.jsonl".into()),
        };
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(serde_json::from_str::<SessionBinding>(&json).unwrap(), b);
        assert!(json.contains(r#""agent":"qoder""#), "{json}");
    }

    /// The custom `Deserialize` is the snapshot-side injection guard: a
    /// hostile sid must fail serde so `restore::load`'s `ok()?` drops the
    /// binding while the rest of the tab survives.
    #[test]
    fn session_binding_deserialize_rejects_unsafe_sids() {
        let ok = r#"{"agent":"claude","sid":"6f9a1b2c-0d3e"}"#;
        assert!(serde_json::from_str::<SessionBinding>(ok).is_ok());
        let hostile_shell = r#"{"agent":"claude","sid":"a; rm -rf /"}"#;
        assert!(serde_json::from_str::<SessionBinding>(hostile_shell).is_err());
        let hostile_newline = r#"{"agent":"claude","sid":"a\nrm -rf /"}"#;
        assert!(serde_json::from_str::<SessionBinding>(hostile_newline).is_err());
        let hostile_subshell = r#"{"agent":"claude","sid":"$(whoami)"}"#;
        assert!(serde_json::from_str::<SessionBinding>(hostile_subshell).is_err());
        let empty = r#"{"agent":"claude","sid":""}"#;
        assert!(serde_json::from_str::<SessionBinding>(empty).is_err());
    }
}

//! The clitab hook protocol: OSC 7777 carrying a small JSON payload.
//!
//! Claude Code hooks (UserPromptSubmit / PreToolUse / Stop / Notification)
//! printf these sequences to the tab's PTY; the OSC parser hands us the raw
//! JSON text and this module gives it meaning. Everything is deliberately
//! tolerant: unknown event kinds, extra fields (v2 will add token/cost) and
//! malformed payloads decode to `None` and are silently dropped — a terminal
//! must never break because a hook emitted something new.

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
}

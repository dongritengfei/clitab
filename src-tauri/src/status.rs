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
    /// UserPromptSubmit: an assistant turn began.
    Prompt,
    /// PreToolUse: a tool is about to run.
    Tool { name: String },
    /// Stop: the turn ended. Duration is computed by the receiver, not sent.
    Stop,
    /// Notification: the session wants attention. `msg` is optional because
    /// the hook degrades to a bare notify when jq is unavailable.
    Notify { msg: Option<String> },
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
        "prompt" => Some(StatusEvent::Prompt),
        // An empty tool name would render as a blank dashboard cell; treat it
        // like a missing one.
        "tool" => Some(StatusEvent::Tool {
            name: wire.tool.filter(|name| !name.is_empty())?,
        }),
        "stop" => Some(StatusEvent::Stop),
        "notify" => Some(StatusEvent::Notify { msg: wire.msg }),
        _ => None,
    }
}

/// The turn state shown on a tab's status line. Timestamps are epoch
/// milliseconds (not `Instant`): the state crosses the IPC boundary and must
/// still make sense after a webview reload. "Idle" is represented as `None`
/// at the storage layer — a tab that never spoke the protocol has no status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TabStatus {
    /// Turn in flight, no tool reported yet.
    Thinking { since: u64 },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_event() {
        assert_eq!(decode(r#"{"e":"prompt"}"#), Some(StatusEvent::Prompt));
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
        let json = serde_json::to_value(TabStatus::Tool { name: "Bash".into(), since: 42 }).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "tool", "name": "Bash", "since": 42}));
        let json = serde_json::to_value(TabStatus::Done { duration: None, at: 7 }).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "done", "duration": null, "at": 7}));
        let json = serde_json::to_value(Notice { msg: Some("hi".into()), at: 7 }).unwrap();
        assert_eq!(json, serde_json::json!({"msg": "hi", "at": 7}));
    }
}

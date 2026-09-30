// OSC escape sequence parser.
//
// Detects:
//   * OSC 0 / 1 / 2 -> terminal title
//   * OSC 7         -> current working directory (`file://host/path`)
//   * OSC 9         -> custom notifications (`claude-done`)
//   * standalone BEL -> attention flash
//
// The parser is byte based and keeps its state between `parse()` calls, so a
// sequence that straddles two PTY reads is still decoded correctly. It is
// deliberately defensive: `cat`-ing a binary file must not make it accumulate
// without bounds or lose every later sequence.

use std::str;

/// Longest OSC payload we are willing to accumulate before giving up on it.
const MAX_OSC_LEN: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscEvent {
    TitleChanged(String),
    CwdChanged(String),
    Bell,
    PromptReady,
    /// OSC 7777: the clitab hook protocol. The payload is the raw text after
    /// the first `;` (parameters rejoined losslessly); JSON semantics live in
    /// `crate::status`, not here.
    Clitab(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    /// Outside of any escape sequence.
    #[default]
    Ground,
    /// Saw `ESC` while on the ground.
    AfterEsc,
    /// Collecting the payload of an OSC sequence.
    InOsc,
    /// Inside an OSC payload and just saw `ESC` (possible `ST` terminator).
    InOscAfterEsc,
}

#[derive(Debug, Default)]
pub struct OscParser {
    state: State,
    buffer: Vec<u8>,
    params: Vec<Vec<u8>>,
}

impl OscParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of PTY output, returning every OSC event it completed.
    pub fn parse(&mut self, data: &[u8]) -> Vec<OscEvent> {
        let mut events = Vec::new();

        for &byte in data {
            match self.state {
                State::Ground => match byte {
                    0x1b => self.state = State::AfterEsc,
                    0x07 => events.push(OscEvent::Bell),
                    _ => {}
                },
                State::AfterEsc => {
                    if byte == b']' {
                        self.begin_osc();
                    } else {
                        // CSI / DCS / anything else: we only care about OSC.
                        self.state = State::Ground;
                    }
                }
                State::InOsc => match byte {
                    0x07 => events.extend(self.finish_osc()),
                    b';' => self.push_param(),
                    0x1b => {
                        // Either the start of an `ESC \` (ST) terminator, or a
                        // nested OSC from truncated / binary output.
                        self.flush_param();
                        self.state = State::InOscAfterEsc;
                    }
                    _ => {
                        if self.buffer.len() >= MAX_OSC_LEN {
                            // Runaway sequence: drop it and resynchronise.
                            self.abort_osc();
                        } else {
                            self.buffer.push(byte);
                        }
                    }
                },
                State::InOscAfterEsc => {
                    if byte == b'\\' {
                        // ST terminator.
                        events.extend(self.finish_osc());
                    } else if byte == b']' {
                        // A new OSC started before the previous one was
                        // terminated: the partial sequence is garbage, so throw
                        // away everything accumulated so far and start clean.
                        self.begin_osc();
                    } else {
                        // Not a terminator after all: keep the payload going.
                        self.buffer.push(0x1b);
                        self.buffer.push(byte);
                        self.state = State::InOsc;
                    }
                }
            }
        }

        events
    }

    fn begin_osc(&mut self) {
        self.buffer.clear();
        self.params.clear();
        self.state = State::InOsc;
    }

    /// A `;` inside the payload: always record the segment, even an empty
    /// one, so a rejoined payload (title, JSON) keeps every separator.
    fn push_param(&mut self) {
        self.params.push(std::mem::take(&mut self.buffer));
    }

    /// End-of-sequence flush: a trailing empty segment carries no information
    /// and would corrupt a rejoined payload, so drop it.
    fn flush_param(&mut self) {
        if !self.buffer.is_empty() {
            self.push_param();
        }
    }

    /// Close the current OSC sequence and interpret it.
    fn finish_osc(&mut self) -> Vec<OscEvent> {
        self.flush_param();
        let event = Self::interpret(&self.params);
        self.reset();
        event.into_iter().collect()
    }

    /// Discard an OSC sequence we decided not to trust.
    fn abort_osc(&mut self) {
        self.reset();
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.params.clear();
        self.state = State::Ground;
    }

    fn interpret(params: &[Vec<u8>]) -> Option<OscEvent> {
        if params.len() < 2 {
            return None;
        }
        let code = str::from_utf8(&params[0]).ok()?;
        // The payload is everything after the first `;`, rejoined: `;` is a
        // legal character inside titles, paths and OSC 7777 JSON, so the
        // parameter split is only a framing convenience.
        let parts: Option<Vec<&str>> = params[1..]
            .iter()
            .map(|p| str::from_utf8(p).ok())
            .collect();
        let value = parts?.join(";");

        match code {
            "0" | "1" | "2" => Some(OscEvent::TitleChanged(value)),
            "7" => Some(OscEvent::CwdChanged(parse_osc7_path(&value))),
            "9" if value == "claude-done" => Some(OscEvent::PromptReady),
            "7777" if !value.is_empty() => Some(OscEvent::Clitab(value)),
            _ => None,
        }
    }
}

/// `OSC 7` payloads look like `file://<hostname><path>`, where the path may be
/// percent-encoded. Some shells emit a bare path instead.
fn parse_osc7_path(value: &str) -> String {
    let rest = match value.strip_prefix("file:") {
        Some(rest) => {
            // Skip the `//` and the authority component.
            let rest = rest.strip_prefix("//").unwrap_or(rest);
            match rest.find('/') {
                Some(pos) => &rest[pos..],
                None => return String::from("/"),
            }
        }
        None => value,
    };
    percent_decode(rest)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        // Work on bytes, never on &str slices: a literal '%' inside a raw
        // UTF-8 path can be followed by a multi-byte character, and slicing
        // two bytes off it would panic mid-char.
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Heuristic: does this title look like a filesystem path (i.e. a shell prompt)
/// rather than a name set by a TUI such as Claude Code?
pub fn looks_like_path(title: &str) -> bool {
    title.is_empty() || title.starts_with('/') || title.starts_with("~/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_osc0() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]0;Hello World\x07");
        assert_eq!(events, vec![OscEvent::TitleChanged("Hello World".into())]);
    }

    #[test]
    fn title_osc2() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]2;Claude Code\x07");
        assert_eq!(events, vec![OscEvent::TitleChanged("Claude Code".into())]);
    }

    #[test]
    fn st_terminated_osc() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]0;Claude Code\x1b\\then some text");
        assert_eq!(events, vec![OscEvent::TitleChanged("Claude Code".into())]);
        // The parser must be back on the ground and still see later sequences.
        let events = parser.parse(b"\x1b]0;Again\x07");
        assert_eq!(events, vec![OscEvent::TitleChanged("Again".into())]);
    }

    #[test]
    fn standalone_bell() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x07");
        assert_eq!(events, vec![OscEvent::Bell]);
    }

    #[test]
    fn bell_only_when_not_inside_osc() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"prompt\x07\x1b]0;Title\x07more");
        assert_eq!(
            events,
            vec![OscEvent::Bell, OscEvent::TitleChanged("Title".into())]
        );
    }

    #[test]
    fn osc7_cwd_with_authority() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]7;file://myhost/Users/yiyi/projects\x07");
        assert_eq!(
            events,
            vec![OscEvent::CwdChanged("/Users/yiyi/projects".into())]
        );
    }

    #[test]
    fn osc7_percent_decoded() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]7;file://h/Users/yiyi/my%20dir\x07");
        assert_eq!(
            events,
            vec![OscEvent::CwdChanged("/Users/yiyi/my dir".into())]
        );
    }

    /// A literal '%' followed by a multi-byte character must not panic (the
    /// bytes after it are not a char-boundary-aligned pair), and a fully
    /// percent-encoded path must still decode.
    #[test]
    fn osc7_raw_percent_and_multibyte() {
        let mut parser = OscParser::new();
        let events = parser.parse("\x1b]7;file://h/tmp/100%中文\x07".as_bytes());
        assert_eq!(events, vec![OscEvent::CwdChanged("/tmp/100%中文".into())]);

        let mut parser = OscParser::new();
        let events = parser.parse("\x1b]7;file://h/tmp/%E4%B8%AD%E6%96%87\x07".as_bytes());
        assert_eq!(events, vec![OscEvent::CwdChanged("/tmp/中文".into())]);
    }

    #[test]
    fn osc7_bare_path() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]7;/tmp/work\x07");
        assert_eq!(events, vec![OscEvent::CwdChanged("/tmp/work".into())]);
    }

    #[test]
    fn osc9_claude_done() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]9;claude-done\x1b\\");
        assert_eq!(events, vec![OscEvent::PromptReady]);
    }

    #[test]
    fn unknown_osc_is_ignored() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"\x1b]133;C\x07").is_empty());
        // ...and must not leak into the next sequence.
        let events = parser.parse(b"\x1b]0;Title\x07");
        assert_eq!(events, vec![OscEvent::TitleChanged("Title".into())]);
    }

    #[test]
    fn sequence_split_across_reads() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"text \x1b]0;Clau").is_empty());
        let events = parser.parse(b"de\x07");
        assert_eq!(events, vec![OscEvent::TitleChanged("Claude".into())]);
    }

    #[test]
    fn split_st_terminator_across_reads() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"\x1b]0;Ti").is_empty());
        assert!(parser.parse(b"tle\x1b").is_empty());
        let events = parser.parse(b"\\rest");
        assert_eq!(events, vec![OscEvent::TitleChanged("Title".into())]);
    }

    #[test]
    fn runaway_osc_is_capped() {
        let mut parser = OscParser::new();
        let noise = vec![b'a'; MAX_OSC_LEN * 2];
        let mut prefixed = vec![0x1b_u8, b']'];
        prefixed.extend_from_slice(b"0;");
        prefixed.extend_from_slice(&noise);
        assert!(parser.parse(&prefixed).is_empty());
        assert!(parser.buffer.len() <= MAX_OSC_LEN);
        // The runaway payload was dropped, so a fresh sequence still parses.
        let events = parser.parse(b"\x1b]0;OK\x07");
        assert_eq!(events, vec![OscEvent::TitleChanged("OK".into())]);
    }

    #[test]
    fn deeply_nested_osc_resynchronises() {
        let mut parser = OscParser::new();
        let mut junk = Vec::new();
        for _ in 0..10 {
            junk.extend_from_slice(b"\x1b]\x1b");
        }
        parser.parse(&junk);
        let events = parser.parse(b"]0;Back\x07");
        assert!(events.contains(&OscEvent::TitleChanged("Back".into())));
    }

    #[test]
    fn csi_sequences_are_skipped() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"\x1b[31mred\x1b[0m").is_empty());
        let events = parser.parse(b"\x1b]0;Title\x07");
        assert_eq!(events, vec![OscEvent::TitleChanged("Title".into())]);
    }

    #[test]
    fn path_heuristic() {
        assert!(looks_like_path("/Users/yiyi/code"));
        assert!(looks_like_path("~/code"));
        assert!(!looks_like_path("✳ Fix the build"));
    }

    #[test]
    fn osc7777_json_payload() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]7777;{\"e\":\"stop\"}\x1b\\");
        assert_eq!(events, vec![OscEvent::Clitab("{\"e\":\"stop\"}".into())]);
    }

    /// `;` is legal inside JSON strings; the parameter splitter must not eat
    /// it, and consecutive semicolons must survive the rejoin.
    #[test]
    fn semicolons_inside_json_survive_rejoin() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]7777;{\"e\":\"notify\",\"msg\":\"a;b;;c\"}\x07");
        assert_eq!(
            events,
            vec![OscEvent::Clitab("{\"e\":\"notify\",\"msg\":\"a;b;;c\"}".into())]
        );
    }

    #[test]
    fn osc7777_empty_payload_is_ignored() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"\x1b]7777;\x07").is_empty());
        assert!(parser.parse(b"\x1b]7777;\x1b\\").is_empty());
        // The parser is still healthy afterwards.
        let events = parser.parse(b"\x1b]7777;{\"e\":\"prompt\"}\x07");
        assert_eq!(events, vec![OscEvent::Clitab("{\"e\":\"prompt\"}".into())]);
    }

    #[test]
    fn osc7777_split_across_reads() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"\x1b]7777;{\"e\":").is_empty());
        let events = parser.parse(b"\"tool\",\"tool\":\"Bash\"}\x1b\\");
        assert_eq!(
            events,
            vec![OscEvent::Clitab("{\"e\":\"tool\",\"tool\":\"Bash\"}".into())]
        );
    }

    /// The rejoin also makes multi-semicolon titles faithful instead of
    /// truncating at the first `;`.
    #[test]
    fn title_with_semicolon_keeps_full_payload() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]0;user@host;project\x07");
        assert_eq!(
            events,
            vec![OscEvent::TitleChanged("user@host;project".into())]
        );
    }
}

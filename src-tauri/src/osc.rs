// OSC escape sequence parser
// Detects OSC 0/1/2 (terminal title) and standalone BEL (flash notification)

#[derive(Debug, Clone, PartialEq)]
pub enum OscEvent {
    TitleChanged(String),
    CwdChanged(String),
    Bell,
    PromptReady,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Normal,
    EscSeen,      // Just saw ESC (\x1b)
    OscAccumulating, // Inside OSC sequence
}

pub struct OscParser {
    state: State,
    buffer: Vec<u8>,
    osc_params: Vec<Vec<u8>>,
}

impl OscParser {
    pub fn new() -> Self {
        Self {
            state: State::Normal,
            buffer: Vec::with_capacity(256),
            osc_params: Vec::new(),
        }
    }

    pub fn parse(&mut self, data: &[u8]) -> Vec<OscEvent> {
        let mut events = Vec::new();

        for &byte in data {
            match self.state {
                State::Normal => {
                    if byte == 0x1b {
                        self.state = State::EscSeen;
                    } else if byte == 0x07 {
                        // Standalone BEL (not part of OSC)
                        events.push(OscEvent::Bell);
                    }
                }
                State::EscSeen => {
                    if byte == b']' {
                        // OSC sequence start
                        self.state = State::OscAccumulating;
                        self.buffer.clear();
                        self.osc_params.clear();
                    } else {
                        // Not OSC, back to normal
                        self.state = State::Normal;
                    }
                }
                State::OscAccumulating => {
                    if byte == 0x07 {
                        // BEL terminates OSC
                        if !self.buffer.is_empty() {
                            self.osc_params.push(self.buffer.clone());
                        }
                        if let Some(event) = self.parse_osc_params() {
                            events.push(event);
                        }
                        self.state = State::Normal;
                    } else if byte == 0x1b {
                        // Could be ST (ESC \) terminator
                        // Check next byte in a real implementation
                        // For now, treat as potential ST
                        if !self.buffer.is_empty() {
                            self.osc_params.push(self.buffer.clone());
                        }
                        if let Some(event) = self.parse_osc_params() {
                            events.push(event);
                        }
                        self.state = State::EscSeen;
                    } else if byte == b';' {
                        // Parameter separator
                        self.osc_params.push(self.buffer.clone());
                        self.buffer.clear();
                    } else {
                        self.buffer.push(byte);
                    }
                }
            }
        }

        events
    }

    fn parse_osc_params(&self) -> Option<OscEvent> {
        if self.osc_params.len() >= 2 {
            let code = std::str::from_utf8(&self.osc_params[0]).ok()?;
            let value = std::str::from_utf8(&self.osc_params[1]).ok()?;

            match code {
                "0" | "1" | "2" => Some(OscEvent::TitleChanged(value.to_string())),
                "7" => {
                    // OSC 7: file://hostname/path
                    if let Some(path) = value.strip_prefix("file://") {
                        // Skip hostname, extract path
                        if let Some(slash_pos) = path.find('/') {
                            Some(OscEvent::CwdChanged(path[slash_pos..].to_string()))
                        } else {
                            Some(OscEvent::CwdChanged(path.to_string()))
                        }
                    } else {
                        Some(OscEvent::CwdChanged(value.to_string()))
                    }
                }
                "9" => {
                    // OSC 9: custom notification
                    if value == "claude-done" {
                        Some(OscEvent::PromptReady)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_title_osc0() {
        let mut parser = OscParser::new();
        let data = b"\x1b]0;Hello World\x07";
        let events = parser.parse(data);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], OscEvent::TitleChanged("Hello World".to_string()));
    }

    #[test]
    fn test_title_osc2() {
        let mut parser = OscParser::new();
        let data = b"\x1b]2;Claude Code\x07";
        let events = parser.parse(data);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], OscEvent::TitleChanged("Claude Code".to_string()));
    }

    #[test]
    fn test_standalone_bell() {
        let mut parser = OscParser::new();
        let data = b"\x07";
        let events = parser.parse(data);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], OscEvent::Bell);
    }

    #[test]
    fn test_mixed_data() {
        let mut parser = OscParser::new();
        let data = b"prompt\x07\x1b]0;Title\x07more text";
        let events = parser.parse(data);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], OscEvent::Bell);
        assert_eq!(events[1], OscEvent::TitleChanged("Title".to_string()));
    }
}

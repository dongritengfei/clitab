use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem, ChildKiller};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{AppHandle, Emitter};
use crate::osc::{OscEvent, OscParser};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct PtySession {
    pub tab_id: String,
    pub cwd: String,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child_killer: Arc<Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
}

impl PtySession {
    pub fn new(tab_id: String, app: AppHandle) -> Result<Self, SessionError> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SessionError::Pty(e.to_string()))?;

        let cwd = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("/"))
            .to_string_lossy()
            .to_string();

        // Determine shell
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());

        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");

        // Shell integration: inject hooks to send OSC 7 (cwd) on prompt
        let shell_name = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("bash");

        match shell_name {
            "zsh" => {
                // For zsh, use ZDOTDIR with a wrapper .zshrc
                let zdotdir = std::env::var("ZDOTDIR").unwrap_or_else(|_| {
                    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
                });
                cmd.env("CLITAB_ZDOTDIR", &zdotdir);
                cmd.env("CLITAB_SHELL_INTEGRATION", "1");
            }
            "bash" => {
                cmd.env("CLITAB_SHELL_INTEGRATION", "1");
            }
            _ => {}
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| SessionError::Pty(e.to_string()))?;

        // Drop slave to allow EOF detection
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| SessionError::Pty(e.to_string()))?;

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| SessionError::Pty(e.to_string()))?;

        let child_killer = child.clone_killer();

        let master = Arc::new(Mutex::new(pair.master));
        let writer = Arc::new(Mutex::new(writer));
        let child_killer = Arc::new(Mutex::new(Some(child_killer)));

        // Inject shell integration after a short delay
        let writer_clone = writer.clone();
        let shell_name_owned = shell_name.to_string();
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(300));

            let integration_code = match shell_name_owned.as_str() {
                "zsh" => {
                    r#"
# clitab shell integration
_clitab_claude_running=0
_clitab_preexec() {
    if [[ "$1" == *"claude"* ]]; then
        _clitab_claude_running=1
    fi
}
_clitab_precmd() {
    printf '\e]7;file://%s%s\e\\' "$(hostname)" "$PWD"
    if [[ $_clitab_claude_running -eq 1 ]]; then
        _clitab_claude_running=0
        printf '\e]9;claude-done\e\\'
    fi
}
[[ -z "${preexec_functions[*]}" ]] && preexec_functions=()
preexec_functions+=(_clitab_preexec)
[[ -z "${precmd_functions[*]}" ]] && precmd_functions=()
precmd_functions+=(_clitab_precmd)
"#
                }
                "bash" => {
                    r#"
# clitab shell integration
_clitab_claude_running=0
_clitab_preexec() {
    if [[ "$BASH_COMMAND" == *"claude"* ]]; then
        _clitab_claude_running=1
    fi
}
_clitab_prompt() {
    printf '\e]7;file://%s%s\e\\' "$(hostname)" "$PWD"
    if [[ $_clitab_claude_running -eq 1 ]]; then
        _clitab_claude_running=0
        printf '\e]9;claude-done\e\\'
    fi
}
trap '_clitab_preexec' DEBUG
PROMPT_COMMAND="_clitab_prompt;${PROMPT_COMMAND:-}"
"#
                }
                _ => "",
            };
            if !integration_code.is_empty() {
                // Write to temp file and source it
                let tmp_path = std::env::temp_dir().join(".clitab_integration.sh");
                let _ = std::fs::write(&tmp_path, integration_code);

                if let Ok(mut w) = writer_clone.lock() {
                    // Source the file (command will be echoed but that's OK)
                    let source_cmd = format!(". {}\n", tmp_path.display());
                    let _ = w.write_all(source_cmd.as_bytes());
                    let _ = w.flush();
                }
            }
        });

        // Spawn reader thread
        let tab_id_clone = tab_id.clone();
        let app_clone = app.clone();

        thread::spawn(move || {
            Self::read_loop(tab_id_clone, app_clone, reader);
        });

        // Spawn exit watcher thread
        let tab_id_exit = tab_id.clone();
        let app_exit = app.clone();

        thread::spawn(move || {
            if let Ok(status) = child.wait() {
                let _ = app_exit.emit("tab-exit", serde_json::json!({
                    "tab_id": tab_id_exit,
                    "code": status.exit_code()
                }));
            }
        });

        Ok(Self {
            tab_id,
            cwd,
            writer,
            child_killer,
            master,
        })
    }

    fn read_loop(tab_id: String, app: AppHandle, mut reader: Box<dyn Read + Send>) {
        let mut buf = [0u8; 4096];
        let mut parser = OscParser::new();

        // Output idle detection: flash when output stops for 2 seconds (only when Claude is active)
        let last_output = Arc::new(Mutex::new(std::time::Instant::now()));
        let pending = Arc::new(Mutex::new(false));
        let claude_active = Arc::new(Mutex::new(false));

        let last_output_clone = last_output.clone();
        let pending_clone = pending.clone();
        let claude_active_clone = claude_active.clone();
        let tab_id_flash = tab_id.clone();
        let app_flash = app.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(std::time::Duration::from_millis(500));
                let is_claude_active = claude_active_clone.lock().ok().map(|c| *c).unwrap_or(false);
                if !is_claude_active {
                    continue; // Only check when Claude is running
                }
                let last = last_output_clone.lock().ok().map(|l| *l);
                if let Some(last_time) = last {
                    if last_time.elapsed() > std::time::Duration::from_secs(2) {
                        let mut p = pending_clone.lock().unwrap();
                        if *p {
                            *p = false;
                            let _ = app_flash.emit("tab-flash", serde_json::json!({
                                "tab_id": tab_id_flash
                            }));
                        }
                    }
                }
            }
        });

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let data = &buf[..n];

                    // Update last output time and set pending flash
                    if let Ok(mut last) = last_output.lock() {
                        *last = std::time::Instant::now();
                    }
                    if let Ok(mut p) = pending.lock() {
                        *p = true;
                    }

                    // Parse for OSC events
                    let events = parser.parse(data);
                    for event in events {
                        match event {
                            OscEvent::TitleChanged(title) => {
                                // Detect if Claude is running based on title
                                // Claude sets title to session name (not a path)
                                let is_path = title.starts_with('/') || title.starts_with('~');
                                let is_claude = !is_path && title.len() > 3;
                                if let Ok(mut active) = claude_active.lock() {
                                    *active = is_claude;
                                }

                                let _ = app.emit("tab-title", serde_json::json!({
                                    "tab_id": tab_id,
                                    "title": title
                                }));
                            }
                            OscEvent::CwdChanged(cwd) => {
                                let _ = app.emit("tab-cwd", serde_json::json!({
                                    "tab_id": tab_id,
                                    "cwd": cwd
                                }));
                            }
                            OscEvent::Bell => {
                                let _ = app.emit("tab-flash", serde_json::json!({
                                    "tab_id": tab_id
                                }));
                            }
                            OscEvent::PromptReady => {
                                let _ = app.emit("tab-flash", serde_json::json!({
                                    "tab_id": tab_id
                                }));
                                let _ = app.emit("prompt-ready", serde_json::json!({
                                    "tab_id": tab_id
                                }));
                            }
                        }
                    }

                    // Forward all data to frontend
                    let _ = app.emit("pty-output", serde_json::json!({
                        "tab_id": tab_id,
                        "data": data
                    }));
                }
                Err(_) => break,
            }
        }
    }

    pub fn write(&self, data: &[u8]) -> Result<(), SessionError> {
        let mut writer = self.writer.lock().map_err(|e| SessionError::Pty(e.to_string()))?;
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), SessionError> {
        let master = self.master.lock().map_err(|e| SessionError::Pty(e.to_string()))?;
        master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }).map_err(|e| SessionError::Pty(e.to_string()))?;
        Ok(())
    }

    pub fn kill(&self) {
        if let Ok(mut killer) = self.child_killer.lock() {
            if let Some(mut k) = killer.take() {
                let _ = k.kill();
            }
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill();
    }
}

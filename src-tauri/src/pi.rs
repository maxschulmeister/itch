//! Inline AI for the editor popover: one `pi --mode rpc` child process per
//! popover conversation. Events stream to the requesting webview through a
//! Tauri `Channel`.

use crate::{check_cli_exists, get_expanded_path, no_window_cmd, AppState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, Window};

const SYSTEM_PROMPT: &str = "You are a writing assistant inside a markdown note editor. \
Follow the user's instruction. Output only the resulting markdown, no preamble, \
no code fences around the whole answer. Do not repeat the rest of the document.";

/// Maximum characters of surrounding document sent as context.
const CONTEXT_BUDGET_CHARS: usize = 24_000;
const TRUNCATION_MARK: &str = "[…]";
const SELECTION_START: &str = "⟦selection⟧";
const SELECTION_END: &str = "⟦/selection⟧";
const CURSOR: &str = "⟦cursor⟧";
const NOT_FOUND: &str = "pi not found. Install pi or set its path in Settings.";
const STDERR_TAIL_BYTES: usize = 4096;

/// Prompt input from the editor. `before` and `after` are the document
/// markdown on each side of the selection (or cursor).
#[derive(Debug, Clone, Deserialize)]
pub struct PromptRequest {
    pub instruction: String,
    pub before: String,
    pub after: String,
    /// Selected text as markdown. `None` in cursor mode.
    pub selection: Option<String>,
}

/// Event sent to the frontend for one prompt run.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PiEvent {
    Delta { text: String },
    Done { text: String, aborted: bool },
    Error { message: String },
}

/// The pi RPC records that matter for one prompt run.
#[derive(Debug, PartialEq)]
enum Record {
    TextDelta(String),
    AssistantEnd { text: String, error: Option<String> },
    PromptAccepted,
    PromptRejected(String),
    Settled { aborted: bool },
    Other,
}

fn parse_record(line: &str) -> Record {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Record::Other;
    };
    match value["type"].as_str() {
        Some("message_update") => {
            let event = &value["assistantMessageEvent"];
            match (event["type"].as_str(), event["delta"].as_str()) {
                (Some("text_delta"), Some(delta)) => Record::TextDelta(delta.to_string()),
                _ => Record::Other,
            }
        }
        Some("message_end") if value["message"]["role"] == "assistant" => {
            let message = &value["message"];
            let text = message["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|block| block["type"] == "text")
                        .filter_map(|block| block["text"].as_str())
                        .collect::<String>()
                })
                .unwrap_or_default();
            let error = (message["stopReason"] == "error").then(|| {
                message["errorMessage"]
                    .as_str()
                    .unwrap_or("The model returned an error.")
                    .to_string()
            });
            Record::AssistantEnd { text, error }
        }
        Some("response") if value["command"] == "prompt" => {
            if value["success"] == true {
                Record::PromptAccepted
            } else {
                Record::PromptRejected(
                    value["error"]
                        .as_str()
                        .unwrap_or("pi rejected the prompt.")
                        .to_string(),
                )
            }
        }
        Some("agent_settled") => Record::Settled {
            aborted: value["aborted"] == true,
        },
        _ => Record::Other,
    }
}

/// Turns the records of one run into frontend events.
#[derive(Debug, Default)]
struct RunTracker {
    text: String,
    error: Option<String>,
}

impl RunTracker {
    fn handle(&mut self, record: Record) -> Option<PiEvent> {
        match record {
            Record::TextDelta(text) => Some(PiEvent::Delta { text }),
            Record::AssistantEnd { text, error } => {
                self.text = text;
                self.error = error;
                None
            }
            Record::PromptRejected(message) => Some(PiEvent::Error { message }),
            Record::Settled { aborted } => Some(match (aborted, self.error.take()) {
                (false, Some(message)) => PiEvent::Error { message },
                _ => PiEvent::Done {
                    text: std::mem::take(&mut self.text),
                    aborted,
                },
            }),
            Record::PromptAccepted | Record::Other => None,
        }
    }
}

pub fn build_args(model: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "--mode",
        "rpc",
        "--no-session",
        "--no-tools",
        "--no-extensions",
        "--no-skills",
        "--no-prompt-templates",
        "--no-context-files",
        "--no-mcp",
        "--no-themes",
        "--offline",
        "--no-approve",
        "--system-prompt",
        SYSTEM_PROMPT,
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect();
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    args
}

/// Keeps the text nearest to the selection: the end of `before` and the start
/// of `after`. Budget that one side does not use goes to the other side.
fn truncate_context(before: &str, after: &str, budget: usize) -> (String, String) {
    let before_len = before.chars().count();
    let after_len = after.chars().count();
    if before_len + after_len <= budget {
        return (before.to_string(), after.to_string());
    }
    let half = budget / 2;
    let keep_after = after_len.min(budget - half.min(before_len));
    let keep_before = before_len.min(budget - keep_after);

    let before = if keep_before < before_len {
        let tail: String = before.chars().skip(before_len - keep_before).collect();
        format!("{TRUNCATION_MARK}\n{tail}")
    } else {
        before.to_string()
    };
    let after = if keep_after < after_len {
        let head: String = after.chars().take(keep_after).collect();
        format!("{head}\n{TRUNCATION_MARK}")
    } else {
        after.to_string()
    };
    (before, after)
}

/// The first prompt of a conversation carries the document as context.
/// Follow-up prompts (refinements) send only the instruction.
fn build_prompt(request: &PromptRequest, include_context: bool) -> String {
    let instruction = request.instruction.trim();
    if !include_context {
        return instruction.to_string();
    }
    let (before, after) = truncate_context(&request.before, &request.after, CONTEXT_BUDGET_CHARS);
    match &request.selection {
        Some(selection) => format!(
            "<document>\n{before}{SELECTION_START}{selection}{SELECTION_END}{after}\n</document>\n\n\
             The selected text is marked with {SELECTION_START} and {SELECTION_END} in the document above:\n\
             <selection>\n{selection}\n</selection>\n\n\
             Instruction: {instruction}\n\n\
             Reply with only the markdown that replaces the selected text."
        ),
        None => format!(
            "<document>\n{before}{CURSOR}{after}\n</document>\n\n\
             The cursor position is marked with {CURSOR} in the document above.\n\n\
             Instruction: {instruction}\n\n\
             Reply with only the markdown to insert at the cursor."
        ),
    }
}

/// Returns the program to run: the path override from settings, or `pi`
/// from the search path.
fn resolve_program(path_override: Option<&str>, search_path: &str) -> Result<String, String> {
    if let Some(path) = path_override.map(str::trim).filter(|p| !p.is_empty()) {
        return if std::path::Path::new(path).is_file() {
            Ok(path.to_string())
        } else {
            Err(format!(
                "pi not found at {path}. Check the pi path in Settings."
            ))
        };
    }
    if check_cli_exists("pi", search_path)? {
        Ok("pi".to_string())
    } else {
        Err(NOT_FOUND.to_string())
    }
}

/// The expanded PATH, plus the default pi launcher folder.
fn search_path() -> String {
    let expanded = get_expanded_path();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => format!("{home}/.pi/agent/bin:{expanded}"),
        _ => expanded,
    }
}

type Sink = Box<dyn Fn(PiEvent) + Send>;

struct ActiveRun {
    sink: Sink,
    tracker: RunTracker,
}

/// State shared between the session and its stdout reader thread. One mutex
/// guards both fields so a run cannot start after the reader saw the exit.
#[derive(Default)]
struct RunSlot {
    active: Option<ActiveRun>,
    /// Set when pi exited. Contains a message for the user.
    exited: Option<String>,
    /// True after pi accepted the first prompt, which carries the context.
    context_sent: bool,
}

/// One running `pi --mode rpc` process. Dropping it kills the process.
pub struct PiProcess {
    child: Child,
    stdin: ChildStdin,
    slot: Arc<Mutex<RunSlot>>,
}

impl PiProcess {
    pub fn spawn(program: &str, args: &[String], path_env: &str) -> Result<Self, String> {
        let mut child = no_window_cmd(program)
            .args(args)
            .env("PATH", path_env)
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start pi: {e}"))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Failed to connect to pi.".to_string());
        };

        let slot = Arc::new(Mutex::new(RunSlot::default()));
        let stderr_reader = std::thread::spawn(move || read_stderr_tail(stderr));
        let reader_slot = Arc::clone(&slot);
        std::thread::spawn(move || read_stdout(stdout, stderr_reader, reader_slot));
        Ok(Self { child, stdin, slot })
    }

    fn is_alive(&self) -> bool {
        self.slot
            .lock()
            .map(|s| s.exited.is_none())
            .unwrap_or(false)
    }

    /// Sends one prompt. Events for this run go to `sink` until a `Done` or
    /// `Error` event.
    pub fn prompt(&mut self, request: &PromptRequest, sink: Sink) -> Result<(), String> {
        let message = {
            let mut slot = self.slot.lock().map_err(|_| "pi session is broken.")?;
            if let Some(message) = &slot.exited {
                return Err(message.clone());
            }
            if slot.active.is_some() {
                return Err("pi is still working on the previous request.".to_string());
            }
            slot.active = Some(ActiveRun {
                sink,
                tracker: RunTracker::default(),
            });
            build_prompt(request, !slot.context_sent)
        };
        let command = serde_json::json!({ "type": "prompt", "message": message });
        if let Err(e) = self.send(&command) {
            if let Ok(mut slot) = self.slot.lock() {
                slot.active = None;
            }
            return Err(format!("Failed to send the prompt to pi: {e}"));
        }
        Ok(())
    }

    /// Asks pi to abort the current run. pi then ends the run with
    /// `agent_settled`, which sends `Done { aborted: true }`.
    pub fn abort(&mut self) -> Result<(), String> {
        let running = self
            .slot
            .lock()
            .map(|s| s.active.is_some())
            .unwrap_or(false);
        if !running {
            return Ok(());
        }
        self.send(&serde_json::json!({ "type": "abort" }))
            .map_err(|e| format!("Failed to stop pi: {e}"))
    }

    fn send(&mut self, command: &Value) -> std::io::Result<()> {
        let mut line = command.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()
    }
}

impl Drop for PiProcess {
    fn drop(&mut self) {
        // The caller closed the conversation: no event for the current run.
        if let Ok(mut slot) = self.slot.lock() {
            slot.active = None;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_stdout(
    stdout: std::process::ChildStdout,
    stderr_reader: JoinHandle<String>,
    slot: Arc<Mutex<RunSlot>>,
) {
    // Split on LF only: JSON strings can contain U+2028/U+2029.
    for line in BufReader::new(stdout).split(b'\n') {
        let Ok(mut bytes) = line else { break };
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        let record = parse_record(&String::from_utf8_lossy(&bytes));
        let Ok(mut slot) = slot.lock() else { return };
        if record == Record::PromptAccepted {
            slot.context_sent = true;
        }
        let Some(run) = slot.active.as_mut() else {
            continue;
        };
        if let Some(event) = run.tracker.handle(record) {
            let finished = !matches!(event, PiEvent::Delta { .. });
            (run.sink)(event);
            if finished {
                slot.active = None;
            }
        }
    }

    let stderr_tail = stderr_reader.join().unwrap_or_default();
    let message = exit_message(&stderr_tail);
    if let Ok(mut slot) = slot.lock() {
        if let Some(run) = slot.active.take() {
            (run.sink)(PiEvent::Error {
                message: message.clone(),
            });
        }
        slot.exited = Some(message);
    }
}

fn read_stderr_tail(mut stderr: std::process::ChildStderr) -> String {
    let mut tail: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 4096];
    while let Ok(n) = stderr.read(&mut buffer) {
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&buffer[..n]);
        if tail.len() > STDERR_TAIL_BYTES {
            tail.drain(..tail.len() - STDERR_TAIL_BYTES);
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

fn exit_message(stderr_tail: &str) -> String {
    match stderr_tail
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
    {
        Some(line) => format!("pi stopped unexpectedly: {line}"),
        None => "pi stopped unexpectedly.".to_string(),
    }
}

struct Session {
    window: String,
    process: PiProcess,
}

/// Open popover conversations, by session id.
#[derive(Default)]
pub struct PiSessions(Mutex<HashMap<String, Session>>);

/// Sends a prompt in the conversation `session_id`. Starts pi for the first
/// prompt, or again when pi exited.
#[tauri::command]
pub async fn pi_prompt(
    window: Window,
    app_state: State<'_, AppState>,
    session_id: String,
    request: PromptRequest,
    on_event: Channel<PiEvent>,
) -> Result<(), String> {
    let (model, path_override) = {
        let settings = app_state.settings.read().expect("settings read lock");
        (settings.pi_model.clone(), settings.pi_path.clone())
    };
    let app = window.app_handle().clone();
    let label = window.label().to_string();

    tauri::async_runtime::spawn_blocking(move || {
        let sessions = app.state::<PiSessions>();
        let mut map = sessions.0.lock().map_err(|_| "pi sessions are broken.")?;
        let alive = map.get(&session_id).is_some_and(|s| s.process.is_alive());
        if !alive {
            // Drop a crashed process before a new one starts.
            map.remove(&session_id);
            let path = search_path();
            let program = resolve_program(path_override.as_deref(), &path)?;
            let process = PiProcess::spawn(&program, &build_args(model.as_deref()), &path)?;
            map.insert(
                session_id.clone(),
                Session {
                    window: label,
                    process,
                },
            );
        }
        let session = map.get_mut(&session_id).ok_or("pi session not found.")?;
        session.process.prompt(
            &request,
            Box::new(move |event| {
                let _ = on_event.send(event);
            }),
        )
    })
    .await
    .map_err(|e| format!("Failed to run pi: {e}"))?
}

#[tauri::command]
pub async fn pi_abort(sessions: State<'_, PiSessions>, session_id: String) -> Result<(), String> {
    let mut map = sessions.0.lock().map_err(|_| "pi sessions are broken.")?;
    match map.get_mut(&session_id) {
        Some(session) => session.process.abort(),
        None => Ok(()),
    }
}

#[tauri::command]
pub async fn pi_close(sessions: State<'_, PiSessions>, session_id: String) -> Result<(), String> {
    let removed = sessions
        .0
        .lock()
        .map_err(|_| "pi sessions are broken.")?
        .remove(&session_id);
    drop(removed);
    Ok(())
}

/// Kills the pi processes of a closed window.
pub fn close_window_sessions(app: &AppHandle, window_label: &str) {
    let Some(sessions) = app.try_state::<PiSessions>() else {
        return;
    };
    let removed: Vec<Session> = match sessions.0.lock() {
        Ok(mut map) => {
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, s)| s.window == window_label)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| map.remove(id)).collect()
        }
        Err(_) => return,
    };
    drop(removed);
}

/// Kills all pi processes. Call when the app exits.
pub fn close_all(app: &AppHandle) {
    let Some(sessions) = app.try_state::<PiSessions>() else {
        return;
    };
    let removed: Vec<Session> = match sessions.0.lock() {
        Ok(mut map) => map.drain().map(|(_, s)| s).collect(),
        Err(_) => return,
    };
    drop(removed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(selection: Option<&str>) -> PromptRequest {
        PromptRequest {
            instruction: "Make it shorter".to_string(),
            before: "# Title\n\nIntro ".to_string(),
            after: " outro.\n".to_string(),
            selection: selection.map(str::to_string),
        }
    }

    #[test]
    fn parses_text_delta() {
        let line = r#"{"type":"message_update","usage":{},"assistantMessageEvent":{"type":"text_delta","contentIndex":1,"delta":"Hel"}}"#;
        assert_eq!(parse_record(line), Record::TextDelta("Hel".to_string()));
    }

    #[test]
    fn ignores_thinking_delta() {
        let line = r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","contentIndex":0,"delta":"hmm"}}"#;
        assert_eq!(parse_record(line), Record::Other);
    }

    #[test]
    fn parses_assistant_message_end_with_text() {
        let line = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"Hello "},{"type":"text","text":"world"}],"stopReason":"stop"}}"#;
        assert_eq!(
            parse_record(line),
            Record::AssistantEnd {
                text: "Hello world".to_string(),
                error: None
            }
        );
    }

    #[test]
    fn parses_assistant_message_end_with_error() {
        let line = r#"{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"429: rate limited"}}"#;
        assert_eq!(
            parse_record(line),
            Record::AssistantEnd {
                text: String::new(),
                error: Some("429: rate limited".to_string())
            }
        );
    }

    #[test]
    fn ignores_user_message_end() {
        let line = r#"{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#;
        assert_eq!(parse_record(line), Record::Other);
    }

    #[test]
    fn parses_prompt_responses() {
        let ok = r#"{"id":"1","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}"#;
        assert_eq!(parse_record(ok), Record::PromptAccepted);
        let rejected = r#"{"type":"response","command":"prompt","success":false,"error":"No API key found for the selected model."}"#;
        assert_eq!(
            parse_record(rejected),
            Record::PromptRejected("No API key found for the selected model.".to_string())
        );
        let abort = r#"{"type":"response","command":"abort","success":true}"#;
        assert_eq!(parse_record(abort), Record::Other);
    }

    #[test]
    fn parses_settled_and_ignores_garbage() {
        assert_eq!(
            parse_record(r#"{"type":"agent_settled","aborted":true}"#),
            Record::Settled { aborted: true }
        );
        assert_eq!(parse_record("not json"), Record::Other);
        assert_eq!(parse_record(r#"{"type":"agent_start"}"#), Record::Other);
    }

    #[test]
    fn tracker_streams_deltas_and_finishes_with_final_text() {
        let mut tracker = RunTracker::default();
        assert_eq!(
            tracker.handle(Record::TextDelta("Hi".to_string())),
            Some(PiEvent::Delta {
                text: "Hi".to_string()
            })
        );
        assert_eq!(
            tracker.handle(Record::AssistantEnd {
                text: "Hi there".to_string(),
                error: None
            }),
            None
        );
        assert_eq!(
            tracker.handle(Record::Settled { aborted: false }),
            Some(PiEvent::Done {
                text: "Hi there".to_string(),
                aborted: false
            })
        );
    }

    #[test]
    fn tracker_reports_error_only_when_settled() {
        let mut tracker = RunTracker::default();
        let failed = Record::AssistantEnd {
            text: String::new(),
            error: Some("overloaded".to_string()),
        };
        assert_eq!(tracker.handle(failed), None);
        assert_eq!(
            tracker.handle(Record::Settled { aborted: false }),
            Some(PiEvent::Error {
                message: "overloaded".to_string()
            })
        );
    }

    #[test]
    fn tracker_clears_error_after_successful_retry() {
        let mut tracker = RunTracker::default();
        tracker.handle(Record::AssistantEnd {
            text: String::new(),
            error: Some("overloaded".to_string()),
        });
        tracker.handle(Record::AssistantEnd {
            text: "ok".to_string(),
            error: None,
        });
        assert_eq!(
            tracker.handle(Record::Settled { aborted: false }),
            Some(PiEvent::Done {
                text: "ok".to_string(),
                aborted: false
            })
        );
    }

    #[test]
    fn tracker_reports_abort_as_done_with_partial_text() {
        let mut tracker = RunTracker::default();
        tracker.handle(Record::AssistantEnd {
            text: "partial".to_string(),
            error: Some("This operation was aborted".to_string()),
        });
        assert_eq!(
            tracker.handle(Record::Settled { aborted: true }),
            Some(PiEvent::Done {
                text: "partial".to_string(),
                aborted: true
            })
        );
    }

    #[test]
    fn tracker_reports_rejected_prompt() {
        let mut tracker = RunTracker::default();
        assert_eq!(
            tracker.handle(Record::PromptRejected("No API key".to_string())),
            Some(PiEvent::Error {
                message: "No API key".to_string()
            })
        );
    }

    #[test]
    fn args_use_minimal_fixed_setup() {
        let args = build_args(None);
        for flag in [
            "--no-session",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
            "--no-mcp",
            "--no-themes",
            "--offline",
            "--no-approve",
        ] {
            assert!(args.iter().any(|a| a == flag), "missing {flag}");
        }
        assert_eq!(args[0..2], ["--mode", "rpc"]);
        let prompt_index = args.iter().position(|a| a == "--system-prompt").unwrap();
        assert!(args[prompt_index + 1].contains("Output only the resulting markdown"));
        assert!(!args.iter().any(|a| a == "--model"));
    }

    #[test]
    fn args_add_model_only_when_set() {
        assert!(!build_args(Some("  ")).iter().any(|a| a == "--model"));
        let args = build_args(Some(" anthropic/claude-sonnet-4-5 "));
        let index = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[index + 1], "anthropic/claude-sonnet-4-5");
    }

    #[test]
    fn context_is_unchanged_within_budget() {
        assert_eq!(
            truncate_context("abc", "def", 10),
            ("abc".to_string(), "def".to_string())
        );
    }

    #[test]
    fn context_keeps_text_nearest_to_the_marker() {
        let (before, after) = truncate_context("0123456789", "abcdefghij", 8);
        assert_eq!(before, "[…]\n6789");
        assert_eq!(after, "abcd\n[…]");
    }

    #[test]
    fn context_gives_unused_budget_to_the_other_side() {
        let (before, after) = truncate_context("0123456789", "ab", 8);
        assert_eq!(before, "[…]\n456789");
        assert_eq!(after, "ab");
    }

    #[test]
    fn context_truncation_respects_char_boundaries() {
        let (before, after) = truncate_context("ääääää", "üüüüüü", 4);
        assert_eq!(before, "[…]\nää");
        assert_eq!(after, "üü\n[…]");
    }

    #[test]
    fn first_prompt_in_selection_mode_contains_marked_document() {
        let prompt = build_prompt(&request(Some("**old** text")), true);
        assert!(prompt.contains(
            "<document>\n# Title\n\nIntro ⟦selection⟧**old** text⟦/selection⟧ outro.\n\n</document>"
        ));
        assert!(prompt.contains("<selection>\n**old** text\n</selection>"));
        assert!(prompt.contains("Instruction: Make it shorter"));
        assert!(prompt.contains("replaces the selected text"));
    }

    #[test]
    fn first_prompt_in_cursor_mode_marks_cursor() {
        let prompt = build_prompt(&request(None), true);
        assert!(prompt.contains("<document>\n# Title\n\nIntro ⟦cursor⟧ outro.\n\n</document>"));
        assert!(!prompt.contains("<selection>"));
        assert!(prompt.contains("Instruction: Make it shorter"));
        assert!(prompt.contains("insert at the cursor"));
    }

    #[test]
    fn follow_up_prompt_is_only_the_instruction() {
        assert_eq!(build_prompt(&request(Some("x")), false), "Make it shorter");
    }

    fn channel_sink() -> (Sink, std::sync::mpsc::Receiver<PiEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: Sink = Box::new(move |event| {
            let _ = tx.lock().unwrap().send(event);
        });
        (sink, rx)
    }

    /// Collects events until the run ends.
    fn collect_run(rx: &std::sync::mpsc::Receiver<PiEvent>) -> (String, PiEvent) {
        let mut streamed = String::new();
        loop {
            let event = rx
                .recv_timeout(std::time::Duration::from_secs(120))
                .expect("run did not end");
            match event {
                PiEvent::Delta { text } => streamed.push_str(&text),
                end => return (streamed, end),
            }
        }
    }

    #[cfg(unix)]
    fn fake_pi(script: &str) -> (tempfile_path::TempScript, String) {
        let script_file = tempfile_path::TempScript::new(script);
        let path = script_file.path.to_string_lossy().into_owned();
        (script_file, path)
    }

    #[cfg(unix)]
    mod tempfile_path {
        use std::os::unix::fs::PermissionsExt;
        use std::path::PathBuf;

        pub struct TempScript {
            pub path: PathBuf,
        }

        impl TempScript {
            pub fn new(body: &str) -> Self {
                static COUNTER: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let path = std::env::temp_dir()
                    .join(format!("scratch-fake-pi-{}-{n}.sh", std::process::id()));
                std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
                Self { path }
            }
        }

        impl Drop for TempScript {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn process_streams_a_scripted_run() {
        let (_script, program) = fake_pi(
            r#"read line
printf '%s\n' '{"type":"response","command":"prompt","success":true}'
printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"Hel"}}'
printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"lo"}}'
printf '%s\n' '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Hello"}],"stopReason":"stop"}}'
printf '%s\n' '{"type":"agent_settled","aborted":false}'
sleep 30"#,
        );
        let mut process = PiProcess::spawn(&program, &[], &get_expanded_path()).unwrap();
        let (sink, rx) = channel_sink();
        process.prompt(&request(None), sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        assert_eq!(streamed, "Hello");
        assert_eq!(
            end,
            PiEvent::Done {
                text: "Hello".to_string(),
                aborted: false
            }
        );
        assert!(process.slot.lock().unwrap().context_sent);
    }

    #[cfg(unix)]
    #[test]
    fn process_exit_during_run_reports_stderr() {
        let (_script, program) = fake_pi(r#"echo 'Error: Model "x/y" not found.' >&2; exit 1"#);
        let mut process = PiProcess::spawn(&program, &[], &get_expanded_path()).unwrap();
        let (sink, rx) = channel_sink();
        let expected = "pi stopped unexpectedly: Error: Model \"x/y\" not found.";
        match process.prompt(&request(None), sink) {
            // pi exited before the prompt was sent.
            Err(message) => assert!(
                message == expected || message.starts_with("Failed to send the prompt to pi"),
                "{message}"
            ),
            Ok(()) => assert_eq!(
                collect_run(&rx).1,
                PiEvent::Error {
                    message: expected.to_string()
                }
            ),
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!process.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_process_kills_it() {
        let (_script, program) = fake_pi("while read line; do :; done");
        let process = PiProcess::spawn(&program, &[], &get_expanded_path()).unwrap();
        let pid = process.child.id().to_string();
        drop(process);
        let still_running = std::process::Command::new("kill")
            .args(["-0", &pid])
            .status()
            .unwrap()
            .success();
        assert!(!still_running);
    }

    /// End-to-end check with the real pi and its default model.
    /// Run with: cargo test --lib pi::tests::real_pi -- --ignored --nocapture
    #[test]
    #[ignore = "needs a real pi install and model credentials"]
    fn real_pi_streams_refines_and_aborts() {
        let path = search_path();
        let program = resolve_program(None, &path).expect("pi on PATH");
        let mut process = PiProcess::spawn(&program, &build_args(None), &path).unwrap();

        let (sink, rx) = channel_sink();
        let first = PromptRequest {
            instruction: "Continue writing with one short sentence.".to_string(),
            before: "# Cats\n\nCats sleep a lot. ".to_string(),
            after: String::new(),
            selection: None,
        };
        process.prompt(&first, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("first: {streamed:?} / {end:?}");
        assert!(!streamed.is_empty());
        assert!(matches!(end, PiEvent::Done { aborted: false, .. }));

        let (sink, rx) = channel_sink();
        let refine = PromptRequest {
            instruction: "Now write it in German.".to_string(),
            ..first.clone()
        };
        process.prompt(&refine, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("refine: {streamed:?} / {end:?}");
        assert!(matches!(end, PiEvent::Done { aborted: false, .. }));

        let (sink, rx) = channel_sink();
        let long = PromptRequest {
            instruction: "Write a 600 word essay about cats.".to_string(),
            ..first
        };
        process.prompt(&long, sink).unwrap();
        // Abort after the first delta.
        let first_delta = rx
            .recv_timeout(std::time::Duration::from_secs(120))
            .expect("no delta");
        assert!(matches!(first_delta, PiEvent::Delta { .. }));
        process.abort().unwrap();
        let (_, end) = collect_run(&rx);
        println!("abort: {end:?}");
        assert!(matches!(end, PiEvent::Done { aborted: true, .. }));
        assert!(process.is_alive());
    }
}

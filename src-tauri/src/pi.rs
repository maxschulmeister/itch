//! Inline AI for the editor popover: one `pi --mode rpc` child process per
//! popover conversation. Events stream to the requesting webview through a
//! Tauri `Channel`.

use crate::{check_cli_exists, get_expanded_path, no_window_cmd, AppState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
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
/// Time pi has to answer one command, for example `set_model`.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum number of `cycle_model` calls to read the scoped models.
const MAX_SCOPED_MODELS: usize = 100;
/// pi thinking levels, from lowest to highest.
const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Prompt input from the editor. `before` and `after` are the document
/// markdown on each side of the selection (or cursor).
#[derive(Debug, Clone, Deserialize)]
pub struct PromptRequest {
    pub instruction: String,
    pub before: String,
    pub after: String,
    /// Selected text as markdown. `None` in cursor mode.
    pub selection: Option<String>,
    /// Thinking level for this prompt. `None` keeps the current level.
    #[serde(default)]
    pub thinking: Option<String>,
    /// Model as `provider/modelId` for this prompt. `None` keeps the current
    /// model (the pi default for a new conversation).
    #[serde(default)]
    pub model: Option<String>,
    /// The answer is for the user to read (for example an explanation). It
    /// does not replace the selection.
    #[serde(default)]
    pub read_only: bool,
}

/// A model that pi can use, for the model select.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PiModel {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub thinking_levels: Vec<String>,
}

/// Splits `provider/modelId` at the first slash. Model ids can contain
/// slashes (`nvidia/deepseek-ai/deepseek-v4.1-flash`).
fn split_model(model: &str) -> Option<(&str, &str)> {
    model
        .split_once('/')
        .filter(|(provider, id)| !provider.is_empty() && !id.is_empty())
}

/// The thinking levels that pi accepts for a model object. Same rule as
/// `getSupportedThinkingLevels` in pi-ai: a `null` map entry removes a level,
/// and `xhigh` and `max` need a map entry.
fn thinking_levels(model: &Value) -> Vec<String> {
    if model["reasoning"] != true {
        return vec!["off".to_string()];
    }
    let map = &model["thinkingLevelMap"];
    THINKING_LEVELS
        .iter()
        .filter(|level| match map.get(**level) {
            Some(Value::Null) => false,
            Some(_) => true,
            None => !matches!(**level, "xhigh" | "max"),
        })
        .map(|level| level.to_string())
        .collect()
}

/// The models for the model selects, and the model and thinking level that
/// pi uses when it starts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PiModelList {
    pub models: Vec<PiModel>,
    /// `provider/modelId`. `None` when pi has no usable model.
    pub default_model: Option<String>,
    pub default_thinking: Option<String>,
}

/// Reads a pi model object. `None` without an id or provider.
fn parse_model(model: &Value) -> Option<PiModel> {
    let id = model["id"].as_str()?;
    let provider = model["provider"].as_str()?;
    Some(PiModel {
        provider: provider.to_string(),
        id: id.to_string(),
        name: model["name"].as_str().unwrap_or(id).to_string(),
        thinking_levels: thinking_levels(model),
    })
}

impl PiModel {
    /// `provider/modelId`, the form of `PromptRequest::model`.
    fn key(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// The data of a successful command response.
fn response_data(response: &Value) -> Result<&Value, String> {
    if response["success"] != true {
        let error = response["error"].as_str().unwrap_or("unknown error");
        return Err(format!("pi could not list the models: {error}"));
    }
    Ok(&response["data"])
}

/// Reads the `get_available_models` response. Skips entries without an id
/// or provider.
fn parse_models(response: &Value) -> Result<Vec<PiModel>, String> {
    let models = response_data(response)?["models"]
        .as_array()
        .ok_or("pi returned no model list.")?;
    Ok(models.iter().filter_map(parse_model).collect())
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
    ModelSet(Result<(), String>),
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
        Some("response") if value["command"] == "set_model" => {
            Record::ModelSet(if value["success"] == true {
                Ok(())
            } else {
                Err(value["error"]
                    .as_str()
                    .unwrap_or("pi rejected the model.")
                    .to_string())
            })
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
            Record::PromptAccepted | Record::ModelSet(_) | Record::Other => None,
        }
    }
}

pub fn build_args() -> Vec<String> {
    [
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
    .collect()
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
        Some(selection) if request.read_only => format!(
            "<document>\n{before}{SELECTION_START}{selection}{SELECTION_END}{after}\n</document>\n\n\
             The selected text is marked with {SELECTION_START} and {SELECTION_END} in the document above:\n\
             <selection>\n{selection}\n</selection>\n\n\
             Instruction: {instruction}\n\n\
             Reply with markdown for the user to read. Do not rewrite the selected text."
        ),
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
    /// Receives the answer to the `set_model` command that is in progress.
    model_reply: Option<mpsc::Sender<Result<(), String>>>,
}

/// One running `pi --mode rpc` process. Dropping it kills the process.
pub struct PiProcess {
    child: Child,
    stdin: ChildStdin,
    slot: Arc<Mutex<RunSlot>>,
    /// Last thinking level sent to pi. `None` until the first change.
    thinking: Option<String>,
    /// Last model that pi accepted. `None` while pi uses its default.
    model: Option<String>,
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
        Ok(Self {
            child,
            stdin,
            slot,
            thinking: None,
            model: None,
        })
    }

    fn is_alive(&self) -> bool {
        self.slot
            .lock()
            .map(|s| s.exited.is_none())
            .unwrap_or(false)
    }

    /// Sends one prompt. Events for this run go to `sink` until a `Done` or
    /// `Error` event. When the model changes, the prompt waits until pi
    /// accepts the model, so that the prompt never runs with another model.
    pub fn prompt(&mut self, request: &PromptRequest, sink: Sink) -> Result<(), String> {
        let model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty() && self.model.as_deref() != Some(*model));
        let set_model = model
            .map(|model| {
                split_model(model)
                    .map(|(provider, id)| {
                        serde_json::json!({ "type": "set_model", "provider": provider, "modelId": id })
                    })
                    .ok_or_else(|| {
                        format!(
                            "Model \"{model}\" is not in the form provider/model. Choose a model in Settings."
                        )
                    })
            })
            .transpose()?;
        let (message, model_reply) = {
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
            let model_reply = set_model.is_some().then(|| {
                let (reply, receiver) = mpsc::channel();
                slot.model_reply = Some(reply);
                receiver
            });
            (build_prompt(request, !slot.context_sent), model_reply)
        };

        if let (Some(model), Some(command), Some(reply)) = (model, set_model, model_reply) {
            if let Err(e) = self.send(&command) {
                self.clear_run();
                return Err(format!("Failed to send the model to pi: {e}"));
            }
            match reply.recv_timeout(COMMAND_TIMEOUT) {
                Ok(Ok(())) => {
                    self.model = Some(model.to_string());
                    // pi fits the thinking level to the new model.
                    self.thinking = None;
                }
                Ok(Err(error)) => {
                    self.fail_run(format!("pi could not use the model {model}: {error}"));
                    return Ok(());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.fail_run(format!("pi did not confirm the model {model} in time."));
                    return Ok(());
                }
                // pi exited. The reader ended the run with an error event.
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }

        let level = request
            .thinking
            .as_deref()
            .map(str::trim)
            .filter(|level| !level.is_empty() && self.thinking.as_deref() != Some(*level));
        let mut commands = Vec::with_capacity(2);
        if let Some(level) = level {
            commands.push(serde_json::json!({ "type": "set_thinking_level", "level": level }));
        }
        commands.push(serde_json::json!({ "type": "prompt", "message": message }));
        if let Err(e) = commands.iter().try_for_each(|command| self.send(command)) {
            self.clear_run();
            return Err(format!("Failed to send the prompt to pi: {e}"));
        }
        if let Some(level) = level {
            self.thinking = Some(level.to_string());
        }
        Ok(())
    }

    fn clear_run(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            slot.active = None;
            slot.model_reply = None;
        }
    }

    /// Ends the current run with an error event.
    fn fail_run(&self, message: String) {
        if let Ok(mut slot) = self.slot.lock() {
            slot.model_reply = None;
            if let Some(run) = slot.active.take() {
                (run.sink)(PiEvent::Error { message });
            }
        }
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
        if let Record::ModelSet(result) = record {
            if let Some(reply) = slot.model_reply.take() {
                let _ = reply.send(result);
            }
            continue;
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
        slot.model_reply = None;
        slot.exited = Some(message);
    }
}

/// Sends commands to a short-lived pi and reads the responses.
struct PiCommands {
    stdin: ChildStdin,
    responses: mpsc::Receiver<Value>,
    stderr_reader: Option<JoinHandle<String>>,
}

impl PiCommands {
    /// Sends `command` and waits for its response.
    fn ask(&mut self, command: &str) -> Result<Value, String> {
        writeln!(self.stdin, "{}", serde_json::json!({ "type": command }))
            .map_err(|e| format!("Failed to ask pi for the models: {e}"))?;
        loop {
            match self.responses.recv_timeout(COMMAND_TIMEOUT) {
                Ok(value) if value["command"] == command => return Ok(value),
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err("pi did not list the models in time.".to_string())
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let stderr = self
                        .stderr_reader
                        .take()
                        .and_then(|reader| reader.join().ok())
                        .unwrap_or_default();
                    return Err(exit_message(&stderr));
                }
            }
        }
    }
}

/// Reads the models of the pi scope (`enabledModels`) with `cycle_model`,
/// in the cycle order after `start`. `None` when pi has no scope.
fn scoped_models(
    pi: &mut PiCommands,
    start: Option<&str>,
) -> Result<Option<Vec<PiModel>>, String> {
    let mut models: Vec<PiModel> = Vec::new();
    while models.len() < MAX_SCOPED_MODELS {
        let data = response_data(&pi.ask("cycle_model")?)?.clone();
        if data["isScoped"] != true {
            return Ok(None);
        }
        let Some(model) = parse_model(&data["model"]) else {
            break;
        };
        let key = model.key();
        if models.iter().any(|m| m.key() == key) {
            break;
        }
        let back_at_start = start == Some(key.as_str());
        models.push(model);
        if back_at_start {
            break;
        }
    }
    // The start model goes first.
    if let Some(index) = models.iter().position(|m| Some(m.key().as_str()) == start) {
        models.rotate_left(index);
    }
    Ok(Some(models))
}

/// Starts a short-lived pi and asks it for the models it can use: the scoped
/// models, else all available models. pi resolves the scope patterns.
fn list_models(program: &str, path_env: &str) -> Result<PiModelList, String> {
    let mut child = no_window_cmd(program)
        .args(build_args())
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
    let stderr_reader = std::thread::spawn(move || read_stderr_tail(stderr));
    let (reply, responses) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).split(b'\n') {
            let Ok(bytes) = line else { break };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if value["type"] == "response" && reply.send(value).is_err() {
                return;
            }
        }
    });
    // Keep stdin open until the last answer: pi stops at the end of its input.
    let mut pi = PiCommands {
        stdin,
        responses,
        stderr_reader: Some(stderr_reader),
    };
    let result = read_model_list(&mut pi);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn read_model_list(pi: &mut PiCommands) -> Result<PiModelList, String> {
    let state = response_data(&pi.ask("get_state")?)?.clone();
    let default_model = parse_model(&state["model"]).map(|model| model.key());
    let default_thinking = state["thinkingLevel"].as_str().map(str::to_string);
    let models = match scoped_models(pi, default_model.as_deref())? {
        Some(models) => models,
        None => parse_models(&pi.ask("get_available_models")?)?,
    };
    Ok(PiModelList {
        models,
        default_model,
        default_thinking,
    })
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

/// The model list for one pi program, kept for the app session.
#[derive(Default)]
pub struct PiModels(Mutex<Option<(String, PiModelList)>>);

/// Returns the models that pi can use, and the pi defaults. Asks pi once per
/// app session.
#[tauri::command]
pub async fn pi_list_models(
    app: AppHandle,
    app_state: State<'_, AppState>,
) -> Result<PiModelList, String> {
    let path_override = app_state
        .settings
        .read()
        .expect("settings read lock")
        .pi_path
        .clone();
    tauri::async_runtime::spawn_blocking(move || {
        let path = search_path();
        let program = resolve_program(path_override.as_deref(), &path)?;
        let cache = app.state::<PiModels>();
        // Hold the lock while pi answers, so that one pi starts at a time.
        let mut cached = cache.0.lock().map_err(|_| "pi model list is broken.")?;
        if let Some((cached_program, models)) = cached.as_ref() {
            if *cached_program == program {
                return Ok(models.clone());
            }
        }
        let models = list_models(&program, &path)?;
        *cached = Some((program, models.clone()));
        Ok(models)
    })
    .await
    .map_err(|e| format!("Failed to run pi: {e}"))?
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
    let path_override = app_state
        .settings
        .read()
        .expect("settings read lock")
        .pi_path
        .clone();
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
            let process = PiProcess::spawn(&program, &build_args(), &path)?;
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
            thinking: None,
            model: None,
            read_only: false,
        }
    }

    #[test]
    fn splits_model_at_the_first_slash() {
        assert_eq!(
            split_model("openai-codex/gpt-5.5"),
            Some(("openai-codex", "gpt-5.5"))
        );
        assert_eq!(
            split_model("nvidia/deepseek-ai/deepseek-v4.1-flash"),
            Some(("nvidia", "deepseek-ai/deepseek-v4.1-flash"))
        );
        assert_eq!(split_model("gpt-5.5"), None);
        assert_eq!(split_model("/gpt-5.5"), None);
        assert_eq!(split_model("openai-codex/"), None);
    }

    fn levels(model: Value) -> Vec<String> {
        thinking_levels(&model)
    }

    #[test]
    fn model_without_reasoning_has_only_off() {
        assert_eq!(levels(serde_json::json!({ "reasoning": false })), ["off"]);
        assert_eq!(levels(serde_json::json!({})), ["off"]);
    }

    #[test]
    fn reasoning_model_without_map_has_off_to_high() {
        assert_eq!(
            levels(serde_json::json!({ "reasoning": true })),
            ["off", "minimal", "low", "medium", "high"]
        );
    }

    #[test]
    fn null_in_the_map_removes_a_level_and_xhigh_max_need_an_entry() {
        // Values from pi 1.3.0, checked against get_available_thinking_levels.
        assert_eq!(
            levels(serde_json::json!({
                "reasoning": true,
                "thinkingLevelMap": { "off": null, "xhigh": "xhigh", "max": "max" }
            })),
            ["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            levels(serde_json::json!({
                "reasoning": true,
                "thinkingLevelMap": { "minimal": null, "low": null, "medium": null, "high": "high", "max": "max" }
            })),
            ["off", "high", "max"]
        );
        assert_eq!(
            levels(serde_json::json!({
                "reasoning": true,
                "thinkingLevelMap": { "off": "none", "xhigh": null, "max": "max", "minimal": "low" }
            })),
            ["off", "minimal", "low", "medium", "high", "max"]
        );
    }

    #[test]
    fn parses_the_available_models_response() {
        let response = serde_json::json!({
            "type": "response",
            "command": "get_available_models",
            "success": true,
            "data": { "models": [
                { "id": "gpt-5.5", "name": "GPT-5.5", "provider": "openai-codex", "reasoning": true,
                  "thinkingLevelMap": { "xhigh": "xhigh" } },
                { "id": "glm-4.7", "provider": "zai", "reasoning": false },
                { "name": "no id", "provider": "zai" }
            ] }
        });
        assert_eq!(
            parse_models(&response),
            Ok(vec![
                PiModel {
                    provider: "openai-codex".to_string(),
                    id: "gpt-5.5".to_string(),
                    name: "GPT-5.5".to_string(),
                    thinking_levels: ["off", "minimal", "low", "medium", "high", "xhigh"]
                        .map(String::from)
                        .to_vec(),
                },
                PiModel {
                    provider: "zai".to_string(),
                    id: "glm-4.7".to_string(),
                    name: "glm-4.7".to_string(),
                    thinking_levels: vec!["off".to_string()],
                },
            ])
        );
    }

    #[test]
    fn model_list_failure_returns_the_pi_error() {
        let response = serde_json::json!({
            "type": "response",
            "command": "get_available_models",
            "success": false,
            "error": "boom"
        });
        assert_eq!(
            parse_models(&response),
            Err("pi could not list the models: boom".to_string())
        );
    }

    #[test]
    fn parses_set_model_responses() {
        assert_eq!(
            parse_record(r#"{"type":"response","command":"set_model","success":true,"data":{}}"#),
            Record::ModelSet(Ok(()))
        );
        assert_eq!(
            parse_record(
                r#"{"type":"response","command":"set_model","success":false,"error":"Model not found: nope/x"}"#
            ),
            Record::ModelSet(Err("Model not found: nope/x".to_string()))
        );
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
        let args = build_args();
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
    fn read_only_prompt_asks_for_an_answer_about_the_selection() {
        let req = PromptRequest {
            read_only: true,
            ..request(Some("old text"))
        };
        let prompt = build_prompt(&req, true);
        assert!(prompt.contains("<selection>\nold text\n</selection>"));
        assert!(prompt.contains("Do not rewrite the selected text"));
        assert!(!prompt.contains("replaces the selected text"));
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
                    .join(format!("itch-fake-pi-{}-{n}.sh", std::process::id()));
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
    fn process_sets_thinking_level_before_prompt_when_it_changes() {
        let log = std::env::temp_dir().join(format!("itch-fake-pi-log-{}", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let (_script, program) = logging_fake_pi(&log, "");
        let mut process = PiProcess::spawn(&program, &[], &get_expanded_path()).unwrap();
        for thinking in [Some("low"), Some("low"), None, Some("high")] {
            let (sink, rx) = channel_sink();
            let req = PromptRequest {
                thinking: thinking.map(str::to_string),
                ..request(None)
            };
            process.prompt(&req, sink).unwrap();
            collect_run(&rx);
        }
        assert_eq!(
            read_command_log(&log),
            [
                "set_thinking_level:low",
                "prompt",
                "prompt",
                "prompt",
                "set_thinking_level:high",
                "prompt"
            ]
        );
    }

    /// A fake pi that logs each command and answers set_model with
    /// `set_model_reply` and each prompt with a short run.
    #[cfg(unix)]
    fn logging_fake_pi(
        log: &std::path::Path,
        set_model_reply: &str,
    ) -> (tempfile_path::TempScript, String) {
        fake_pi(&format!(
            r#"while read line; do
printf '%s\n' "$line" >> '{}'
case "$line" in
*'"type":"set_model"'*)
printf '%s\n' '{set_model_reply}';;
*'"type":"prompt"'*)
printf '%s\n' '{{"type":"response","command":"prompt","success":true}}'
printf '%s\n' '{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"text","text":"ok"}}],"stopReason":"stop"}}}}'
printf '%s\n' '{{"type":"agent_settled","aborted":false}}';;
esac
done"#,
            log.display()
        ))
    }

    /// The sent commands as `type[:detail]` strings.
    fn read_command_log(log: &std::path::Path) -> Vec<String> {
        let sent = std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                let kind = value["type"].as_str().unwrap();
                match (value["level"].as_str(), value["provider"].as_str()) {
                    (Some(level), _) => format!("{kind}:{level}"),
                    (_, Some(provider)) => {
                        format!("{kind}:{provider}|{}", value["modelId"].as_str().unwrap())
                    }
                    _ => kind.to_string(),
                }
            })
            .collect();
        let _ = std::fs::remove_file(log);
        sent
    }

    #[cfg(unix)]
    #[test]
    fn process_sets_model_before_thinking_and_prompt_when_it_changes() {
        let log =
            std::env::temp_dir().join(format!("itch-fake-pi-model-{}", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let (_script, program) = logging_fake_pi(
            &log,
            r#"{"type":"response","command":"set_model","success":true,"data":{}}"#,
        );
        let mut process = PiProcess::spawn(&program, &[], &get_expanded_path()).unwrap();
        for model in [
            Some("openai-codex/gpt-5.5"),
            Some("openai-codex/gpt-5.5"),
            None,
            Some("nvidia/deepseek-ai/deepseek-v4.1-flash"),
        ] {
            let (sink, rx) = channel_sink();
            let req = PromptRequest {
                model: model.map(str::to_string),
                thinking: Some("low".to_string()),
                ..request(None)
            };
            process.prompt(&req, sink).unwrap();
            assert!(matches!(collect_run(&rx).1, PiEvent::Done { .. }));
        }
        assert_eq!(
            read_command_log(&log),
            [
                "set_model:openai-codex|gpt-5.5",
                "set_thinking_level:low",
                "prompt",
                "prompt",
                "prompt",
                // pi clamps the level to the model: send it again.
                "set_model:nvidia|deepseek-ai/deepseek-v4.1-flash",
                "set_thinking_level:low",
                "prompt"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_ends_run_with_error_when_set_model_fails() {
        let log =
            std::env::temp_dir().join(format!("itch-fake-pi-badmodel-{}", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let (_script, program) = logging_fake_pi(
            &log,
            r#"{"type":"response","command":"set_model","success":false,"error":"Model not found: nope/x"}"#,
        );
        let mut process = PiProcess::spawn(&program, &[], &get_expanded_path()).unwrap();
        for _ in 0..2 {
            let (sink, rx) = channel_sink();
            let req = PromptRequest {
                model: Some("nope/x".to_string()),
                thinking: Some("low".to_string()),
                ..request(None)
            };
            process.prompt(&req, sink).unwrap();
            assert_eq!(
                collect_run(&rx).1,
                PiEvent::Error {
                    message: "pi could not use the model nope/x: Model not found: nope/x"
                        .to_string()
                }
            );
        }
        // No prompt runs with another model, and the next prompt tries again.
        assert_eq!(
            read_command_log(&log),
            ["set_model:nope|x", "set_model:nope|x"]
        );
        assert!(process.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn process_rejects_a_model_without_provider() {
        let (_script, program) = fake_pi("while read line; do :; done");
        let mut process = PiProcess::spawn(&program, &[], &get_expanded_path()).unwrap();
        let (sink, _rx) = channel_sink();
        let req = PromptRequest {
            model: Some("gpt-5.5".to_string()),
            ..request(None)
        };
        assert_eq!(
            process.prompt(&req, sink),
            Err(
                "Model \"gpt-5.5\" is not in the form provider/model. Choose a model in Settings."
                    .to_string()
            )
        );
        assert!(process.slot.lock().unwrap().active.is_none());
    }

    /// A fake pi for `list_models`. `get_state` returns `state_model` (JSON
    /// model object or `null`) and the level `medium`. Each `cycle_model`
    /// returns the next line of `cycle` (shell `case` arms for `$n`, the
    /// 1-based call count). `get_available_models` returns one model.
    #[cfg(unix)]
    fn model_list_fake_pi(state_model: &str, cycle: &str) -> (tempfile_path::TempScript, String) {
        fake_pi(&format!(
            r#"n=0
while read line; do
case "$line" in
*'"type":"get_state"'*)
printf '%s\n' '{{"type":"agent_start"}}'
printf '%s\n' '{{"type":"response","command":"get_state","success":true,"data":{{"model":{state_model},"thinkingLevel":"medium"}}}}';;
*'"type":"cycle_model"'*)
n=$((n+1))
case $n in
{cycle}
esac;;
*'"type":"get_available_models"'*)
printf '%s\n' '{{"type":"response","command":"get_available_models","success":true,"data":{{"models":[{{"id":"all-1","provider":"p","reasoning":false}}]}}}}';;
esac
done"#
        ))
    }

    fn fake_model(provider: &str, id: &str) -> String {
        format!(r#"{{"id":"{id}","name":"{id}","provider":"{provider}","reasoning":true}}"#)
    }

    /// A `cycle_model` case arm that answers with `data`.
    fn cycle_arm(n: &str, data: &str) -> String {
        format!(
            r#"{n}) printf '%s\n' '{{"type":"response","command":"cycle_model","success":true,"data":{data}}}';;"#
        )
    }

    fn scoped(model: &str) -> String {
        format!(r#"{{"model":{model},"thinkingLevel":"high","isScoped":true}}"#)
    }

    fn keys(list: &PiModelList) -> Vec<String> {
        list.models
            .iter()
            .map(|m| format!("{}/{}", m.provider, m.id))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn list_models_returns_the_scoped_models_and_the_pi_defaults() {
        let (a, b, c) = (
            fake_model("anthropic", "a"),
            fake_model("nvidia", "z-ai/b"),
            fake_model("openai", "c"),
        );
        // pi starts with b and cycles c, a, b, c, ...
        let cycle = [
            cycle_arm("1|4", &scoped(&c)),
            cycle_arm("2|5", &scoped(&a)),
            cycle_arm("3|6", &scoped(&b)),
        ]
        .join("\n");
        let (_script, program) = model_list_fake_pi(&b, &cycle);
        let started = std::time::Instant::now();
        let list = list_models(&program, &get_expanded_path()).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert_eq!(keys(&list), ["nvidia/z-ai/b", "openai/c", "anthropic/a"]);
        assert_eq!(list.default_model.as_deref(), Some("nvidia/z-ai/b"));
        assert_eq!(list.default_thinking.as_deref(), Some("medium"));
        assert_eq!(
            list.models[0].thinking_levels,
            ["off", "minimal", "low", "medium", "high"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_models_without_a_scope_returns_all_models() {
        let a = fake_model("anthropic", "a");
        for data in [
            "null".to_string(),
            format!(r#"{{"model":{a},"thinkingLevel":"high","isScoped":false}}"#),
        ] {
            let (_script, program) = model_list_fake_pi(&a, &cycle_arm("*", &data));
            let list = list_models(&program, &get_expanded_path()).unwrap();
            assert_eq!(keys(&list), ["p/all-1"], "cycle data {data}");
            assert_eq!(list.default_model.as_deref(), Some("anthropic/a"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn list_models_stops_a_cycle_that_does_not_come_back() {
        // A scope that does not contain the start model, and a pi that never
        // repeats a model: the list stops at the limit.
        let (_script, program) = model_list_fake_pi(
            "null",
            &cycle_arm("*", &scoped(&fake_model("p", "m-'\"$n\"'"))),
        );
        let list = list_models(&program, &get_expanded_path()).unwrap();
        assert_eq!(list.models.len(), MAX_SCOPED_MODELS);
        assert_eq!(list.models[1].id, "m-2");
        assert_eq!(list.default_model, None);
    }

    #[cfg(unix)]
    #[test]
    fn list_models_reports_a_failed_command() {
        let (_script, program) = fake_pi(
            r#"read line
printf '%s\n' '{"type":"response","command":"get_state","success":false,"error":"boom"}'
sleep 30"#,
        );
        assert_eq!(
            list_models(&program, &get_expanded_path()),
            Err("pi could not list the models: boom".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_models_reports_pi_exit() {
        let (_script, program) = fake_pi(r#"echo 'Error: bad config' >&2; exit 1"#);
        assert_eq!(
            list_models(&program, &get_expanded_path()),
            Err("pi stopped unexpectedly: Error: bad config".to_string())
        );
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

    /// End-to-end check of the model list and `set_model` with the real pi.
    /// Run with: PI_TEST_MODEL=openai-codex/gpt-5.5 cargo test --lib
    /// pi::tests::real_pi_lists -- --ignored --nocapture
    #[test]
    #[ignore = "needs a real pi install and model credentials"]
    fn real_pi_lists_models_and_switches_model() {
        let path = search_path();
        let program = resolve_program(None, &path).expect("pi on PATH");
        let list = list_models(&program, &path).unwrap();
        println!(
            "default {:?} / {:?}, {} models:",
            list.default_model,
            list.default_thinking,
            list.models.len()
        );
        for model in &list.models {
            println!("  {} {:?}", model.key(), model.thinking_levels);
        }
        let model = std::env::var("PI_TEST_MODEL").expect("PI_TEST_MODEL");
        assert!(list.models.iter().any(|m| m.key() == model));
        let default_model = list.default_model.expect("a default model");
        assert!(list.models.iter().any(|m| m.key() == default_model));

        let mut process = PiProcess::spawn(&program, &build_args(), &path).unwrap();
        let (sink, rx) = channel_sink();
        let missing = PromptRequest {
            model: Some("nope/missing-model".to_string()),
            ..request(Some("Cats sleep."))
        };
        process.prompt(&missing, sink).unwrap();
        let (_, end) = collect_run(&rx);
        println!("missing model: {end:?}");
        assert!(matches!(end, PiEvent::Error { .. }));

        let (sink, rx) = channel_sink();
        let req = PromptRequest {
            model: Some(model),
            thinking: Some("low".to_string()),
            ..request(Some("Cats sleeps a lot."))
        };
        process.prompt(&req, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("model: {streamed:?} / {end:?}");
        assert!(matches!(end, PiEvent::Done { aborted: false, .. }));
    }

    /// End-to-end check with the real pi and its default model.
    /// Run with: cargo test --lib pi::tests::real_pi -- --ignored --nocapture
    #[test]
    #[ignore = "needs a real pi install and model credentials"]
    fn real_pi_streams_refines_and_aborts() {
        let path = search_path();
        let program = resolve_program(None, &path).expect("pi on PATH");
        let mut process = PiProcess::spawn(&program, &build_args(), &path).unwrap();

        let (sink, rx) = channel_sink();
        // Set PI_TEST_MODEL (provider/model) to test with a model that is not the default.
        let first = PromptRequest {
            instruction: "Continue writing with one short sentence.".to_string(),
            before: "# Cats\n\nCats sleep a lot. ".to_string(),
            after: String::new(),
            selection: None,
            thinking: None,
            model: std::env::var("PI_TEST_MODEL").ok(),
            read_only: false,
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

//! The pi backend: one `pi --mode rpc` child process per popover
//! conversation.

use super::process::{exit_message, read_stderr_tail};
use super::{
    build_prompt, AiEvent, Conversation, Model, ModelList, PromptRequest, Sink, SYSTEM_PROMPT,
};
use crate::no_window_cmd;
use serde::Serialize;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Time pi has to answer one command, for example `set_model`.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum number of `cycle_model` calls to read the scoped models.
const MAX_SCOPED_MODELS: usize = 100;
/// pi thinking levels, from lowest to highest.
const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

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
    fn handle(&mut self, record: Record) -> Option<AiEvent> {
        match record {
            Record::TextDelta(text) => Some(AiEvent::Delta { text }),
            Record::AssistantEnd { text, error } => {
                self.text = text;
                self.error = error;
                None
            }
            Record::PromptRejected(message) => Some(AiEvent::Error { message }),
            Record::Settled { aborted } => Some(match (aborted, self.error.take()) {
                (false, Some(message)) => AiEvent::Error { message },
                _ => AiEvent::Done {
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
            .model()
            .filter(|model| self.model.as_deref() != Some(*model));
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
            .thinking()
            .filter(|level| self.thinking.as_deref() != Some(*level));
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
                (run.sink)(AiEvent::Error { message });
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
            let finished = !matches!(event, AiEvent::Delta { .. });
            (run.sink)(event);
            if finished {
                slot.active = None;
            }
        }
    }

    let stderr_tail = stderr_reader.join().unwrap_or_default();
    let message = exit_message("pi", &stderr_tail);
    if let Ok(mut slot) = slot.lock() {
        if let Some(run) = slot.active.take() {
            (run.sink)(AiEvent::Error {
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
                    return Err(exit_message("pi", &stderr));
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
pub fn list_models(program: &str, path_env: &str) -> Result<PiModelList, String> {
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

impl From<PiModelList> for ModelList {
    fn from(list: PiModelList) -> Self {
        ModelList {
            models: list
                .models
                .into_iter()
                .map(|model| Model {
                    id: model.key(),
                    name: model.name,
                    group: model.provider,
                    thinking_levels: model.thinking_levels,
                })
                .collect(),
            default_model: list.default_model,
            default_thinking: list.default_thinking,
        }
    }
}

impl Conversation for PiProcess {
    fn prompt(&mut self, request: &PromptRequest, sink: Sink) -> Result<(), String> {
        PiProcess::prompt(self, request, sink)
    }

    fn abort(&mut self) -> Result<(), String> {
        PiProcess::abort(self)
    }

    fn is_alive(&self) -> bool {
        PiProcess::is_alive(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::test_support::{channel_sink, collect_run, request};
    use crate::get_expanded_path;

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
            Some(AiEvent::Delta {
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
            Some(AiEvent::Done {
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
            Some(AiEvent::Error {
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
            Some(AiEvent::Done {
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
            Some(AiEvent::Done {
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
            Some(AiEvent::Error {
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

    #[cfg(unix)]
    fn fake_pi(script: &str) -> (crate::ai::test_support::TempFile, String) {
        crate::ai::test_support::fake_tool(script)
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
            AiEvent::Done {
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
    ) -> (crate::ai::test_support::TempFile, String) {
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
            assert!(matches!(collect_run(&rx).1, AiEvent::Done { .. }));
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
                AiEvent::Error {
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
    fn model_list_fake_pi(state_model: &str, cycle: &str) -> (crate::ai::test_support::TempFile, String) {
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
                AiEvent::Error {
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
        let path = crate::ai::search_path();
        let program = crate::ai::resolve_program(crate::ai::Provider::Pi, None, &path)
            .expect("pi on PATH");
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
        assert!(matches!(end, AiEvent::Error { .. }));

        let (sink, rx) = channel_sink();
        let req = PromptRequest {
            model: Some(model),
            thinking: Some("low".to_string()),
            ..request(Some("Cats sleeps a lot."))
        };
        process.prompt(&req, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("model: {streamed:?} / {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));
    }

    /// End-to-end check with the real pi and its default model.
    /// Run with: cargo test --lib pi::tests::real_pi -- --ignored --nocapture
    #[test]
    #[ignore = "needs a real pi install and model credentials"]
    fn real_pi_streams_refines_and_aborts() {
        let path = crate::ai::search_path();
        let program = crate::ai::resolve_program(crate::ai::Provider::Pi, None, &path)
            .expect("pi on PATH");
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
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

        let (sink, rx) = channel_sink();
        let refine = PromptRequest {
            instruction: "Now write it in German.".to_string(),
            ..first.clone()
        };
        process.prompt(&refine, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("refine: {streamed:?} / {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

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
        assert!(matches!(first_delta, AiEvent::Delta { .. }));
        process.abort().unwrap();
        let (_, end) = collect_run(&rx);
        println!("abort: {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: true, .. }));
        assert!(process.is_alive());
    }
}

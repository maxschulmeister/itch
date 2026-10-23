//! The Claude Code backend: one `claude -p` child process per popover
//! conversation, with stream-json input and output.

use super::process::{JsonProcess, JsonWriter};
use super::{
    build_prompt, Conversation, Model, ModelList, PromptRequest, Run, Sink, SYSTEM_PROMPT,
};
use crate::no_window_cmd;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

/// The tool name for messages to the user.
const TOOL: &str = "Claude Code";
/// Time Claude Code has to answer one control request, for example `set_model`.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// The request id of the `initialize` request in `list_models`.
const INITIALIZE_ID: &str = "itch-initialize";
/// The message when Claude Code does not know one of our flags.
const TOO_OLD: &str = "This Claude Code version is too old for Itch. \
Update Claude Code (for example with `claude update`) and try again.";

/// The message for the user when Claude Code exited. Old versions exit
/// with "unknown option" for the flags that they do not know.
fn exit_message_for_user(message: String) -> String {
    if message.contains("unknown option") {
        TOO_OLD.to_string()
    } else {
        message
    }
}

/// Arguments for a text-only Claude Code without tools, user customizations
/// (hooks, CLAUDE.md, plugins, MCP servers, skills) or saved sessions. Login
/// and the model settings of the user still apply.
pub fn build_args() -> Vec<String> {
    [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--verbose",
        "--tools",
        "",
        "--safe-mode",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--permission-prompts",
        "none",
        "--no-session-persistence",
        "--system-prompt",
        SYSTEM_PROMPT,
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect()
}

/// The stream-json messages that matter.
#[derive(Debug, PartialEq)]
enum Message {
    TextDelta(String),
    /// The end of a turn: the final text (if any), or the error text.
    TurnEnd(Result<Option<String>, String>),
    /// The answer to one of our control requests: its data, or the error.
    ControlResponse {
        id: String,
        result: Result<Value, String>,
    },
    /// A request from Claude Code, for example a permission request.
    ControlRequest {
        id: String,
    },
    Other,
}

fn parse_message(value: &Value) -> Message {
    match value["type"].as_str() {
        Some("stream_event") => {
            let delta = &value["event"]["delta"];
            match (delta["type"].as_str(), delta["text"].as_str()) {
                (Some("text_delta"), Some(text)) => Message::TextDelta(text.to_string()),
                _ => Message::Other,
            }
        }
        Some("result") => Message::TurnEnd(parse_result(value)),
        Some("control_response") => {
            let response = &value["response"];
            let Some(id) = response["request_id"].as_str() else {
                return Message::Other;
            };
            let result = if response["subtype"] == "success" {
                Ok(response["response"].clone())
            } else {
                Err(response["error"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string())
            };
            Message::ControlResponse {
                id: id.to_string(),
                result,
            }
        }
        Some("control_request") => match value["request_id"].as_str() {
            Some(id) => Message::ControlRequest { id: id.to_string() },
            None => Message::Other,
        },
        _ => Message::Other,
    }
}

/// Reads a `result` line. Error results without a text (for example after
/// an interrupt) get a general message.
fn parse_result(value: &Value) -> Result<Option<String>, String> {
    let text = value["result"].as_str();
    if value["is_error"] != true {
        return Ok(text.map(str::to_string));
    }
    Err(text
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("Claude Code could not finish the request.")
        .to_string())
}

/// Reads the models of the `initialize` response. Skips the `default`
/// entry: the frontend has its own default.
fn parse_models(response: &Value) -> Result<Vec<Model>, String> {
    let models = response["models"]
        .as_array()
        .ok_or("Claude Code returned no model list.")?;
    Ok(models
        .iter()
        .filter_map(|model| {
            let id = model["value"].as_str().filter(|id| *id != "default")?;
            Some(Model {
                id: id.to_string(),
                name: model["displayName"].as_str().unwrap_or(id).to_string(),
                group: String::new(),
                thinking_levels: model["supportedEffortLevels"]
                    .as_array()
                    .map(|levels| {
                        levels
                            .iter()
                            .filter_map(|level| level.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect())
}

fn control_request(id: &str, request: Value) -> Value {
    json!({ "type": "control_request", "request_id": id, "request": request })
}

/// The answer to a request from Claude Code: Itch allows nothing.
fn refusal(id: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": id,
            "error": "Itch does not allow this request.",
        }
    })
}

/// State shared between the conversation and the stdout reader thread.
#[derive(Default)]
struct Shared {
    run: Run,
    /// True after `abort` asked Claude Code to stop the current run.
    aborting: bool,
    /// Set when Claude Code exited. Contains a message for the user.
    exited: Option<String>,
    /// Control requests that wait for an answer, by request id.
    replies: HashMap<String, mpsc::Sender<Result<Value, String>>>,
}

impl Shared {
    fn handle(&mut self, message: Message, writer: &JsonWriter) {
        match message {
            Message::TextDelta(text) => self.run.delta(&text),
            // An interrupted turn ends with an error result: keep the streamed text.
            Message::TurnEnd(result) => match (std::mem::take(&mut self.aborting), result) {
                (true, _) => self.run.finish(None, true),
                (false, Ok(text)) => self.run.finish(text, false),
                (false, Err(message)) => self.run.fail(message),
            },
            Message::ControlResponse { id, result } => {
                if let Some(reply) = self.replies.remove(&id) {
                    let _ = reply.send(result);
                }
            }
            Message::ControlRequest { id } => {
                let _ = writer.send(&refusal(&id));
            }
            Message::Other => {}
        }
    }

    fn exit(&mut self, message: String) {
        self.run.fail(message.clone());
        self.replies.clear();
        self.exited = Some(message);
    }
}

/// One running Claude Code process. Dropping it kills the process.
pub struct ClaudeProcess {
    process: JsonProcess,
    shared: Arc<Mutex<Shared>>,
    /// Last model that Claude Code accepted. `None` while it uses its default.
    model: Option<String>,
    /// Last effort level that Claude Code accepted. `None` for its default.
    effort: Option<String>,
    /// True after the first prompt, which carries the document, was sent.
    context_sent: bool,
    next_id: u64,
}

impl ClaudeProcess {
    pub fn spawn(program: &str, args: &[String], path_env: &str) -> Result<Self, String> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let reader = Arc::clone(&shared);
        let exited = Arc::clone(&shared);
        let mut command = no_window_cmd(program);
        command.args(args).env("PATH", path_env);
        let process = JsonProcess::spawn(
            TOOL,
            command,
            move |value, writer| {
                if let Ok(mut shared) = reader.lock() {
                    shared.handle(parse_message(&value), writer);
                }
            },
            move |message| {
                if let Ok(mut shared) = exited.lock() {
                    shared.exit(exit_message_for_user(message));
                }
            },
        )?;
        Ok(Self {
            process,
            shared,
            model: None,
            effort: None,
            context_sent: false,
            next_id: 0,
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Shared>, String> {
        self.shared
            .lock()
            .map_err(|_| "Claude Code session is broken.".to_string())
    }

    fn next_request_id(&mut self) -> String {
        self.next_id += 1;
        format!("itch-{}", self.next_id)
    }

    /// Sends a control request and waits for its answer.
    fn control(&mut self, request: Value) -> Result<Value, String> {
        let id = self.next_request_id();
        let (reply, answer) = mpsc::channel();
        {
            let mut shared = self.lock()?;
            if let Some(message) = &shared.exited {
                return Err(message.clone());
            }
            shared.replies.insert(id.clone(), reply);
        }
        let result = match self.process.send(&control_request(&id, request)) {
            Err(e) => Err(e.to_string()),
            Ok(()) => match answer.recv_timeout(COMMAND_TIMEOUT) {
                Ok(result) => result,
                Err(mpsc::RecvTimeoutError::Timeout) => Err("no answer in time".to_string()),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err("Claude Code stopped".to_string()),
            },
        };
        self.lock()?.replies.remove(&id);
        result
    }

    /// Switches the model and the effort level when the request changes them.
    fn apply_settings(&mut self, request: &PromptRequest) -> Result<(), String> {
        if let Some(model) = request
            .model()
            .filter(|model| self.model.as_deref() != Some(*model))
        {
            self.control(json!({ "subtype": "set_model", "model": model }))
                .map_err(|e| format!("Claude Code could not use the model {model}: {e}"))?;
            self.model = Some(model.to_string());
        }
        if let Some(level) = request
            .thinking()
            .filter(|level| self.effort.as_deref() != Some(*level))
        {
            self.control(json!({
                "subtype": "apply_flag_settings",
                "settings": { "effortLevel": level },
            }))
            .map_err(|e| format!("Claude Code could not use the effort level {level}: {e}"))?;
            self.effort = Some(level.to_string());
        }
        Ok(())
    }

    /// Sends one prompt. Events for this run go to `sink` until a `Done` or
    /// `Error` event. A model or effort change waits until Claude Code
    /// accepts it, so that the prompt never runs with other settings.
    pub fn prompt(&mut self, request: &PromptRequest, sink: Sink) -> Result<(), String> {
        {
            let mut shared = self.lock()?;
            if let Some(message) = &shared.exited {
                return Err(message.clone());
            }
            shared.run.start(TOOL, sink)?;
            shared.aborting = false;
        }
        if let Err(message) = self.apply_settings(request) {
            self.lock()?.run.fail(message);
            return Ok(());
        }
        let text = build_prompt(request, !self.context_sent);
        let message = json!({ "type": "user", "message": { "role": "user", "content": text } });
        if let Err(e) = self.process.send(&message) {
            self.lock()?.run = Run::default();
            return Err(format!("Failed to send the prompt to Claude Code: {e}"));
        }
        self.context_sent = true;
        Ok(())
    }

    /// Asks Claude Code to stop the current run. The turn then ends with a
    /// `result`, which sends `Done { aborted: true }`.
    pub fn abort(&mut self) -> Result<(), String> {
        {
            let mut shared = self.lock()?;
            if !shared.run.is_active() || shared.aborting {
                return Ok(());
            }
            shared.aborting = true;
        }
        let id = self.next_request_id();
        self.process
            .send(&control_request(&id, json!({ "subtype": "interrupt" })))
            .map_err(|e| format!("Failed to stop Claude Code: {e}"))
    }

    fn is_alive(&self) -> bool {
        self.lock().map(|s| s.exited.is_none()).unwrap_or(false)
    }
}

impl Drop for ClaudeProcess {
    fn drop(&mut self) {
        // The caller closed the conversation: no event for the current run.
        if let Ok(mut shared) = self.shared.lock() {
            shared.run = Run::default();
        }
    }
}

impl Conversation for ClaudeProcess {
    fn prompt(&mut self, request: &PromptRequest, sink: Sink) -> Result<(), String> {
        ClaudeProcess::prompt(self, request, sink)
    }

    fn abort(&mut self) -> Result<(), String> {
        ClaudeProcess::abort(self)
    }

    fn is_alive(&self) -> bool {
        ClaudeProcess::is_alive(self)
    }
}

/// Starts a conversation with Claude Code.
pub fn start(program: &str, path_env: &str) -> Result<Box<dyn Conversation>, String> {
    Ok(Box::new(ClaudeProcess::spawn(
        program,
        &build_args(),
        path_env,
    )?))
}

/// Starts a short-lived Claude Code and asks it for its models with the
/// `initialize` control request. This does not call the model.
pub fn list_models(program: &str, path_env: &str) -> Result<ModelList, String> {
    list_models_with(program, &build_args(), path_env)
}

fn list_models_with(program: &str, args: &[String], path_env: &str) -> Result<ModelList, String> {
    let (reply, answer) = mpsc::channel();
    let exit_reply = reply.clone();
    let mut command = no_window_cmd(program);
    command.args(args).env("PATH", path_env);
    let process = JsonProcess::spawn(
        TOOL,
        command,
        move |value, _| {
            if let Message::ControlResponse { id, result } = parse_message(&value) {
                if id == INITIALIZE_ID {
                    let _ =
                        reply
                            .send(result.map_err(|e| {
                                format!("Claude Code could not list the models: {e}")
                            }));
                }
            }
        },
        move |message| {
            let _ = exit_reply.send(Err(exit_message_for_user(message)));
        },
    )?;
    process
        .send(&control_request(
            INITIALIZE_ID,
            json!({ "subtype": "initialize" }),
        ))
        .map_err(|e| format!("Failed to ask Claude Code for the models: {e}"))?;
    let response = answer
        .recv_timeout(COMMAND_TIMEOUT)
        .map_err(|_| "Claude Code did not list the models in time.".to_string())??;
    Ok(ModelList {
        models: parse_models(&response)?,
        default_model: None,
        default_thinking: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::test_support::{channel_sink, collect_run, request};
    use crate::ai::AiEvent;
    use crate::get_expanded_path;

    fn parse(line: &str) -> Message {
        parse_message(&serde_json::from_str(line).unwrap())
    }

    #[test]
    fn parses_text_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":" Qu"}},"session_id":"s","parent_tool_use_id":null,"uuid":"u"}"#;
        assert_eq!(parse(line), Message::TextDelta(" Qu".to_string()));
    }

    #[test]
    fn ignores_thinking_and_other_lines() {
        for line in [
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm","estimated_tokens":50}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"CAQ"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"system","subtype":"init","cwd":"/private/tmp","tools":[],"model":"claude-haiku-5-5"}"#,
            r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart:startup"}"#,
            r#"{"type":"assistant","message":{"model":"claude-haiku-5-5","content":[{"type":"text","text":"391"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#,
        ] {
            assert_eq!(parse(line), Message::Other, "{line}");
        }
    }

    #[test]
    fn parses_successful_result() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":2000,"result":"391","stop_reason":"end_turn","session_id":"s"}"#;
        assert_eq!(parse(line), Message::TurnEnd(Ok(Some("391".to_string()))));
    }

    #[test]
    fn parses_error_results() {
        let not_logged_in = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login","stop_reason":"stop_sequence","terminal_reason":"api_error"}"#;
        assert_eq!(
            parse(not_logged_in),
            Message::TurnEnd(Err("Not logged in · Please run /login".to_string()))
        );
        let interrupted = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"stop_reason":null,"errors":["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=null"]}"#;
        assert_eq!(
            parse(interrupted),
            Message::TurnEnd(Err("Claude Code could not finish the request.".to_string()))
        );
    }

    #[test]
    fn parses_control_responses_and_requests() {
        assert_eq!(
            parse(
                r#"{"type":"control_response","response":{"subtype":"success","request_id":"itch-3"}}"#
            ),
            Message::ControlResponse {
                id: "itch-3".to_string(),
                result: Ok(Value::Null)
            }
        );
        assert_eq!(
            parse(
                r#"{"type":"control_response","response":{"subtype":"error","request_id":"itch-2","error":"Model 'nope' not found","error_code":"catalog_unknown"}}"#
            ),
            Message::ControlResponse {
                id: "itch-2".to_string(),
                result: Err("Model 'nope' not found".to_string())
            }
        );
        assert_eq!(
            parse(
                r#"{"type":"control_request","request_id":"cli-1","request":{"subtype":"can_use_tool","tool_name":"Bash"}}"#
            ),
            Message::ControlRequest {
                id: "cli-1".to_string()
            }
        );
    }

    #[test]
    fn parses_the_models_of_the_initialize_response() {
        // Shortened from the answer of Claude Code 2.1.302.
        let response = serde_json::json!({
            "commands": [],
            "models": [
                { "value": "default", "resolvedModel": "claude-opus-5-5", "displayName": "Default (recommended)",
                  "supportsEffort": true, "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"] },
                { "value": "haiku", "resolvedModel": "claude-haiku-5-5", "displayName": "Haiku 5.5",
                  "description": "Fastest for quick answers", "supportsEffort": true,
                  "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"] },
                { "value": "claude-haiku-4-5-20251001", "resolvedModel": "claude-haiku-4-5-20251001",
                  "displayName": "Haiku 4.5", "supportsThinkingOff": true },
                { "displayName": "no value" }
            ]
        });
        assert_eq!(
            parse_models(&response),
            Ok(vec![
                Model {
                    id: "haiku".to_string(),
                    name: "Haiku 5.5".to_string(),
                    group: String::new(),
                    thinking_levels: ["low", "medium", "high", "xhigh", "max"]
                        .map(String::from)
                        .to_vec(),
                },
                Model {
                    id: "claude-haiku-4-5-20251001".to_string(),
                    name: "Haiku 4.5".to_string(),
                    group: String::new(),
                    thinking_levels: Vec::new(),
                },
            ])
        );
        assert_eq!(
            parse_models(&serde_json::json!({ "commands": [] })),
            Err("Claude Code returned no model list.".to_string())
        );
    }

    #[test]
    fn args_turn_off_tools_customizations_and_sessions() {
        let args = build_args();
        let tools = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[tools + 1], "");
        let prompts = args
            .iter()
            .position(|a| a == "--permission-prompts")
            .unwrap();
        assert_eq!(args[prompts + 1], "none");
        for flag in [
            "--safe-mode",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-session-persistence",
            "--include-partial-messages",
        ] {
            assert!(args.iter().any(|a| a == flag), "missing {flag}");
        }
        let system = args.iter().position(|a| a == "--system-prompt").unwrap();
        assert_eq!(args[system + 1], SYSTEM_PROMPT);
        assert!(!args
            .iter()
            .any(|a| a.contains("dangerously") || a == "--model"));
    }

    #[cfg(unix)]
    fn spawn_fake(script: &str) -> (crate::ai::test_support::TempFile, ClaudeProcess) {
        let (file, program) = crate::ai::test_support::fake_tool(script);
        let process = ClaudeProcess::spawn(&program, &[], &get_expanded_path()).unwrap();
        (file, process)
    }

    #[cfg(unix)]
    #[test]
    fn process_streams_a_scripted_run() {
        let (_script, mut process) = spawn_fake(
            r#"read -r line
printf '%s\n' '{"type":"system","subtype":"init","tools":[]}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}}'
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"Hello!"}'
sleep 30"#,
        );
        let (sink, rx) = channel_sink();
        process.prompt(&request(None), sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        assert_eq!(streamed, "Hello");
        assert_eq!(
            end,
            AiEvent::Done {
                text: "Hello!".to_string(),
                aborted: false
            }
        );
        assert!(process.is_alive());
    }

    /// A fake Claude Code that logs each stdin line. It answers control
    /// requests with success (an error for the model `nope`), interrupts
    /// with an interrupted turn, prompts with `LONG` with one delta and no
    /// end, and other prompts with a short run.
    #[cfg(unix)]
    fn logging_fake(log: &crate::ai::test_support::TempFile) -> String {
        format!(
            r#"while read -r line; do
printf '%s\n' "$line" >> '{log}'
id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
case "$line" in
*'"nope"'*)
printf '{{"type":"control_response","response":{{"subtype":"error","request_id":"%s","error":"Model '"'"'nope'"'"' not found"}}}}\n' "$id";;
*'"interrupt"'*)
printf '{{"type":"control_response","response":{{"subtype":"success","request_id":"%s","response":{{}}}}}}\n' "$id"
printf '%s\n' '{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"[Request interrupted by user]"}}]}}}}'
printf '%s\n' '{{"type":"result","subtype":"error_during_execution","is_error":true,"stop_reason":null}}';;
*'"control_request"'*)
printf '{{"type":"control_response","response":{{"subtype":"success","request_id":"%s"}}}}\n' "$id";;
*LONG*)
printf '%s\n' '{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"part"}}}}}}';;
*'"type":"user"'*)
printf '%s\n' '{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"ok"}}}}}}'
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"ok"}}';;
esac
done"#,
            log = log.path.display()
        )
    }

    /// The logged stdin lines as `subtype[:detail]` for control requests and
    /// `user:<text>` for prompts.
    fn read_log(log: &crate::ai::test_support::TempFile) -> Vec<String> {
        log.read()
            .lines()
            .map(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                match value["type"].as_str().unwrap() {
                    "control_request" => {
                        let request = &value["request"];
                        let subtype = request["subtype"].as_str().unwrap();
                        match (
                            request["model"].as_str(),
                            request["settings"]["effortLevel"].as_str(),
                        ) {
                            (Some(model), _) => format!("{subtype}:{model}"),
                            (_, Some(level)) => format!("{subtype}:{level}"),
                            _ => subtype.to_string(),
                        }
                    }
                    "user" => format!("user:{}", value["message"]["content"].as_str().unwrap()),
                    other => other.to_string(),
                }
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn process_switches_model_and_effort_only_when_they_change() {
        let log = crate::ai::test_support::TempFile::new("claude-log");
        let (_script, mut process) = spawn_fake(&logging_fake(&log));
        for (model, thinking) in [
            (Some("haiku"), Some("low")),
            (Some("haiku"), Some("low")),
            (None, None),
            (Some("sonnet"), Some("high")),
        ] {
            let (sink, rx) = channel_sink();
            let req = PromptRequest {
                model: model.map(str::to_string),
                thinking: thinking.map(str::to_string),
                instruction: "Shorter".to_string(),
                ..request(None)
            };
            process.prompt(&req, sink).unwrap();
            assert_eq!(
                collect_run(&rx).1,
                AiEvent::Done {
                    text: "ok".to_string(),
                    aborted: false
                }
            );
        }
        let sent = read_log(&log);
        assert_eq!(sent[0..2], ["set_model:haiku", "apply_flag_settings:low"]);
        // The first prompt carries the document, follow-ups only the instruction.
        assert!(sent[2].starts_with("user:<document>"), "{}", sent[2]);
        assert_eq!(
            sent[3..],
            [
                "user:Shorter",
                "user:Shorter",
                "set_model:sonnet",
                "apply_flag_settings:high",
                "user:Shorter"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_ends_run_with_error_when_the_model_is_unknown() {
        let log = crate::ai::test_support::TempFile::new("claude-badmodel");
        let (_script, mut process) = spawn_fake(&logging_fake(&log));
        for _ in 0..2 {
            let (sink, rx) = channel_sink();
            let req = PromptRequest {
                model: Some("nope".to_string()),
                ..request(None)
            };
            process.prompt(&req, sink).unwrap();
            assert_eq!(
                collect_run(&rx).1,
                AiEvent::Error {
                    message: "Claude Code could not use the model nope: Model 'nope' not found"
                        .to_string()
                }
            );
        }
        // No prompt runs with another model, and the next prompt tries again.
        assert_eq!(read_log(&log), ["set_model:nope", "set_model:nope"]);
        assert!(process.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn abort_ends_the_run_with_the_partial_text_and_allows_a_follow_up() {
        let log = crate::ai::test_support::TempFile::new("claude-abort");
        let (_script, mut process) = spawn_fake(&logging_fake(&log));
        // No active run: no interrupt.
        process.abort().unwrap();
        let (sink, rx) = channel_sink();
        let long = PromptRequest {
            instruction: "LONG".to_string(),
            ..request(None)
        };
        process.prompt(&long, sink).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            AiEvent::Delta {
                text: "part".to_string()
            }
        );
        process.abort().unwrap();
        assert_eq!(
            collect_run(&rx).1,
            AiEvent::Done {
                text: "part".to_string(),
                aborted: true
            }
        );
        let (sink, rx) = channel_sink();
        process.prompt(&request(None), sink).unwrap();
        assert_eq!(
            collect_run(&rx).1,
            AiEvent::Done {
                text: "ok".to_string(),
                aborted: false
            }
        );
        let sent = read_log(&log);
        assert_eq!(sent[1], "interrupt");
        assert_eq!(sent[2], "user:Make it shorter");
    }

    #[cfg(unix)]
    #[test]
    fn process_reports_the_error_of_a_turn() {
        let (_script, mut process) = spawn_fake(
            r#"read -r line
printf '%s\n' '{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"Not logged in · Please run /login"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}'
sleep 30"#,
        );
        let (sink, rx) = channel_sink();
        process.prompt(&request(None), sink).unwrap();
        assert_eq!(
            collect_run(&rx),
            (
                String::new(),
                AiEvent::Error {
                    message: "Not logged in · Please run /login".to_string()
                }
            )
        );
        assert!(process.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn process_refuses_requests_from_claude_code() {
        let log = crate::ai::test_support::TempFile::new("claude-refuse");
        let (_script, _process) = spawn_fake(&format!(
            r#"printf '%s\n' '{{"type":"control_request","request_id":"cli-1","request":{{"subtype":"can_use_tool","tool_name":"Bash","input":{{}}}}}}'
read -r line
printf '%s\n' "$line" > '{}'
sleep 30"#,
            log.path.display()
        ));
        let started = std::time::Instant::now();
        while log.read().is_empty() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let reply: Value = serde_json::from_str(&log.read()).unwrap();
        assert_eq!(reply["type"], "control_response");
        assert_eq!(reply["response"]["subtype"], "error");
        assert_eq!(reply["response"]["request_id"], "cli-1");
    }

    #[cfg(unix)]
    #[test]
    fn process_exit_during_run_reports_stderr() {
        let (_script, mut process) =
            spawn_fake("read -r line\necho 'Error: bad config' >&2\nexit 1");
        let (sink, rx) = channel_sink();
        process.prompt(&request(None), sink).unwrap();
        assert_eq!(
            collect_run(&rx).1,
            AiEvent::Error {
                message: "Claude Code stopped unexpectedly: Error: bad config".to_string()
            }
        );
        assert!(!process.is_alive());
        let (sink, _rx) = channel_sink();
        assert_eq!(
            process.prompt(&request(None), sink),
            Err("Claude Code stopped unexpectedly: Error: bad config".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_process_kills_it_without_an_event() {
        let (_script, mut process) = spawn_fake("while read -r line; do :; done");
        let (sink, rx) = channel_sink();
        process.prompt(&request(None), sink).unwrap();
        let pid = process.process.id();
        drop(process);
        assert!(!crate::ai::test_support::is_running(pid));
        std::thread::sleep(Duration::from_millis(200));
        assert!(rx.try_recv().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn list_models_reads_the_initialize_response() {
        let (_script, program) = crate::ai::test_support::fake_tool(
            r#"read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
case "$line" in *'"initialize"'*)
printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"models":[{"value":"default","displayName":"Default"},{"value":"opus","displayName":"Opus 5.5","supportedEffortLevels":["low","max"]}]}}}\n' "$id";;
esac
sleep 30"#,
        );
        let started = std::time::Instant::now();
        let list = list_models_with(&program, &[], &get_expanded_path()).unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(
            list,
            ModelList {
                models: vec![Model {
                    id: "opus".to_string(),
                    name: "Opus 5.5".to_string(),
                    group: String::new(),
                    thinking_levels: vec!["low".to_string(), "max".to_string()],
                }],
                default_model: None,
                default_thinking: None,
            }
        );
    }

    /// A Claude Code that does not know our flags, like an old version.
    #[cfg(unix)]
    const OLD_CLAUDE: &str = "echo \"error: unknown option '--safe-mode'\" >&2; exit 1";

    #[cfg(unix)]
    #[test]
    fn old_claude_code_asks_for_an_update_in_a_run() {
        let (_script, mut process) = spawn_fake(&format!("read -r line\n{OLD_CLAUDE}"));
        let (sink, rx) = channel_sink();
        process.prompt(&request(None), sink).unwrap();
        assert_eq!(
            collect_run(&rx).1,
            AiEvent::Error {
                message: TOO_OLD.to_string()
            }
        );
        let (sink, _rx) = channel_sink();
        assert_eq!(
            process.prompt(&request(None), sink),
            Err(TOO_OLD.to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn old_claude_code_asks_for_an_update_in_the_model_list() {
        let (_script, program) = crate::ai::test_support::fake_tool(OLD_CLAUDE);
        assert_eq!(
            list_models_with(&program, &[], &get_expanded_path()),
            Err(TOO_OLD.to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_models_reports_errors_and_exit() {
        let (_script, program) = crate::ai::test_support::fake_tool(
            r#"read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
printf '{"type":"control_response","response":{"subtype":"error","request_id":"%s","error":"boom"}}\n' "$id"
sleep 30"#,
        );
        assert_eq!(
            list_models_with(&program, &[], &get_expanded_path()),
            Err("Claude Code could not list the models: boom".to_string())
        );
        let (_script, program) =
            crate::ai::test_support::fake_tool("echo 'Error: bad flag' >&2; exit 1");
        assert_eq!(
            list_models_with(&program, &[], &get_expanded_path()),
            Err("Claude Code stopped unexpectedly: Error: bad flag".to_string())
        );
    }

    /// End-to-end check with the real Claude Code, the model `haiku` and the
    /// effort `low`. Set CLAUDE_TEST_PROGRAM to test another `claude`. Run with:
    /// cargo test --lib claude::tests::real_claude -- --ignored --nocapture
    #[test]
    #[ignore = "needs a real Claude Code install and login"]
    fn real_claude_lists_streams_refines_and_aborts() {
        let path = crate::ai::search_path();
        let program = std::env::var("CLAUDE_TEST_PROGRAM").unwrap_or_else(|_| {
            crate::ai::resolve_program(crate::ai::Provider::Claude, None, &path)
                .expect("claude on PATH")
        });
        let list = list_models(&program, &path).unwrap();
        println!("{} models:", list.models.len());
        for model in &list.models {
            println!(
                "  {} ({}) {:?}",
                model.id, model.name, model.thinking_levels
            );
        }
        assert!(list.models.iter().any(|m| m.id == "haiku"));

        let mut process = ClaudeProcess::spawn(&program, &build_args(), &path).unwrap();
        let (sink, rx) = channel_sink();
        let missing = PromptRequest {
            model: Some("nope".to_string()),
            ..request(Some("Cats sleep."))
        };
        process.prompt(&missing, sink).unwrap();
        let (_, end) = collect_run(&rx);
        println!("missing model: {end:?}");
        assert!(matches!(end, AiEvent::Error { .. }));

        let first = PromptRequest {
            instruction: "Continue writing with one short sentence.".to_string(),
            before: "# Cats\n\nCats sleep a lot. ".to_string(),
            after: String::new(),
            selection: None,
            thinking: Some("low".to_string()),
            model: Some("haiku".to_string()),
            read_only: false,
        };
        let (sink, rx) = channel_sink();
        process.prompt(&first, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("first: {streamed:?} / {end:?}");
        assert!(!streamed.is_empty());
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

        let refine = PromptRequest {
            instruction: "Now write it in German.".to_string(),
            ..first.clone()
        };
        let (sink, rx) = channel_sink();
        process.prompt(&refine, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("refine: {streamed:?} / {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

        let long = PromptRequest {
            instruction: "Write a 600 word essay about cats.".to_string(),
            ..first.clone()
        };
        let (sink, rx) = channel_sink();
        process.prompt(&long, sink).unwrap();
        let first_delta = rx.recv_timeout(Duration::from_secs(120)).expect("no delta");
        assert!(matches!(first_delta, AiEvent::Delta { .. }));
        process.abort().unwrap();
        let (_, end) = collect_run(&rx);
        println!("abort: {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: true, .. }));

        let recall = PromptRequest {
            instruction: "Which language did you use two answers ago? One word.".to_string(),
            ..first
        };
        let (sink, rx) = channel_sink();
        process.prompt(&recall, sink).unwrap();
        let (_, end) = collect_run(&rx);
        println!("after abort: {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));
        assert!(process.is_alive());
    }
}

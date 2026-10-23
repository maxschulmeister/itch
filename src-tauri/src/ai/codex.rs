//! The Codex backend: one `codex app-server` child process per popover
//! conversation. Codex speaks JSON-RPC 2.0 on stdin and stdout. Each
//! conversation is one ephemeral, read-only thread without tools.

use super::rpc::{Rpc, RpcHandler};
use super::{
    build_prompt, Conversation, Model, ModelList, PromptRequest, Run, Sink, SYSTEM_PROMPT,
};
use crate::no_window_cmd;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TOOL: &str = "Codex";
/// Time Codex has to answer one request, for example `thread/start`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Maximum number of `model/list` pages.
const MAX_MODEL_PAGES: usize = 20;
/// Codex features that give the model tools. The thread turns them off.
/// Codex ignores names that it does not know. Codex 0.163.0 keeps the
/// sub-agent tools; sub-agents get the same read-only sandbox.
const TOOL_FEATURES: [&str; 11] = [
    "shell_tool",
    "unified_exec",
    "apps",
    "plugins",
    "tool_suggest",
    "skill_search",
    "goals",
    "image_generation",
    "view_image",
    "computer_use",
    "browser_use",
];

/// The Codex notifications that matter for one run.
#[derive(Debug, PartialEq)]
enum Event {
    /// An answer message started. Commentary between tool calls is not an
    /// answer.
    AnswerStarted(String),
    Delta {
        item: String,
        text: String,
    },
    AnswerCompleted(String),
    TurnCompleted(TurnEnd),
    Other,
}

#[derive(Debug, PartialEq)]
enum TurnEnd {
    Completed,
    Interrupted,
    Failed(String),
}

/// `item` if it is an answer message: an agent message that is not
/// commentary.
fn answer_message(item: &Value) -> Option<&Value> {
    (item["type"] == "agentMessage" && item["phase"] != "commentary").then_some(item)
}

fn parse_event(method: &str, params: &Value) -> Event {
    match method {
        "item/started" => answer_message(&params["item"])
            .and_then(|item| item["id"].as_str())
            .map_or(Event::Other, |id| Event::AnswerStarted(id.to_string())),
        "item/agentMessage/delta" => match (params["itemId"].as_str(), params["delta"].as_str()) {
            (Some(item), Some(text)) => Event::Delta {
                item: item.to_string(),
                text: text.to_string(),
            },
            _ => Event::Other,
        },
        "item/completed" => answer_message(&params["item"])
            .and_then(|item| item["text"].as_str())
            .map_or(Event::Other, |text| {
                Event::AnswerCompleted(text.to_string())
            }),
        "turn/completed" => {
            let turn = &params["turn"];
            Event::TurnCompleted(match turn["status"].as_str() {
                Some("interrupted") => TurnEnd::Interrupted,
                Some("failed") => TurnEnd::Failed(
                    turn["error"]["message"]
                        .as_str()
                        .map_or_else(|| "Codex could not answer.".to_string(), error_text),
                ),
                _ => TurnEnd::Completed,
            })
        }
        _ => Event::Other,
    }
}

/// The message of a Codex error. Codex puts the JSON error body of the
/// model API into the message; this returns the readable part.
fn error_text(message: &str) -> String {
    serde_json::from_str::<Value>(message)
        .ok()
        .and_then(|body| body["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| message.to_string())
}

/// The answer to a request from Codex: deny every approval.
fn answer_request(method: &str) -> Result<Value, String> {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            Ok(json!({ "decision": "decline" }))
        }
        // Grant no permissions.
        "item/permissions/requestApproval" => Ok(json!({ "permissions": {} })),
        "mcpServer/elicitation/request" => Ok(json!({ "action": "decline" })),
        _ => Err(format!("Itch does not support {method}.")),
    }
}

/// The `thread/start` parameters. `config` is the `config/read` result.
/// The thread keeps no history, runs read-only, never asks for approval,
/// and has no tools: the tool features, web search and the MCP servers of
/// the user config are off. It is a writing helper, not a coding session,
/// so the user hooks and the `notify` program do not run.
fn thread_params(config: &Value) -> Value {
    let features: serde_json::Map<String, Value> = TOOL_FEATURES
        .iter()
        .chain(["hooks"].iter())
        .map(|feature| (feature.to_string(), Value::Bool(false)))
        .collect();
    let mcp_servers: serde_json::Map<String, Value> = config["mcp_servers"]
        .as_object()
        .into_iter()
        .flat_map(|servers| servers.keys())
        .map(|name| (name.clone(), json!({ "enabled": false })))
        .collect();
    json!({
        "cwd": std::env::temp_dir(),
        "sandbox": "read-only",
        "approvalPolicy": "never",
        "ephemeral": true,
        // Added to the Codex base instructions, which stay in place.
        "developerInstructions": SYSTEM_PROMPT,
        "config": {
            "features": features,
            "web_search": "disabled",
            "mcp_servers": mcp_servers,
            "notify": [],
        },
    })
}

/// The `turn/start` parameters. Codex keeps `model` and `effort` for the
/// next turns.
fn turn_params(thread_id: &str, text: &str, model: Option<&str>, effort: Option<&str>) -> Value {
    let mut params = json!({ "threadId": thread_id, "input": [{ "type": "text", "text": text }] });
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    if let Some(effort) = effort {
        params["effort"] = json!(effort);
    }
    params
}

/// Reads a `model/list` entry. `None` for hidden models.
fn parse_model(entry: &Value) -> Option<Model> {
    if entry["hidden"] == true {
        return None;
    }
    let id = entry["model"].as_str()?;
    Some(Model {
        id: id.to_string(),
        name: entry["displayName"].as_str().unwrap_or(id).to_string(),
        group: String::new(),
        thinking_levels: entry["supportedReasoningEfforts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|effort| effort["reasoningEffort"].as_str())
            .map(str::to_string)
            .collect(),
    })
}

/// The model list from the `model/list` entries. `config` is the
/// `config/read` result. The defaults come from the user config, else from
/// the model that Codex marks as default.
fn model_list(config: &Value, entries: &[Value]) -> ModelList {
    let default_model = config["model"].as_str().map(str::to_string).or_else(|| {
        entries
            .iter()
            .find(|entry| entry["isDefault"] == true)
            .and_then(|entry| entry["model"].as_str())
            .map(str::to_string)
    });
    let default_thinking = config["model_reasoning_effort"]
        .as_str()
        .or_else(|| {
            entries
                .iter()
                .find(|entry| entry["model"].as_str() == default_model.as_deref())
                .and_then(|entry| entry["defaultReasoningEffort"].as_str())
        })
        .map(str::to_string);
    ModelList {
        models: entries.iter().filter_map(parse_model).collect(),
        default_model,
        default_thinking,
    }
}

/// State shared between the conversation and the reader thread.
#[derive(Default)]
struct RunState {
    /// The thread of the conversation. Events of other threads (sub-agents
    /// that the model starts) are not part of the answer.
    thread_id: String,
    run: Run,
    /// Ids of the answer messages of the current run.
    answer_items: Vec<String>,
    /// Text of the last completed answer message of the current run.
    answer: Option<String>,
    /// Set when Codex exited. A message for the user.
    exited: Option<String>,
}

#[derive(Default)]
struct Handler(Arc<Mutex<RunState>>);

impl RpcHandler for Handler {
    fn notification(&mut self, method: &str, params: &Value) {
        let Ok(mut state) = self.0.lock() else { return };
        if !state.run.is_active() || params["threadId"] != state.thread_id.as_str() {
            return;
        }
        match parse_event(method, params) {
            Event::AnswerStarted(item) => state.answer_items.push(item),
            Event::Delta { item, text } if state.answer_items.contains(&item) => {
                state.run.delta(&text)
            }
            Event::AnswerCompleted(text) => state.answer = Some(text),
            Event::TurnCompleted(end) => {
                let answer = state.answer.take();
                match end {
                    TurnEnd::Completed => state.run.finish(answer, false),
                    TurnEnd::Interrupted => state.run.finish(answer, true),
                    TurnEnd::Failed(message) => state.run.fail(message),
                }
            }
            Event::Delta { .. } | Event::Other => {}
        }
    }

    fn request(&mut self, method: &str, _params: &Value) -> Result<Value, String> {
        answer_request(method)
    }

    fn exited(&mut self, message: &str) {
        if let Ok(mut state) = self.0.lock() {
            state.exited = Some(message.to_string());
            state.run.fail(message.to_string());
        }
    }
}

/// Starts `codex app-server`, does the handshake and reads the effective
/// Codex config.
fn connect(program: &str, path: &str, handler: Handler) -> Result<(Rpc, Value), String> {
    let mut command = no_window_cmd(program);
    command.arg("app-server").env("PATH", path);
    let mut rpc = Rpc::spawn(TOOL, command, handler)?;
    let client = json!({ "name": "itch", "title": "Itch", "version": env!("CARGO_PKG_VERSION") });
    rpc.call(
        "initialize",
        json!({ "clientInfo": client }),
        REQUEST_TIMEOUT,
    )?;
    rpc.notify("initialized", json!({}))?;
    let cwd = std::env::temp_dir();
    let mut config = rpc.call("config/read", json!({ "cwd": cwd }), REQUEST_TIMEOUT)?;
    Ok((rpc, config["config"].take()))
}

/// One Codex thread. Dropping it stops Codex.
pub struct CodexConversation {
    rpc: Rpc,
    thread_id: String,
    state: Arc<Mutex<RunState>>,
    /// The turn of the current or last run, for `turn/interrupt`.
    turn_id: Option<String>,
    /// Last model sent to Codex. `None` while Codex uses its default.
    model: Option<String>,
    /// Last reasoning effort sent to Codex. `None` while Codex uses its default.
    effort: Option<String>,
    /// True after Codex accepted the first prompt, which carries the context.
    context_sent: bool,
}

/// Starts Codex and a new thread.
pub fn start(program: &str, path: &str) -> Result<Box<dyn Conversation>, String> {
    Ok(Box::new(start_thread(program, path)?))
}

fn start_thread(program: &str, path: &str) -> Result<CodexConversation, String> {
    let state = Arc::new(Mutex::new(RunState::default()));
    let (mut rpc, config) = connect(program, path, Handler(Arc::clone(&state)))?;
    let result = rpc
        .call("thread/start", thread_params(&config), REQUEST_TIMEOUT)
        .map_err(|e| format!("Codex could not start a conversation: {e}"))?;
    let thread_id = result["thread"]["id"]
        .as_str()
        .ok_or("Codex did not start a conversation.")?
        .to_string();
    state
        .lock()
        .map_err(|_| "Codex session is broken.")?
        .thread_id
        .clone_from(&thread_id);
    Ok(CodexConversation {
        rpc,
        thread_id,
        state,
        turn_id: None,
        model: None,
        effort: None,
        context_sent: false,
    })
}

/// Starts a short-lived Codex and asks it for its models.
pub fn list_models(program: &str, path: &str) -> Result<ModelList, String> {
    let (mut rpc, config) = connect(program, path, Handler::default())?;
    let mut entries = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_MODEL_PAGES {
        let mut page = rpc
            .call("model/list", json!({ "cursor": cursor }), REQUEST_TIMEOUT)
            .map_err(|e| format!("Codex could not list the models: {e}"))?;
        match page["data"].take() {
            Value::Array(data) => entries.extend(data),
            _ => return Err("Codex returned no model list.".to_string()),
        }
        cursor = page["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(model_list(&config, &entries))
}

impl CodexConversation {
    fn state(&self) -> Result<std::sync::MutexGuard<'_, RunState>, String> {
        self.state
            .lock()
            .map_err(|_| "Codex session is broken.".to_string())
    }
}

impl Conversation for CodexConversation {
    fn prompt(&mut self, request: &PromptRequest, sink: Sink) -> Result<(), String> {
        {
            let mut state = self.state()?;
            if let Some(message) = &state.exited {
                return Err(message.clone());
            }
            state.run.start(TOOL, sink)?;
            state.answer_items.clear();
            state.answer = None;
        }
        let model = request
            .model()
            .filter(|model| self.model.as_deref() != Some(*model));
        let effort = request
            .thinking()
            .filter(|effort| self.effort.as_deref() != Some(*effort));
        let text = build_prompt(request, !self.context_sent);
        let params = turn_params(&self.thread_id, &text, model, effort);
        // Codex answers at once and then streams the turn as notifications.
        match self.rpc.call("turn/start", params, REQUEST_TIMEOUT) {
            Ok(result) => {
                self.turn_id = result["turn"]["id"].as_str().map(str::to_string);
                self.context_sent = true;
                if let Some(model) = model {
                    self.model = Some(model.to_string());
                }
                if let Some(effort) = effort {
                    self.effort = Some(effort.to_string());
                }
            }
            // After an exit the run already ended with the exit message.
            Err(e) => self
                .state()?
                .run
                .fail(format!("Codex could not start the answer: {e}")),
        }
        Ok(())
    }

    /// Asks Codex to interrupt the turn. Codex then completes the turn as
    /// interrupted, which sends `Done { aborted: true }`.
    fn abort(&mut self) -> Result<(), String> {
        if !self.state()?.run.is_active() {
            return Ok(());
        }
        let Some(turn_id) = &self.turn_id else {
            return Ok(());
        };
        let params = json!({ "threadId": self.thread_id, "turnId": turn_id });
        // The turn end comes as `turn/completed`; the answer is not needed.
        self.rpc
            .call_async("turn/interrupt", params, |_| {})
            .map_err(|e| format!("Failed to stop Codex: {e}"))
    }

    fn is_alive(&self) -> bool {
        self.rpc.is_alive()
    }
}

impl Drop for CodexConversation {
    fn drop(&mut self) {
        // The caller closed the conversation: no event for the current run.
        if let Ok(mut state) = self.state.lock() {
            state.run = Run::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::test_support::{channel_sink, collect_run, request};
    use crate::ai::AiEvent;

    fn event(line: &str) -> Event {
        let value: Value = serde_json::from_str(line).unwrap();
        parse_event(value["method"].as_str().unwrap(), &value["params"])
    }

    // Lines from codex-cli 0.163.0 (`codex app-server`), shortened.
    const FINAL_STARTED: &str = r#"{"method":"item/started","params":{"item":{"type":"agentMessage","id":"msg_1","text":"","phase":"final_answer","memoryCitation":null,"delivery":null,"questions":null},"threadId":"t1","turnId":"u1","startedAtMs":1}}"#;
    const COMMENTARY_STARTED: &str = r#"{"method":"item/started","params":{"item":{"type":"agentMessage","id":"msg_0","text":"","phase":"commentary","memoryCitation":null,"delivery":null,"questions":null},"threadId":"t1","turnId":"u1","startedAtMs":1}}"#;
    const DELTA: &str = r#"{"method":"item/agentMessage/delta","params":{"threadId":"t1","turnId":"u1","itemId":"msg_1","delta":"Hi"},"emittedAtMs":1}"#;
    const FINAL_COMPLETED: &str = r#"{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"msg_1","text":"Hi!","phase":"final_answer","memoryCitation":null,"delivery":null,"questions":null},"threadId":"t1","turnId":"u1","completedAtMs":1}}"#;

    #[test]
    fn parses_answer_messages_and_deltas() {
        assert_eq!(
            event(FINAL_STARTED),
            Event::AnswerStarted("msg_1".to_string())
        );
        assert_eq!(
            event(DELTA),
            Event::Delta {
                item: "msg_1".to_string(),
                text: "Hi".to_string()
            }
        );
        assert_eq!(
            event(FINAL_COMPLETED),
            Event::AnswerCompleted("Hi!".to_string())
        );
    }

    #[test]
    fn commentary_and_other_items_are_not_answers() {
        assert_eq!(event(COMMENTARY_STARTED), Event::Other);
        let commentary_done = COMMENTARY_STARTED.replace("item/started", "item/completed");
        assert_eq!(event(&commentary_done), Event::Other);
        let reasoning = r#"{"method":"item/started","params":{"item":{"type":"reasoning","id":"rs_1","summary":[],"content":[]},"threadId":"t1","turnId":"u1"}}"#;
        assert_eq!(event(reasoning), Event::Other);
        let usage = r#"{"method":"thread/tokenUsage/updated","params":{"threadId":"t1"}}"#;
        assert_eq!(event(usage), Event::Other);
    }

    #[test]
    fn message_without_phase_is_an_answer() {
        // Codex: "callers must treat `None` as phase unknown".
        let line = FINAL_STARTED.replace(r#""phase":"final_answer""#, r#""phase":null"#);
        assert_eq!(event(&line), Event::AnswerStarted("msg_1".to_string()));
    }

    fn turn_completed(status: &str, error: &str) -> String {
        format!(
            r#"{{"method":"turn/completed","params":{{"threadId":"t1","turn":{{"id":"u1","items":[],"itemsView":"notLoaded","status":"{status}","error":{error},"startedAt":1,"completedAt":2,"durationMs":4400}}}}}}"#
        )
    }

    #[test]
    fn parses_turn_completed_statuses() {
        assert_eq!(
            event(&turn_completed("completed", "null")),
            Event::TurnCompleted(TurnEnd::Completed)
        );
        assert_eq!(
            event(&turn_completed("interrupted", "null")),
            Event::TurnCompleted(TurnEnd::Interrupted)
        );
        let api_error = r#"{"message":"{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'nope-model-x' model is not supported when using Codex with a ChatGPT account.\"}}","codexErrorInfo":"other","additionalDetails":null}"#;
        assert_eq!(
            event(&turn_completed("failed", api_error)),
            Event::TurnCompleted(TurnEnd::Failed(
                "The 'nope-model-x' model is not supported when using Codex with a ChatGPT account."
                    .to_string()
            ))
        );
        assert_eq!(
            event(&turn_completed("failed", "null")),
            Event::TurnCompleted(TurnEnd::Failed("Codex could not answer.".to_string()))
        );
    }

    #[test]
    fn error_text_keeps_plain_messages() {
        assert_eq!(error_text("stream disconnected"), "stream disconnected");
        assert_eq!(error_text("{\"x\":1}"), "{\"x\":1}");
    }

    #[test]
    fn approvals_are_declined() {
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
        ] {
            assert_eq!(answer_request(method), Ok(json!({ "decision": "decline" })));
        }
        assert_eq!(
            answer_request("item/permissions/requestApproval"),
            Ok(json!({ "permissions": {} }))
        );
        assert_eq!(
            answer_request("mcpServer/elicitation/request"),
            Ok(json!({ "action": "decline" }))
        );
        assert_eq!(
            answer_request("item/tool/requestUserInput"),
            Err("Itch does not support item/tool/requestUserInput.".to_string())
        );
    }

    #[test]
    fn thread_is_ephemeral_read_only_and_without_tools() {
        let config = json!({ "model": "gpt-6.1-sol", "mcp_servers": { "node_repl": { "command": "node" }, "docs": {} } });
        let params = thread_params(&config);
        assert_eq!(params["sandbox"], "read-only");
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["ephemeral"], true);
        assert_eq!(params["developerInstructions"], SYSTEM_PROMPT);
        assert!(params.get("baseInstructions").is_none());
        assert_eq!(
            params["cwd"].as_str(),
            Some(std::env::temp_dir().to_string_lossy().as_ref())
        );
        let overrides = &params["config"];
        assert_eq!(overrides["features"]["shell_tool"], false);
        assert_eq!(overrides["features"]["plugins"], false);
        assert_eq!(overrides["web_search"], "disabled");
        // No user hooks and no notify program for a writing helper.
        assert_eq!(overrides["features"]["hooks"], false);
        assert_eq!(overrides["notify"], json!([]));
        assert_eq!(
            overrides["mcp_servers"],
            json!({ "node_repl": { "enabled": false }, "docs": { "enabled": false } })
        );
        assert_eq!(
            thread_params(&json!({}))["config"]["mcp_servers"],
            json!({})
        );
    }

    #[test]
    fn turn_sends_model_and_effort_only_when_given() {
        let params = turn_params("t1", "Hi", Some("gpt-5.6-luna"), Some("low"));
        assert_eq!(
            params,
            json!({
                "threadId": "t1",
                "input": [{ "type": "text", "text": "Hi" }],
                "model": "gpt-5.6-luna",
                "effort": "low"
            })
        );
        let params = turn_params("t1", "Hi", None, None);
        assert!(params.get("model").is_none() && params.get("effort").is_none());
    }

    fn entry(model: &str, default: bool, hidden: bool, effort: &str) -> Value {
        json!({
            "id": model, "model": model, "displayName": model.to_uppercase(),
            "description": "", "hidden": hidden, "isDefault": default,
            "defaultReasoningEffort": effort,
            "supportedReasoningEfforts": [
                { "reasoningEffort": "low", "description": "" },
                { "reasoningEffort": "high", "description": "" }
            ]
        })
    }

    #[test]
    fn model_list_skips_hidden_models_and_uses_the_config_defaults() {
        let entries = [
            entry("gpt-6.1-sol", true, false, "low"),
            entry("gpt-secret", false, true, "low"),
            entry("gpt-5.6-luna", false, false, "medium"),
        ];
        let config = json!({ "model": "gpt-5.6-luna", "model_reasoning_effort": "high" });
        let list = model_list(&config, &entries);
        assert_eq!(
            list.models,
            [
                Model {
                    id: "gpt-6.1-sol".to_string(),
                    name: "GPT-6.1-SOL".to_string(),
                    group: String::new(),
                    thinking_levels: vec!["low".to_string(), "high".to_string()],
                },
                Model {
                    id: "gpt-5.6-luna".to_string(),
                    name: "GPT-5.6-LUNA".to_string(),
                    group: String::new(),
                    thinking_levels: vec!["low".to_string(), "high".to_string()],
                },
            ]
        );
        assert_eq!(list.default_model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(list.default_thinking.as_deref(), Some("high"));
    }

    #[test]
    fn model_list_without_config_uses_the_catalog_default() {
        let entries = [
            entry("gpt-6.1-sol", false, false, "low"),
            entry("gpt-5.6-luna", true, false, "medium"),
        ];
        let config = json!({ "model": null, "model_reasoning_effort": null });
        let list = model_list(&config, &entries);
        assert_eq!(list.default_model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(list.default_thinking.as_deref(), Some("medium"));
        assert_eq!(model_list(&json!({}), &[]), ModelList::default());
    }

    #[cfg(unix)]
    mod process {
        use super::*;
        use crate::ai::test_support::{fake_tool, is_running, TempFile};
        use crate::get_expanded_path;

        /// A fake `codex app-server` that logs each line it reads, answers
        /// the handshake, and runs `turn` (shell code) for each `turn/start`
        /// after it answers with turn `u$n` (`$n` counts the turns).
        /// `turn/interrupt` ends the turn as interrupted.
        fn fake_codex(log: &TempFile, turn: &str) -> (TempFile, String) {
            fake_tool(&format!(
                r#"[ "$1" = app-server ] || exit 2
n=0
send() {{ printf '%s\n' "$1"; }}
reply() {{ printf '{{"jsonrpc":"2.0","id":%s,"result":%s}}\n' "$id" "$1"; }}
note() {{ printf '{{"jsonrpc":"2.0","method":"%s","params":%s}}\n' "$1" "$2"; }}
while read line; do
printf '%s\n' "$line" >> '{log}'
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
case "$line" in
*'"method":"initialize"'*) reply '{{"userAgent":"fake"}}';;
*'"method":"config/read"'*) reply '{{"config":{{"model":null,"mcp_servers":{{"node_repl":{{"command":"x"}}}}}}}}';;
*'"method":"thread/start"'*) reply '{{"thread":{{"id":"t1"}},"model":"gpt-6.1-sol"}}';;
*'"method":"turn/start"'*) n=$((n+1)); reply "{{\"turn\":{{\"id\":\"u$n\",\"items\":[],\"status\":\"inProgress\"}}}}"
{turn}
;;
*'"method":"turn/interrupt"'*) reply '{{}}'; note turn/completed '{{"threadId":"t1","turn":{{"id":"u1","items":[],"status":"interrupted","error":null}}}}';;
esac
done"#,
                log = log.path.display()
            ))
        }

        const ANSWER_TURN: &str = r#"note item/started '{"item":{"type":"agentMessage","id":"m'$n'","text":"","phase":"final_answer"},"threadId":"t1","turnId":"u1"}'
note item/agentMessage/delta '{"threadId":"t1","turnId":"u1","itemId":"m'$n'","delta":"ok"}'
note item/completed '{"item":{"type":"agentMessage","id":"m'$n'","text":"ok","phase":"final_answer"},"threadId":"t1","turnId":"u1"}'
note turn/completed '{"threadId":"t1","turn":{"id":"u1","items":[],"status":"completed","error":null}}'"#;

        fn start_fake(log: &TempFile, turn: &str) -> (TempFile, Box<dyn Conversation>) {
            let (file, program) = fake_codex(log, turn);
            let conversation = start(&program, &get_expanded_path()).unwrap();
            (file, conversation)
        }

        /// The logged requests with `method`.
        fn sent(log: &TempFile, method: &str) -> Vec<Value> {
            log.read()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|value| value["method"] == method)
                .collect()
        }

        #[test]
        fn streams_only_the_answer_and_finishes_with_the_final_text() {
            let log = TempFile::new("codex-stream");
            let (_file, mut codex) = start_fake(
                &log,
                r#"note item/started '{"item":{"type":"agentMessage","id":"c1","text":"","phase":"commentary"},"threadId":"t1","turnId":"u1"}'
note item/agentMessage/delta '{"threadId":"t1","turnId":"u1","itemId":"c1","delta":"Let me look."}'
note item/completed '{"item":{"type":"agentMessage","id":"c1","text":"Let me look.","phase":"commentary"},"threadId":"t1","turnId":"u1"}'
note thread/tokenUsage/updated '{"threadId":"t1"}'
note item/started '{"item":{"type":"agentMessage","id":"a1","text":"","phase":"final_answer"},"threadId":"t1","turnId":"u1"}'
note item/agentMessage/delta '{"threadId":"t1","turnId":"u1","itemId":"a1","delta":"Hel"}'
note item/agentMessage/delta '{"threadId":"t1","turnId":"u1","itemId":"a1","delta":"lo"}'
note item/completed '{"item":{"type":"agentMessage","id":"a1","text":"Hello!","phase":"final_answer"},"threadId":"t1","turnId":"u1"}'
note turn/completed '{"threadId":"t1","turn":{"id":"u1","items":[],"status":"completed","error":null}}'"#,
            );
            let (sink, rx) = channel_sink();
            codex.prompt(&request(None), sink).unwrap();
            assert_eq!(
                collect_run(&rx),
                (
                    "Hello".to_string(),
                    AiEvent::Done {
                        text: "Hello!".to_string(),
                        aborted: false
                    }
                )
            );
            let threads = sent(&log, "thread/start");
            assert_eq!(threads.len(), 1);
            assert_eq!(threads[0]["params"]["sandbox"], "read-only");
            assert_eq!(
                threads[0]["params"]["config"]["mcp_servers"],
                json!({ "node_repl": { "enabled": false } })
            );
            assert_eq!(sent(&log, "initialized").len(), 1);
        }

        #[test]
        fn ignores_the_events_of_sub_agent_threads() {
            let log = TempFile::new("codex-sub-agent");
            let (_file, mut codex) = start_fake(
                &log,
                &format!(
                    r#"note item/started '{{"item":{{"type":"agentMessage","id":"s1","text":"","phase":"final_answer"}},"threadId":"t2","turnId":"v1"}}'
note item/agentMessage/delta '{{"threadId":"t2","turnId":"v1","itemId":"s1","delta":"sub"}}'
note item/completed '{{"item":{{"type":"agentMessage","id":"s1","text":"sub","phase":"final_answer"}},"threadId":"t2","turnId":"v1"}}'
note turn/completed '{{"threadId":"t2","turn":{{"id":"v1","items":[],"status":"completed","error":null}}}}'
{ANSWER_TURN}"#
                ),
            );
            let (sink, rx) = channel_sink();
            codex.prompt(&request(None), sink).unwrap();
            assert_eq!(
                collect_run(&rx),
                (
                    "ok".to_string(),
                    AiEvent::Done {
                        text: "ok".to_string(),
                        aborted: false
                    }
                )
            );
        }

        #[test]
        fn follow_ups_continue_the_thread_and_switch_model_and_effort() {
            let log = TempFile::new("codex-follow-up");
            let (_file, mut codex) = start_fake(&log, ANSWER_TURN);
            let prompts = [
                (Some("gpt-5.6-luna"), Some("low")),
                (Some("gpt-5.6-luna"), Some("low")),
                (None, None),
                (Some("gpt-6.1-sol"), Some("high")),
            ];
            for (model, thinking) in prompts {
                let (sink, rx) = channel_sink();
                let req = PromptRequest {
                    model: model.map(str::to_string),
                    thinking: thinking.map(str::to_string),
                    ..request(Some("old"))
                };
                codex.prompt(&req, sink).unwrap();
                assert_eq!(
                    collect_run(&rx).1,
                    AiEvent::Done {
                        text: "ok".to_string(),
                        aborted: false
                    }
                );
            }
            let turns = sent(&log, "turn/start");
            let summary: Vec<String> = turns
                .iter()
                .map(|t| {
                    let p = &t["params"];
                    format!("{} {} {}", p["threadId"], p["model"], p["effort"])
                })
                .collect();
            assert_eq!(
                summary,
                [
                    r#""t1" "gpt-5.6-luna" "low""#,
                    r#""t1" null null"#,
                    r#""t1" null null"#,
                    r#""t1" "gpt-6.1-sol" "high""#,
                ]
            );
            let texts: Vec<&str> = turns
                .iter()
                .map(|t| t["params"]["input"][0]["text"].as_str().unwrap())
                .collect();
            assert!(texts[0].contains("<document>"), "{}", texts[0]);
            assert_eq!(texts[1..], ["Make it shorter"; 3]);
            assert_eq!(sent(&log, "thread/start").len(), 1);
        }

        #[test]
        fn abort_interrupts_the_turn_and_keeps_the_partial_text() {
            let log = TempFile::new("codex-abort");
            let (_file, mut codex) = start_fake(
                &log,
                r#"if [ $n = 1 ]; then
note item/started '{"item":{"type":"agentMessage","id":"a1","text":"","phase":"final_answer"},"threadId":"t1","turnId":"u1"}'
note item/agentMessage/delta '{"threadId":"t1","turnId":"u1","itemId":"a1","delta":"partial"}'
else
note turn/completed '{"threadId":"t1","turn":{"id":"u2","items":[],"status":"completed","error":null}}'
fi"#,
            );
            codex.abort().unwrap();
            let (sink, rx) = channel_sink();
            codex.prompt(&request(None), sink).unwrap();
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(10)).unwrap(),
                AiEvent::Delta {
                    text: "partial".to_string()
                }
            );
            codex.abort().unwrap();
            assert_eq!(
                collect_run(&rx).1,
                AiEvent::Done {
                    text: "partial".to_string(),
                    aborted: true
                }
            );
            let interrupts = sent(&log, "turn/interrupt");
            assert_eq!(interrupts.len(), 1);
            assert_eq!(
                interrupts[0]["params"],
                json!({ "threadId": "t1", "turnId": "u1" })
            );
            // The thread stays usable.
            assert!(codex.is_alive());
            let (sink, rx) = channel_sink();
            codex.prompt(&request(None), sink).unwrap();
            assert!(matches!(
                collect_run(&rx).1,
                AiEvent::Done { aborted: false, .. }
            ));
        }

        #[test]
        fn failed_turn_ends_with_the_codex_error() {
            let log = TempFile::new("codex-failed");
            let (_file, mut codex) = start_fake(
                &log,
                r#"note error '{"error":{"message":"rate limited"},"willRetry":true,"threadId":"t1","turnId":"u1"}'
note turn/completed '{"threadId":"t1","turn":{"id":"u1","items":[],"status":"failed","error":{"message":"Quota exceeded."}}}'"#,
            );
            let (sink, rx) = channel_sink();
            codex.prompt(&request(None), sink).unwrap();
            assert_eq!(
                collect_run(&rx).1,
                AiEvent::Error {
                    message: "Quota exceeded.".to_string()
                }
            );
            assert!(codex.is_alive());
        }

        #[test]
        fn rejected_turn_ends_with_an_error() {
            let (_file, program) = fake_tool(
                r#"while read line; do
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
case "$line" in
*'"method":"turn/start"'*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32600,"message":"bad effort"}}\n' "$id";;
*'"method":"thread/start"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"thread":{"id":"t1"}}}\n' "$id";;
*'"id":'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"config":{}}}\n' "$id";;
esac
done"#,
            );
            let mut codex = start(&program, &get_expanded_path()).unwrap();
            let (sink, rx) = channel_sink();
            codex.prompt(&request(None), sink).unwrap();
            assert_eq!(
                collect_run(&rx).1,
                AiEvent::Error {
                    message: "Codex could not start the answer: bad effort".to_string()
                }
            );
        }

        #[test]
        fn approval_requests_are_declined() {
            let log = TempFile::new("codex-approval");
            let (_file, mut codex) = start_fake(
                &log,
                &format!(
                    r#"send '{{"jsonrpc":"2.0","id":"a1","method":"item/commandExecution/requestApproval","params":{{"threadId":"t1","turnId":"u1","itemId":"x"}}}}'
read answer; printf '%s\n' "$answer" >> '{log}'
{ANSWER_TURN}"#,
                    log = log.path.display()
                ),
            );
            let (sink, rx) = channel_sink();
            codex.prompt(&request(None), sink).unwrap();
            collect_run(&rx);
            assert!(
                log.read()
                    .contains(r#"{"id":"a1","jsonrpc":"2.0","result":{"decision":"decline"}}"#),
                "{}",
                log.read()
            );
        }

        #[test]
        fn exit_during_a_run_ends_it_with_the_stderr_line() {
            let log = TempFile::new("codex-exit");
            let (_file, mut codex) = start_fake(&log, "echo 'Error: boom' >&2; exit 1");
            let (sink, rx) = channel_sink();
            codex.prompt(&request(None), sink).unwrap();
            let expected = "Codex stopped unexpectedly: Error: boom".to_string();
            assert_eq!(
                collect_run(&rx).1,
                AiEvent::Error {
                    message: expected.clone()
                }
            );
            std::thread::sleep(Duration::from_millis(100));
            assert!(!codex.is_alive());
            let (sink, _rx) = channel_sink();
            assert_eq!(codex.prompt(&request(None), sink), Err(expected));
        }

        #[test]
        fn start_reports_an_exit_during_the_handshake() {
            let (_file, program) = fake_tool("echo 'Error: not logged in' >&2; exit 1");
            assert_eq!(
                start(&program, &get_expanded_path()).err(),
                Some("Codex stopped unexpectedly: Error: not logged in".to_string())
            );
        }

        #[test]
        fn dropping_the_conversation_kills_codex_without_an_event() {
            let log = TempFile::new("codex-drop");
            let (file, program) = fake_codex(&log, "");
            let mut codex = CodexConversationTestHandle::start(&program);
            let (sink, rx) = channel_sink();
            codex.0.prompt(&request(None), sink).unwrap();
            let pid = codex.0.rpc.id();
            drop(codex);
            assert!(!is_running(pid));
            assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
            drop(file);
        }

        /// The concrete conversation, for access to the process id.
        struct CodexConversationTestHandle(CodexConversation);

        impl CodexConversationTestHandle {
            fn start(program: &str) -> Self {
                Self(start_thread(program, &get_expanded_path()).unwrap())
            }
        }

        #[test]
        fn list_models_follows_the_cursor_and_reads_the_defaults() {
            let log = TempFile::new("codex-models");
            let (_file, program) = fake_tool(&format!(
                r#"while read line; do
printf '%s\n' "$line" >> '{log}'
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
case "$line" in
*'"cursor":"c2"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"data":[{{"id":"b","model":"b","displayName":"B","hidden":false,"isDefault":true,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}}],"nextCursor":null}}}}\n' "$id";;
*'"method":"model/list"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"data":[{{"id":"a","model":"a","displayName":"A","hidden":false,"isDefault":false,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{{"reasoningEffort":"low"}}]}}],"nextCursor":"c2"}}}}\n' "$id";;
*'"method":"config/read"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"config":{{"model":null}}}}}}\n' "$id";;
*'"id":'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id";;
esac
done"#,
                log = log.path.display()
            ));
            let list = list_models(&program, &get_expanded_path()).unwrap();
            let ids: Vec<&str> = list.models.iter().map(|m| m.id.as_str()).collect();
            assert_eq!(ids, ["a", "b"]);
            assert_eq!(list.models[0].thinking_levels, ["low"]);
            assert_eq!(list.default_model.as_deref(), Some("b"));
            assert_eq!(list.default_thinking.as_deref(), Some("medium"));
            assert_eq!(sent(&log, "model/list").len(), 2);
        }
    }

    /// End-to-end check with the real Codex. Set CODEX_TEST_MODEL to use a
    /// small model. Run with:
    /// cargo test --lib codex::tests::real_codex -- --ignored --nocapture
    #[test]
    #[ignore = "needs a real Codex install and login"]
    fn real_codex_streams_refines_and_aborts() {
        let path = crate::ai::search_path();
        let program = crate::ai::resolve_program(crate::ai::Provider::Codex, None, &path)
            .expect("codex on PATH");
        let list = list_models(&program, &path).unwrap();
        println!(
            "default {:?} / {:?}, {} models:",
            list.default_model,
            list.default_thinking,
            list.models.len()
        );
        for model in &list.models {
            println!(
                "  {} ({}) {:?}",
                model.id, model.name, model.thinking_levels
            );
        }
        let model = std::env::var("CODEX_TEST_MODEL").ok();
        if let Some(model) = &model {
            assert!(
                list.models.iter().any(|m| &m.id == model),
                "{model} not listed"
            );
        }

        let mut codex = start(&program, &path).unwrap();
        let first = PromptRequest {
            instruction: "Continue writing with one short sentence.".to_string(),
            before: "# Cats\n\nCats sleep a lot. ".to_string(),
            after: String::new(),
            selection: None,
            thinking: Some("low".to_string()),
            model,
            read_only: false,
        };
        let (sink, rx) = channel_sink();
        let started = std::time::Instant::now();
        codex.prompt(&first, sink).unwrap();
        println!("prompt returned after {:?}", started.elapsed());
        let (streamed, end) = collect_run(&rx);
        println!("first: {streamed:?} / {end:?}");
        assert!(!streamed.is_empty());
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

        let refine = PromptRequest {
            instruction: "Now write it in German.".to_string(),
            ..first.clone()
        };
        let (sink, rx) = channel_sink();
        codex.prompt(&refine, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("refine: {streamed:?} / {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

        let long = PromptRequest {
            instruction: "Write a 600 word essay about cats.".to_string(),
            ..first
        };
        let (sink, rx) = channel_sink();
        codex.prompt(&long, sink).unwrap();
        let first_delta = rx.recv_timeout(Duration::from_secs(120)).expect("no delta");
        assert!(matches!(first_delta, AiEvent::Delta { .. }));
        codex.abort().unwrap();
        let (_, end) = collect_run(&rx);
        println!("abort: {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: true, .. }));
        assert!(codex.is_alive());

        let (sink, rx) = channel_sink();
        let again = PromptRequest {
            instruction: "What was the topic of the essay? One word.".to_string(),
            ..refine
        };
        codex.prompt(&again, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("after abort: {streamed:?} / {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));
    }
}

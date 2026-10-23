//! The OpenCode backend: one `opencode acp` child process per popover
//! conversation. `opencode acp` speaks the Agent Client Protocol (JSON-RPC
//! 2.0 on stdin and stdout).

use super::process::{exit_message, read_stderr_tail};
use super::rpc::{Rpc, RpcHandler};
use super::{
    build_prompt, Conversation, Model, ModelList, PromptRequest, Run, Sink, SYSTEM_PROMPT,
};
use crate::no_window_cmd;
use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const TOOL: &str = "OpenCode";
/// The OpenCode agent that Itch defines: our system prompt and no tools.
const AGENT: &str = "itch";
/// Time OpenCode has to start and open a session. The first start after an
/// install or update is slow.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Time OpenCode has to answer one command, for example a model change.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// Time `opencode models` has to list the models.
const MODELS_TIMEOUT: Duration = Duration::from_secs(30);

/// OpenCode config that Itch adds on top of the user config: the `itch`
/// agent, which has the Itch system prompt and may use no tool, no title
/// agent (it sends the first prompt to a second model call), and the user
/// MCP servers `mcp_servers` turned off.
fn config_content(mcp_servers: &[String]) -> String {
    let mcp: serde_json::Map<String, Value> = mcp_servers
        .iter()
        .map(|name| (name.clone(), json!({ "enabled": false })))
        .collect();
    json!({
        "agent": {
            "title": { "disable": true },
            AGENT: {
                "description": "Itch writing assistant",
                "mode": "primary",
                "prompt": SYSTEM_PROMPT,
                "permission": { "*": "deny" },
            },
        },
        "mcp": mcp,
    })
    .to_string()
}

/// The MCP server names in the resolved config of `opencode debug config`.
fn mcp_server_names(config: &str) -> Result<Vec<String>, String> {
    let config: Value = serde_json::from_str(config)
        .map_err(|e| format!("Failed to read the {TOOL} config: {e}"))?;
    Ok(config["mcp"]
        .as_object()
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default())
}

/// A new temp file that only the user can read. Removed on drop.
struct PrivateFile(PathBuf);

impl PrivateFile {
    fn create() -> Result<(Self, File), String> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("itch-opencode-{}-{n}", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options
            .open(&path)
            .map_err(|e| format!("Failed to create a temp file: {e}"))?;
        Ok((Self(path), file))
    }
}

impl Drop for PrivateFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The names of the MCP servers in the user config, which OpenCode starts
/// with each session. The resolved config contains secrets: only the names
/// are kept. Its output goes to a private temp file, because OpenCode cuts
/// piped output at 64 KiB.
fn user_mcp_servers(program: &str, path: &str) -> Result<Vec<String>, String> {
    let (output, file) = PrivateFile::create()?;
    let mut child = no_window_cmd(program)
        .args(["debug", "config", "--pure"])
        .env("PATH", path)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start {TOOL}: {e}"))?;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(e) => format!("Failed to run {TOOL}: {e}"),
                    Ok(_) => format!("{TOOL} did not show its config in time."),
                });
            }
        }
    };
    if !status.success() {
        return Err(format!("{TOOL} could not show its config."));
    }
    let config = std::fs::read_to_string(&output.0)
        .map_err(|e| format!("Failed to read the {TOOL} config: {e}"))?;
    mcp_server_names(&config)
}

/// `opencode acp` without external plugins, with the Itch config (see
/// `config_content`) and a session database in memory (no session history
/// on disk).
fn acp_command(program: &str, path: &str, mcp_servers: &[String]) -> Command {
    let mut command = no_window_cmd(program);
    command
        .args(["acp", "--pure"])
        .env("PATH", path)
        .env("OPENCODE_CONFIG_CONTENT", config_content(mcp_servers))
        .env("OPENCODE_DB", ":memory:")
        // Keeps `~/.claude/CLAUDE.md` out of the system prompt.
        .env("OPENCODE_DISABLE_CLAUDE_CODE", "1");
    command
}

/// Removes terminal color codes, which OpenCode writes into its errors.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
        } else if chars.next() == Some('[') {
            // Skip the parameters up to the final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// A short error for the user from an OpenCode error or exit message.
fn user_message(error: &str) -> String {
    let error = strip_ansi(error);
    ["Internal error: ", "Invalid params: "]
        .iter()
        .find_map(|prefix| error.strip_prefix(prefix))
        .unwrap_or(&error)
        .to_string()
}

/// The answer text of a `session/update` notification. `None` for other
/// updates, for example thinking text or tool calls.
fn message_chunk(params: &Value) -> Option<&str> {
    let update = &params["update"];
    if update["sessionUpdate"] != "agent_message_chunk" || update["content"]["type"] != "text" {
        return None;
    }
    update["content"]["text"].as_str()
}

/// A session setting (`configOptions` entry) and its current value.
#[derive(Debug, Clone, PartialEq)]
struct ConfigOption {
    id: String,
    value: Option<String>,
}

/// The setting of `category` (`model`, `mode`, `thought_level`) in a
/// `session/new` or `session/set_config_option` result.
fn config_option(result: &Value, category: &str) -> Option<ConfigOption> {
    let option = result["configOptions"]
        .as_array()?
        .iter()
        .find(|option| option["category"] == category)?;
    Some(ConfigOption {
        id: option["id"].as_str()?.to_string(),
        value: option["currentValue"].as_str().map(str::to_string),
    })
}

/// How a `session/prompt` call ended: `Ok(true)` when it was cancelled, or
/// the error for the user.
fn prompt_outcome(result: Result<Value, String>) -> Result<bool, String> {
    let result = result.map_err(|e| user_message(&e))?;
    Ok(result["stopReason"] == "cancelled")
}

/// Reads the output of `opencode models --verbose`: a `provider/model` line,
/// then the model as JSON from a `{` line to a `}` line.
fn parse_models(output: &str) -> Vec<Model> {
    let mut models = Vec::new();
    let mut object: Option<String> = None;
    for line in output.lines() {
        match object.as_mut() {
            None if line == "{" => object = Some(line.to_string()),
            None => {}
            Some(text) => {
                text.push_str(line);
                if line == "}" {
                    models.extend(parse_model(text));
                    object = None;
                }
            }
        }
    }
    models
}

/// A model object of `opencode models --verbose`.
#[derive(Deserialize)]
struct VerboseModel {
    id: String,
    #[serde(rename = "providerID")]
    provider: String,
    name: Option<String>,
    #[serde(default)]
    variants: Keys,
}

/// The keys of a JSON object, in their order (low, medium, high, ...).
#[derive(Default)]
struct Keys(Vec<String>);

impl<'de> Deserialize<'de> for Keys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeysVisitor;
        impl<'de> Visitor<'de> for KeysVisitor {
            type Value = Keys;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                let mut keys = Vec::new();
                while let Some((key, IgnoredAny)) = map.next_entry::<String, IgnoredAny>()? {
                    keys.push(key);
                }
                Ok(Keys(keys))
            }
        }
        deserializer.deserialize_map(KeysVisitor)
    }
}

/// Reads one model object. `None` when it has no id or provider. The
/// thinking levels are the variant names.
fn parse_model(json: &str) -> Option<Model> {
    let model: VerboseModel = serde_json::from_str(json).ok()?;
    Some(Model {
        id: format!("{}/{}", model.provider, model.id),
        name: model.name.unwrap_or_else(|| model.id.clone()),
        group: model.provider,
        thinking_levels: model.variants.0,
    })
}

/// Runs `opencode models --verbose` and returns the models. OpenCode does
/// not tell its default model without a session, so there is no default.
pub fn list_models(program: &str, path: &str) -> Result<ModelList, String> {
    let mut child = no_window_cmd(program)
        .args(["models", "--verbose", "--pure"])
        .env("PATH", path)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start {TOOL}: {e}"))?;
    let (Some(mut stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("Failed to connect to {TOOL}."));
    };
    let stderr_reader = std::thread::spawn(move || read_stderr_tail(stderr));
    let (reply, output) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = reply.send(stdout.read_to_string(&mut text).map(|_| text));
    });
    let output = output.recv_timeout(MODELS_TIMEOUT);
    if output.is_err() {
        let _ = child.kill();
    }
    let status = child.wait();
    let output = match output {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => return Err(format!("Failed to read the {TOOL} models: {e}")),
        Err(_) => return Err(format!("{TOOL} did not list the models in time.")),
    };
    if !status.is_ok_and(|status| status.success()) {
        let stderr = stderr_reader.join().unwrap_or_default();
        return Err(user_message(&exit_message(TOOL, &stderr)));
    }
    Ok(ModelList {
        models: parse_models(&output),
        default_model: None,
        default_thinking: None,
    })
}

/// State shared with the callbacks on the reader thread.
#[derive(Default)]
struct Shared {
    run: Run,
    /// True after the first prompt ended, which carries the context.
    context_sent: bool,
}

/// Receives the OpenCode notifications and requests.
struct Handler(Arc<Mutex<Shared>>);

impl RpcHandler for Handler {
    fn notification(&mut self, method: &str, params: &Value) {
        if method != "session/update" {
            return;
        }
        if let (Some(text), Ok(mut shared)) = (message_chunk(params), self.0.lock()) {
            shared.run.delta(text);
        }
    }

    fn request(&mut self, method: &str, _params: &Value) -> Result<Value, String> {
        match method {
            // Itch allows no tool use.
            "session/request_permission" => Ok(json!({ "outcome": { "outcome": "cancelled" } })),
            _ => Err(format!("{method} is not supported.")),
        }
    }

    fn exited(&mut self, message: &str) {
        if let Ok(mut shared) = self.0.lock() {
            shared.run.fail(user_message(message));
        }
    }
}

/// One running `opencode acp` process with one session. Dropping it kills
/// the process.
pub struct OpenCode {
    rpc: Rpc,
    shared: Arc<Mutex<Shared>>,
    session_id: String,
    /// The model setting of the session.
    model: Option<ConfigOption>,
    /// The thinking level setting. `None` when the model has no levels.
    thinking: Option<ConfigOption>,
}

/// Starts OpenCode and opens a session with the Itch agent.
pub fn start(program: &str, path: &str) -> Result<OpenCode, String> {
    // Without the names the MCP servers start too: slower, but it works.
    let mcp_servers = user_mcp_servers(program, path).unwrap_or_default();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let mut rpc = Rpc::spawn(
        TOOL,
        acp_command(program, path, &mcp_servers),
        Handler(Arc::clone(&shared)),
    )?;
    let initialize = json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
        },
    });
    rpc.call("initialize", initialize, START_TIMEOUT)
        .map_err(|e| user_message(&e))?;
    let new_session = json!({ "cwd": std::env::temp_dir(), "mcpServers": [] });
    let session = rpc
        .call("session/new", new_session, START_TIMEOUT)
        .map_err(|e| user_message(&e))?;
    let session_id = session["sessionId"]
        .as_str()
        .ok_or(format!("{TOOL} did not open a session."))?
        .to_string();
    let mut opencode = OpenCode {
        rpc,
        shared,
        session_id,
        model: config_option(&session, "model"),
        thinking: config_option(&session, "thought_level"),
    };
    let mode = config_option(&session, "mode").map_or("mode".to_string(), |mode| mode.id);
    let selected = opencode
        .set_option(&mode, AGENT)
        .map_err(|e| format!("{TOOL} could not select the Itch agent: {e}"))?;
    if selected != Some(AGENT.to_string()) {
        return Err(format!("{TOOL} could not select the Itch agent."));
    }
    Ok(opencode)
}

impl OpenCode {
    /// Sets the session setting `option` to `value` and keeps the new model
    /// and thinking settings. Returns the new mode (agent).
    fn set_option(&mut self, option: &str, value: &str) -> Result<Option<String>, String> {
        let params = json!({ "sessionId": self.session_id, "configId": option, "value": value });
        let result = self
            .rpc
            .call("session/set_config_option", params, COMMAND_TIMEOUT)
            .map_err(|e| user_message(&e))?;
        if let Some(model) = config_option(&result, "model") {
            self.model = Some(model);
        }
        // A model without thinking levels has no thinking setting.
        self.thinking = config_option(&result, "thought_level");
        Ok(config_option(&result, "mode").and_then(|mode| mode.value))
    }

    /// Switches the model and thinking level of `request` when they differ
    /// from the current ones. A level for a model without levels is skipped.
    fn configure(&mut self, request: &PromptRequest) -> Result<(), String> {
        if let Some(model) = request.model() {
            let current = self.model.as_ref();
            if current.and_then(|o| o.value.as_deref()) != Some(model) {
                let id = current.map_or("model".to_string(), |o| o.id.clone());
                self.set_option(&id, model)
                    .map_err(|e| format!("{TOOL} could not use the model {model}: {e}"))?;
            }
        }
        if let (Some(level), Some(thinking)) = (request.thinking(), &self.thinking) {
            if thinking.value.as_deref() != Some(level) {
                let id = thinking.id.clone();
                self.set_option(&id, level)
                    .map_err(|e| format!("{TOOL} could not use the thinking level {level}: {e}"))?;
            }
        }
        Ok(())
    }

    /// Ends the current run with `Error { message }`, or without an event.
    fn end_run(&self, message: Option<String>) {
        if let Ok(mut shared) = self.shared.lock() {
            match message {
                Some(message) => shared.run.fail(message),
                None => shared.run = Run::default(),
            }
        }
    }
}

impl Drop for OpenCode {
    fn drop(&mut self) {
        // The caller closed the conversation: no event for the current run.
        // The `rpc` field then kills the process.
        self.end_run(None);
    }
}

impl Conversation for OpenCode {
    /// Switches model and thinking level (waits for OpenCode), then sends
    /// the prompt. The answer streams on the reader thread.
    fn prompt(&mut self, request: &PromptRequest, sink: Sink) -> Result<(), String> {
        let include_context = {
            let mut shared = self
                .shared
                .lock()
                .map_err(|_| format!("{TOOL} session is broken."))?;
            shared.run.start(TOOL, sink)?;
            !shared.context_sent
        };
        if let Err(message) = self.configure(request) {
            self.end_run(Some(message));
            return Ok(());
        }
        let params = json!({
            "sessionId": self.session_id,
            "prompt": [{ "type": "text", "text": build_prompt(request, include_context) }],
        });
        let shared = Arc::clone(&self.shared);
        let sent = self
            .rpc
            .call_async("session/prompt", params, move |result| {
                let Ok(mut shared) = shared.lock() else {
                    return;
                };
                match prompt_outcome(result) {
                    Ok(aborted) => {
                        shared.context_sent = true;
                        shared.run.finish(None, aborted);
                    }
                    Err(message) => shared.run.fail(message),
                }
            });
        if let Err(e) = sent {
            self.end_run(None);
            return Err(user_message(&e));
        }
        Ok(())
    }

    /// Sends `session/cancel`. OpenCode then ends the prompt with the stop
    /// reason `cancelled`.
    fn abort(&mut self) -> Result<(), String> {
        let running = self
            .shared
            .lock()
            .map(|shared| shared.run.is_active())
            .unwrap_or(false);
        if !running {
            return Ok(());
        }
        self.rpc
            .notify("session/cancel", json!({ "sessionId": self.session_id }))
            .map_err(|e| format!("Failed to stop {TOOL}: {e}"))
    }

    fn is_alive(&self) -> bool {
        self.rpc.is_alive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::test_support::{channel_sink, collect_run, request};
    use crate::ai::AiEvent;
    use crate::get_expanded_path;

    // Lines below are copied from opencode 1.18.33 (`opencode acp`), with
    // shorter option lists.
    const SESSION_NEW: &str = r#"{"sessionId":"ses_e95418566ffeL0eX75FCsBYYzt","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"modal/zai-org/GLM-5-FP8","options":[{"value":"openrouter/aion-labs/aion-3.5","name":"OpenRouter/Aion 3.5"}]},{"id":"mode","name":"Session Mode","category":"mode","type":"select","currentValue":"build","options":[{"value":"build","name":"build","description":"The default agent."},{"value":"itch","name":"itch"}]}]}"#;
    const MODEL_SET: &str = r#"{"configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"openrouter/~anthropic/claude-sonnet-latest","options":[]},{"id":"effort","name":"Effort","description":"Available effort levels for this model","category":"thought_level","type":"select","currentValue":"low","options":[{"value":"low","name":"low"},{"value":"high","name":"high"},{"value":"default","name":"default"}]},{"id":"mode","name":"Session Mode","category":"mode","type":"select","currentValue":"itch","options":[]}]}"#;

    fn value(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn config_defines_a_tool_free_agent_with_the_system_prompt() {
        let config = value(&config_content(&[]));
        let agent = &config["agent"][AGENT];
        assert_eq!(agent["prompt"], SYSTEM_PROMPT);
        assert_eq!(agent["mode"], "primary");
        assert_eq!(agent["permission"], json!({ "*": "deny" }));
        assert_eq!(config["agent"]["title"], json!({ "disable": true }));
        assert_eq!(config["mcp"], json!({}));
    }

    #[test]
    fn config_turns_off_the_given_mcp_servers() {
        let config = value(&config_content(&[
            "one".to_string(),
            "my server".to_string(),
        ]));
        assert_eq!(
            config["mcp"],
            json!({ "one": { "enabled": false }, "my server": { "enabled": false } })
        );
    }

    #[test]
    fn reads_the_mcp_server_names_of_the_resolved_config() {
        let config = r#"{"$schema":"https://opencode.ai/config.json","mcp":{"local-one":{"type":"local","command":["node","x.js"],"environment":{"API_KEY":"secret"}},"remote-two":{"type":"remote","url":"https://example.com/mcp"}},"agent":{}}"#;
        assert_eq!(
            mcp_server_names(config),
            Ok(vec!["local-one".to_string(), "remote-two".to_string()])
        );
        assert_eq!(mcp_server_names(r#"{"agent":{}}"#), Ok(vec![]));
        assert!(mcp_server_names(r#"{"mcp":{"cut"#).is_err());
    }

    #[test]
    fn acp_command_runs_without_plugins_history_or_claude_files() {
        let servers = ["one".to_string()];
        let command = acp_command("opencode", "/bin", &servers);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["acp", "--pure"]);
        let env: Vec<(String, String)> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        for (key, expected) in [
            ("PATH", "/bin".to_string()),
            ("OPENCODE_DB", ":memory:".to_string()),
            ("OPENCODE_DISABLE_CLAUDE_CODE", "1".to_string()),
            ("OPENCODE_CONFIG_CONTENT", config_content(&servers)),
        ] {
            assert!(
                env.iter().any(|(k, v)| k == key && *v == expected),
                "{key} in {env:?}"
            );
        }
    }

    #[test]
    fn user_message_removes_colors_and_protocol_prefixes() {
        assert_eq!(
            user_message("OpenCode stopped unexpectedly: \u{1b}[91m\u{1b}[1mError: \u{1b}[0mProvider not found: nope"),
            "OpenCode stopped unexpectedly: Error: Provider not found: nope"
        );
        assert_eq!(
            user_message("Internal error: This request requires more credits"),
            "This request requires more credits"
        );
        assert_eq!(
            user_message("Invalid params: model not found: nope/missing"),
            "model not found: nope/missing"
        );
    }

    #[test]
    fn reads_answer_chunks_and_ignores_thinking_and_other_updates() {
        let chunk = value(
            r#"{"sessionId":"ses_1","update":{"sessionUpdate":"agent_message_chunk","messageId":"msg_16abe7ad0001kzf2MPfopr48NO","content":{"type":"text","text":"Hi"}}}"#,
        );
        assert_eq!(message_chunk(&chunk), Some("Hi"));
        let thought = value(
            r#"{"sessionId":"ses_1","update":{"sessionUpdate":"agent_thought_chunk","messageId":"prt_16abf5236001ORMyFyIHwmL4lM","content":{"type":"text","text":"Okay"}}}"#,
        );
        assert_eq!(message_chunk(&thought), None);
        let commands = value(
            r#"{"sessionId":"ses_1","update":{"sessionUpdate":"available_commands_update","availableCommands":[]}}"#,
        );
        assert_eq!(message_chunk(&commands), None);
    }

    #[test]
    fn reads_session_settings_by_category() {
        let session = value(SESSION_NEW);
        assert_eq!(
            config_option(&session, "model"),
            Some(ConfigOption {
                id: "model".to_string(),
                value: Some("modal/zai-org/GLM-5-FP8".to_string())
            })
        );
        assert_eq!(
            config_option(&session, "mode").and_then(|o| o.value),
            Some("build".to_string())
        );
        assert_eq!(config_option(&session, "thought_level"), None);
        assert_eq!(
            config_option(&value(MODEL_SET), "thought_level"),
            Some(ConfigOption {
                id: "effort".to_string(),
                value: Some("low".to_string())
            })
        );
    }

    #[test]
    fn prompt_outcome_tells_cancel_and_errors() {
        let end = r#"{"stopReason":"end_turn","usage":{"inputTokens":1177,"outputTokens":176,"totalTokens":1353},"_meta":{}}"#;
        assert_eq!(prompt_outcome(Ok(value(end))), Ok(false));
        let cancelled = r#"{"stopReason":"cancelled","usage":{"inputTokens":0,"outputTokens":0,"totalTokens":0},"_meta":{}}"#;
        assert_eq!(prompt_outcome(Ok(value(cancelled))), Ok(true));
        assert_eq!(
            prompt_outcome(Err(
                "Internal error: This request requires more credits, or fewer max_tokens."
                    .to_string()
            )),
            Err("This request requires more credits, or fewer max_tokens.".to_string())
        );
    }

    /// `opencode models --verbose` output, shortened.
    const MODELS_OUTPUT: &str = r#"opencode/big-pickle
{
  "id": "big-pickle",
  "providerID": "opencode",
  "name": "Big Pickle",
  "api": {
    "id": "big-pickle"
  },
  "options": {},
  "variants": {}
}
openrouter/~anthropic/claude-sonnet-latest
{
  "id": "~anthropic/claude-sonnet-latest",
  "providerID": "openrouter",
  "name": "Claude Sonnet Latest",
  "variants": {
    "low": {
      "reasoningEffort": "low"
    },
    "high": {
      "reasoningEffort": "high"
    }
  }
}
"#;

    #[test]
    fn parses_the_verbose_model_list() {
        assert_eq!(
            parse_models(MODELS_OUTPUT),
            [
                Model {
                    id: "opencode/big-pickle".to_string(),
                    name: "Big Pickle".to_string(),
                    group: "opencode".to_string(),
                    thinking_levels: vec![],
                },
                Model {
                    id: "openrouter/~anthropic/claude-sonnet-latest".to_string(),
                    name: "Claude Sonnet Latest".to_string(),
                    group: "openrouter".to_string(),
                    thinking_levels: vec!["low".to_string(), "high".to_string()],
                },
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_models_runs_the_models_command() {
        let log = crate::ai::test_support::TempFile::new("opencode-models");
        let output = crate::ai::test_support::TempFile::new("opencode-models-out");
        std::fs::write(&output.path, MODELS_OUTPUT).unwrap();
        let (_script, program) = crate::ai::test_support::fake_tool(&format!(
            "echo \"$@\" > '{}'\ncat '{}'",
            log.path.display(),
            output.path.display()
        ));
        let list = list_models(&program, &get_expanded_path()).unwrap();
        assert_eq!(log.read().trim(), "models --verbose --pure");
        assert_eq!(list.models.len(), 2);
        assert_eq!(list.default_model, None);
        assert_eq!(list.default_thinking, None);
    }

    #[cfg(unix)]
    #[test]
    fn list_models_reports_the_opencode_error() {
        let (_script, program) = crate::ai::test_support::fake_tool(
            "printf '\\033[91m\\033[1mError: \\033[0mConfiguration is invalid\\n' >&2; exit 1",
        );
        assert_eq!(
            list_models(&program, &get_expanded_path()),
            Err("OpenCode stopped unexpectedly: Error: Configuration is invalid".to_string())
        );
    }

    /// A fake `opencode acp` that logs each line it reads. It knows the
    /// models `p/m1` (default), `p/m2`, `p/m3` (with thinking levels) and
    /// `p/plain` (without). The prompt text selects the run: `slow` waits
    /// for a cancel, `perm` asks for a permission first, `fail` returns an
    /// error, `crash` exits, else it streams "Hel" "lo".
    #[cfg(unix)]
    fn fake_opencode(
        log: &crate::ai::test_support::TempFile,
    ) -> (crate::ai::test_support::TempFile, String) {
        crate::ai::test_support::fake_tool(&format!(
            r#"if [ "$*" = "debug config --pure" ]; then
printf '%s\n' '{{"mcp":{{"one":{{"type":"local","command":["x"],"environment":{{"KEY":"secret"}}}},"two":{{"type":"remote","url":"u"}}}}}}'
exit 0
fi
printf 'config %s\n' "$OPENCODE_CONFIG_CONTENT" >> '{log}'
model=p/m1
effort=
reply() {{ printf '%s\n' "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$1}}"; }}
options() {{
  if [ -n "$effort" ]; then e=",{{\"id\":\"effort\",\"category\":\"thought_level\",\"currentValue\":\"$effort\",\"options\":[]}}"; else e=; fi
  reply "{{\"configOptions\":[{{\"id\":\"model\",\"category\":\"model\",\"currentValue\":\"$model\",\"options\":[]}}$e,{{\"id\":\"mode\",\"category\":\"mode\",\"currentValue\":\"$mode\",\"options\":[]}}]}}"
}}
chunk() {{ printf '%s\n' "{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"sessionId\":\"ses_1\",\"update\":{{\"sessionUpdate\":\"$1\",\"messageId\":\"m\",\"content\":{{\"type\":\"text\",\"text\":\"$2\"}}}}}}}}"; }}
mode=build
while read line; do
printf '%s\n' "$line" >> '{log}'
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
value=$(printf '%s' "$line" | sed -n 's/.*"value":"\([^"]*\)".*/\1/p')
case "$line" in
*'"method":"initialize"'*) reply '{{"protocolVersion":1}}';;
*'"method":"session/new"'*) reply '{{"sessionId":"ses_1","configOptions":[{{"id":"model","category":"model","currentValue":"p/m1","options":[]}},{{"id":"mode","category":"mode","currentValue":"build","options":[]}}]}}';;
*'"configId":"mode"'*) mode=$value; options;;
*'"configId":"model"'*)
  case "$value" in
  p/plain) model=$value; effort=; options;;
  p/*) model=$value; effort=low; options;;
  *) printf '%s\n' "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{{\"code\":-32602,\"message\":\"Invalid params: model not found: $value\"}}}}";;
  esac;;
*'"configId":"effort"'*) effort=$value; options;;
*'"method":"session/cancel"'*) id=$prompt; reply '{{"stopReason":"cancelled"}}';;
*'"id":"p1"'*) id=$prompt; reply '{{"stopReason":"end_turn"}}';;
*'"method":"session/prompt"'*)
  prompt=$id
  case "$line" in
  *slow*) chunk agent_message_chunk 'Part';;
  *perm*) printf '%s\n' '{{"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{{"sessionId":"ses_1","toolCall":{{"toolCallId":"t1"}},"options":[{{"optionId":"once","kind":"allow_once","name":"Allow once"}},{{"optionId":"reject","kind":"reject_once","name":"Reject"}}]}}}}';;
  *fail*) printf '%s\n' "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{{\"code\":-32603,\"message\":\"Internal error: No credits\"}}}}";;
  *crash*) echo 'Error: boom' >&2; exit 1;;
  *) chunk agent_thought_chunk 'hmm'; chunk agent_message_chunk 'Hel'; chunk agent_message_chunk 'lo'; reply '{{"stopReason":"end_turn"}}';;
  esac;;
esac
done"#,
            log = log.path.display()
        ))
    }

    /// The logged JSON lines as `method`, `set:<id>=<value>` or
    /// `prompt:<text>`.
    fn read_log(log: &crate::ai::test_support::TempFile) -> Vec<String> {
        log.read()
            .lines()
            .filter(|line| !line.starts_with("config "))
            .map(|line| {
                let message = value(line);
                let params = &message["params"];
                match message["method"].as_str() {
                    Some("session/set_config_option") => format!(
                        "set:{}={}",
                        params["configId"].as_str().unwrap(),
                        params["value"].as_str().unwrap()
                    ),
                    Some("session/prompt") => {
                        format!("prompt:{}", params["prompt"][0]["text"].as_str().unwrap())
                    }
                    Some(method) => method.to_string(),
                    None => format!("answer:{}", message["result"]),
                }
            })
            .collect()
    }

    #[cfg(unix)]
    fn start_fake() -> (
        crate::ai::test_support::TempFile,
        crate::ai::test_support::TempFile,
        OpenCode,
    ) {
        let log = crate::ai::test_support::TempFile::new("opencode-log");
        let (script, program) = fake_opencode(&log);
        let opencode = start(&program, &get_expanded_path()).unwrap();
        (log, script, opencode)
    }

    #[cfg(unix)]
    fn instruction(text: &str) -> PromptRequest {
        PromptRequest {
            instruction: text.to_string(),
            ..request(Some("old text"))
        }
    }

    #[cfg(unix)]
    #[test]
    fn start_selects_the_itch_agent_in_a_temp_dir_session() {
        let (log, _script, _opencode) = start_fake();
        let text = log.read();
        let lines: Vec<&str> = text.lines().collect();
        let config = value(lines[0].strip_prefix("config ").unwrap());
        // The user MCP servers do not start. Only their names are copied.
        assert_eq!(
            config["mcp"],
            json!({ "one": { "enabled": false }, "two": { "enabled": false } })
        );
        assert!(!lines[0].contains("secret"));
        let first = value(lines[1]);
        assert_eq!(first["method"], "initialize");
        assert_eq!(first["params"]["clientCapabilities"]["terminal"], false);
        let new = value(lines[2]);
        assert_eq!(new["params"]["mcpServers"], json!([]));
        assert_eq!(
            new["params"]["cwd"],
            std::env::temp_dir().to_string_lossy().as_ref()
        );
        assert_eq!(
            read_log(&log),
            ["initialize", "session/new", "set:mode=itch"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn streams_a_run_and_sends_only_the_instruction_in_follow_ups() {
        let (log, _script, mut opencode) = start_fake();
        let (sink, rx) = channel_sink();
        opencode.prompt(&request(Some("old text")), sink).unwrap();
        assert_eq!(
            collect_run(&rx),
            (
                "Hello".to_string(),
                AiEvent::Done {
                    text: "Hello".to_string(),
                    aborted: false
                }
            )
        );
        let (sink, rx) = channel_sink();
        opencode
            .prompt(&instruction("Now in German"), sink)
            .unwrap();
        assert!(matches!(
            collect_run(&rx).1,
            AiEvent::Done { aborted: false, .. }
        ));

        let lines = read_log(&log);
        assert!(lines[3].starts_with("prompt:<document>"), "{lines:?}");
        assert!(lines[3].contains("Make it shorter"));
        assert_eq!(lines[4], "prompt:Now in German");
        assert_eq!(lines.len(), 5);
        assert!(opencode.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn switches_model_and_thinking_only_when_they_change() {
        let (log, _script, mut opencode) = start_fake();
        for (model, thinking) in [
            (Some("p/m1"), None),
            (Some("p/m2"), Some("high")),
            (Some("p/m2"), Some("high")),
            (None, None),
            (Some("p/m3"), Some("high")),
            (Some("p/plain"), Some("high")),
        ] {
            let (sink, rx) = channel_sink();
            let req = PromptRequest {
                model: model.map(str::to_string),
                thinking: thinking.map(str::to_string),
                ..instruction("go")
            };
            opencode.prompt(&req, sink).unwrap();
            assert!(matches!(collect_run(&rx).1, AiEvent::Done { .. }));
        }
        let lines: Vec<String> = read_log(&log)
            .into_iter()
            .skip(3)
            .map(|line| {
                if line.starts_with("prompt:") {
                    "prompt".to_string()
                } else {
                    line
                }
            })
            .collect();
        assert_eq!(
            lines,
            [
                // p/m1 is the default model.
                "prompt",
                "set:model=p/m2",
                "set:effort=high",
                "prompt",
                "prompt",
                "prompt",
                // The new model starts at its own level: set it again.
                "set:model=p/m3",
                "set:effort=high",
                "prompt",
                // A model without levels gets no level.
                "set:model=p/plain",
                "prompt",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn unknown_model_ends_the_run_with_an_error_and_sends_no_prompt() {
        let (log, _script, mut opencode) = start_fake();
        let (sink, rx) = channel_sink();
        let req = PromptRequest {
            model: Some("nope/x".to_string()),
            ..request(None)
        };
        opencode.prompt(&req, sink).unwrap();
        assert_eq!(
            collect_run(&rx).1,
            AiEvent::Error {
                message: "OpenCode could not use the model nope/x: model not found: nope/x"
                    .to_string()
            }
        );
        assert_eq!(read_log(&log).last().unwrap(), "set:model=nope/x");
        // The next prompt still carries the context.
        let (sink, rx) = channel_sink();
        opencode.prompt(&request(None), sink).unwrap();
        collect_run(&rx);
        assert!(read_log(&log)
            .last()
            .unwrap()
            .starts_with("prompt:<document>"));
    }

    #[cfg(unix)]
    #[test]
    fn abort_cancels_the_run_and_keeps_the_session() {
        let (log, _script, mut opencode) = start_fake();
        opencode.abort().unwrap();
        let (sink, rx) = channel_sink();
        opencode.prompt(&instruction("slow"), sink).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            AiEvent::Delta {
                text: "Part".to_string()
            }
        );
        let (busy, _) = channel_sink();
        assert_eq!(
            opencode.prompt(&instruction("again"), busy),
            Err("OpenCode is still working on the previous request.".to_string())
        );
        opencode.abort().unwrap();
        assert_eq!(
            collect_run(&rx).1,
            AiEvent::Done {
                text: "Part".to_string(),
                aborted: true
            }
        );
        let lines = read_log(&log);
        // Abort without a run sends nothing.
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(lines[3].contains("Instruction: slow"));
        assert_eq!(lines[4], "session/cancel");
        let (sink, rx) = channel_sink();
        opencode.prompt(&instruction("next"), sink).unwrap();
        assert!(matches!(
            collect_run(&rx).1,
            AiEvent::Done { aborted: false, .. }
        ));
        // The cancelled prompt is in the session: no context again.
        assert_eq!(read_log(&log).last().unwrap(), "prompt:next");
    }

    #[cfg(unix)]
    #[test]
    fn permission_requests_are_cancelled() {
        let (log, _script, mut opencode) = start_fake();
        let (sink, rx) = channel_sink();
        opencode.prompt(&instruction("perm"), sink).unwrap();
        assert!(matches!(
            collect_run(&rx).1,
            AiEvent::Done { aborted: false, .. }
        ));
        assert!(
            read_log(&log).contains(&r#"answer:{"outcome":{"outcome":"cancelled"}}"#.to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn tool_error_ends_the_run_with_the_error() {
        let (_log, _script, mut opencode) = start_fake();
        let (sink, rx) = channel_sink();
        opencode.prompt(&instruction("fail"), sink).unwrap();
        assert_eq!(
            collect_run(&rx).1,
            AiEvent::Error {
                message: "No credits".to_string()
            }
        );
        assert!(opencode.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn tool_exit_ends_the_run_with_the_exit_message() {
        let (_log, _script, mut opencode) = start_fake();
        let (sink, rx) = channel_sink();
        opencode.prompt(&instruction("crash"), sink).unwrap();
        assert_eq!(
            collect_run(&rx).1,
            AiEvent::Error {
                message: "OpenCode stopped unexpectedly: Error: boom".to_string()
            }
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(!opencode.is_alive());
    }

    #[cfg(unix)]
    #[test]
    fn start_reports_an_opencode_that_exits() {
        let (_script, program) = crate::ai::test_support::fake_tool(
            "printf '\\033[91mError: \\033[0mConfiguration is invalid\\n' >&2; exit 1",
        );
        assert_eq!(
            start(&program, &get_expanded_path()).err(),
            Some("OpenCode stopped unexpectedly: Error: Configuration is invalid".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_kills_opencode_without_an_event() {
        let (_log, _script, mut opencode) = start_fake();
        let (sink, rx) = channel_sink();
        opencode.prompt(&instruction("slow"), sink).unwrap();
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let pid = opencode.rpc.id();
        drop(opencode);
        assert!(!crate::ai::test_support::is_running(pid));
        std::thread::sleep(Duration::from_millis(100));
        assert!(rx.try_recv().is_err());
    }

    /// End-to-end check with the real OpenCode. OpenCode needs a model that
    /// works from outside its app, for example a local Ollama model in an
    /// extra config file:
    /// OPENCODE_CONFIG=/path/ollama.json OPENCODE_TEST_MODEL=ollama/qwen3:0.6b
    /// cargo test --lib opencode::tests::real_opencode -- --ignored --nocapture
    #[test]
    #[ignore = "needs a real OpenCode install and a working model"]
    fn real_opencode_lists_streams_refines_and_aborts() {
        let path = crate::ai::search_path();
        let program = crate::ai::resolve_program(crate::ai::Provider::OpenCode, None, &path)
            .expect("opencode on PATH");
        let model = std::env::var("OPENCODE_TEST_MODEL").expect("OPENCODE_TEST_MODEL");
        let list = list_models(&program, &path).unwrap();
        println!("{} models", list.models.len());
        let listed = list
            .models
            .iter()
            .find(|m| m.id == model)
            .expect("test model listed");
        println!("test model: {listed:?}");

        let started = std::time::Instant::now();
        let mut opencode = start(&program, &path).unwrap();
        println!("started in {:?}", started.elapsed());

        let (sink, rx) = channel_sink();
        let missing = PromptRequest {
            model: Some("nope/missing-model".to_string()),
            ..request(Some("Cats sleeps a lot."))
        };
        opencode.prompt(&missing, sink).unwrap();
        let (_, end) = collect_run(&rx);
        println!("missing model: {end:?}");
        assert!(matches!(end, AiEvent::Error { .. }));

        let first = PromptRequest {
            instruction: "Continue writing with one short sentence.".to_string(),
            before: "# Cats\n\nCats sleep a lot. ".to_string(),
            after: String::new(),
            selection: None,
            thinking: None,
            model: Some(model),
            read_only: false,
        };
        let (sink, rx) = channel_sink();
        opencode.prompt(&first, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("first: {streamed:?} / {end:?}");
        assert!(!streamed.is_empty());
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

        let (sink, rx) = channel_sink();
        let refine = PromptRequest {
            instruction: "Repeat your previous sentence, but in German.".to_string(),
            ..first.clone()
        };
        opencode.prompt(&refine, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("refine: {streamed:?} / {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

        let (sink, rx) = channel_sink();
        let long = PromptRequest {
            instruction: "Write a 600 word essay about cats.".to_string(),
            ..first
        };
        opencode.prompt(&long, sink).unwrap();
        let first_delta = rx.recv_timeout(Duration::from_secs(120)).expect("no delta");
        assert!(matches!(first_delta, AiEvent::Delta { .. }));
        opencode.abort().unwrap();
        let (_, end) = collect_run(&rx);
        println!("abort: {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: true, .. }));
        assert!(opencode.is_alive());
    }
}

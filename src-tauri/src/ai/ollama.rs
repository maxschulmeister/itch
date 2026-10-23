//! The Ollama backend: the Ollama HTTP API (`POST /api/chat`, streaming).
//! Ollama keeps no conversation, so the history lives here.

use super::{
    build_prompt, AiEvent, Conversation, Model, ModelList, PromptRequest, Sink, SYSTEM_PROMPT,
};
use serde::Serialize;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: &str = "11434";
/// Time to connect to the server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Time for one short request, for example `/api/tags`.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const NOT_RUNNING: &str = "Ollama is not running. Start the Ollama app or run `ollama serve`.";
const NO_MODELS: &str = "No Ollama models found. Download a model with `ollama pull <model>`.";

/// Splits `host:port` or `[host]:port`. `None` without a port.
fn split_host_port(hostport: &str) -> Option<(&str, &str)> {
    if let Some(rest) = hostport.strip_prefix('[') {
        return rest.split_once("]:");
    }
    hostport
        .rsplit_once(':')
        .filter(|(host, _)| !host.contains(':'))
}

/// The base URL of the Ollama server, without a trailing slash. Reads
/// `OLLAMA_HOST` like the Ollama CLI (`envconfig.Host`): `host`,
/// `host:port`, `http://host:port/path`. A bind-all address (`0.0.0.0`,
/// `::`) means this computer.
fn base_url(host: Option<&str>) -> Result<String, String> {
    let host = host.unwrap_or_default().trim();
    let (hostport, default_port) = match host.split_once("://") {
        None => (host, DEFAULT_PORT),
        Some(("http", rest)) => (rest, "80"),
        Some(_) => {
            return Err("Itch connects to Ollama only over http. Check OLLAMA_HOST.".to_string())
        }
    };
    let (hostport, path) = hostport.split_once('/').unwrap_or((hostport, ""));
    let (host, port) = split_host_port(hostport)
        .unwrap_or((hostport.trim_start_matches('[').trim_end_matches(']'), ""));
    let host = match host {
        "" | "0.0.0.0" => DEFAULT_HOST,
        "::" => "::1",
        host => host,
    };
    let port = port
        .parse::<u16>()
        .map_or(default_port.to_string(), |port| port.to_string());
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let path = path.trim_end_matches('/');
    Ok(if path.is_empty() {
        format!("http://{host}:{port}")
    } else {
        format!("http://{host}:{port}/{path}")
    })
}

/// The address of the Ollama server.
#[derive(Debug, Clone)]
struct Server {
    base: String,
}

impl Server {
    /// The server of `OLLAMA_HOST`, else the local default.
    fn from_env() -> Result<Self, String> {
        let host = std::env::var("OLLAMA_HOST").ok();
        Ok(Self {
            base: base_url(host.as_deref())?,
        })
    }

    /// A client without proxy: the server is usually on this computer.
    /// HTTP errors are responses, so that their JSON body can be read.
    fn agent(timeout: Option<Duration>) -> ureq::Agent {
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(timeout)
            .build()
            .into()
    }

    /// Sends `body` to `path`. Fails with the Ollama error for a non-2xx status.
    fn post(
        &self,
        path: &str,
        body: &Value,
        timeout: Option<Duration>,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        let response = Self::agent(timeout)
            .post(format!("{}{path}", self.base))
            .content_type("application/json")
            .send(body.to_string())
            .map_err(request_error)?;
        check_status(response)
    }

    /// Sends a short request and reads the JSON answer.
    fn ask(&self, path: &str, body: Option<&Value>) -> Result<Value, String> {
        let mut response = match body {
            Some(body) => self.post(path, body, Some(COMMAND_TIMEOUT))?,
            None => check_status(
                Self::agent(Some(COMMAND_TIMEOUT))
                    .get(format!("{}{path}", self.base))
                    .call()
                    .map_err(request_error)?,
            )?,
        };
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(request_error)?;
        serde_json::from_str(&text)
            .map_err(|_| "Ollama sent an answer that Itch cannot read.".to_string())
    }

    /// The names of the local models, in the `/api/tags` order.
    fn model_names(&self) -> Result<Vec<String>, String> {
        let tags = self.ask("/api/tags", None)?;
        Ok(tags["models"]
            .as_array()
            .map(|models| {
                models
                    .iter()
                    .filter_map(|model| model["name"].as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// A user-friendly message for a failed request.
fn request_error(error: ureq::Error) -> String {
    match &error {
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            NOT_RUNNING.to_string()
        }
        ureq::Error::ConnectionFailed => NOT_RUNNING.to_string(),
        _ => format!("Failed to connect to Ollama: {error}"),
    }
}

/// Returns a 2xx response. Else reads the `{"error": "..."}` body.
fn check_status(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.body_mut().read_to_string().unwrap_or_default();
    Err(match serde_json::from_str::<Value>(&body) {
        Ok(value) if value["error"].is_string() => {
            format!("Ollama: {}", value["error"].as_str().unwrap_or_default())
        }
        _ => format!("Ollama returned HTTP {}.", status.as_u16()),
    })
}

/// One line of the `/api/chat` stream.
#[derive(Debug, PartialEq)]
enum Chunk {
    /// Answer text (thinking text is left out) and the end mark.
    Message {
        content: String,
        done: bool,
    },
    Error(String),
    Other,
}

fn parse_chunk(line: &str) -> Chunk {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Chunk::Other;
    };
    if let Some(error) = value["error"].as_str() {
        return Chunk::Error(error.to_string());
    }
    match (
        value["message"]["content"].as_str(),
        value["done"].as_bool(),
    ) {
        (content, Some(done)) => Chunk::Message {
            content: content.unwrap_or_default().to_string(),
            done,
        },
        _ => Chunk::Other,
    }
}

/// The thinking levels of a model from its `/api/show` answer. Newer servers
/// send `thinking.values` (booleans or level names; `[false]` means no
/// thinking). Older servers only have the `thinking` capability, which
/// means on/off.
fn thinking_levels(show: &Value) -> Vec<String> {
    if let Some(values) = show["thinking"]["values"].as_array() {
        let levels: Vec<String> = values
            .iter()
            .filter_map(|value| match value {
                Value::Bool(false) => Some("off".to_string()),
                Value::Bool(true) => Some("on".to_string()),
                Value::String(level) => Some(level.clone()),
                _ => None,
            })
            .collect();
        return if levels == ["off"] {
            Vec::new()
        } else {
            levels
        };
    }
    let thinks = show["capabilities"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|cap| cap == "thinking"));
    if thinks {
        vec!["off".to_string(), "on".to_string()]
    } else {
        Vec::new()
    }
}

/// The `think` field for a thinking level: `off`/`on` are booleans, other
/// levels are sent as they are.
fn think_value(level: &str) -> Value {
    match level {
        "off" => Value::Bool(false),
        "on" => Value::Bool(true),
        level => Value::String(level.to_string()),
    }
}

/// One chat message.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

struct ActiveRun {
    sink: Sink,
    /// The user message of this run. It goes into the history with the answer.
    prompt: Message,
    text: String,
    /// Set when the run ended outside the stream thread (abort, drop). The
    /// thread then closes the connection and sends no event.
    cancel: Arc<AtomicBool>,
}

/// State shared between the conversation and its stream thread.
#[derive(Default)]
struct State {
    /// Finished turns: user and assistant messages, in order.
    history: Vec<Message>,
    /// The model of the conversation. `None` until the first prompt.
    model: Option<String>,
    /// The `think` field. `None` uses the model default.
    think: Option<Value>,
    run: Option<ActiveRun>,
}

impl State {
    /// Ends the run with `Done` and keeps the turn in the history. An
    /// aborted run without text leaves no turn.
    fn finish(&mut self, aborted: bool) {
        if let Some(run) = self.run.take() {
            run.cancel.store(true, Ordering::SeqCst);
            if !aborted || !run.text.is_empty() {
                self.history.push(run.prompt);
                self.history.push(Message {
                    role: "assistant",
                    content: run.text.clone(),
                });
            }
            (run.sink)(AiEvent::Done {
                text: run.text,
                aborted,
            });
        }
    }

    fn fail(&mut self, message: String) {
        if let Some(run) = self.run.take() {
            (run.sink)(AiEvent::Error { message });
        }
    }
}

/// One popover conversation with Ollama. Each prompt sends the full history
/// to `/api/chat` on a stream thread.
pub struct OllamaChat {
    server: Server,
    state: Arc<Mutex<State>>,
}

impl OllamaChat {
    fn new(server: Server) -> Self {
        Self {
            server,
            state: Arc::default(),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, String> {
        self.state
            .lock()
            .map_err(|_| "Ollama session is broken.".to_string())
    }
}

/// The current run of `state`, if it is still the run of `cancel`.
fn current_run<'a>(state: &'a mut State, cancel: &AtomicBool) -> Option<&'a mut ActiveRun> {
    if cancel.load(Ordering::SeqCst) {
        None
    } else {
        state.run.as_mut()
    }
}

/// Runs one chat request and ends the run, unless it was cancelled.
fn run_chat(server: Server, mut body: Value, state: Arc<Mutex<State>>, cancel: Arc<AtomicBool>) {
    let result = stream_chat(&server, &mut body, &state, &cancel);
    let Ok(mut state) = state.lock() else { return };
    if cancel.load(Ordering::SeqCst) {
        return;
    }
    match result {
        Ok(()) => state.finish(false),
        Err(message) => state.fail(message),
    }
}

/// Sends the chat request and streams the answer until `done`. Returns early
/// when the run is cancelled; dropping the response closes the connection,
/// which stops Ollama.
fn stream_chat(
    server: &Server,
    body: &mut Value,
    state: &Mutex<State>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if body["model"].is_null() {
        let model = server.model_names()?.into_iter().next().ok_or(NO_MODELS)?;
        let mut state = state.lock().map_err(|_| "Ollama session is broken.")?;
        if current_run(&mut state, cancel).is_none() {
            return Ok(());
        }
        state.model = Some(model.clone());
        body["model"] = Value::String(model);
    }
    let response = server.post("/api/chat", body, None)?;
    // Split on LF only: JSON strings can contain U+2028/U+2029.
    for line in BufReader::new(response.into_body().into_reader()).split(b'\n') {
        let line = line.map_err(|e| format!("Failed to read the Ollama answer: {e}"))?;
        let mut state = state.lock().map_err(|_| "Ollama session is broken.")?;
        let Some(run) = current_run(&mut state, cancel) else {
            return Ok(());
        };
        match parse_chunk(&String::from_utf8_lossy(&line)) {
            Chunk::Message { content, done } => {
                if !content.is_empty() {
                    run.text.push_str(&content);
                    (run.sink)(AiEvent::Delta { text: content });
                }
                if done {
                    return Ok(());
                }
            }
            Chunk::Error(error) => return Err(format!("Ollama: {error}")),
            Chunk::Other => {}
        }
    }
    Err("Ollama stopped the answer unexpectedly.".to_string())
}

impl Conversation for OllamaChat {
    fn prompt(&mut self, request: &PromptRequest, sink: Sink) -> Result<(), String> {
        let mut state = self.lock()?;
        if state.run.is_some() {
            return Err("Ollama is still working on the previous request.".to_string());
        }
        if let Some(model) = request.model() {
            state.model = Some(model.to_string());
        }
        if let Some(level) = request.thinking() {
            state.think = Some(think_value(level));
        }
        let prompt = Message {
            role: "user",
            content: build_prompt(request, state.history.is_empty()),
        };
        let system = Message {
            role: "system",
            content: SYSTEM_PROMPT.to_string(),
        };
        let messages: Vec<&Message> = std::iter::once(&system)
            .chain(&state.history)
            .chain(std::iter::once(&prompt))
            .collect();
        let mut body = serde_json::json!({
            "model": state.model,
            "messages": messages,
            "stream": true,
        });
        if let Some(think) = &state.think {
            body["think"] = think.clone();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.run = Some(ActiveRun {
            sink,
            prompt,
            text: String::new(),
            cancel: Arc::clone(&cancel),
        });
        drop(state);
        let (server, shared) = (self.server.clone(), Arc::clone(&self.state));
        std::thread::spawn(move || run_chat(server, body, shared, cancel));
        Ok(())
    }

    /// Ends the run at once with the text so far. The stream thread closes
    /// the connection at the next line from Ollama.
    fn abort(&mut self) -> Result<(), String> {
        self.lock()?.finish(true);
        Ok(())
    }

    /// Ollama runs as a server: the conversation always takes prompts.
    fn is_alive(&self) -> bool {
        true
    }
}

impl Drop for OllamaChat {
    fn drop(&mut self) {
        // The caller closed the conversation: no event for the current run.
        if let Ok(mut state) = self.state.lock() {
            if let Some(run) = state.run.take() {
                run.cancel.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// Reads the models from `/api/tags` and their thinking levels from
/// `/api/show`.
fn read_model_list(server: &Server) -> Result<ModelList, String> {
    let models = server
        .model_names()?
        .into_iter()
        .map(|name| {
            let show = server.ask("/api/show", Some(&serde_json::json!({ "model": name })))?;
            Ok(Model {
                id: name.clone(),
                name,
                group: String::new(),
                thinking_levels: thinking_levels(&show),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ModelList {
        models,
        default_model: None,
        default_thinking: None,
    })
}

/// Starts a conversation with the Ollama server. The `ollama` program only
/// shows that Ollama is installed; the chat uses the HTTP API.
pub fn start(_program: &str, _path: &str) -> Result<Box<dyn Conversation>, String> {
    Ok(Box::new(OllamaChat::new(Server::from_env()?)))
}

/// Asks the Ollama server for its models.
pub fn list_models(_program: &str, _path: &str) -> Result<ModelList, String> {
    read_model_list(&Server::from_env()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::test_support::{channel_sink, collect_run, request};
    use serde_json::json;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::Receiver;

    /// A canned answer of the fake server.
    #[derive(Clone)]
    enum Reply {
        /// Status and JSON body.
        Json(u16, String),
        /// NDJSON lines, then the server closes the connection.
        Stream(Vec<String>),
        /// NDJSON lines, then empty chunks until the client closes the
        /// connection.
        Endless(Vec<String>),
    }

    /// A small HTTP server on 127.0.0.1 that records the requests.
    struct FakeServer {
        server: Server,
        /// `METHOD /path` and the JSON body of each request.
        requests: Arc<Mutex<Vec<(String, Value)>>>,
        /// Set when the client closed an `Endless` reply.
        closed: Arc<AtomicBool>,
    }

    impl FakeServer {
        fn start(reply: impl Fn(&str, &Value) -> Reply + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let closed = Arc::new(AtomicBool::new(false));
            let reply = Arc::new(reply);
            let (log, closed_flag) = (Arc::clone(&requests), Arc::clone(&closed));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { return };
                    let (reply, log, closed) = (
                        Arc::clone(&reply),
                        Arc::clone(&log),
                        Arc::clone(&closed_flag),
                    );
                    std::thread::spawn(move || serve(stream, &*reply, &log, &closed));
                }
            });
            Self {
                server: Server { base },
                requests,
                closed,
            }
        }

        fn requests(&self) -> Vec<(String, Value)> {
            self.requests.lock().unwrap().clone()
        }

        fn chat_bodies(&self) -> Vec<Value> {
            self.requests()
                .into_iter()
                .filter(|(target, _)| target == "POST /api/chat")
                .map(|(_, body)| body)
                .collect()
        }

        fn chat(&self) -> OllamaChat {
            OllamaChat::new(self.server.clone())
        }
    }

    fn serve(
        stream: TcpStream,
        reply: &(dyn Fn(&str, &Value) -> Reply + Send + Sync),
        log: &Mutex<Vec<(String, Value)>>,
        closed: &AtomicBool,
    ) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let target = request_line
            .split(' ')
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        log.lock().unwrap().push((target.clone(), body.clone()));

        let mut stream = stream;
        match reply(&target, &body) {
            Reply::Json(status, json) => {
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
            }
            Reply::Stream(lines) => {
                let _ = write!(stream, "{}", STREAM_HEAD);
                for line in lines {
                    let _ = writeln!(stream, "{line}");
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Reply::Endless(lines) => {
                let _ = write!(stream, "{}", STREAM_HEAD);
                for line in lines {
                    let _ = writeln!(stream, "{line}");
                }
                for _ in 0..500 {
                    let sent = writeln!(stream, "{}", delta("")).and_then(|_| stream.flush());
                    if sent.is_err() {
                        closed.store(true, Ordering::SeqCst);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    const STREAM_HEAD: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n";

    fn delta(text: &str) -> String {
        json!({"model":"m","message":{"role":"assistant","content":text},"done":false}).to_string()
    }

    fn thinking(text: &str) -> String {
        json!({"model":"m","message":{"role":"assistant","content":"","thinking":text},"done":false})
            .to_string()
    }

    fn done() -> String {
        json!({"model":"m","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"})
            .to_string()
    }

    fn tags(names: &[&str]) -> Reply {
        let models: Vec<Value> = names
            .iter()
            .map(|name| json!({"name": name, "model": name, "details": {"family": "qwen3"}}))
            .collect();
        Reply::Json(200, json!({ "models": models }).to_string())
    }

    /// A server that answers each chat with "Hello" and lists `qwen3:0.6b`.
    fn hello_server() -> FakeServer {
        FakeServer::start(|target, _| match target {
            "GET /api/tags" => tags(&["qwen3:0.6b", "llama3.2"]),
            _ => Reply::Stream(vec![thinking("hmm"), delta("Hel"), delta("lo"), done()]),
        })
    }

    fn run(chat: &mut OllamaChat, request: &PromptRequest) -> (String, AiEvent) {
        let (sink, rx) = channel_sink();
        chat.prompt(request, sink).unwrap();
        collect_run(&rx)
    }

    fn done_event(text: &str, aborted: bool) -> AiEvent {
        AiEvent::Done {
            text: text.to_string(),
            aborted,
        }
    }

    fn error_event(message: &str) -> AiEvent {
        AiEvent::Error {
            message: message.to_string(),
        }
    }

    fn message(role: &str, content: &str) -> Value {
        json!({ "role": role, "content": content })
    }

    #[test]
    fn base_url_accepts_the_ollama_host_forms() {
        for (host, expected) in [
            (None, "http://127.0.0.1:11434"),
            (Some(""), "http://127.0.0.1:11434"),
            (Some("localhost"), "http://localhost:11434"),
            (Some("example.com:8080"), "http://example.com:8080"),
            (Some("0.0.0.0"), "http://127.0.0.1:11434"),
            (Some(":5000"), "http://127.0.0.1:5000"),
            (Some("::1"), "http://[::1]:11434"),
            (Some("[::]:5000"), "http://[::1]:5000"),
            (Some("http://example.com"), "http://example.com:80"),
            (
                Some(" http://10.0.0.2:11434/ollama/ "),
                "http://10.0.0.2:11434/ollama",
            ),
        ] {
            assert_eq!(base_url(host).as_deref(), Ok(expected), "{host:?}");
        }
    }

    #[test]
    fn base_url_rejects_https() {
        assert_eq!(
            base_url(Some("https://ollama.example.com")),
            Err("Itch connects to Ollama only over http. Check OLLAMA_HOST.".to_string())
        );
    }

    #[test]
    fn parses_chat_lines() {
        let line = r#"{"model":"qwen3:0.6b","created_at":"2026-10-23T21:34:58.089348Z","message":{"role":"assistant","content":"?"},"done":false}"#;
        assert_eq!(
            parse_chunk(line),
            Chunk::Message {
                content: "?".to_string(),
                done: false
            }
        );
        let thinking = r#"{"model":"qwen3:0.6b","created_at":"2026-10-23T21:34:57.182463Z","message":{"role":"assistant","content":"","thinking":"Okay"},"done":false}"#;
        assert_eq!(
            parse_chunk(thinking),
            Chunk::Message {
                content: String::new(),
                done: false
            }
        );
        let last = r#"{"model":"qwen3:0.6b","created_at":"2026-10-23T21:34:58.098642Z","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","total_duration":210775083,"eval_count":10}"#;
        assert_eq!(
            parse_chunk(last),
            Chunk::Message {
                content: String::new(),
                done: true
            }
        );
        assert_eq!(
            parse_chunk(r#"{"error":"model 'nope' not found"}"#),
            Chunk::Error("model 'nope' not found".to_string())
        );
        assert_eq!(parse_chunk("not json"), Chunk::Other);
    }

    #[test]
    fn thinking_levels_come_from_the_show_response() {
        // Ollama 0.23.0 for qwen3:0.6b: only the capabilities.
        assert_eq!(
            thinking_levels(&json!({"capabilities": ["completion", "tools", "thinking"]})),
            ["off", "on"]
        );
        assert!(thinking_levels(&json!({"capabilities": ["completion"]})).is_empty());
        assert!(thinking_levels(&json!({})).is_empty());
        // Newer servers tell the values.
        assert_eq!(
            thinking_levels(&json!({
                "capabilities": ["completion", "thinking"],
                "thinking": {"values": ["low", "medium", "high"], "default": "medium"}
            })),
            ["low", "medium", "high"]
        );
        assert_eq!(
            thinking_levels(&json!({"thinking": {"values": [false, true]}})),
            ["off", "on"]
        );
        assert!(thinking_levels(&json!({
            "capabilities": ["completion", "thinking"],
            "thinking": {"values": [false]}
        }))
        .is_empty());
    }

    #[test]
    fn think_value_maps_off_and_on_to_booleans() {
        assert_eq!(think_value("off"), json!(false));
        assert_eq!(think_value("on"), json!(true));
        assert_eq!(think_value("high"), json!("high"));
    }

    #[test]
    fn chat_streams_the_answer_without_thinking() {
        let server = hello_server();
        let mut chat = server.chat();
        let req = PromptRequest {
            model: Some("llama3.2".to_string()),
            thinking: Some("on".to_string()),
            ..request(Some("old"))
        };
        assert_eq!(
            run(&mut chat, &req),
            ("Hello".to_string(), done_event("Hello", false))
        );
        assert_eq!(
            server.chat_bodies(),
            [json!({
                "model": "llama3.2",
                "messages": [
                    message("system", SYSTEM_PROMPT),
                    message("user", &build_prompt(&req, true)),
                ],
                "stream": true,
                "think": true,
            })]
        );
        assert!(chat.is_alive());
    }

    #[test]
    fn follow_up_sends_the_history_and_keeps_model_and_thinking() {
        let server = hello_server();
        let mut chat = server.chat();
        let first = PromptRequest {
            model: Some("llama3.2".to_string()),
            thinking: Some("off".to_string()),
            ..request(None)
        };
        run(&mut chat, &first);
        let follow_up = PromptRequest {
            instruction: "Now in German".to_string(),
            ..request(None)
        };
        assert_eq!(run(&mut chat, &follow_up).1, done_event("Hello", false));
        let bodies = server.chat_bodies();
        assert_eq!(bodies.len(), 2);
        assert_eq!(
            bodies[1],
            json!({
                "model": "llama3.2",
                "messages": [
                    message("system", SYSTEM_PROMPT),
                    message("user", &build_prompt(&first, true)),
                    message("assistant", "Hello"),
                    message("user", "Now in German"),
                ],
                "stream": true,
                "think": false,
            })
        );
    }

    #[test]
    fn without_a_model_the_chat_uses_the_first_listed_model() {
        let server = hello_server();
        let mut chat = server.chat();
        run(&mut chat, &request(None));
        let switch = PromptRequest {
            model: Some("llama3.2".to_string()),
            ..request(None)
        };
        run(&mut chat, &switch);
        run(&mut chat, &request(None));
        let targets: Vec<String> = server.requests().into_iter().map(|(t, _)| t).collect();
        assert_eq!(
            targets,
            [
                "GET /api/tags",
                "POST /api/chat",
                "POST /api/chat",
                "POST /api/chat"
            ]
        );
        let models: Vec<Value> = server
            .chat_bodies()
            .iter()
            .map(|body| body["model"].clone())
            .collect();
        assert_eq!(models, ["qwen3:0.6b", "llama3.2", "llama3.2"]);
        // No thinking level: Ollama uses the model default.
        assert!(server.chat_bodies()[0].get("think").is_none());
    }

    #[test]
    fn without_models_the_run_fails() {
        let server = FakeServer::start(|_, _| tags(&[]));
        let mut chat = server.chat();
        assert_eq!(run(&mut chat, &request(None)).1, error_event(NO_MODELS));
    }

    #[test]
    fn error_line_fails_the_run_and_the_next_prompt_resends_the_document() {
        let server = FakeServer::start(|_, _| {
            Reply::Stream(vec![
                delta("Hel"),
                r#"{"error":"out of memory"}"#.to_string(),
            ])
        });
        let mut chat = server.chat();
        let req = PromptRequest {
            model: Some("m".to_string()),
            ..request(None)
        };
        assert_eq!(run(&mut chat, &req).1, error_event("Ollama: out of memory"));
        run(&mut chat, &req);
        let bodies = server.chat_bodies();
        assert_eq!(bodies[1]["messages"], bodies[0]["messages"]);
    }

    #[test]
    fn http_error_returns_the_ollama_message() {
        let server = FakeServer::start(|_, _| {
            Reply::Json(404, r#"{"error":"model 'nope' not found"}"#.into())
        });
        let mut chat = server.chat();
        let req = PromptRequest {
            model: Some("nope".to_string()),
            ..request(None)
        };
        assert_eq!(
            run(&mut chat, &req).1,
            error_event("Ollama: model 'nope' not found")
        );
    }

    #[test]
    fn stream_end_without_done_fails_the_run() {
        let server = FakeServer::start(|_, _| Reply::Stream(vec![delta("Hel")]));
        let mut chat = server.chat();
        let req = PromptRequest {
            model: Some("m".to_string()),
            ..request(None)
        };
        assert_eq!(
            run(&mut chat, &req).1,
            error_event("Ollama stopped the answer unexpectedly.")
        );
    }

    /// The address of a closed port.
    fn closed_server() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        Server { base }
    }

    #[test]
    fn server_not_running_has_a_clear_message() {
        let server = closed_server();
        let mut chat = OllamaChat::new(server.clone());
        let req = PromptRequest {
            model: Some("m".to_string()),
            ..request(None)
        };
        assert_eq!(run(&mut chat, &req).1, error_event(NOT_RUNNING));
        assert_eq!(run(&mut chat, &request(None)).1, error_event(NOT_RUNNING));
        assert_eq!(read_model_list(&server), Err(NOT_RUNNING.to_string()));
    }

    /// Collects deltas until the streamed text is `text`.
    fn wait_for_text(rx: &Receiver<AiEvent>, text: &str) {
        let mut streamed = String::new();
        while streamed != text {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                AiEvent::Delta { text } => streamed.push_str(&text),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    fn wait_until(flag: &AtomicBool) {
        for _ in 0..250 {
            if flag.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the connection stayed open");
    }

    fn endless_server() -> FakeServer {
        FakeServer::start(|_, _| Reply::Endless(vec![delta("Hel"), delta("lo")]))
    }

    #[test]
    fn abort_ends_the_run_with_the_partial_text_and_keeps_it() {
        let server = endless_server();
        let mut chat = server.chat();
        chat.abort().unwrap();
        let req = PromptRequest {
            model: Some("m".to_string()),
            ..request(None)
        };
        let (sink, rx) = channel_sink();
        chat.prompt(&req, sink).unwrap();
        wait_for_text(&rx, "Hello");
        let (sink, _) = channel_sink();
        assert_eq!(
            chat.prompt(&req, sink),
            Err("Ollama is still working on the previous request.".to_string())
        );
        chat.abort().unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            done_event("Hello", true)
        );
        wait_until(&server.closed);
        assert!(rx.try_recv().is_err());

        let (sink, rx) = channel_sink();
        let follow_up = PromptRequest {
            instruction: "More".to_string(),
            ..request(None)
        };
        chat.prompt(&follow_up, sink).unwrap();
        wait_for_text(&rx, "Hello");
        assert_eq!(
            server.chat_bodies()[1]["messages"],
            json!([
                message("system", SYSTEM_PROMPT),
                message("user", &build_prompt(&req, true)),
                message("assistant", "Hello"),
                message("user", "More"),
            ])
        );
    }

    #[test]
    fn drop_closes_the_connection_without_an_event() {
        let server = endless_server();
        let mut chat = server.chat();
        let (sink, rx) = channel_sink();
        let req = PromptRequest {
            model: Some("m".to_string()),
            ..request(None)
        };
        chat.prompt(&req, sink).unwrap();
        wait_for_text(&rx, "Hello");
        drop(chat);
        wait_until(&server.closed);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn list_models_reads_tags_and_thinking_capabilities() {
        let server = FakeServer::start(|target, body| match target {
            "GET /api/tags" => tags(&["qwen3:0.6b", "llama3.2"]),
            _ if body["model"] == "qwen3:0.6b" => Reply::Json(
                200,
                json!({"capabilities": ["completion", "tools", "thinking"]}).to_string(),
            ),
            _ => Reply::Json(200, json!({"capabilities": ["completion"]}).to_string()),
        });
        let list = read_model_list(&server.server).unwrap();
        assert_eq!(
            list,
            ModelList {
                models: vec![
                    Model {
                        id: "qwen3:0.6b".to_string(),
                        name: "qwen3:0.6b".to_string(),
                        group: String::new(),
                        thinking_levels: vec!["off".to_string(), "on".to_string()],
                    },
                    Model {
                        id: "llama3.2".to_string(),
                        name: "llama3.2".to_string(),
                        group: String::new(),
                        thinking_levels: Vec::new(),
                    },
                ],
                default_model: None,
                default_thinking: None,
            }
        );
        let shown: Vec<(String, Value)> = server
            .requests()
            .into_iter()
            .filter(|(target, _)| target == "POST /api/show")
            .collect();
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].1, json!({"model": "qwen3:0.6b"}));
    }

    /// End-to-end check with the real Ollama server and a small thinking
    /// model. Run with: OLLAMA_TEST_MODEL=qwen3:0.6b cargo test --lib
    /// ollama::tests::real_ollama -- --ignored --nocapture
    #[test]
    #[ignore = "needs a running Ollama server with a pulled model"]
    fn real_ollama_lists_streams_refines_and_aborts() {
        let list = list_models("ollama", "").unwrap();
        for model in &list.models {
            println!("model {} {:?}", model.id, model.thinking_levels);
        }
        let model = std::env::var("OLLAMA_TEST_MODEL").unwrap_or_else(|_| "qwen3:0.6b".into());
        assert!(list.models.iter().any(|m| m.id == model));

        let mut chat = start("ollama", "").unwrap();
        let (sink, rx) = channel_sink();
        let missing = PromptRequest {
            model: Some("nope-missing-model".to_string()),
            ..request(None)
        };
        chat.prompt(&missing, sink).unwrap();
        let (_, end) = collect_run(&rx);
        println!("missing model: {end:?}");
        assert!(matches!(end, AiEvent::Error { .. }));

        let first = PromptRequest {
            instruction: "Continue writing with one short sentence.".to_string(),
            before: "# Cats\n\nCats sleep a lot. ".to_string(),
            after: String::new(),
            selection: None,
            thinking: Some("off".to_string()),
            model: Some(model),
            read_only: false,
        };
        let (sink, rx) = channel_sink();
        chat.prompt(&first, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("first: {streamed:?} / {end:?}");
        assert!(!streamed.is_empty());
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

        let refine = PromptRequest {
            instruction: "Now write it in German.".to_string(),
            thinking: Some("on".to_string()),
            model: None,
            ..first.clone()
        };
        let (sink, rx) = channel_sink();
        chat.prompt(&refine, sink).unwrap();
        let (streamed, end) = collect_run(&rx);
        println!("refine: {streamed:?} / {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: false, .. }));

        let long = PromptRequest {
            instruction: "Write a 600 word essay about cats.".to_string(),
            thinking: Some("off".to_string()),
            ..refine
        };
        let (sink, rx) = channel_sink();
        chat.prompt(&long, sink).unwrap();
        let first_delta = rx.recv_timeout(Duration::from_secs(120)).expect("no delta");
        assert!(matches!(first_delta, AiEvent::Delta { .. }));
        chat.abort().unwrap();
        let (_, end) = collect_run(&rx);
        println!("abort: {end:?}");
        assert!(matches!(end, AiEvent::Done { aborted: true, .. }));
        assert!(chat.is_alive());
    }
}

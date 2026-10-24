//! Inline AI for the editor popover. Each provider (pi, Claude Code, ...) is a
//! backend behind the `Conversation` trait. One conversation per popover.
//! Events stream to the requesting webview through a Tauri `Channel`.

mod claude;
mod codex;
mod ollama;
mod opencode;
mod pi;
pub(crate) mod process;
pub(crate) mod rpc;

use crate::{check_cli_exists, get_expanded_path, AppState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, Window};

pub(crate) const SYSTEM_PROMPT: &str =
    "You are a writing assistant inside a markdown note editor. \
Follow the user's instruction. Output only the resulting markdown, no preamble, \
no code fences around the whole answer. Do not repeat the rest of the document.";

/// Maximum characters of surrounding document sent as context.
const CONTEXT_BUDGET_CHARS: usize = 24_000;
const TRUNCATION_MARK: &str = "[…]";
const SELECTION_START: &str = "⟦selection⟧";
const SELECTION_END: &str = "⟦/selection⟧";
const CURSOR: &str = "⟦cursor⟧";

/// An AI tool that can run the inline AI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Pi,
    Claude,
    Codex,
    OpenCode,
    Ollama,
}

impl Provider {
    /// All providers, in the order of the settings and of the automatic choice.
    pub const ALL: [Provider; 5] = [
        Provider::Pi,
        Provider::Claude,
        Provider::Codex,
        Provider::OpenCode,
        Provider::Ollama,
    ];

    /// The name for messages to the user.
    pub fn name(self) -> &'static str {
        match self {
            Provider::Pi => "pi",
            Provider::Claude => "Claude Code",
            Provider::Codex => "Codex",
            Provider::OpenCode => "OpenCode",
            Provider::Ollama => "Ollama",
        }
    }

    /// The executable on the search path.
    fn command(self) -> &'static str {
        match self {
            Provider::Pi => "pi",
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::OpenCode => "opencode",
            Provider::Ollama => "ollama",
        }
    }
}

/// Prompt input from the editor. `before` and `after` are the document
/// markdown on each side of the selection (or cursor).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub instruction: String,
    pub before: String,
    pub after: String,
    /// Selected text as markdown. `None` in cursor mode.
    pub selection: Option<String>,
    /// Thinking level for this prompt. `None` keeps the current level.
    #[serde(default)]
    pub thinking: Option<String>,
    /// Model id (`Model::id`) for this prompt. `None` keeps the current
    /// model (the tool default for a new conversation).
    #[serde(default)]
    pub model: Option<String>,
    /// The answer is for the user to read (for example an explanation). It
    /// does not replace the selection.
    #[serde(default)]
    pub read_only: bool,
}

impl PromptRequest {
    /// The model of this prompt, without blanks. `None` for the default.
    pub fn model(&self) -> Option<&str> {
        self.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
    }

    /// The thinking level of this prompt, without blanks. `None` for the default.
    pub fn thinking(&self) -> Option<&str> {
        self.thinking
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
    }
}

/// Event sent to the frontend for one prompt run.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AiEvent {
    Delta { text: String },
    Done { text: String, aborted: bool },
    Error { message: String },
}

/// Receives the events of one run.
pub type Sink = Box<dyn Fn(AiEvent) + Send>;

/// The current run of a conversation. A backend shares it with the thread
/// that reads the tool output.
#[derive(Default)]
pub struct Run {
    sink: Option<Sink>,
    text: String,
}

impl Run {
    pub fn is_active(&self) -> bool {
        self.sink.is_some()
    }

    /// Starts a run. Fails while the previous run of `tool` is active.
    pub fn start(&mut self, tool: &str, sink: Sink) -> Result<(), String> {
        if self.is_active() {
            return Err(format!("{tool} is still working on the previous request."));
        }
        self.sink = Some(sink);
        self.text.clear();
        Ok(())
    }

    /// Streams `text`. Does nothing without an active run.
    pub fn delta(&mut self, text: &str) {
        if let Some(sink) = &self.sink {
            self.text.push_str(text);
            sink(AiEvent::Delta {
                text: text.to_string(),
            });
        }
    }

    /// Ends the run with `Done`. `text` is the final answer; `None` keeps
    /// the streamed text.
    pub fn finish(&mut self, text: Option<String>, aborted: bool) {
        if let Some(sink) = self.sink.take() {
            let text = text.unwrap_or_else(|| std::mem::take(&mut self.text));
            sink(AiEvent::Done { text, aborted });
        }
    }

    /// Ends the run with `Error`.
    pub fn fail(&mut self, message: String) {
        if let Some(sink) = self.sink.take() {
            sink(AiEvent::Error { message });
        }
    }
}

/// A model for the model selects.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    /// The value for `PromptRequest::model`.
    pub id: String,
    pub name: String,
    /// The group in the select, for example the model provider. Can be empty.
    pub group: String,
    /// Thinking levels that the model supports. Empty when the tool does not
    /// tell, or the model has no levels.
    pub thinking_levels: Vec<String>,
}

/// The models of a provider, and the model and thinking level that it uses
/// when it starts (if the tool tells).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelList {
    pub models: Vec<Model>,
    pub default_model: Option<String>,
    pub default_thinking: Option<String>,
}

/// One popover conversation with an AI tool. Dropping it stops the tool.
pub trait Conversation: Send {
    /// Sends one prompt. Events for this run go to `sink` until a `Done` or
    /// `Error` event. The first prompt carries the document as context
    /// (`build_prompt`); follow-ups send only the instruction.
    fn prompt(&mut self, request: &PromptRequest, sink: Sink) -> Result<(), String>;

    /// Stops the current run, which then ends with `Done { aborted: true }`.
    /// Does nothing when no run is active.
    fn abort(&mut self) -> Result<(), String>;

    /// False when the conversation cannot take more prompts (the tool
    /// exited). The next prompt then starts a new conversation.
    fn is_alive(&self) -> bool;
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
pub(crate) fn build_prompt(request: &PromptRequest, include_context: bool) -> String {
    let instruction = request.instruction.trim();
    if !include_context {
        return instruction.to_string();
    }
    let (before, after) = truncate_context(&request.before, &request.after, CONTEXT_BUDGET_CHARS);
    match &request.selection {
        Some(selection) => {
            let reply = if request.read_only {
                "Reply with markdown for the user to read. Do not rewrite the selected text."
            } else {
                "Reply with only the markdown that replaces the selected text."
            };
            format!(
                "<document>\n{before}{SELECTION_START}{selection}{SELECTION_END}{after}\n</document>\n\n\
                 The selected text is marked with {SELECTION_START} and {SELECTION_END} in the document above:\n\
                 <selection>\n{selection}\n</selection>\n\n\
                 Instruction: {instruction}\n\n\
                 {reply}"
            )
        }
        None => format!(
            "<document>\n{before}{CURSOR}{after}\n</document>\n\n\
             The cursor position is marked with {CURSOR} in the document above.\n\n\
             Instruction: {instruction}\n\n\
             Reply with only the markdown to insert at the cursor."
        ),
    }
}

/// Marks the PATH in the output of the login shell, which can also print
/// other text (for example a welcome message).
const PATH_MARK: &str = "__ITCH_PATH__";
/// Time the login shell has to print its PATH.
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// The text between the first two `PATH_MARK`s. `None` when it is empty or
/// not there.
fn parse_marked_path(output: &str) -> Option<String> {
    let start = output.find(PATH_MARK)? + PATH_MARK.len();
    let len = output[start..].find(PATH_MARK)?;
    Some(output[start..start + len].to_string()).filter(|path| !path.is_empty())
}

/// Asks `shell` as an interactive login shell for its PATH. `None` when
/// the shell fails or does not answer within `timeout`.
#[cfg(unix)]
fn read_login_shell_path(shell: &str, timeout: Duration) -> Option<String> {
    use process::{kill_tree, SpawnTree};
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::mpsc;
    let mut child = std::process::Command::new(shell)
        .args([
            "-ilc",
            &format!("printf '{PATH_MARK}%s{PATH_MARK}' \"$PATH\""),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_tree()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stdout.read_to_string(&mut output);
        let _ = tx.send(output);
    });
    let output = rx.recv_timeout(timeout);
    kill_tree(&mut child);
    parse_marked_path(&output.ok()?)
}

/// The PATH of the user's login shell, read once. An app that starts from
/// the Finder does not get it, but it decides which tool (and which version)
/// the user runs in a terminal.
fn login_shell_path() -> Option<&'static str> {
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    PATH.get_or_init(|| {
        #[cfg(unix)]
        {
            read_login_shell_path(&std::env::var("SHELL").ok()?, LOGIN_SHELL_TIMEOUT)
        }
        #[cfg(not(unix))]
        {
            None
        }
    })
    .as_deref()
}

/// Reads the login shell PATH in the background, so that the first AI menu
/// does not wait for it.
pub fn warm_up() {
    std::thread::spawn(|| {
        login_shell_path();
    });
}

/// The login shell PATH first, so that Itch runs the same tools as a
/// terminal. Then the default install folders of pi and OpenCode, and the
/// expanded PATH.
fn search_path() -> String {
    let mut parts: Vec<String> = login_shell_path().map(str::to_string).into_iter().collect();
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            parts.push(format!("{home}/.pi/agent/bin:{home}/.opencode/bin"));
        }
    }
    parts.push(get_expanded_path());
    parts.join(":")
}

/// Returns the program to run for `provider`: the pi path from settings, or
/// the command on the search path.
fn resolve_program(
    provider: Provider,
    pi_path: Option<&str>,
    search_path: &str,
) -> Result<String, String> {
    let name = provider.name();
    if provider == Provider::Pi {
        if let Some(path) = pi_path.map(str::trim).filter(|p| !p.is_empty()) {
            return if std::path::Path::new(path).is_file() {
                Ok(path.to_string())
            } else {
                Err(format!(
                    "pi not found at {path}. Check the pi path in Settings."
                ))
            };
        }
    }
    let command = provider.command();
    if check_cli_exists(command, search_path)? {
        Ok(command.to_string())
    } else if provider == Provider::Pi {
        Err("pi not found. Install pi or set its path in Settings.".to_string())
    } else {
        Err(format!(
            "{name} not found. Install {name} or choose another AI tool in Settings."
        ))
    }
}

/// Starts a conversation with `provider`.
fn start(provider: Provider, program: &str, path: &str) -> Result<Box<dyn Conversation>, String> {
    match provider {
        Provider::Pi => Ok(Box::new(pi::PiProcess::spawn(
            program,
            &pi::build_args(),
            path,
        )?)),
        Provider::Claude => claude::start(program, path),
        Provider::Codex => codex::start(program, path),
        Provider::OpenCode => Ok(Box::new(opencode::start(program, path)?)),
        Provider::Ollama => ollama::start(program, path),
    }
}

/// Asks `provider` for its models.
fn list_models(provider: Provider, program: &str, path: &str) -> Result<ModelList, String> {
    match provider {
        Provider::Pi => pi::list_models(program, path).map(ModelList::from),
        Provider::Claude => claude::list_models(program, path),
        Provider::Codex => codex::list_models(program, path),
        Provider::OpenCode => opencode::list_models(program, path),
        Provider::Ollama => ollama::list_models(program, path),
    }
}

fn pi_path(app_state: &AppState) -> Option<String> {
    app_state
        .settings
        .read()
        .expect("settings read lock")
        .pi_path
        .clone()
}

/// Returns the installed AI tools, in the order of `Provider::ALL`.
#[tauri::command]
pub async fn ai_providers(app_state: State<'_, AppState>) -> Result<Vec<Provider>, String> {
    let pi_path = pi_path(&app_state);
    tauri::async_runtime::spawn_blocking(move || {
        let path = search_path();
        Provider::ALL
            .into_iter()
            .filter(|provider| resolve_program(*provider, pi_path.as_deref(), &path).is_ok())
            .collect()
    })
    .await
    .map_err(|e| format!("Failed to find the AI tools: {e}"))
}

/// The model list of each provider and program, kept for the app session.
#[derive(Default)]
pub struct AiModels(Mutex<HashMap<Provider, (String, ModelList)>>);

/// Returns the models of `provider`. Asks the tool once per app session, or
/// again when `refresh` is true.
#[tauri::command]
pub async fn ai_list_models(
    app: AppHandle,
    app_state: State<'_, AppState>,
    provider: Provider,
    refresh: Option<bool>,
) -> Result<ModelList, String> {
    let pi_path = pi_path(&app_state);
    tauri::async_runtime::spawn_blocking(move || {
        let path = search_path();
        let program = resolve_program(provider, pi_path.as_deref(), &path)?;
        let cache = app.state::<AiModels>();
        // Hold the lock while the tool answers, so that one tool starts at a time.
        let mut cached = cache.0.lock().map_err(|_| "The model list is broken.")?;
        if let Some((cached_program, models)) = cached.get(&provider) {
            if *cached_program == program && refresh != Some(true) {
                return Ok(models.clone());
            }
        }
        let models = list_models(provider, &program, &path)?;
        cached.insert(provider, (program, models.clone()));
        Ok(models)
    })
    .await
    .map_err(|e| format!("Failed to run {}: {e}", provider.name()))?
}

struct Session {
    provider: Provider,
    conversation: Box<dyn Conversation>,
}

type SharedSession = Arc<Mutex<Session>>;

#[derive(Default)]
struct SessionMap {
    /// Open conversations, by session id, with the label of their window.
    open: HashMap<String, (String, SharedSession)>,
    /// Sessions whose tool starts now, with the label of their window.
    starting: HashMap<String, String>,
    /// Starting sessions that were closed. Their tool stops when it started.
    cancelled: HashSet<String>,
}

/// Open popover conversations. The map lock is held only for short map
/// changes: a tool starts, and a prompt runs, without it. So a slow tool
/// does not block the other sessions, and a session that closes while its
/// tool starts does not leave the tool running.
#[derive(Default)]
pub struct AiSessions(Mutex<SessionMap>);

impl AiSessions {
    fn map(&self) -> Result<std::sync::MutexGuard<'_, SessionMap>, String> {
        self.0
            .lock()
            .map_err(|_| "AI sessions are broken.".to_string())
    }

    fn get(&self, session_id: &str) -> Result<Option<SharedSession>, String> {
        Ok(self
            .map()?
            .open
            .get(session_id)
            .map(|(_, session)| Arc::clone(session)))
    }

    /// Sends a prompt in the conversation `session_id` of `window`. Calls
    /// `start` for the first prompt, or again when the tool exited.
    fn prompt(
        &self,
        session_id: &str,
        window: &str,
        provider: Provider,
        start: impl FnOnce() -> Result<Box<dyn Conversation>, String>,
        request: &PromptRequest,
        sink: Sink,
    ) -> Result<(), String> {
        if let Some(session) = self.get(session_id)? {
            let mut session = session.lock().map_err(|_| "AI session is broken.")?;
            if session.provider == provider && session.conversation.is_alive() {
                return session.conversation.prompt(request, sink);
            }
        }
        let old = {
            let mut map = self.map()?;
            map.starting
                .insert(session_id.to_string(), window.to_string());
            map.open.remove(session_id)
        };
        // Stop an exited tool before a new one starts.
        drop(old);
        let started = start();
        let session = {
            let mut map = self.map()?;
            map.starting.remove(session_id);
            if map.cancelled.remove(session_id) {
                None
            } else {
                let session = Arc::new(Mutex::new(Session {
                    provider,
                    conversation: started?,
                }));
                map.open.insert(
                    session_id.to_string(),
                    (window.to_string(), Arc::clone(&session)),
                );
                Some(session)
            }
        };
        // The session closed while its tool started: `started` stops the tool.
        let Some(session) = session else {
            return Ok(());
        };
        let mut session = session.lock().map_err(|_| "AI session is broken.")?;
        session.conversation.prompt(request, sink)
    }

    fn abort(&self, session_id: &str) -> Result<(), String> {
        match self.get(session_id)? {
            Some(session) => session
                .lock()
                .map_err(|_| "AI session is broken.")?
                .conversation
                .abort(),
            None => Ok(()),
        }
    }

    /// Closes the sessions for which `closes` is true. A tool stops when the
    /// last prompt that uses it returns.
    fn close_where(&self, closes: impl Fn(&str, &str) -> bool) {
        let removed: Vec<(String, SharedSession)> = match self.map() {
            Ok(mut map) => {
                let starting: Vec<String> = map
                    .starting
                    .iter()
                    .filter(|(id, window)| closes(id, window))
                    .map(|(id, _)| id.clone())
                    .collect();
                map.cancelled.extend(starting);
                let ids: Vec<String> = map
                    .open
                    .iter()
                    .filter(|(id, (window, _))| closes(id, window))
                    .map(|(id, _)| id.clone())
                    .collect();
                ids.iter().filter_map(|id| map.open.remove(id)).collect()
            }
            Err(_) => return,
        };
        drop(removed);
    }

    fn close(&self, session_id: &str) {
        self.close_where(|id, _| id == session_id);
    }

    fn close_window(&self, window_label: &str) {
        self.close_where(|_, window| window == window_label);
    }

    fn close_all(&self) {
        self.close_where(|_, _| true);
    }
}

/// Sends a prompt in the conversation `session_id`. Starts the tool for the
/// first prompt, or again when it exited.
#[tauri::command]
pub async fn ai_prompt(
    window: Window,
    app_state: State<'_, AppState>,
    provider: Provider,
    session_id: String,
    request: PromptRequest,
    on_event: Channel<AiEvent>,
) -> Result<(), String> {
    let pi_path = pi_path(&app_state);
    let app = window.app_handle().clone();
    let label = window.label().to_string();

    tauri::async_runtime::spawn_blocking(move || {
        let start = || {
            let path = search_path();
            let program = resolve_program(provider, pi_path.as_deref(), &path)?;
            start(provider, &program, &path)
        };
        app.state::<AiSessions>().prompt(
            &session_id,
            &label,
            provider,
            start,
            &request,
            Box::new(move |event| {
                let _ = on_event.send(event);
            }),
        )
    })
    .await
    .map_err(|e| format!("Failed to run {}: {e}", provider.name()))?
}

#[tauri::command]
pub async fn ai_abort(sessions: State<'_, AiSessions>, session_id: String) -> Result<(), String> {
    sessions.abort(&session_id)
}

#[tauri::command]
pub async fn ai_close(sessions: State<'_, AiSessions>, session_id: String) -> Result<(), String> {
    sessions.close(&session_id);
    Ok(())
}

/// Stops the AI tools of a closed window.
pub fn close_window_sessions(app: &AppHandle, window_label: &str) {
    if let Some(sessions) = app.try_state::<AiSessions>() {
        sessions.close_window(window_label);
    }
}

/// Stops all AI tools. Call when the app exits.
pub fn close_all(app: &AppHandle) {
    if let Some(sessions) = app.try_state::<AiSessions>() {
        sessions.close_all();
    }
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use test_support::request;

    #[test]
    fn reads_the_request_that_the_editor_sends() {
        let request: PromptRequest = serde_json::from_value(serde_json::json!({
            "instruction": "Explain",
            "before": "",
            "after": "",
            "selection": "text",
            "thinking": "low",
            "model": "anthropic/claude-haiku-5-5",
            "readOnly": true,
        }))
        .unwrap();
        assert!(request.read_only);
        assert_eq!(request.thinking(), Some("low"));
        assert_eq!(request.model(), Some("anthropic/claude-haiku-5-5"));
    }

    #[test]
    fn run_streams_and_finishes_once() {
        let (sink, rx) = test_support::channel_sink();
        let mut run = Run::default();
        run.start("tool", sink).unwrap();
        let (other, _) = test_support::channel_sink();
        assert_eq!(
            run.start("tool", other),
            Err("tool is still working on the previous request.".to_string())
        );
        run.delta("Hel");
        run.delta("lo");
        run.finish(None, false);
        run.fail("late".to_string());
        assert_eq!(
            test_support::collect_run(&rx),
            (
                "Hello".to_string(),
                AiEvent::Done {
                    text: "Hello".to_string(),
                    aborted: false
                }
            )
        );
        assert!(rx.try_recv().is_err());
        assert!(!run.is_active());
    }

    #[test]
    fn run_final_text_replaces_the_streamed_text() {
        let (sink, rx) = test_support::channel_sink();
        let mut run = Run::default();
        run.start("tool", sink).unwrap();
        run.delta("draft");
        run.finish(Some("final".to_string()), true);
        assert_eq!(
            test_support::collect_run(&rx).1,
            AiEvent::Done {
                text: "final".to_string(),
                aborted: true
            }
        );
    }

    #[test]
    fn blank_model_and_thinking_mean_the_default() {
        let req = PromptRequest {
            model: Some("  ".to_string()),
            thinking: Some(String::new()),
            ..request(None)
        };
        assert_eq!(req.model(), None);
        assert_eq!(req.thinking(), None);
    }

    /// A conversation that counts prompts and tells when it is dropped.
    struct FakeConversation {
        prompts: Arc<std::sync::atomic::AtomicUsize>,
        dropped: mpsc::Sender<()>,
        alive: bool,
    }

    impl Conversation for FakeConversation {
        fn prompt(&mut self, _: &PromptRequest, _: Sink) -> Result<(), String> {
            self.prompts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn abort(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn is_alive(&self) -> bool {
            self.alive
        }
    }

    impl Drop for FakeConversation {
        fn drop(&mut self) {
            let _ = self.dropped.send(());
        }
    }

    struct Fake {
        prompts: Arc<std::sync::atomic::AtomicUsize>,
        dropped: mpsc::Receiver<()>,
        conversation: Option<FakeConversation>,
    }

    fn fake(alive: bool) -> Fake {
        let prompts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, dropped) = mpsc::channel();
        Fake {
            prompts: Arc::clone(&prompts),
            dropped,
            conversation: Some(FakeConversation {
                prompts,
                dropped: tx,
                alive,
            }),
        }
    }

    impl Fake {
        fn prompts(&self) -> usize {
            self.prompts.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn is_dropped(&self) -> bool {
            self.dropped
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok()
        }
    }

    fn sink() -> Sink {
        Box::new(|_| {})
    }

    /// Starts `fake` in the session `id` on a thread. The start waits until
    /// `release` gets a message. `entered` gets a message when it starts.
    fn start_in_background(
        sessions: &Arc<AiSessions>,
        id: &str,
        window: &str,
        conversation: FakeConversation,
    ) -> (
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        std::thread::JoinHandle<Result<(), String>>,
    ) {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let (sessions, id, window) = (Arc::clone(sessions), id.to_string(), window.to_string());
        let handle = std::thread::spawn(move || {
            sessions.prompt(
                &id,
                &window,
                Provider::Pi,
                move || {
                    let _ = entered_tx.send(());
                    let _ = release_rx.recv();
                    Ok(Box::new(conversation) as Box<dyn Conversation>)
                },
                &request(None),
                sink(),
            )
        });
        (entered, release, handle)
    }

    #[test]
    fn sessions_start_once_and_reuse_the_conversation() {
        let sessions = AiSessions::default();
        let mut first = fake(true);
        let conversation = first.conversation.take().unwrap();
        let start = || Ok(Box::new(conversation) as Box<dyn Conversation>);
        sessions
            .prompt("s", "w", Provider::Pi, start, &request(None), sink())
            .unwrap();
        let unused = || Err::<Box<dyn Conversation>, String>("no second start".to_string());
        sessions
            .prompt("s", "w", Provider::Pi, unused, &request(None), sink())
            .unwrap();
        assert_eq!(first.prompts(), 2);
        sessions.close("s");
        assert!(first.is_dropped());
    }

    #[test]
    fn sessions_replace_an_exited_conversation() {
        let sessions = AiSessions::default();
        let mut dead = fake(false);
        let conversation = dead.conversation.take().unwrap();
        let start = || Ok(Box::new(conversation) as Box<dyn Conversation>);
        sessions
            .prompt("s", "w", Provider::Pi, start, &request(None), sink())
            .unwrap();
        let mut next = fake(true);
        let conversation = next.conversation.take().unwrap();
        let start = || Ok(Box::new(conversation) as Box<dyn Conversation>);
        sessions
            .prompt("s", "w", Provider::Pi, start, &request(None), sink())
            .unwrap();
        assert!(dead.is_dropped());
        assert_eq!(next.prompts(), 1);
    }

    #[test]
    fn closing_during_the_start_stops_the_new_tool() {
        let sessions = Arc::new(AiSessions::default());
        let mut tool = fake(true);
        let (entered, release, handle) =
            start_in_background(&sessions, "s", "w", tool.conversation.take().unwrap());
        entered.recv().unwrap();
        sessions.close("s");
        release.send(()).unwrap();
        assert_eq!(handle.join().unwrap(), Ok(()));
        assert!(tool.is_dropped());
        assert_eq!(tool.prompts(), 0);
        assert!(sessions.0.lock().unwrap().open.is_empty());
    }

    #[test]
    fn closing_the_window_during_the_start_stops_the_new_tool() {
        let sessions = Arc::new(AiSessions::default());
        let mut tool = fake(true);
        let (entered, release, handle) =
            start_in_background(&sessions, "s", "w", tool.conversation.take().unwrap());
        entered.recv().unwrap();
        sessions.close_window("other");
        sessions.close_window("w");
        release.send(()).unwrap();
        assert_eq!(handle.join().unwrap(), Ok(()));
        assert!(tool.is_dropped());
        assert_eq!(tool.prompts(), 0);
    }

    #[test]
    fn a_slow_start_does_not_block_other_sessions() {
        let sessions = Arc::new(AiSessions::default());
        let mut slow = fake(true);
        let (entered, release, handle) =
            start_in_background(&sessions, "slow", "w", slow.conversation.take().unwrap());
        entered.recv().unwrap();
        // While "slow" starts, other sessions work.
        let mut other = fake(true);
        let conversation = other.conversation.take().unwrap();
        let start = || Ok(Box::new(conversation) as Box<dyn Conversation>);
        sessions
            .prompt("other", "w", Provider::Pi, start, &request(None), sink())
            .unwrap();
        sessions.abort("other").unwrap();
        sessions.close("other");
        assert!(other.is_dropped());
        release.send(()).unwrap();
        assert_eq!(handle.join().unwrap(), Ok(()));
        assert_eq!(slow.prompts(), 1);
        sessions.close_all();
        assert!(slow.is_dropped());
    }

    #[test]
    fn reads_the_path_between_the_markers() {
        assert_eq!(
            parse_marked_path("Welcome!\n__ITCH_PATH__/a/bin:/b/bin__ITCH_PATH__\n"),
            Some("/a/bin:/b/bin".to_string())
        );
        assert_eq!(parse_marked_path("no markers"), None);
        assert_eq!(parse_marked_path("__ITCH_PATH____ITCH_PATH__"), None);
    }

    #[cfg(unix)]
    #[test]
    fn reads_the_login_shell_path() {
        // A fake shell that prints noise, then runs the command it gets.
        let (_shell, shell) = test_support::fake_tool(
            "echo 'motd'; PATH=/login/bin:/usr/bin; export PATH; eval \"$2\"",
        );
        assert_eq!(
            read_login_shell_path(&shell, std::time::Duration::from_secs(10)),
            Some("/login/bin:/usr/bin".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_slow_login_shell_gives_no_path() {
        let (_shell, shell) = test_support::fake_tool("sleep 30");
        let started = std::time::Instant::now();
        assert_eq!(
            read_login_shell_path(&shell, std::time::Duration::from_millis(300)),
            None
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn providers_use_lowercase_names() {
        assert_eq!(
            serde_json::to_value(Provider::ALL).unwrap(),
            serde_json::json!(["pi", "claude", "codex", "opencode", "ollama"])
        );
        let provider: Provider = serde_json::from_value(serde_json::json!("opencode")).unwrap();
        assert_eq!(provider, Provider::OpenCode);
    }

    #[test]
    fn missing_tool_names_the_tool() {
        let empty = std::env::temp_dir().join("itch-empty-path");
        let _ = std::fs::create_dir_all(&empty);
        // `which` must stay on the search path.
        let path = format!("{}:/usr/bin:/bin", empty.display());
        assert_eq!(
            resolve_program(Provider::Claude, Some("/ignored"), &path),
            Err(
                "Claude Code not found. Install Claude Code or choose another AI tool in Settings."
                    .to_string()
            )
        );
        assert_eq!(
            resolve_program(Provider::Pi, None, &path),
            Err("pi not found. Install pi or set its path in Settings.".to_string())
        );
        assert_eq!(
            resolve_program(Provider::Pi, Some("/nope/pi"), &path),
            Err("pi not found at /nope/pi. Check the pi path in Settings.".to_string())
        );
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
}

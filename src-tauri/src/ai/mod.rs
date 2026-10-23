//! Inline AI for the editor popover. Each provider (pi, Claude Code, ...) is a
//! backend behind the `Conversation` trait. One conversation per popover.
//! Events stream to the requesting webview through a Tauri `Channel`.

mod claude;
mod codex;
mod opencode;
mod pi;
pub(crate) mod process;
pub(crate) mod rpc;

use crate::{check_cli_exists, get_expanded_path, AppState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
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

/// The expanded PATH, plus the default install folders of pi and OpenCode.
fn search_path() -> String {
    let expanded = get_expanded_path();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => {
            format!("{home}/.pi/agent/bin:{home}/.opencode/bin:{expanded}")
        }
        _ => expanded,
    }
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
        other => Err(format!("{} is not available yet.", other.name())),
    }
}

/// Asks `provider` for its models.
fn list_models(provider: Provider, program: &str, path: &str) -> Result<ModelList, String> {
    match provider {
        Provider::Pi => pi::list_models(program, path).map(ModelList::from),
        Provider::Claude => claude::list_models(program, path),
        Provider::Codex => codex::list_models(program, path),
        Provider::OpenCode => opencode::list_models(program, path),
        other => Err(format!("{} is not available yet.", other.name())),
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
    window: String,
    provider: Provider,
    conversation: Box<dyn Conversation>,
}

/// Open popover conversations, by session id.
#[derive(Default)]
pub struct AiSessions(Mutex<HashMap<String, Session>>);

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
        let sessions = app.state::<AiSessions>();
        let mut map = sessions.0.lock().map_err(|_| "AI sessions are broken.")?;
        let usable = map
            .get(&session_id)
            .is_some_and(|s| s.provider == provider && s.conversation.is_alive());
        if !usable {
            // Stop an exited tool before a new one starts.
            map.remove(&session_id);
            let path = search_path();
            let program = resolve_program(provider, pi_path.as_deref(), &path)?;
            let conversation = start(provider, &program, &path)?;
            map.insert(
                session_id.clone(),
                Session {
                    window: label,
                    provider,
                    conversation,
                },
            );
        }
        let session = map.get_mut(&session_id).ok_or("AI session not found.")?;
        session.conversation.prompt(
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
    let mut map = sessions.0.lock().map_err(|_| "AI sessions are broken.")?;
    match map.get_mut(&session_id) {
        Some(session) => session.conversation.abort(),
        None => Ok(()),
    }
}

#[tauri::command]
pub async fn ai_close(sessions: State<'_, AiSessions>, session_id: String) -> Result<(), String> {
    let removed = sessions
        .0
        .lock()
        .map_err(|_| "AI sessions are broken.")?
        .remove(&session_id);
    drop(removed);
    Ok(())
}

/// Stops the AI tools of a closed window.
pub fn close_window_sessions(app: &AppHandle, window_label: &str) {
    let Some(sessions) = app.try_state::<AiSessions>() else {
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

/// Stops all AI tools. Call when the app exits.
pub fn close_all(app: &AppHandle) {
    let Some(sessions) = app.try_state::<AiSessions>() else {
        return;
    };
    let removed: Vec<Session> = match sessions.0.lock() {
        Ok(mut map) => map.drain().map(|(_, s)| s).collect(),
        Err(_) => return,
    };
    drop(removed);
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
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

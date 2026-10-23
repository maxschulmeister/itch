//! A JSON-RPC 2.0 client for AI tools that speak it on stdin and stdout
//! (`codex app-server`, `opencode acp`).

use super::process::{JsonProcess, JsonWriter};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// Receives the messages that the tool sends on its own. Called on the
/// reader thread, in order.
pub trait RpcHandler: Send + 'static {
    fn notification(&mut self, method: &str, params: &Value);
    /// Answers a request from the tool, for example a permission request.
    fn request(&mut self, method: &str, params: &Value) -> Result<Value, String>;
    /// The tool exited. `message` is for the user.
    fn exited(&mut self, message: &str);
}

type Responder = Box<dyn FnOnce(Result<Value, String>) + Send>;

#[derive(Default)]
struct Pending {
    responders: HashMap<u64, Responder>,
    /// Set when the tool exited.
    exited: Option<String>,
}

pub struct Rpc {
    process: JsonProcess,
    writer: JsonWriter,
    next_id: u64,
    pending: Arc<Mutex<Pending>>,
    alive: Arc<AtomicBool>,
}

impl Rpc {
    pub fn spawn(
        tool: &'static str,
        command: Command,
        handler: impl RpcHandler,
    ) -> Result<Self, String> {
        let pending = Arc::new(Mutex::new(Pending::default()));
        let alive = Arc::new(AtomicBool::new(true));
        let handler = Arc::new(Mutex::new(handler));
        let (reader_pending, reader_handler) = (Arc::clone(&pending), Arc::clone(&handler));
        let (exit_pending, exit_alive) = (Arc::clone(&pending), Arc::clone(&alive));
        let process = JsonProcess::spawn(
            tool,
            command,
            move |value, writer| {
                if let Ok(mut handler) = reader_handler.lock() {
                    dispatch(&value, &reader_pending, &mut *handler, writer);
                }
            },
            move |message| {
                exit_alive.store(false, Ordering::SeqCst);
                let responders = match exit_pending.lock() {
                    Ok(mut pending) => {
                        pending.exited = Some(message.clone());
                        std::mem::take(&mut pending.responders)
                    }
                    Err(_) => HashMap::new(),
                };
                for (_, respond) in responders {
                    respond(Err(message.clone()));
                }
                if let Ok(mut handler) = handler.lock() {
                    handler.exited(&message);
                }
            },
        )?;
        let writer = process.writer();
        Ok(Self {
            process,
            writer,
            next_id: 0,
            pending,
            alive,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Sends a request. `respond` gets the result, or the error message.
    pub fn call_async(
        &mut self,
        method: &str,
        params: Value,
        respond: impl FnOnce(Result<Value, String>) + Send + 'static,
    ) -> Result<(), String> {
        self.next_id += 1;
        let id = self.next_id;
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| "The tool connection is broken.")?;
            if let Some(message) = &pending.exited {
                return Err(message.clone());
            }
            pending.responders.insert(id, Box::new(respond));
        }
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.writer.send(&message) {
            if let Ok(mut pending) = self.pending.lock() {
                pending.responders.remove(&id);
            }
            return Err(format!("Failed to send {method}: {e}"));
        }
        Ok(())
    }

    /// Sends a request and waits for the result.
    pub fn call(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let (tx, rx) = mpsc::channel();
        self.call_async(method, params, move |result| {
            let _ = tx.send(result);
        })?;
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => Err(format!("No answer to {method} in time.")),
        }
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.process
            .send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .map_err(|e| format!("Failed to send {method}: {e}"))
    }

    #[cfg(test)]
    pub fn id(&self) -> u32 {
        self.process.id()
    }
}

fn dispatch(
    message: &Value,
    pending: &Mutex<Pending>,
    handler: &mut impl RpcHandler,
    writer: &JsonWriter,
) {
    let method = message["method"].as_str();
    match (message.get("id"), method) {
        // A response to one of our requests.
        (Some(id), None) => {
            let respond = id.as_u64().and_then(|id| {
                pending
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.responders.remove(&id))
            });
            if let Some(respond) = respond {
                respond(match message.get("error") {
                    Some(error) => Err(error["message"]
                        .as_str()
                        .unwrap_or("The tool returned an error.")
                        .to_string()),
                    None => Ok(message["result"].clone()),
                });
            }
        }
        // A request from the tool.
        (Some(id), Some(method)) => {
            let reply = match handler.request(method, &message["params"]) {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(error) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": error }
                }),
            };
            let _ = writer.send(&reply);
        }
        (None, Some(method)) => handler.notification(method, &message["params"]),
        (None, None) => {}
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ai::test_support::{fake_tool, is_running, TempFile};

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl RpcHandler for Recorder {
        fn notification(&mut self, method: &str, params: &Value) {
            self.0.lock().unwrap().push(format!("{method} {params}"));
        }
        fn request(&mut self, method: &str, _params: &Value) -> Result<Value, String> {
            match method {
                "ask" => Ok(json!({ "answer": 42 })),
                _ => Err(format!("{method} is not supported")),
            }
        }
        fn exited(&mut self, message: &str) {
            self.0.lock().unwrap().push(format!("exit {message}"));
        }
    }

    /// A fake tool that answers each request with a notification and a
    /// result (or an error for `fail`), asks the client once, and logs the
    /// lines it reads.
    fn echo_tool(log: &TempFile) -> (TempFile, String) {
        fake_tool(&format!(
            r#"printf '%s\n' '{{"jsonrpc":"2.0","id":"s1","method":"ask","params":{{}}}}'
printf '%s\n' '{{"jsonrpc":"2.0","id":"s2","method":"other","params":{{}}}}'
while read line; do
printf '%s\n' "$line" >> '{log}'
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
case "$line" in
*'"method":"fail"'*) printf '%s\n' "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{{\"code\":1,\"message\":\"bad\"}}}}";;
*'"method":"quit"'*) echo 'Error: bye' >&2; exit 1;;
*'"method"'*) [ -n "$id" ] && printf '%s\n' '{{"jsonrpc":"2.0","method":"note","params":{{"n":1}}}}' "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{{\"ok\":$id}}}}";;
esac
done"#,
            log = log.path.display()
        ))
    }

    #[test]
    fn calls_get_results_errors_and_notifications_in_order() {
        let log = TempFile::new("rpc-log");
        let (_file, program) = echo_tool(&log);
        let recorder = Recorder::default();
        let mut rpc = Rpc::spawn("tool", Command::new(program), recorder.clone()).unwrap();
        let timeout = Duration::from_secs(10);
        assert_eq!(rpc.call("one", json!({}), timeout), Ok(json!({ "ok": 1 })));
        assert_eq!(rpc.call("fail", json!({}), timeout), Err("bad".to_string()));
        rpc.notify("hello", json!({})).unwrap();
        assert_eq!(rpc.call("two", json!({}), timeout), Ok(json!({ "ok": 3 })));
        assert_eq!(
            recorder.0.lock().unwrap().as_slice(),
            ["note {\"n\":1}", "note {\"n\":1}"]
        );
        // The tool got our answers to its requests.
        let lines = log.read();
        assert!(
            lines.contains(r#""id":"s1","jsonrpc":"2.0","result":{"answer":42}"#),
            "{lines}"
        );
        assert!(
            lines.contains(r#""message":"other is not supported""#),
            "{lines}"
        );
    }

    #[test]
    fn exit_fails_pending_calls_and_tells_the_handler() {
        let log = TempFile::new("rpc-exit");
        let (_file, program) = echo_tool(&log);
        let recorder = Recorder::default();
        let mut rpc = Rpc::spawn("tool", Command::new(program), recorder.clone()).unwrap();
        assert_eq!(
            rpc.call("quit", json!({}), Duration::from_secs(10)),
            Err("tool stopped unexpectedly: Error: bye".to_string())
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(!rpc.is_alive());
        assert_eq!(
            recorder.0.lock().unwrap().last().map(String::as_str),
            Some("exit tool stopped unexpectedly: Error: bye")
        );
        assert_eq!(
            rpc.call("again", json!({}), Duration::from_secs(10)),
            Err("tool stopped unexpectedly: Error: bye".to_string())
        );
    }

    #[test]
    fn dropping_the_client_kills_the_tool() {
        let log = TempFile::new("rpc-drop");
        let (_file, program) = echo_tool(&log);
        let rpc = Rpc::spawn("tool", Command::new(program), Recorder::default()).unwrap();
        let pid = rpc.id();
        drop(rpc);
        assert!(!is_running(pid));
    }
}

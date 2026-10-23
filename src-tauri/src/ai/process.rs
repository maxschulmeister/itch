//! AI tools that run as a child process and talk JSON lines on stdin and
//! stdout.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

const STDERR_TAIL_BYTES: usize = 4096;

/// Writes JSON lines to the stdin of a tool. Clones share the same stdin.
#[derive(Clone)]
pub struct JsonWriter(Arc<Mutex<ChildStdin>>);

impl JsonWriter {
    pub fn send(&self, value: &Value) -> std::io::Result<()> {
        let mut line = value.to_string();
        line.push('\n');
        let mut stdin = self
            .0
            .lock()
            .map_err(|_| std::io::Error::other("stdin is broken"))?;
        stdin.write_all(line.as_bytes())?;
        stdin.flush()
    }
}

/// A running tool. Dropping it kills the process.
pub struct JsonProcess {
    child: Child,
    writer: JsonWriter,
}

impl JsonProcess {
    /// Starts `command` in the temp folder. Each stdout line that is a JSON
    /// value goes to `on_message`, in order, with a writer for answers. When
    /// stdout closes (the tool
    /// exited), `on_exit` gets a message for the user that names `tool` and
    /// contains the last stderr line.
    pub fn spawn(
        tool: &'static str,
        mut command: Command,
        mut on_message: impl FnMut(Value, &JsonWriter) + Send + 'static,
        on_exit: impl FnOnce(String) + Send + 'static,
    ) -> Result<Self, String> {
        let mut child = command
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start {tool}: {e}"))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Failed to connect to {tool}."));
        };
        let writer = JsonWriter(Arc::new(Mutex::new(stdin)));
        let reader_writer = writer.clone();
        let stderr_reader = std::thread::spawn(move || read_stderr_tail(stderr));
        std::thread::spawn(move || {
            // Split on LF only: JSON strings can contain U+2028/U+2029.
            for line in BufReader::new(stdout).split(b'\n') {
                let Ok(bytes) = line else { break };
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    on_message(value, &reader_writer);
                }
            }
            let stderr_tail = stderr_reader.join().unwrap_or_default();
            on_exit(exit_message(tool, &stderr_tail));
        });
        Ok(Self { child, writer })
    }

    pub fn send(&self, value: &Value) -> std::io::Result<()> {
        self.writer.send(value)
    }

    #[cfg(test)]
    pub fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for JsonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn read_stderr_tail(mut stderr: impl Read) -> String {
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

/// The message when `tool` exits: its last non-empty stderr line, if any.
pub fn exit_message(tool: &str, stderr_tail: &str) -> String {
    match stderr_tail
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
    {
        Some(line) => format!("{tool} stopped unexpectedly: {line}"),
        None => format!("{tool} stopped unexpectedly."),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ai::test_support::{fake_tool, is_running};
    use std::sync::mpsc;
    use std::time::Duration;

    fn spawn(
        script: &str,
    ) -> (
        crate::ai::test_support::TempFile,
        JsonProcess,
        mpsc::Receiver<Value>,
        mpsc::Receiver<String>,
    ) {
        let (file, program) = fake_tool(script);
        let (tx, messages) = mpsc::channel();
        let (exit_tx, exits) = mpsc::channel();
        let process = JsonProcess::spawn(
            "tool",
            Command::new(program),
            move |value, _| {
                let _ = tx.send(value);
            },
            move |message| {
                let _ = exit_tx.send(message);
            },
        )
        .unwrap();
        (file, process, messages, exits)
    }

    #[test]
    fn echoes_json_lines_and_skips_other_output() {
        let (_file, process, messages, _exits) =
            spawn("echo 'starting'\nwhile read line; do printf '%s\\n' \"$line\"; done");
        process
            .send(&serde_json::json!({ "a": "x\u{2028}y" }))
            .unwrap();
        assert_eq!(
            messages.recv_timeout(Duration::from_secs(10)).unwrap(),
            serde_json::json!({ "a": "x\u{2028}y" })
        );
    }

    #[test]
    fn reports_exit_with_the_last_stderr_line() {
        let (_file, _process, _messages, exits) =
            spawn("echo 'first' >&2; echo 'Error: no auth' >&2; exit 1");
        assert_eq!(
            exits.recv_timeout(Duration::from_secs(10)).unwrap(),
            "tool stopped unexpectedly: Error: no auth"
        );
    }

    #[test]
    fn dropping_the_process_kills_it() {
        let (_file, process, _messages, _exits) = spawn("while read line; do :; done");
        let pid = process.id();
        drop(process);
        assert!(!is_running(pid));
    }
}

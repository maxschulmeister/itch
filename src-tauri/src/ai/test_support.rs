//! Helpers for the backend tests: requests, event sinks and fake tools.

use super::{AiEvent, PromptRequest, Sink};
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use std::time::Duration;

pub fn request(selection: Option<&str>) -> PromptRequest {
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

pub fn channel_sink() -> (Sink, Receiver<AiEvent>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let sink: Sink = Box::new(move |event| {
        let _ = tx.lock().unwrap().send(event);
    });
    (sink, rx)
}

/// Collects events until the run ends. Returns the streamed text and the
/// last event.
pub fn collect_run(rx: &Receiver<AiEvent>) -> (String, AiEvent) {
    let mut streamed = String::new();
    loop {
        let event = rx
            .recv_timeout(Duration::from_secs(120))
            .expect("run did not end");
        match event {
            AiEvent::Delta { text } => streamed.push_str(&text),
            end => return (streamed, end),
        }
    }
}

/// A temporary file that is removed on drop, for fake tool scripts and logs.
pub struct TempFile {
    pub path: std::path::PathBuf,
}

impl TempFile {
    pub fn new(name: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("itch-test-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// An executable shell script with `body`. Returns the file and its path.
#[cfg(unix)]
pub fn fake_tool(body: &str) -> (TempFile, String) {
    use std::os::unix::fs::PermissionsExt;
    let file = TempFile::new("tool.sh");
    std::fs::write(&file.path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&file.path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = file.path.to_string_lossy().into_owned();
    (file, path)
}

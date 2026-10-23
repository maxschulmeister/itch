//! Helpers for AI tools that run as a child process.

use std::io::Read;

const STDERR_TAIL_BYTES: usize = 4096;

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

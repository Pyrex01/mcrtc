//! Interactive stdin. A plain thread does the blocking reads, because a blocking read inside the
//! runtime would keep it from shutting down until the user pressed Enter.

use std::io::BufRead;
use std::thread;

use tokio::sync::mpsc;

pub struct Console {
    lines: mpsc::Receiver<String>,
}

impl Console {
    pub fn from_stdin() -> Self {
        let (tx, lines) = mpsc::channel(64);
        thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { return };
                if tx.blocking_send(line).is_err() {
                    return;
                }
            }
        });
        Self { lines }
    }

    pub fn from_channel(lines: mpsc::Receiver<String>) -> Self {
        Self { lines }
    }

    /// One line, or `None` once stdin is closed.
    pub async fn read_line(&mut self) -> Option<String> {
        self.lines.recv().await
    }

    /// A pasted invite or reply: a single `mcrtc1:` line, or JSON lines up to a blank line or EOF.
    pub async fn read_paste(&mut self) -> Option<String> {
        let first = loop {
            let line = self.read_line().await?;
            if !line.trim().is_empty() {
                break line;
            }
        };
        if !first.trim_start().starts_with('{') {
            return Some(first);
        }
        let mut paste = first;
        while let Some(line) = self.read_line().await {
            if line.trim().is_empty() {
                break;
            }
            paste.push('\n');
            paste.push_str(&line);
        }
        Some(paste)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn console_with(lines: &[&str]) -> Console {
        let (tx, rx) = mpsc::channel(lines.len().max(1));
        for line in lines {
            tx.send(line.to_string()).await.unwrap();
        }
        Console::from_channel(rx)
    }

    #[tokio::test]
    async fn compact_paste_is_one_line_and_skips_blanks() {
        let mut console = console_with(&["", "  ", "mcrtc1:abc", "next"]).await;
        assert_eq!(console.read_paste().await.unwrap(), "mcrtc1:abc");
        assert_eq!(console.read_line().await.unwrap(), "next");
    }

    #[tokio::test]
    async fn json_paste_stops_at_a_blank_line() {
        let mut console = console_with(&["{", "  \"v\": 1", "}", "", "next"]).await;
        assert_eq!(console.read_paste().await.unwrap(), "{\n  \"v\": 1\n}");
        assert_eq!(console.read_line().await.unwrap(), "next");
    }

    #[tokio::test]
    async fn json_paste_stops_at_eof() {
        let mut console = console_with(&["{", "}"]).await;
        assert_eq!(console.read_paste().await.unwrap(), "{\n}");
        assert!(console.read_paste().await.is_none());
    }
}

//! Frontend-neutral input. Desktop and Android adapters feed lines through a channel.

use tokio::sync::mpsc;

pub struct Console {
    lines: mpsc::Receiver<String>,
}

impl Console {
    pub fn from_channel(lines: mpsc::Receiver<String>) -> Self {
        Self { lines }
    }
    pub async fn read_line(&mut self) -> Option<String> {
        self.lines.recv().await
    }
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

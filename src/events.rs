//! What the host and join flows have to say, independent of how it reaches a person.

use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Hosting {
        server: String,
    },
    AskName {
        friend: usize,
        default: String,
    },
    WaitingForSlot {
        limit: usize,
    },
    Gathering,
    Invite {
        label: String,
        text: String,
        minutes: u64,
    },
    AskReply {
        label: String,
    },
    BadPaste {
        error: String,
    },
    Connecting {
        label: String,
    },
    FriendConnected {
        label: String,
        route: Option<String>,
    },
    FriendLeft {
        label: String,
        reason: String,
    },
    Stats {
        label: String,
        connections: usize,
        ping_ms: Option<u64>,
    },
    Cancelled,
    InviteExpired,
    ConnectFailed {
        error: String,
    },
    AskNext,
    StdinClosed,
    ShuttingDown,

    YourId {
        id: String,
    },
    AskInvite,
    Reply {
        text: String,
    },
    WaitingForHost,
    Joined {
        address: String,
        route: Option<String>,
    },
    Left {
        reason: String,
    },

    SessionEnded {
        error: Option<String>,
    },
}

impl Event {
    /// The terminal rendering, if this event has one.
    fn line(&self) -> Option<String> {
        Some(match self {
            Event::Hosting { server } => format!("Hosting {server}. Press Ctrl+C to stop."),
            Event::AskName { friend, default } => {
                format!("\n=== Friend {friend} ===\nName for this friend [{default}]:")
            }
            Event::WaitingForSlot { limit } => {
                format!("--max-peers reached ({limit}). Waiting for a friend to leave...")
            }
            Event::Gathering => "Gathering network candidates...".to_owned(),
            Event::Invite {
                label,
                text,
                minutes,
            } => {
                format!("\nSend this INVITE to {label} (valid for {minutes} minutes):\n\n{text}\n")
            }
            Event::AskReply { label } => {
                format!("Paste {label}'s REPLY here (or type `cancel`):")
            }
            Event::BadPaste { error } => format!("That did not work: {error}"),
            Event::Connecting { label } => format!("Connecting to {label}..."),
            Event::FriendConnected { label, .. } => format!("{label} connected."),
            Event::FriendLeft { label, reason } => format!("{label} {reason}."),
            Event::Stats { .. } | Event::SessionEnded { .. } => return None,
            Event::Cancelled => "Cancelled.".to_owned(),
            Event::InviteExpired => "The invite expired.".to_owned(),
            Event::ConnectFailed { error } => format!("Could not connect: {error}"),
            Event::AskNext => "Press Enter for another friend.".to_owned(),
            Event::StdinClosed => {
                "Stdin closed, no more invites. Current friends stay connected until Ctrl+C."
                    .to_owned()
            }
            Event::ShuttingDown => "Shutting down...".to_owned(),
            Event::YourId { id } => format!("Your mcrtc ID: {id}"),
            Event::AskInvite => "Paste the INVITE from the host:".to_owned(),
            Event::Reply { text } => format!("\nSend this REPLY to the host:\n\n{text}\n"),
            Event::WaitingForHost => "Waiting for the host to paste it...".to_owned(),
            Event::Joined { address, .. } => format!("Connected! Point Minecraft at {address}."),
            Event::Left { reason } => reason.clone(),
        })
    }
}

/// Where events go: printed for the terminal, or handed to another frontend.
#[derive(Clone)]
pub struct Reporter(Option<mpsc::UnboundedSender<Event>>);

impl Reporter {
    pub fn terminal() -> Self {
        Self(None)
    }

    pub fn channel(events: mpsc::UnboundedSender<Event>) -> Self {
        Self(Some(events))
    }

    pub fn report(&self, event: Event) {
        match &self.0 {
            Some(events) => {
                let _ = events.send(event);
            }
            None => {
                if let Some(line) = event.line() {
                    println!("{line}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_stay_off_the_terminal() {
        let event = Event::Stats {
            label: "alex".into(),
            connections: 2,
            ping_ms: None,
        };
        assert_eq!(event.line(), None);
        assert!(Event::Gathering.line().is_some());
    }

    #[test]
    fn channel_reporter_delivers_events() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        Reporter::channel(tx).report(Event::Cancelled);
        assert_eq!(rx.try_recv().unwrap(), Event::Cancelled);
    }
}

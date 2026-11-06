//! The session as the window sees it, folded from its events. Nothing here draws or does I/O.

use crate::edition::Edition;
use crate::events::Event;

use super::session::{Mode, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Title,
    HostSetup,
    Friends,
    Name,
    Invite,
    JoinSetup,
    Reply,
    Joined,
    Options,
    Progress,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Friend {
    pub name: String,
    pub route: Option<String>,
    pub connections: usize,
    pub ping: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub detail: String,
}

impl Message {
    pub fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joined {
    pub address: String,
    pub route: Option<String>,
    pub connections: usize,
    pub ping: Option<u64>,
}

/// The text of every form, kept while moving between screens.
#[derive(Debug, Default, Clone)]
pub struct Forms {
    pub edition: Edition,
    pub server: String,
    pub max_peers: String,
    pub max_connections: String,
    pub stun: String,
    pub turn_url: String,
    pub turn_user: String,
    pub turn_pass: String,
    pub invite: String,
    pub listen: String,
    pub name: String,
    pub reply: String,
}

impl Forms {
    pub fn settings(&self) -> Settings {
        Settings {
            edition: self.edition,
            server: self.server.clone(),
            listen: self.listen.clone(),
            max_peers: positive(&self.max_peers),
            max_connections: positive(&self.max_connections),
            stun: self.stun.clone(),
            turn_url: self.turn_url.clone(),
            turn_user: self.turn_user.clone(),
            turn_pass: self.turn_pass.clone(),
        }
    }

    /// Addresses typed for one edition are wrong for the other, so they do not carry over.
    pub fn choose_edition(&mut self, edition: Edition) {
        if self.edition != edition {
            self.edition = edition;
            self.server.clear();
            self.listen.clear();
        }
    }
}

fn positive(text: &str) -> Option<usize> {
    text.trim().parse().ok().filter(|value| *value > 0)
}

/// Chat apps like to wrap long strings, and none of ours contain whitespace.
pub fn clean_paste(text: &str) -> String {
    text.split_whitespace().collect()
}

pub fn describe_route(route: Option<&str>) -> Option<&'static str> {
    route.map(|route| {
        if route.contains("relayed") {
            "Relayed through TURN"
        } else {
            "Direct connection"
        }
    })
}

pub struct State {
    pub screen: Screen,
    pub mode: Option<Mode>,
    pub forms: Forms,
    pub friends: Vec<Friend>,
    pub default_name: String,
    pub name_ready: bool,
    pub waiting_for_slot: Option<usize>,
    pub invite: String,
    pub reply: String,
    pub label: String,
    pub error: String,
    pub progress: Message,
    pub failure: Message,
    pub joined: Option<Joined>,
    /// The flow is waiting for an invite to be pasted.
    pub awaiting_invite: bool,
    sent_invite: bool,
    left_reason: String,
    /// Set when there is something new to pop up in the corner.
    pub toast: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            screen: Screen::Title,
            mode: None,
            forms: Forms::default(),
            friends: Vec::new(),
            default_name: String::new(),
            name_ready: false,
            waiting_for_slot: None,
            invite: String::new(),
            reply: String::new(),
            label: String::new(),
            error: String::new(),
            progress: Message::default(),
            failure: Message::default(),
            joined: None,
            awaiting_invite: false,
            sent_invite: false,
            left_reason: String::new(),
            toast: None,
        }
    }
}

impl State {
    pub fn fail(&mut self, title: &str, detail: impl Into<String>) {
        self.failure = Message::new(title, detail);
        self.screen = Screen::Failed;
    }

    pub fn back_from_failure(&self) -> Screen {
        match self.mode {
            Some(Mode::Host) => Screen::Friends,
            _ => Screen::Title,
        }
    }

    fn progress(&mut self, title: impl Into<String>, detail: &str) {
        self.progress = Message::new(title, detail);
        self.screen = Screen::Progress;
    }

    pub fn begin_join(&mut self) {
        self.sent_invite = false;
    }

    /// The invite to hand to the flow, marking it as sent.
    pub fn take_invite(&mut self) -> String {
        self.awaiting_invite = false;
        self.sent_invite = true;
        clean_paste(&self.forms.invite)
    }

    /// Folds in one event. Returns a line the flow is waiting for, if the answer needs no person.
    pub fn apply(&mut self, event: Event) -> Option<String> {
        match event {
            Event::Hosting { .. } => {
                self.mode = Some(Mode::Host);
                self.friends.clear();
                self.waiting_for_slot = None;
                self.screen = Screen::Friends;
            }
            Event::AskName { default, .. } => {
                self.name_ready = true;
                self.waiting_for_slot = None;
                self.default_name = default;
                if self.screen != Screen::Failed {
                    self.screen = Screen::Friends;
                }
            }
            Event::WaitingForSlot { limit } => {
                self.waiting_for_slot = Some(limit);
                self.screen = Screen::Friends;
            }
            Event::Gathering => {
                let title = match self.mode {
                    Some(Mode::Host) => "Generating Invite",
                    _ => "Joining",
                };
                self.progress(title, "Gathering network candidates");
            }
            Event::Invite { label, text, .. } => {
                self.invite = text;
                self.label = label;
                self.error.clear();
                self.forms.reply.clear();
            }
            Event::AskReply { .. } => self.screen = Screen::Invite,
            Event::BadPaste { error } => {
                self.error = error;
                if self.mode == Some(Mode::Join) {
                    self.sent_invite = false;
                }
            }
            Event::Connecting { label } => {
                self.progress(format!("Connecting to {label}"), "Opening the tunnel");
            }
            Event::FriendConnected { label, route } => {
                self.toast = Some(format!("{label} joined"));
                let friend = Friend {
                    name: label,
                    route,
                    connections: 0,
                    ping: None,
                };
                match self.friends.iter_mut().find(|f| f.name == friend.name) {
                    Some(existing) => *existing = friend,
                    None => self.friends.push(friend),
                }
            }
            Event::FriendLeft { label, reason } => {
                self.friends.retain(|friend| friend.name != label);
                self.toast = Some(format!("{label} {reason}"));
            }
            Event::Stats {
                label,
                connections,
                ping_ms,
            } => {
                if let Some(friend) = self.friends.iter_mut().find(|f| f.name == label) {
                    friend.connections = connections;
                    friend.ping = ping_ms;
                } else if let Some(joined) = &mut self.joined {
                    joined.connections = connections;
                    joined.ping = ping_ms;
                }
            }
            Event::InviteExpired => self.toast = Some("The invite expired".to_owned()),
            Event::ConnectFailed { error } => self.fail("Could Not Connect", error),
            // The window has its own Invite button, so the invite loop never has to wait here.
            Event::AskNext => return Some(String::new()),
            Event::ShuttingDown => self.progress("Stopping", "Saying goodbye"),
            Event::YourId { .. } => self.mode = Some(Mode::Join),
            Event::AskInvite => {
                self.mode = Some(Mode::Join);
                self.awaiting_invite = true;
                if !self.error.is_empty() {
                    self.screen = Screen::JoinSetup;
                } else if !self.sent_invite && !self.forms.invite.trim().is_empty() {
                    return Some(self.take_invite());
                }
            }
            Event::Reply { text } => {
                self.reply = text;
                self.screen = Screen::Reply;
            }
            Event::Joined { address, route } => {
                self.joined = Some(Joined {
                    address,
                    route,
                    connections: 0,
                    ping: None,
                });
                self.screen = Screen::Joined;
            }
            Event::Left { reason } => self.left_reason = reason,
            Event::SessionEnded { error } => self.session_ended(error),
            Event::Cancelled | Event::StdinClosed | Event::WaitingForHost => {}
        }
        None
    }

    fn session_ended(&mut self, error: Option<String>) {
        let mode = self.mode.take();
        let reason = std::mem::take(&mut self.left_reason);
        self.awaiting_invite = false;
        self.name_ready = false;
        self.joined = None;
        self.friends.clear();
        self.screen = Screen::Title;
        if let Some(error) = error {
            self.fail("Connection Lost", error);
        } else if mode == Some(Mode::Join) && !reason.is_empty() && reason != "Disconnected." {
            self.fail("Disconnected", reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask_name() -> Event {
        Event::AskName {
            friend: 1,
            default: "friend-1".into(),
        }
    }

    fn hosting() -> State {
        let mut state = State::default();
        state.apply(Event::Hosting { server: "s".into() });
        state.apply(ask_name());
        state
    }

    fn connect(state: &mut State, label: &str) {
        state.apply(Event::FriendConnected {
            label: label.into(),
            route: Some("host <-> srflx (direct)".into()),
        });
    }

    #[test]
    fn hosting_lands_on_the_friends_list_ready_to_invite() {
        let state = hosting();
        assert_eq!(state.screen, Screen::Friends);
        assert!(state.name_ready);
        assert_eq!(state.default_name, "friend-1");
    }

    #[test]
    fn the_invite_screen_opens_once_the_flow_asks_for_a_reply() {
        let mut state = hosting();
        state.apply(Event::Gathering);
        assert_eq!(state.screen, Screen::Progress);
        assert_eq!(state.progress.title, "Generating Invite");
        state.forms.reply = "stale".into();
        state.apply(Event::Invite {
            label: "alex".into(),
            text: "mcrtc1:abc".into(),
            minutes: 10,
        });
        state.apply(Event::AskReply {
            label: "alex".into(),
        });
        assert_eq!(state.screen, Screen::Invite);
        assert_eq!(state.invite, "mcrtc1:abc");
        assert!(state.forms.reply.is_empty());
    }

    #[test]
    fn the_next_friend_prompt_answers_itself() {
        let mut state = hosting();
        assert_eq!(state.apply(Event::AskNext), Some(String::new()));
    }

    #[test]
    fn a_failure_stays_on_screen_when_the_invite_loop_moves_on() {
        let mut state = hosting();
        state.apply(Event::ConnectFailed {
            error: "timed out".into(),
        });
        state.apply(ask_name());
        assert_eq!(state.screen, Screen::Failed);
        assert_eq!(state.failure.detail, "timed out");
        assert_eq!(state.back_from_failure(), Screen::Friends);
    }

    #[test]
    fn friends_keep_their_place_and_stats() {
        let mut state = hosting();
        connect(&mut state, "alex");
        connect(&mut state, "sam");
        assert_eq!(state.toast.as_deref(), Some("sam joined"));
        state.apply(Event::Stats {
            label: "alex".into(),
            connections: 2,
            ping_ms: Some(31),
        });
        connect(&mut state, "alex");
        state.apply(Event::Stats {
            label: "alex".into(),
            connections: 3,
            ping_ms: Some(40),
        });
        let names: Vec<_> = state.friends.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["alex", "sam"]);
        assert_eq!(state.friends[0].connections, 3);
        assert_eq!(state.friends[0].ping, Some(40));

        state.apply(Event::FriendLeft {
            label: "alex".into(),
            reason: "left".into(),
        });
        assert_eq!(state.friends.len(), 1);
        assert_eq!(state.toast.as_deref(), Some("alex left"));
    }

    #[test]
    fn a_pasted_invite_goes_out_as_soon_as_the_flow_asks() {
        let mut state = State::default();
        state.forms.invite = " mcrtc1:ab\n cd ".into();
        state.begin_join();
        state.apply(Event::YourId { id: "id".into() });
        assert_eq!(
            state.apply(Event::AskInvite).as_deref(),
            Some("mcrtc1:abcd")
        );
        assert!(!state.awaiting_invite);
    }

    #[test]
    fn a_bad_invite_returns_to_the_form_and_can_be_resent() {
        let mut state = State {
            screen: Screen::JoinSetup,
            ..State::default()
        };
        state.forms.invite = "garbage".into();
        state.begin_join();
        state.apply(Event::AskInvite);
        state.apply(Event::BadPaste {
            error: "not an invite".into(),
        });
        assert_eq!(state.apply(Event::AskInvite), None);
        assert_eq!(state.screen, Screen::JoinSetup);
        assert!(state.awaiting_invite);
        assert_eq!(state.error, "not an invite");
    }

    #[test]
    fn joining_tracks_the_host_connection() {
        let mut state = State::default();
        state.apply(Event::YourId { id: "id".into() });
        state.apply(Event::Reply { text: "r".into() });
        assert_eq!(state.screen, Screen::Reply);
        state.apply(Event::Joined {
            address: "127.0.0.1:25565".into(),
            route: None,
        });
        state.apply(Event::Stats {
            label: "host".into(),
            connections: 1,
            ping_ms: Some(12),
        });
        assert_eq!(state.screen, Screen::Joined);
        let joined = state.joined.as_ref().unwrap();
        assert_eq!((joined.connections, joined.ping), (1, Some(12)));
    }

    #[test]
    fn a_host_leaving_explains_itself_but_our_own_disconnect_does_not() {
        let mut state = State::default();
        state.apply(Event::YourId { id: "id".into() });
        state.apply(Event::Left {
            reason: "The host closed the connection.".into(),
        });
        state.apply(Event::SessionEnded { error: None });
        assert_eq!(state.screen, Screen::Failed);
        assert_eq!(state.failure.title, "Disconnected");

        let mut state = State::default();
        state.apply(Event::YourId { id: "id".into() });
        state.apply(Event::Left {
            reason: "Disconnected.".into(),
        });
        state.apply(Event::SessionEnded { error: None });
        assert_eq!(state.screen, Screen::Title);
    }

    #[test]
    fn a_session_error_is_shown_and_the_way_back_is_the_title() {
        let mut state = hosting();
        connect(&mut state, "alex");
        state.apply(Event::SessionEnded {
            error: Some("boom".into()),
        });
        assert_eq!(state.screen, Screen::Failed);
        assert_eq!(state.failure.title, "Connection Lost");
        assert_eq!(state.back_from_failure(), Screen::Title);
        assert!(state.friends.is_empty());
    }

    #[test]
    fn addresses_do_not_follow_a_change_of_edition() {
        let mut forms = Forms {
            server: "10.0.0.5:25565".into(),
            listen: "127.0.0.1:25566".into(),
            max_peers: "4".into(),
            ..Forms::default()
        };
        forms.choose_edition(Edition::Java);
        assert_eq!(forms.server, "10.0.0.5:25565");

        forms.choose_edition(Edition::Bedrock);
        assert_eq!(forms.settings().edition, Edition::Bedrock);
        assert!(forms.server.is_empty() && forms.listen.is_empty());
        assert_eq!(forms.max_peers, "4");
    }

    #[test]
    fn numbers_in_forms_must_be_positive() {
        assert_eq!(positive(" 4 "), Some(4));
        assert_eq!(positive("0"), None);
        assert_eq!(positive("-1"), None);
        assert_eq!(positive("four"), None);
    }

    #[test]
    fn routes_read_like_a_person_wrote_them() {
        assert_eq!(describe_route(None), None);
        assert_eq!(
            describe_route(Some("relay <-> host (relayed through TURN)")),
            Some("Relayed through TURN")
        );
        assert_eq!(
            describe_route(Some("host <-> srflx (direct)")),
            Some("Direct connection")
        );
    }
}

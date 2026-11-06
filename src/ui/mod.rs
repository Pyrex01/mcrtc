//! The graphical interface: a native window, drawn in the style of Minecraft's own menus.
//!
//! This drives the same host and join flows as the terminal. The window owns the main thread, and
//! the flows run on the Tokio runtime, reporting back through [`Sessions`].

mod art;
mod session;
mod state;
#[cfg(test)]
mod tests;
mod widgets;

use std::rc::Rc;
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, IconData, Key, Layout, Rect, ScrollArea, Ui, UiBuilder, Vec2,
    ViewportBuilder, ViewportCommand,
};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::control::stop_requested;
use crate::events::Event;

pub use session::UiOptions;
use session::{Mode, Sessions};
use state::{clean_paste, describe_route, Message, Screen, State};
use widgets::{Theme, BUTTON_HEIGHT, COLUMN, GAP, MUTED, RED, WHITE, YELLOW};

const SPLASHES: [&str; 10] = [
    "No port forwarding!",
    "Copy, paste, play!",
    "Peer to peer!",
    "Also try TURN!",
    "Now with 0 VPNs!",
    "127.0.0.1 is home!",
    "Bring a friend!",
    "NAT? What NAT?",
    "16 KiB at a time!",
    "Reliable and ordered!",
];
const LOADING: [&str; 4] = ["O o o", "o O o", "o o O", "o O o"];

const WIDE_COLUMN: f32 = 340.0;
const HEADER: f32 = 33.0;
const FOOTER: f32 = 33.0;
const STRING_BOX: f32 = 54.0;
const TOAST_SECONDS: f64 = 3.5;
const TOAST_SLIDE_SECONDS: f64 = 0.2;
const COPIED_SECONDS: f64 = 1.8;
const ANIMATION_FRAME: Duration = Duration::from_millis(33);

pub fn run(options: UiOptions, runtime: Handle, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let window = Arc::new(OnceLock::<egui::Context>::new());
    let wake = {
        let window = window.clone();
        Arc::new(move || {
            if let Some(ctx) = window.get() {
                ctx.request_repaint();
            }
        })
    };
    let (sessions, events) = Sessions::new(options, runtime.clone(), wake);
    let sessions = Arc::new(sessions);

    let (rgba, width, height) = art::icon();
    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("mcrtc")
            .with_app_id("mcrtc")
            .with_inner_size([1000.0, 640.0])
            .with_min_inner_size([640.0, 480.0])
            .with_icon(IconData {
                rgba,
                width,
                height,
            }),
        ..Default::default()
    };
    let session_handle = sessions.clone();
    let result = eframe::run_native(
        "mcrtc",
        native_options,
        Box::new(move |context| {
            let ctx = context.egui_ctx.clone();
            let _ = window.set(ctx.clone());
            let closing = ctx.clone();
            runtime.spawn(async move {
                stop_requested(&mut shutdown).await;
                closing.send_viewport_cmd(ViewportCommand::Close);
                closing.request_repaint();
            });
            Ok(Box::new(App::new(&ctx, session_handle, events)))
        }),
    );
    // Closing the window ends the session, and friends still deserve a goodbye.
    sessions.shut_down();
    result.map_err(|error| {
        anyhow!("could not open a window: {error}. `mcrtc host` and `mcrtc join` work without one")
    })
}

/// The largest whole scale that keeps a 320x240 screen in the window, like GUI Scale: Auto.
fn gui_scale(window: Vec2) -> f32 {
    (window.x / 320.0).min(window.y / 240.0).floor().max(1.0)
}

struct App {
    state: State,
    sessions: Arc<Sessions>,
    events: mpsc::Receiver<Event>,
    theme: Rc<Theme>,
    splash: &'static str,
    toast: Option<(String, f64)>,
    /// The copy button showing its confirmation, and since when.
    copied: Option<(String, f64)>,
    shown: Option<Screen>,
    /// True on the first frame of a screen.
    entering: bool,
}

fn random_splash() -> &'static str {
    SPLASHES[rand::random::<u32>() as usize % SPLASHES.len()]
}

fn plural(count: usize, word: &str) -> String {
    let ending = if count == 1 { "" } else { "s" };
    format!("{count} {word}{ending}")
}

impl App {
    fn new(ctx: &egui::Context, sessions: Arc<Sessions>, events: mpsc::Receiver<Event>) -> Self {
        widgets::install_style(ctx);
        Self {
            state: State::default(),
            sessions,
            events,
            theme: Rc::new(Theme::new(ctx)),
            splash: random_splash(),
            toast: None,
            copied: None,
            shown: None,
            entering: true,
        }
    }

    fn send(&mut self, line: String) {
        if let Err(error) = self.sessions.send(line) {
            self.state
                .fail("Something Went Wrong", format!("{error:#}"));
        }
    }

    fn drain_events(&mut self, now: f64) {
        while let Ok(event) = self.events.try_recv() {
            if let Some(line) = self.state.apply(event) {
                self.send(line);
            }
        }
        if let Some(text) = self.state.toast.take() {
            self.toast = Some((text, now));
        }
    }

    fn start_host(&mut self) {
        // Before the call: the session's first events can arrive ahead of anything after it.
        self.state.progress = Message::new("Starting", "One moment");
        self.state.screen = Screen::Progress;
        let settings = self.state.forms.settings();
        if let Err(error) = self.sessions.start(Mode::Host, settings) {
            self.state.fail("Could Not Start", format!("{error:#}"));
        }
    }

    fn start_join(&mut self) {
        if self.state.forms.invite.trim().is_empty() {
            self.state.error = "Paste the invite first.".to_owned();
            return;
        }
        self.state.error.clear();
        if self.state.awaiting_invite {
            let invite = self.state.take_invite();
            self.send(invite);
            return;
        }
        self.state.begin_join();
        let settings = self.state.forms.settings();
        if let Err(error) = self.sessions.start(Mode::Join, settings) {
            self.state.fail("Could Not Start", format!("{error:#}"));
        }
    }

    fn cancel_join(&mut self) {
        self.state.error.clear();
        if self.state.awaiting_invite {
            self.sessions.stop();
        } else {
            self.state.screen = Screen::Title;
        }
    }

    fn submit_name(&mut self) {
        self.state.name_ready = false;
        let name = self.state.forms.name.trim().to_owned();
        self.state.progress = Message::new("Generating Invite", "Gathering network candidates");
        self.state.screen = Screen::Progress;
        self.send(name);
    }

    fn submit_reply(&mut self) {
        let reply = clean_paste(&self.state.forms.reply);
        if !reply.is_empty() {
            self.state.error.clear();
            self.send(reply);
        }
    }

    /// Everything the window draws, in points of one Minecraft GUI pixel each.
    fn show(&mut self, ui: &mut Ui) {
        let now = ui.input(|input| input.time);
        self.drain_events(now);
        self.entering = self.shown != Some(self.state.screen);
        if self.entering {
            self.shown = Some(self.state.screen);
            self.copied = None;
            if self.state.screen == Screen::Title {
                self.splash = random_splash();
            }
        }

        let screen = ui.max_rect();
        self.theme.background(ui, screen);
        match self.state.screen {
            Screen::Title => self.title(ui, screen, now),
            Screen::HostSetup => self.host_setup(ui),
            Screen::Friends => self.friends(ui),
            Screen::Name => self.name(ui),
            Screen::Invite => self.invite(ui, now),
            Screen::JoinSetup => self.join_setup(ui),
            Screen::Reply => self.reply(ui, now),
            Screen::Joined => self.joined(ui, now),
            Screen::Options => self.options(ui),
            Screen::Progress => self.progress(ui, screen, now),
            Screen::Failed => self.failed(ui, screen),
        }
        self.show_toast(ui, screen, now);
    }

    fn show_toast(&mut self, ui: &Ui, screen: Rect, now: f64) {
        let Some((text, since)) = &self.toast else {
            return;
        };
        let age = now - since;
        if age > TOAST_SECONDS {
            self.toast = None;
            return;
        }
        let visible = (age / TOAST_SLIDE_SECONDS)
            .min((TOAST_SECONDS - age) / TOAST_SLIDE_SECONDS)
            .min(1.0);
        widgets::toast(ui, screen, text, visible as f32);
        ui.ctx().request_repaint_after(ANIMATION_FRAME);
    }

    /// The layout most screens share: a title, a scrolling column, and buttons along the bottom.
    fn page(
        &mut self,
        ui: &mut Ui,
        title: &str,
        column: f32,
        banded: bool,
        body: impl FnOnce(&mut Self, &mut Ui),
        footer: impl FnOnce(&mut Self, &mut Ui),
    ) {
        let screen = ui.max_rect();
        let area = Rect::from_min_max(
            screen.min + vec2(0.0, HEADER),
            screen.max - vec2(0.0, FOOTER),
        );
        if banded {
            widgets::list_band(ui, area);
        }
        widgets::title(ui, pos2(screen.center().x, screen.top() + 13.0), title);

        let width = column.min(screen.width() - 32.0);
        let mut scroller = ui.new_child(UiBuilder::new().id_salt("body").max_rect(area));
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(&mut scroller, |ui| {
                let left = (area.center().x - width / 2.0).round();
                let top = ui.cursor().top() + GAP;
                let column = Rect::from_min_size(pos2(left, top), vec2(width, 10_000.0));
                let builder = UiBuilder::new()
                    .max_rect(column)
                    .layout(Layout::top_down(Align::Min));
                ui.scope_builder(builder, |ui| body(self, ui));
                ui.add_space(GAP);
            });

        let center = pos2(
            screen.center().x,
            screen.bottom() - 27.0 + BUTTON_HEIGHT / 2.0,
        );
        let row = Rect::from_center_size(center, vec2(width, BUTTON_HEIGHT));
        let mut buttons = ui.new_child(UiBuilder::new().id_salt("footer").max_rect(row));
        footer(self, &mut buttons);
    }

    /// Up to a few buttons side by side, returning the index of the one pressed.
    fn buttons(&self, ui: &mut Ui, row: &[(&str, bool)], each: f32) -> Option<usize> {
        let total = (each + GAP) * row.len() as f32 - GAP;
        let left = (ui.max_rect().center().x - total / 2.0).round();
        let rect = Rect::from_min_size(pos2(left, ui.cursor().top()), vec2(total, BUTTON_HEIGHT));
        let builder = UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Min));
        ui.scope_builder(builder, |ui| {
            let mut pressed = None;
            for (index, (label, enabled)) in row.iter().enumerate() {
                if widgets::button(ui, &self.theme, label, each, *enabled).clicked() {
                    pressed = Some(index);
                }
            }
            pressed
        })
        .inner
    }

    fn escape_pressed(ui: &Ui) -> bool {
        ui.input(|input| input.key_pressed(Key::Escape))
    }

    fn copy_button(&mut self, ui: &mut Ui, label: &str, text: String, now: f64) {
        let confirmed = self
            .copied
            .as_ref()
            .is_some_and(|(copied, since)| copied == label && now - since < COPIED_SECONDS);
        let shown = if confirmed { "Copied!" } else { label };
        let width = ui.available_width();
        if widgets::button(ui, &self.theme, shown, width, true).clicked() {
            ui.ctx().copy_text(text);
            self.copied = Some((label.to_owned(), now));
        }
        if confirmed {
            ui.ctx().request_repaint_after(ANIMATION_FRAME);
        }
    }

    fn title(&mut self, ui: &mut Ui, screen: Rect, now: f64) {
        let center = screen.center().x;
        let logo = self.theme.logo(ui, pos2(center, screen.top() + 30.0));
        let pulse = 1.06 - ((now % 1.0) * std::f64::consts::TAU).sin().abs() as f32 * 0.12;
        widgets::splash(
            ui,
            logo.right_bottom() + vec2(-28.0, -2.0),
            self.splash,
            pulse,
        );

        let top = (screen.top() + screen.height() / 4.0 + 48.0).round();
        let row_top = |index: f32| top + index * (BUTTON_HEIGHT + GAP);
        let column = |y: f32| {
            Rect::from_min_size(
                pos2((center - COLUMN / 2.0).round(), y),
                vec2(COLUMN, BUTTON_HEIGHT),
            )
        };
        let theme = self.theme.clone();
        let mut host = ui.new_child(
            UiBuilder::new()
                .id_salt("host")
                .max_rect(column(row_top(0.0))),
        );
        if widgets::button(&mut host, &theme, "Host a Server", COLUMN, true).clicked() {
            self.state.screen = Screen::HostSetup;
        }
        let mut join = ui.new_child(
            UiBuilder::new()
                .id_salt("join")
                .max_rect(column(row_top(1.0))),
        );
        if widgets::button(&mut join, &theme, "Join a Friend", COLUMN, true).clicked() {
            self.state.screen = Screen::JoinSetup;
        }
        let mut last = ui.new_child(
            UiBuilder::new()
                .id_salt("last")
                .max_rect(column(row_top(2.0))),
        );
        let half = (COLUMN - GAP) / 2.0;
        match self.buttons(&mut last, &[("Options...", true), ("Quit", true)], half) {
            Some(0) => self.state.screen = Screen::Options,
            Some(_) => ui.ctx().send_viewport_cmd(ViewportCommand::Close),
            None => {}
        }

        let painter = ui.painter();
        let bottom = screen.bottom() - 10.0;
        let version = concat!("mcrtc ", env!("CARGO_PKG_VERSION"));
        widgets::text_at(
            ui,
            painter,
            pos2(screen.left() + 2.0, bottom),
            Align2::LEFT_TOP,
            version,
            WHITE,
        );
        let legal = "Not affiliated with Mojang or Microsoft";
        widgets::text_at(
            ui,
            painter,
            pos2(screen.right() - 2.0, bottom),
            Align2::RIGHT_TOP,
            legal,
            WHITE,
        );
        ui.ctx().request_repaint_after(ANIMATION_FRAME);
    }

    fn host_setup(&mut self, ui: &mut Ui) {
        let default_server = self.sessions.default_server().to_owned();
        self.page(
            ui,
            "Host a Server",
            COLUMN,
            false,
            |app, ui| {
                let forms = &mut app.state.forms;
                widgets::field(
                    ui,
                    "Minecraft Server Address",
                    &mut forms.server,
                    &default_server,
                    |edit| edit,
                );
                halves(ui, |left, right| {
                    widgets::field(
                        left,
                        "Max Friends",
                        &mut forms.max_peers,
                        "No limit",
                        |edit| edit,
                    );
                    widgets::field(
                        right,
                        "Connections Each",
                        &mut forms.max_connections,
                        "No limit",
                        |edit| edit,
                    );
                });
                widgets::gap(ui, GAP);
                let hint = "Start your Minecraft server first, then press Start Hosting.";
                widgets::paragraph(ui, hint, MUTED, Align::Min);
            },
            |app, ui| {
                let half = (COLUMN - GAP) / 2.0;
                let row = [("Start Hosting", true), ("Cancel", true)];
                match app.buttons(ui, &row, half) {
                    Some(0) => app.start_host(),
                    Some(_) => app.state.screen = Screen::Title,
                    None if Self::escape_pressed(ui) => app.state.screen = Screen::Title,
                    None => {}
                }
            },
        );
    }

    fn friends(&mut self, ui: &mut Ui) {
        let title = format!("Friends ({})", self.state.friends.len());
        self.page(
            ui,
            &title,
            WIDE_COLUMN,
            true,
            |app, ui| {
                if let Some(limit) = app.state.waiting_for_slot {
                    let text = format!("Full ({limit}). Waiting for a friend to leave.");
                    widgets::paragraph(ui, &text, YELLOW, Align::Center);
                }
                if app.state.friends.is_empty() {
                    widgets::gap(ui, 56.0);
                    widgets::paragraph(ui, "No friends yet. Invite one!", MUTED, Align::Center);
                }
                for friend in &app.state.friends {
                    let route = describe_route(friend.route.as_deref());
                    let connections = plural(friend.connections, "connection");
                    let detail = match route {
                        Some(route) => format!("{route} · {connections}"),
                        None => connections,
                    };
                    widgets::list_entry(ui, &app.theme, &friend.name, &detail, friend.ping);
                }
            },
            |app, ui| {
                let row = [
                    ("Invite a Friend", app.state.name_ready),
                    ("Stop Hosting", true),
                ];
                let each = ((ui.max_rect().width() - GAP) / 2.0).min(150.0);
                match app.buttons(ui, &row, each) {
                    Some(0) => {
                        app.state.forms.name.clear();
                        app.state.screen = Screen::Name;
                    }
                    Some(_) => app.sessions.stop(),
                    None => {}
                }
            },
        );
    }

    fn name(&mut self, ui: &mut Ui) {
        let submitted = std::cell::Cell::new(false);
        self.page(
            ui,
            "Invite a Friend",
            COLUMN,
            false,
            |app, ui| {
                let entering = app.entering;
                let placeholder = app.state.default_name.clone();
                let forms = &mut app.state.forms;
                let field =
                    widgets::field(ui, "Friend's Name", &mut forms.name, &placeholder, |edit| {
                        edit.char_limit(32)
                    });
                if entering {
                    field.request_focus();
                }
                submitted
                    .set(field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)));
            },
            |app, ui| {
                let half = (COLUMN - GAP) / 2.0;
                let row = [("Create Invite", true), ("Cancel", true)];
                match app.buttons(ui, &row, half) {
                    Some(0) => submitted.set(true),
                    Some(_) => app.state.screen = Screen::Friends,
                    None if Self::escape_pressed(ui) => app.state.screen = Screen::Friends,
                    None => {}
                }
            },
        );
        if submitted.get() {
            self.submit_name();
        }
    }

    fn invite(&mut self, ui: &mut Ui, now: f64) {
        let title = format!("Invite {}", self.state.label);
        self.page(
            ui,
            &title,
            WIDE_COLUMN,
            false,
            |app, ui| {
                let mut invite = app.state.invite.as_str();
                let label = "1. Send this invite (valid for 10 minutes)";
                widgets::text_box(ui, label, STRING_BOX, &mut invite);
                let text = app.state.invite.clone();
                app.copy_button(ui, "Copy Invite", text, now);
                let warning =
                    "Invites contain your IP address. Only share them with people you trust.";
                widgets::paragraph(ui, warning, YELLOW, Align::Min);
                widgets::gap(ui, GAP);
                let label = format!("2. Paste the reply from {}", app.state.label);
                widgets::text_box(ui, &label, STRING_BOX, &mut app.state.forms.reply);
                if !app.state.error.is_empty() {
                    widgets::paragraph(ui, &app.state.error, RED, Align::Min);
                }
            },
            |app, ui| {
                let half = (COLUMN - GAP) / 2.0;
                match app.buttons(ui, &[("Connect", true), ("Cancel", true)], half) {
                    Some(0) => app.submit_reply(),
                    Some(_) => app.send("cancel".to_owned()),
                    None => {}
                }
            },
        );
    }

    fn join_setup(&mut self, ui: &mut Ui) {
        let default_listen = self.sessions.default_listen().to_owned();
        self.page(
            ui,
            "Join a Friend",
            WIDE_COLUMN,
            false,
            |app, ui| {
                let forms = &mut app.state.forms;
                widgets::text_box(
                    ui,
                    "Paste the invite from the host",
                    STRING_BOX * 1.5,
                    &mut forms.invite,
                );
                widgets::gap(ui, GAP);
                widgets::field(
                    ui,
                    "Local Address for Minecraft",
                    &mut forms.listen,
                    &default_listen,
                    |edit| edit,
                );
                if !app.state.error.is_empty() {
                    widgets::paragraph(ui, &app.state.error, RED, Align::Min);
                }
            },
            |app, ui| {
                let half = (COLUMN - GAP) / 2.0;
                match app.buttons(ui, &[("Join Server", true), ("Cancel", true)], half) {
                    Some(0) => app.start_join(),
                    Some(_) => app.cancel_join(),
                    None if Self::escape_pressed(ui) => app.cancel_join(),
                    None => {}
                }
            },
        );
    }

    fn reply(&mut self, ui: &mut Ui, now: f64) {
        self.page(
            ui,
            "Send This Reply to the Host",
            WIDE_COLUMN,
            false,
            |app, ui| {
                let mut reply = app.state.reply.as_str();
                widgets::text_box(ui, "Your reply", STRING_BOX * 1.5, &mut reply);
                let text = app.state.reply.clone();
                app.copy_button(ui, "Copy Reply", text, now);
                let warning =
                    "Replies contain your IP address. Only share them with people you trust.";
                widgets::paragraph(ui, warning, YELLOW, Align::Min);
                widgets::gap(ui, GAP * 2.0);
                widgets::paragraph(ui, "Waiting for the host", WHITE, Align::Center);
                widgets::paragraph(ui, loading_dots(now), MUTED, Align::Center);
                ui.ctx().request_repaint_after(ANIMATION_FRAME);
            },
            |app, ui| {
                if app.buttons(ui, &[("Cancel", true)], COLUMN).is_some() {
                    app.sessions.stop();
                }
            },
        );
    }

    fn joined(&mut self, ui: &mut Ui, now: f64) {
        self.page(
            ui,
            "Connected!",
            WIDE_COLUMN,
            false,
            |app, ui| {
                let Some(joined) = app.state.joined.clone() else {
                    return;
                };
                widgets::paragraph(ui, "Add this server in Minecraft", MUTED, Align::Center);
                widgets::gap(ui, GAP);
                widgets::big_paragraph(ui, &joined.address, WHITE);
                widgets::gap(ui, GAP);
                app.copy_button(ui, "Copy Address", joined.address.clone(), now);
                widgets::gap(ui, GAP);
                let detail = plural(joined.connections, "connection");
                widgets::list_entry(ui, &app.theme, "Host", &detail, joined.ping);
                if let Some(route) = describe_route(joined.route.as_deref()) {
                    widgets::paragraph(ui, route, MUTED, Align::Center);
                }
            },
            |app, ui| {
                if app.buttons(ui, &[("Disconnect", true)], COLUMN).is_some() {
                    app.sessions.stop();
                }
            },
        );
    }

    fn options(&mut self, ui: &mut Ui) {
        self.page(
            ui,
            "Options",
            COLUMN,
            false,
            |app, ui| {
                let forms = &mut app.state.forms;
                widgets::field(
                    ui,
                    "STUN Servers (space separated)",
                    &mut forms.stun,
                    "Public defaults",
                    |edit| edit,
                );
                widgets::field(
                    ui,
                    "TURN Server",
                    &mut forms.turn_url,
                    "turn:example.com:3478",
                    |edit| edit,
                );
                halves(ui, |left, right| {
                    widgets::field(left, "TURN Username", &mut forms.turn_user, "", |edit| edit);
                    widgets::field(right, "TURN Password", &mut forms.turn_pass, "", |edit| {
                        edit.password(true)
                    });
                });
                widgets::gap(ui, GAP);
                let hint = "TURN relays your traffic when a direct connection is impossible.";
                widgets::paragraph(ui, hint, MUTED, Align::Min);
            },
            |app, ui| {
                let done = app.buttons(ui, &[("Done", true)], COLUMN).is_some();
                if done || Self::escape_pressed(ui) {
                    app.state.screen = Screen::Title;
                }
            },
        );
    }

    fn progress(&mut self, ui: &mut Ui, screen: Rect, now: f64) {
        let Message { title, detail } = self.state.progress.clone();
        let mut column = centered_column(ui, screen, WIDE_COLUMN, screen.height() / 2.0 - 30.0);
        widgets::paragraph(&mut column, &title, WHITE, Align::Center);
        widgets::gap(&mut column, GAP);
        widgets::paragraph(&mut column, &detail, MUTED, Align::Center);
        widgets::gap(&mut column, GAP);
        widgets::paragraph(&mut column, loading_dots(now), MUTED, Align::Center);
        ui.ctx().request_repaint_after(ANIMATION_FRAME);
    }

    fn failed(&mut self, ui: &mut Ui, screen: Rect) {
        let Message { title, detail } = self.state.failure.clone();
        let back = self.state.back_from_failure();
        let label = match back {
            Screen::Friends => "Back to Friends",
            _ => "Back to Title Screen",
        };
        let mut column = centered_column(ui, screen, WIDE_COLUMN, screen.height() / 2.0 - 50.0);
        widgets::paragraph(&mut column, &title, MUTED, Align::Center);
        widgets::gap(&mut column, GAP * 2.0);
        widgets::paragraph(&mut column, &detail, RED, Align::Center);
        widgets::gap(&mut column, GAP * 3.0);
        if self
            .buttons(&mut column, &[(label, true)], COLUMN)
            .is_some()
        {
            self.state.screen = back;
        }
    }
}

fn loading_dots(now: f64) -> &'static str {
    LOADING[(now / 0.3) as usize % LOADING.len()]
}

fn centered_column(ui: &mut Ui, screen: Rect, width: f32, top: f32) -> Ui {
    let width = width.min(screen.width() - 32.0);
    let left = (screen.center().x - width / 2.0).round();
    let rect = Rect::from_min_max(
        pos2(left, screen.top() + top.round()),
        pos2(left + width, screen.bottom()),
    );
    let builder = UiBuilder::new()
        .id_salt("centered")
        .max_rect(rect)
        .layout(Layout::top_down(Align::Min));
    ui.new_child(builder)
}

/// Two columns side by side, for fields that come in pairs.
fn halves(ui: &mut Ui, add: impl FnOnce(&mut Ui, &mut Ui)) {
    let width = (ui.available_width() - GAP) / 2.0;
    let top = ui.cursor().min;
    let column = |left: f32, salt: &str, ui: &mut Ui| {
        let rect = Rect::from_min_size(pos2(left, top.y), vec2(width, 10_000.0));
        let builder = UiBuilder::new()
            .id_salt(salt.to_owned())
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min));
        ui.new_child(builder)
    };
    let mut left = column(top.x, "left", ui);
    let mut right = column(top.x + width + GAP, "right", ui);
    add(&mut left, &mut right);
    let bottom = left.min_rect().bottom().max(right.min_rect().bottom());
    ui.advance_cursor_after_rect(Rect::from_min_max(
        top,
        pos2(top.x + width * 2.0 + GAP, bottom),
    ));
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let ppp = ctx.pixels_per_point();
        let scale = gui_scale(ctx.content_rect().size() * ppp);
        if scale != ppp {
            ctx.set_pixels_per_point(scale);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

//! Drives the real interface without a window: clicks, typing and pastes go in through egui, and
//! the two-window test tunnels over real peer connections on loopback.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::accesskit::Role;
use eframe::egui::{Event, OutputCommand};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use tokio::runtime::Runtime;

use super::session::Settings;
use super::*;
use crate::peer::PeerOptions;

const LIMIT: Duration = Duration::from_secs(30);

type Window = Harness<'static, Option<App>>;

fn options(server: String, listen: String) -> UiOptions {
    UiOptions {
        peer: PeerOptions::loopback(),
        server,
        listen,
        max_peers: None,
        max_connections: None,
        whitelist: None,
    }
}

fn window_with(runtime: &Runtime, options: UiOptions, size: (f32, f32)) -> Window {
    let (sessions, events) = Sessions::new(options, runtime.handle().clone(), Arc::new(|| {}));
    let mut starting = Some((Arc::new(sessions), events));
    let mut window = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(2.0)
        .with_step_dt(0.05)
        .build_ui_state(
            move |ui, app: &mut Option<App>| {
                if let Some((sessions, events)) = starting.take() {
                    *app = Some(App::new(ui.ctx(), sessions, events));
                }
                app.as_mut().unwrap().show(ui);
            },
            None,
        );
    window.run_steps(3);
    window
}

fn window(runtime: &Runtime) -> Window {
    let free = "127.0.0.1:0".to_owned();
    window_with(
        runtime,
        options("127.0.0.1:9".to_owned(), free),
        (500.0, 320.0),
    )
}

fn state(window: &Window) -> &State {
    &window.state().as_ref().unwrap().state
}

fn state_mut(window: &mut Window) -> &mut State {
    &mut window.state_mut().as_mut().unwrap().state
}

fn click(window: &mut Window, label: &str) {
    window.get_by_label(label).click();
    window.run_steps(3);
}

fn paste(window: &mut Window, role: Role, label: &str, text: &str) {
    window.get_by_role_and_label(role, label).focus();
    window.run_steps(2);
    window.event(Event::Paste(text.to_owned()));
    window.run_steps(2);
}

fn wait(windows: &mut [&mut Window], done: impl Fn(&[&mut Window]) -> bool) {
    let start = Instant::now();
    while !done(windows) {
        assert!(start.elapsed() < LIMIT, "the interface never got there");
        for window in windows.iter_mut() {
            window.step();
        }
        thread::sleep(Duration::from_millis(10));
    }
    for window in windows.iter_mut() {
        window.run_steps(2);
    }
}

fn wait_for(window: &mut Window, screen: Screen) {
    let mut windows = [window];
    wait(&mut windows, |windows| state(windows[0]).screen == screen);
}

fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            thread::spawn(move || {
                let mut buffer = [0; 4096];
                while let Ok(read) = stream.read(&mut buffer) {
                    if read == 0 || stream.write_all(&buffer[..read]).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Presses a copy button and returns what reached the clipboard.
fn click_to_copy(window: &mut Window, label: &str) -> String {
    window.get_by_label(label).click();
    for _ in 0..4 {
        window.step();
        let commands = &window.output().platform_output.commands;
        let copied = commands.iter().find_map(|command| match command {
            OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        });
        if let Some(copied) = copied {
            window.run_steps(2);
            return copied;
        }
    }
    panic!("{label} copied nothing");
}

/// Hosting up to the friends list, ready to invite.
fn start_hosting(window: &mut Window) {
    click(window, "Host a Server");
    click(window, "Start Hosting");
    let mut windows = [window];
    wait(&mut windows, |windows| state(windows[0]).name_ready);
}

fn make_invite(window: &mut Window, name: &str) {
    click(window, "Invite a Friend");
    window
        .get_by_role_and_label(Role::TextInput, "Friend's Name")
        .type_text(name);
    window.run_steps(2);
    click(window, "Create Invite");
    wait_for(window, Screen::Invite);
}

#[test]
fn gui_scale_is_the_largest_that_fits_like_the_game() {
    assert_eq!(gui_scale(vec2(1000.0, 640.0)), 2.0);
    assert_eq!(gui_scale(vec2(1920.0, 1080.0)), 4.0);
    assert_eq!(gui_scale(vec2(640.0, 480.0)), 2.0);
    assert_eq!(gui_scale(vec2(300.0, 200.0)), 1.0);
}

#[test]
fn the_title_screen_leads_everywhere_and_back() {
    let runtime = Runtime::new().unwrap();
    let mut window = window(&runtime);
    assert_eq!(state(&window).screen, Screen::Title);

    click(&mut window, "Options...");
    assert_eq!(state(&window).screen, Screen::Options);
    window.key_press(Key::Escape);
    window.run_steps(3);
    assert_eq!(state(&window).screen, Screen::Title);

    click(&mut window, "Host a Server");
    assert_eq!(state(&window).screen, Screen::HostSetup);
    click(&mut window, "Cancel");

    click(&mut window, "Join a Friend");
    assert_eq!(state(&window).screen, Screen::JoinSetup);
    click(&mut window, "Cancel");
    assert_eq!(state(&window).screen, Screen::Title);
}

#[test]
fn options_are_kept_and_a_bad_one_stops_the_session_from_starting() {
    let runtime = Runtime::new().unwrap();
    let mut window = window(&runtime);
    click(&mut window, "Options...");
    let stun = "STUN Servers (space separated)";
    paste(&mut window, Role::TextInput, stun, "not-a-url");
    click(&mut window, "Done");
    assert_eq!(state(&window).forms.stun, "not-a-url");

    click(&mut window, "Host a Server");
    click(&mut window, "Start Hosting");
    assert_eq!(state(&window).screen, Screen::Failed);
    assert_eq!(state(&window).failure.title, "Could Not Start");
    click(&mut window, "Back to Title Screen");
    assert_eq!(state(&window).screen, Screen::Title);
}

#[test]
fn joining_needs_an_invite_and_a_bad_one_can_be_corrected() {
    let runtime = Runtime::new().unwrap();
    let mut window = window(&runtime);
    click(&mut window, "Join a Friend");
    click(&mut window, "Join Server");
    assert_eq!(state(&window).error, "Paste the invite first.");

    let label = "Paste the invite from the host";
    paste(
        &mut window,
        Role::MultilineTextInput,
        label,
        "mcrtc1:garbage!",
    );
    click(&mut window, "Join Server");
    let mut windows = [&mut window];
    wait(&mut windows, |windows| {
        let state = state(windows[0]);
        state.awaiting_invite && !state.error.is_empty()
    });
    assert_eq!(state(&window).screen, Screen::JoinSetup);

    // Cancel stops the session the first attempt left waiting.
    click(&mut window, "Cancel");
    wait_for(&mut window, Screen::Title);
}

#[test]
fn a_bad_reply_keeps_the_invite_and_cancel_goes_back_to_the_list() {
    let runtime = Runtime::new().unwrap();
    let mut window = window(&runtime);
    start_hosting(&mut window);
    make_invite(&mut window, "alex");
    assert!(state(&window).invite.starts_with("mcrtc1:"));

    let label = "2. Paste the reply from alex";
    paste(
        &mut window,
        Role::MultilineTextInput,
        label,
        "mcrtc1:garbage!",
    );
    click(&mut window, "Connect");
    let mut windows = [&mut window];
    wait(&mut windows, |windows| !state(windows[0]).error.is_empty());
    assert_eq!(state(&window).screen, Screen::Invite);

    click(&mut window, "Cancel");
    wait_for(&mut window, Screen::Friends);
    assert_eq!(state(&window).friends.len(), 0);

    click(&mut window, "Stop Hosting");
    wait_for(&mut window, Screen::Title);
}

#[test]
fn only_one_session_runs_at_a_time() {
    let runtime = Runtime::new().unwrap();
    let mut window = window(&runtime);
    start_hosting(&mut window);
    let sessions = window.state().as_ref().unwrap().sessions.clone();
    let second = sessions.start(Mode::Join, Settings::default());
    assert!(second.unwrap_err().to_string().contains("already running"));
    sessions.shut_down();
    assert!(sessions.send("anyone there?".to_owned()).is_err());
}

#[test]
fn a_failed_send_shows_up_instead_of_vanishing() {
    let runtime = Runtime::new().unwrap();
    let mut window = window(&runtime);
    state_mut(&mut window).name_ready = true;
    state_mut(&mut window).screen = Screen::Name;
    window.run_steps(3);
    click(&mut window, "Create Invite");
    assert_eq!(state(&window).screen, Screen::Failed);
    assert_eq!(state(&window).failure.detail, "no session is running");
}

#[test]
fn the_smallest_window_still_reaches_every_button() {
    let runtime = Runtime::new().unwrap();
    let free = "127.0.0.1:0".to_owned();
    let options = options("127.0.0.1:9".to_owned(), free);
    let mut window = window_with(&runtime, options, (320.0, 240.0));
    start_hosting(&mut window);
    make_invite(&mut window, "alex");
    for label in ["Copy Invite", "Connect", "Cancel"] {
        let rect = window.get_by_label(label).rect();
        assert!(rect.bottom() <= 240.0, "{label} is off screen at {rect:?}");
    }
    click(&mut window, "Cancel");
    wait_for(&mut window, Screen::Friends);
}

/// A host and a friend, each in their own interface, doing what the README tells them to.
#[test]
fn two_windows_tunnel_to_the_server() {
    let runtime = Runtime::new().unwrap();
    let port = free_port();
    let server = format!("127.0.0.1:{}", echo_server());
    let host_options = options(server, "127.0.0.1:0".to_owned());
    let mut host = window_with(&runtime, host_options, (500.0, 320.0));
    let friend_options = options("127.0.0.1:9".to_owned(), format!("127.0.0.1:{port}"));
    let mut friend = window_with(&runtime, friend_options, (500.0, 320.0));

    start_hosting(&mut host);
    make_invite(&mut host, "alex");
    let invite = click_to_copy(&mut host, "Copy Invite");
    assert_eq!(invite, state(&host).invite);
    assert!(host.query_by_label("Copied!").is_some());

    click(&mut friend, "Join a Friend");
    // Chat apps wrap long strings, so the paste has to survive that.
    let (start, end) = invite.split_at(40);
    let wrapped = format!("  {start}\n{end}\n");
    let label = "Paste the invite from the host";
    paste(&mut friend, Role::MultilineTextInput, label, &wrapped);
    click(&mut friend, "Join Server");
    wait_for(&mut friend, Screen::Reply);
    let reply = click_to_copy(&mut friend, "Copy Reply");

    let label = "2. Paste the reply from alex";
    paste(&mut host, Role::MultilineTextInput, label, &reply);
    click(&mut host, "Connect");
    let mut windows = [&mut host, &mut friend];
    wait(&mut windows, |windows| {
        state(windows[1]).screen == Screen::Joined
            && state(windows[0]).screen == Screen::Friends
            && state(windows[0]).friends.len() == 1
    });
    assert!(host.query_by_label("alex").is_some());
    let shown = state(&friend).joined.as_ref().unwrap().address.clone();
    assert_eq!(shown, format!("127.0.0.1:{port}"));
    assert!(friend.query_by_label(&shown).is_some());

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    stream.write_all(b"hello through the window").unwrap();
    let mut answer = [0; 24];
    stream.read_exact(&mut answer).unwrap();
    assert_eq!(&answer, b"hello through the window");

    let mut windows = [&mut host, &mut friend];
    wait(&mut windows, |windows| {
        state(windows[0])
            .friends
            .first()
            .is_some_and(|f| f.connections == 1)
    });
    drop(stream);

    // Leaving: the friend's window says goodbye and the host's list empties.
    click(&mut friend, "Disconnect");
    let mut windows = [&mut host, &mut friend];
    wait(&mut windows, |windows| {
        state(windows[1]).screen == Screen::Title && state(windows[0]).friends.is_empty()
    });
    click(&mut host, "Stop Hosting");
    wait_for(&mut host, Screen::Title);
}

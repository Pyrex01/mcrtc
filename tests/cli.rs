//! Drives the real binary the way a person would: two processes, stdin and stdout, and strings
//! carried between them. Uses local interfaces only, with no STUN servers.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(30);

struct Process {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Process {
    fn spawn(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcrtc"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").unwrap();
    }

    fn wait_for(&self, predicate: impl Fn(&str) -> bool) -> String {
        loop {
            let line = self
                .lines
                .recv_timeout(LIMIT)
                .expect("the process stopped talking");
            if predicate(&line) {
                return line;
            }
        }
    }

    fn wait_contains(&self, text: &str) -> String {
        self.wait_for(|line| line.contains(text))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn no_stun_config(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("mcrtc-{}-{name}.toml", std::process::id()));
    fs::write(&path, "stun = []\n").unwrap();
    path
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
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn assert_tunnel_works(port: u16, message: &[u8]) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    stream.write_all(message).unwrap();
    let mut reply = vec![0; message.len()];
    stream.read_exact(&mut reply).unwrap();
    assert_eq!(reply, message);
}

fn invite_friend(host: &mut Process, name: &str, port: u16, config: &str) -> Process {
    host.wait_contains("Name for this friend");
    host.send(name);
    let invite = host.wait_for(|line| line.starts_with("mcrtc1:"));
    println!("invite is {} characters", invite.len());

    let listen = format!("127.0.0.1:{port}");
    let mut friend = Process::spawn(&["join", "--listen", &listen, "--config", config]);
    friend.wait_contains("Paste the INVITE");
    friend.send(&invite);
    let reply = friend.wait_for(|line| line.starts_with("mcrtc1:"));
    println!("reply is {} characters", reply.len());

    host.wait_contains("REPLY here");
    host.send(&reply);
    host.wait_contains(&format!("{name} connected."));
    friend.wait_contains("Connected!");
    friend
}

#[test]
fn two_friends_reach_the_server_through_real_processes() {
    let config = no_stun_config("friends");
    let config = config.to_str().unwrap();
    let server = format!("127.0.0.1:{}", echo_server());
    let mut host = Process::spawn(&["host", "--server", &server, "--config", config]);

    let first_port = free_port();
    let first = invite_friend(&mut host, "alex", first_port, config);
    assert_tunnel_works(first_port, b"hello from alex");

    host.wait_contains("Press Enter for another friend");
    host.send("");
    let second_port = free_port();
    let second = invite_friend(&mut host, "sam", second_port, config);

    assert_tunnel_works(second_port, b"hello from sam");
    assert_tunnel_works(first_port, b"alex is still here");
    drop((first, second));
}

/// The friend's mcrtc takes one port on both protocols for Bedrock.
fn free_tcp_and_udp_address() -> SocketAddr {
    loop {
        let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = tcp.local_addr().unwrap();
        if UdpSocket::bind(address).is_ok() {
            return address;
        }
    }
}

fn udp_echo_server() -> u16 {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    thread::spawn(move || {
        let mut buffer = [0; 2048];
        while let Ok((read, client)) = socket.recv_from(&mut buffer) {
            let _ = socket.send_to(&buffer[..read], client);
        }
    });
    port
}

#[test]
fn a_bedrock_game_reaches_the_server_through_real_processes() {
    let config = no_stun_config("bedrock");
    let config = config.to_str().unwrap();
    let server = format!("127.0.0.1:{}", udp_echo_server());
    let args = [
        "host",
        "--edition",
        "bedrock",
        "--server",
        &server,
        "--config",
        config,
    ];
    let mut host = Process::spawn(&args);
    host.wait_contains("Name for this friend");
    host.send("alex");
    let invite = host.wait_for(|line| line.starts_with("mcrtc1:"));

    let address = free_tcp_and_udp_address();
    let listen = address.to_string();
    let args = [
        "join",
        "--edition",
        "bedrock",
        "--listen",
        &listen,
        "--config",
        config,
    ];
    let mut friend = Process::spawn(&args);
    friend.wait_contains("Paste the INVITE");
    friend.send(&invite);
    let reply = friend.wait_for(|line| line.starts_with("mcrtc1:"));
    host.wait_contains("REPLY here");
    host.send(&reply);
    host.wait_contains("alex connected.");
    friend.wait_contains("Connected!");

    let game = UdpSocket::bind("127.0.0.1:0").unwrap();
    game.connect(address).unwrap();
    game.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut answer = [0; 32];
    for _ in 0..(LIMIT.as_millis() / 200) {
        game.send(b"hello from bedrock").unwrap();
        if let Ok(read) = game.recv(&mut answer) {
            assert_eq!(&answer[..read], b"hello from bedrock");
            return;
        }
    }
    panic!("no datagram came back");
}

#[test]
fn an_invite_for_the_other_edition_is_refused_with_directions() {
    let config = no_stun_config("mismatch");
    let config = config.to_str().unwrap();
    let args = ["host", "--edition", "bedrock", "--config", config];
    let mut host = Process::spawn(&args);
    host.wait_contains("Name for this friend");
    host.send("alex");
    let invite = host.wait_for(|line| line.starts_with("mcrtc1:"));

    let listen = format!("127.0.0.1:{}", free_port());
    let mut friend = Process::spawn(&["join", "--listen", &listen, "--config", config]);
    friend.wait_contains("Paste the INVITE");
    friend.send(&invite);
    friend.wait_contains("--edition bedrock");
}

#[test]
fn a_bad_paste_does_not_cost_the_invite() {
    let config = no_stun_config("bad-paste");
    let config = config.to_str().unwrap();
    let server = format!("127.0.0.1:{}", echo_server());
    let mut host = Process::spawn(&["host", "--server", &server, "--config", config]);
    host.wait_contains("Name for this friend");
    host.send("alex");
    let invite = host.wait_for(|line| line.starts_with("mcrtc1:"));
    host.wait_contains("REPLY here");

    host.send("mcrtc1:garbage!");
    host.wait_contains("That did not work");
    host.send(&invite);
    host.wait_contains("expected a reply");
    host.send("cancel");
    host.wait_contains("Cancelled.");
}

#[test]
fn the_whitelist_is_enforced_with_a_stable_identity() {
    let config = no_stun_config("whitelist");
    let config = config.to_str().unwrap();
    let directory = std::env::temp_dir().join(format!("mcrtc-cli-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let identity = directory.join("friend.pem");
    let identity = identity.to_str().unwrap();
    let whitelist = directory.join("ids.txt");
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");

    // A first run creates the identity and shows the ID to send to the host.
    let friend = Process::spawn(&[
        "join",
        "--listen",
        &listen,
        "--config",
        config,
        "--identity",
        identity,
    ]);
    let line = friend.wait_contains("Your mcrtc ID:");
    let id = line.trim_start_matches("Your mcrtc ID: ").to_owned();
    drop(friend);

    let server = format!("127.0.0.1:{}", echo_server());
    let run_host = |ids: &str| {
        fs::write(&whitelist, ids).unwrap();
        let path = whitelist.to_str().unwrap();
        let mut host = Process::spawn(&[
            "host",
            "--server",
            &server,
            "--config",
            config,
            "--whitelist-file",
            path,
        ]);
        host.wait_contains("Name for this friend");
        host.send("alex");
        let invite = host.wait_for(|line| line.starts_with("mcrtc1:"));
        host.wait_contains("REPLY here");
        (host, invite)
    };
    let run_friend = |invite: &str| {
        let mut friend = Process::spawn(&[
            "join",
            "--listen",
            &listen,
            "--config",
            config,
            "--identity",
            identity,
        ]);
        friend.wait_contains("Paste the INVITE");
        friend.send(invite);
        let reply = friend.wait_for(|line| line.starts_with("mcrtc1:"));
        (friend, reply)
    };

    // Not listed: refused before any connection is attempted.
    let (mut host, invite) = run_host("sha-256 00:11\n");
    let (friend, reply) = run_friend(&invite);
    host.send(&reply);
    host.wait_contains("not on the whitelist");
    drop((friend, host));

    // Listed: the same identity gets in.
    let (mut host, invite) = run_host(&format!("{id} # alex\n"));
    let (friend, reply) = run_friend(&invite);
    host.send(&reply);
    host.wait_contains("alex connected.");
    friend.wait_contains("Connected!");
    assert_tunnel_works(port, b"on the list");
    fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn raw_json_invites_work_with_multi_line_pastes() {
    let config = no_stun_config("raw-json");
    let config = config.to_str().unwrap();
    let server = format!("127.0.0.1:{}", echo_server());
    let mut host = Process::spawn(&[
        "host",
        "--server",
        &server,
        "--config",
        config,
        "--raw-json",
    ]);
    host.wait_contains("Name for this friend");
    host.send("alex");

    let read_json = |process: &Process| {
        let mut json = vec![process.wait_for(|line| line == "{")];
        loop {
            let line = process.wait_for(|_| true);
            let done = line == "}";
            json.push(line);
            if done {
                return json;
            }
        }
    };

    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let invite = read_json(&host);
    let mut friend = Process::spawn(&[
        "join",
        "--listen",
        &listen,
        "--config",
        config,
        "--raw-json",
    ]);
    friend.wait_contains("Paste the INVITE");
    for line in &invite {
        friend.send(line);
    }
    friend.send("");

    let reply = read_json(&friend);
    host.wait_contains("REPLY here");
    for line in &reply {
        host.send(line);
    }
    host.send("");
    host.wait_contains("alex connected.");
    friend.wait_contains("Connected!");
    assert_tunnel_works(port, b"raw json");
}

#[cfg(unix)]
#[test]
fn ctrl_c_says_goodbye_and_exits_cleanly() {
    let config = no_stun_config("ctrl-c");
    let config = config.to_str().unwrap();
    let server = format!("127.0.0.1:{}", echo_server());
    let mut host = Process::spawn(&["host", "--server", &server, "--config", config]);
    let port = free_port();
    let mut friend = invite_friend(&mut host, "alex", port, config);
    assert_tunnel_works(port, b"before ctrl-c");

    let interrupted = Command::new("kill")
        .args(["-INT", &host.child.id().to_string()])
        .status()
        .unwrap();
    assert!(interrupted.success());

    friend.wait_contains("The host closed the connection.");
    assert!(host.child.wait().unwrap().success());
    assert!(friend.child.wait().unwrap().success());
}

#[cfg(unix)]
#[test]
fn max_peers_holds_back_the_next_invite() {
    let config = no_stun_config("max-peers");
    let config = config.to_str().unwrap();
    let server = format!("127.0.0.1:{}", echo_server());
    let mut host = Process::spawn(&[
        "host",
        "--server",
        &server,
        "--config",
        config,
        "--max-peers",
        "1",
    ]);
    let port = free_port();
    let friend = invite_friend(&mut host, "alex", port, config);

    host.wait_contains("Press Enter for another friend");
    host.send("");
    host.wait_contains("--max-peers reached");

    // The slot opens up as soon as alex says goodbye.
    let interrupted = Command::new("kill")
        .args(["-INT", &friend.child.id().to_string()])
        .status()
        .unwrap();
    assert!(interrupted.success());
    host.wait_contains("Name for this friend");
}

#[test]
fn the_interface_explains_itself_without_a_display() {
    let config = no_stun_config("no-display");
    let output = Command::new(env!("CARGO_BIN_EXE_mcrtc"))
        .args(["ui", "--config", config.to_str().unwrap()])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("could not open a window"), "{error}");
    assert!(error.contains("mcrtc host"), "{error}");
}

# Android migration plan

## Goal

Move the networking and tunnel implementation into a reusable Rust library, then keep two
frontends:

1. **Desktop:** the existing terminal and `egui` interface, with current behavior and visual
   design preserved.
2. **Android:** a small Kotlin app using the Android SDK Views toolkit, not Jetpack Compose.

The Android app is a controller for the tunnel. It does not implement Minecraft networking itself.
The shared Rust library continues to own WebRTC, ICE, DTLS, invite encoding, TCP forwarding,
whitelisting, identity certificates, limits, and connection state.

## Target layout

The current crate can be split incrementally. The intended end state is:

```text
crates/
  mcrtc-core/          # platform-neutral Rust networking library
  mcrtc-cli/           # desktop terminal frontend
  mcrtc-desktop/       # existing egui frontend
android/
  app/                 # minimal Kotlin Android frontend
  mcrtc-android/       # JNI/UniFFI bridge and packaged native libraries
```

The exact directory names are not mandatory. The important boundary is that `mcrtc-core` must not
depend on `eframe`, `egui`, terminal input, desktop windowing, or desktop filesystem conventions.

## Rust library boundary

Move or keep these areas in the library:

- `bridge.rs`: TCP to reliable ordered WebRTC data-channel forwarding
- `peer.rs`: peer connection, ICE gathering/checking, DTLS identity and connection monitoring
- `host.rs` and `join.rs`: host/join session state machines
- `invite.rs`: invite/reply parsing, compression, encoding, expiry, and validation
- `config.rs`: ICE, STUN, TURN, server/listen, limits, identity, and whitelist settings
- `control.rs`: heartbeat, channel negotiation, shutdown, and session control
- `events.rs`: frontend-neutral progress, status, error, friend, and connection events
- `datagram.rs` and related networking changes already in progress

The library should expose operations and events rather than print text. It must not read stdin,
write terminal output, open a window, access Android UI APIs, or assume desktop paths.

## Proposed library API

The API should be small and asynchronous internally. The binding layer can expose a simpler
command/event model to Kotlin.

```rust
pub struct Config {
    pub server: String,              // host target, e.g. 127.0.0.1:25565
    pub listen: String,              // join local endpoint
    pub stun: Vec<String>,
    pub turn: Vec<TurnServer>,
    pub identity_path: Option<PathBuf>,
    pub whitelist: Option<HashSet<String>>,
    pub max_peers: Option<usize>,
    pub max_connections: Option<usize>,
}

pub enum Mode { Host, Join }

pub enum Command {
    Start { mode: Mode, config: Config },
    SubmitName(String),
    SubmitInvite(String),
    SubmitReply(String),
    Stop,
}

pub enum Event {
    Started { mode: Mode },
    Gathering,
    InviteReady { label: String, text: String },
    ReplyReady { text: String },
    WaitingForReply,
    Connected { route: String, address: String },
    FriendUpdated { name: String, connections: usize, ping_ms: Option<u64> },
    Failed { message: String },
    Stopped,
}
```

The exact Rust types may differ, but both frontends must receive the same capabilities. Prefer a
blocking `send(Command)` plus an event receiver at the binding boundary. Rust can continue using
Tokio internally without requiring Kotlin to understand Tokio tasks.

## Desktop frontend

The desktop version remains the default product:

- Keep the current CLI commands and flags.
- Keep the current `egui` UI and Minecraft-style rendering.
- Replace direct calls to `host::run` and `join::run` with the library command/event API.
- Keep terminal input/output in desktop adapters.
- Keep `src/ui` as a desktop-only presentation layer.

This is an API migration, not a UI redesign. Existing invite strings, config files, whitelist
files, identity files, and command behavior must remain compatible.

## Android frontend: minimum-weight design

Use the standard Android SDK rather than Jetpack Compose:

- Kotlin Activities and Fragments only where needed
- XML layouts or programmatic `View` construction
- `TextView`, `EditText`, `Button`, `LinearLayout`, `ScrollView`, and `RecyclerView` only when a
  friend list needs it
- Android `ClipboardManager` for invite/reply copy and paste
- A small `ForegroundService` to keep the tunnel alive while Minecraft is running
- A persistent notification showing Host/Join and Connected/Disconnected state

Avoid Compose, a web runtime, an embedded browser, and a second networking implementation. The
Android UI only needs the existing flow:

```text
Title -> Host setup -> Friends -> Invite/reply -> Connected
Title -> Join setup -> Reply -> Connected
                         \-> Error / Options
```

Use a single-activity app with a simple screen/state controller. A ViewModel is optional; if used,
keep it small and make the Rust session the source of truth for network state.

## Rust-to-Kotlin boundary

Use UniFFI if the generated Kotlin bindings meet the API shape. Otherwise use a narrow JNI layer.
Do not expose Tokio, trait objects, Rust channels, or WebRTC objects to Kotlin.

The binding should provide:

- create/destroy a native session handle
- start host or join mode
- submit name, invite, and reply strings
- stop the session
- poll or subscribe to serialized events
- report the local listening address
- return validation errors as plain strings or error codes

Build the native library for at least `arm64-v8a`; add other ABIs only when device coverage
requires them. Package the `.so` files through the Android Gradle project. Keep Android-specific
code limited to library loading, lifecycle, and foreground-service integration.

## Android lifecycle and networking

The tunnel cannot rely on an Activity remaining visible. The service must:

1. Start as a foreground service before hosting or joining.
2. Own the native session handle while the tunnel is active.
3. Publish state changes to the Activity when it is visible.
4. Continue working when the screen is locked or the Activity is recreated.
5. Stop the native session and release sockets when the user stops the service.

Request only the permissions required by the selected Android SDK, including applicable
foreground-service network permissions. The exact manifest entries must be checked during
implementation against the target SDK.

The app should warn users that Minecraft Java on Android generally requires a Java launcher such as
PojavLauncher, while normal Minecraft Bedrock cannot directly connect to a Java server through
this project. `127.0.0.1:25565` works when the Java client and mcrtc run on the same device and the
launcher can reach the local port. TURN relay mode consumes more data and battery than direct mode.

## Resource expectations

The Rust tunnel should be light while idle: a few async tasks, UDP sockets, a TCP listener, and
WebRTC state. Resource use grows with Minecraft traffic, number of friends, and TURN relaying.
The Android-specific risks are lifecycle and battery policy, not rendering cost.

Do not store Minecraft payloads beyond the existing bounded queues. Preserve the current backpressure
limits in `bridge.rs` so a slow phone or peer cannot cause unbounded memory growth.

## Migration stages

### 1. Stabilize the core boundary

- Identify all UI/terminal calls in host, join, and session code.
- Replace console prompts and printed status with typed commands/events.
- Keep temporary desktop adapters so behavior does not change.
- Add tests for invite compatibility, expiry, whitelist checks, and loopback forwarding.

### 2. Split the Cargo workspace

- Create `mcrtc-core` and move platform-neutral modules into it.
- Make the existing binary depend on `mcrtc-core`.
- Keep `eframe`, `egui`, and desktop-only code out of the core dependency graph.
- Run CLI, UI, and loopback tests after each move.

### 3. Add Android cross-compilation

- Add Android Rust targets and verify the core builds for `arm64-v8a`.
- Resolve native crypto or WebRTC build requirements before writing UI code.
- Add a native smoke test that creates and destroys a session.

### 4. Add the Kotlin Views frontend

- Implement host/join forms and clipboard actions.
- Add the foreground service and persistent notification.
- Connect native events to the small screen/state controller.
- Test Activity recreation, screen lock, backgrounding, and service shutdown.

### 5. Interoperability testing

- Desktop host ↔ Android join and Android host ↔ desktop join.
- Direct ICE and TURN relay.
- Multiple Minecraft TCP connections, server-list ping, reconnect, and clean shutdown.
- IPv4-only, IPv6-capable, metered, and restrictive NAT networks.

## Acceptance criteria

- Desktop behavior and command-line compatibility are unchanged.
- Desktop and Android use the same invite/reply format and connect to each other.
- Android has no Compose dependency and no duplicated WebRTC/TCP implementation.
- The tunnel remains alive while the Activity is backgrounded or recreated.
- Stop closes the listener, peer connection, data channels, and foreground service.
- Backpressure and connection limits remain enforced by shared Rust code.
- Core networking tests run without an Android device or desktop window.

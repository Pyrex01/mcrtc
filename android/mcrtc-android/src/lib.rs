use std::sync::Mutex;

use jni::objects::{JClass, JString};
use jni::sys::{jlong, jstring};
use jni::JNIEnv;
use mcrtc_core::{Config, Edition, Event, Mode, Session};

struct NativeState {
    session: Option<Session>,
}

fn text(env: &mut JNIEnv<'_>, value: String) -> jstring {
    env.new_string(value)
        .map_or(std::ptr::null_mut(), |value| value.into_raw())
}

fn error_text(error: impl std::fmt::Display) -> String {
    format!("error: {error}")
}

fn value(env: &mut JNIEnv<'_>, value: JString<'_>) -> anyhow::Result<String> {
    Ok(env.get_string(&value)?.to_string_lossy().into_owned())
}

fn config(mode: Mode, value: &str) -> Config {
    Config {
        server: if mode == Mode::Host && !value.trim().is_empty() {
            value.trim().to_owned()
        } else {
            Edition::Java.default_address().to_owned()
        },
        listen: Edition::Java.default_address().to_owned(),
        stun: vec![
            "stun:stun.l.google.com:19302".to_owned(),
            "stun:stun.cloudflare.com:3478".to_owned(),
        ],
        turn: Vec::new(),
        identity_path: None,
        whitelist: None,
        max_peers: None,
        max_connections: None,
    }
}

#[no_mangle]
pub extern "system" fn Java_com_mcrtc_android_NativeSession_create(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jlong {
    Box::into_raw(Box::new(Mutex::new(NativeState { session: None }))) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_mcrtc_android_NativeSession_command(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    command: JString<'_>,
    argument: JString<'_>,
) -> jstring {
    if handle == 0 {
        return text(
            &mut env,
            "error: native session is not initialized".to_owned(),
        );
    }
    let result = (|| -> anyhow::Result<String> {
        let command = value(&mut env, command)?;
        let argument = value(&mut env, argument)?;
        // The handle is created only by this library and is kept alive by the foreground service.
        let state = unsafe { &*(handle as *const Mutex<NativeState>) };
        let mut state = state
            .lock()
            .map_err(|_| anyhow::anyhow!("native session lock poisoned"))?;
        match command.as_str() {
            "start_host" | "start_join" => {
                if state.session.is_some() {
                    anyhow::bail!("a tunnel is already running");
                }
                let mode = if command == "start_host" {
                    Mode::Host
                } else {
                    Mode::Join
                };
                let session = Session::start(mode, config(mode, &argument))?;
                if mode == Mode::Join && !argument.trim().is_empty() {
                    session.send(mcrtc_core::Command::SubmitInvite(argument))?;
                }
                state.session = Some(session);
                Ok("ok".to_owned())
            }
            "name" => send(&state, mcrtc_core::Command::SubmitName(argument)),
            "invite" => send(&state, mcrtc_core::Command::SubmitInvite(argument)),
            "reply" => send(&state, mcrtc_core::Command::SubmitReply(argument)),
            "stop" => send(&state, mcrtc_core::Command::Stop),
            other => anyhow::bail!("unknown native command `{other}`"),
        }
    })();
    text(&mut env, result.unwrap_or_else(error_text))
}

fn send(state: &NativeState, command: mcrtc_core::Command) -> anyhow::Result<String> {
    state
        .session
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no tunnel is running"))?
        .send(command)?;
    Ok("ok".to_owned())
}

#[no_mangle]
pub extern "system" fn Java_com_mcrtc_android_NativeSession_poll(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jstring {
    if handle == 0 {
        return std::ptr::null_mut();
    }
    let state = unsafe { &*(handle as *const Mutex<NativeState>) };
    let event = state
        .lock()
        .ok()
        .and_then(|state| state.session.as_ref().and_then(Session::try_recv));
    event
        .and_then(|event: Event| serde_json::to_string(&event).ok())
        .map_or(std::ptr::null_mut(), |event| text(&mut env, event))
}

#[no_mangle]
pub extern "system" fn Java_com_mcrtc_android_NativeSession_destroy(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if handle != 0 {
        unsafe {
            drop(Box::from_raw(handle as *mut Mutex<NativeState>));
        }
    }
}

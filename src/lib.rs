//! mcrtc: tunnel Minecraft traffic over WebRTC data channels, with no signaling server.
//!
//! The binary is a thin wrapper around this library. Frontends other than the terminal (the egui
//! window, the Android app) drive the flows through [`session::Sessions`].

pub mod bridge;
pub mod config;
pub mod console;
pub mod control;
pub mod datagram;
pub mod edition;
pub mod events;
pub mod host;
pub mod invite;
pub mod join;
#[cfg(test)]
mod loopback_tests;
pub mod nethernet;
pub mod peer;
pub mod session;
#[cfg(feature = "gui")]
pub mod ui;

use std::time::Duration;

use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::time::timeout;

use super::*;

const LIMIT: Duration = Duration::from_secs(5);

const OFFER: &str = "v=0\r\n\
o=- 4611731400430051336 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0\r\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
c=IN IP4 0.0.0.0\r\n\
a=candidate:1 1 udp 2122260223 10.0.0.174 50000 typ host generation 0\r\n\
a=candidate:2 1 udp 2122194687 2001:db8:5439:ff5f::a4 50001 typ host generation 0\r\n\
a=candidate:3 1 udp 2122129151 abc.local 50002 typ host generation 0\r\n\
a=ice-ufrag:abcd\r\n\
a=ice-pwd:abcdefghijklmnopqrstuvwx\r\n\
a=fingerprint:sha-256 7B:8B:F0:65\r\n\
a=setup:actpass\r\n\
a=mid:0\r\n\
a=sctp-port:5000\r\n";

const ANSWER: &str = "v=0\r\n\
o=- 1 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=identity:eyJpZHAiOnt9fQ==\r\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
a=candidate:1 1 udp 2122260223 10.0.0.65 19132 typ host\r\n\
a=candidate:2 1 udp 1686052607 203.0.113.248 19132 typ srflx raddr 0.0.0.0 rport 0\r\n\
a=ice-ufrag:wxyz\r\n\
a=fingerprint:sha-256 AA:BB\r\n\
a=setup:passive\r\n";

fn relay() -> Arc<Relay> {
    Arc::new(Relay::new("192.168.1.20:40000".parse().unwrap()))
}

fn without_candidates(sdp: &str) -> Vec<&str> {
    sdp.split("\r\n")
        .filter(|line| !line.starts_with(CANDIDATE))
        .collect()
}

#[test]
fn an_offer_loses_its_addresses_and_nothing_else() {
    let (sdp, ips) = rewrite_offer(OFFER);
    assert_eq!(
        ips,
        vec![
            "10.0.0.174".parse::<IpAddr>().unwrap(),
            "2001:db8:5439:ff5f::a4".parse().unwrap(),
        ]
    );
    let candidates: Vec<_> = sdp.lines().filter(|l| l.starts_with(CANDIDATE)).collect();
    assert_eq!(candidates, vec![PLACEHOLDER_CANDIDATE]);
    assert!(!sdp.contains("10.0.0.174") && !sdp.contains("abc.local"));
    assert_eq!(without_candidates(&sdp), without_candidates(OFFER));
    assert!(sdp.contains(&format!("{PLACEHOLDER_CANDIDATE}\r\na=ice-ufrag")));
}

#[test]
fn an_answer_points_the_game_at_the_relay() {
    let (sdp, server) = rewrite_answer(ANSWER, "192.168.1.20:40000".parse().unwrap());
    assert_eq!(server.len(), 2);
    assert!(server.iter().all(|address| address.port() == 19132));
    let candidates: Vec<_> = sdp.lines().filter(|l| l.starts_with(CANDIDATE)).collect();
    assert_eq!(
        candidates,
        vec!["a=candidate:mcrtc 1 udp 2130706431 192.168.1.20 40000 typ host"]
    );
    // The identity and fingerprint the game checks are untouched.
    assert_eq!(without_candidates(&sdp), without_candidates(ANSWER));
}

#[test]
fn an_ipv6_relay_is_written_without_brackets() {
    let (sdp, _) = rewrite_answer(ANSWER, "[2001:db8::5]:40000".parse().unwrap());
    assert!(sdp.contains("a=candidate:mcrtc 1 udp 2130706431 2001:db8::5 40000 typ host\r\n"));
}

#[test]
fn an_answer_without_candidates_still_gets_the_relay() {
    let (sdp, server) = rewrite_answer("v=0\r\na=setup:passive\r\n", relay().candidate);
    assert!(server.is_empty());
    assert!(sdp.ends_with("a=candidate:mcrtc 1 udp 2130706431 192.168.1.20 40000 typ host\r\n"));
}

#[test]
fn only_the_game_and_loopback_may_use_the_relay() {
    let relay = relay();
    assert!(relay.allows("127.0.0.1".parse().unwrap()));
    assert!(!relay.allows("10.0.0.174".parse().unwrap()));
    relay.allow(rewrite_offer(OFFER).1);
    assert!(relay.allows("10.0.0.174".parse().unwrap()));
    assert!(!relay.allows("203.0.113.9".parse().unwrap()));
}

/// The rewriter between a fake game and a fake server.
fn start() -> (DuplexStream, DuplexStream, Arc<Relay>) {
    let (game, game_side) = duplex(64 * 1024);
    let (tunnel_side, server) = duplex(64 * 1024);
    let relay = relay();
    tokio::spawn(rewrite_signaling(game_side, tunnel_side, relay.clone()));
    (game, server, relay)
}

/// Reads one message framed by `Content-Length`.
async fn read_message(stream: &mut DuplexStream) -> (String, String) {
    let mut data = Vec::new();
    let mut byte = [0; 1];
    while !data.ends_with(b"\r\n\r\n") {
        timeout(LIMIT, stream.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        data.push(byte[0]);
    }
    let head = String::from_utf8(data).unwrap();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    timeout(LIMIT, stream.read_exact(&mut body))
        .await
        .unwrap()
        .unwrap();
    (head, String::from_utf8(body).unwrap())
}

fn request(method_path: &str, content_type: &str, body: &str) -> String {
    format!(
        "{method_path} HTTP/1.1\r\nHost: 127.0.0.1:19132\r\nContent-Type: {content_type}\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn a_whole_join_is_rewritten_both_ways() {
    let (mut game, mut server, relay) = start();

    // The capability check passes through untouched, both ways.
    let get = "GET /v1/join?version=1.26.62&protocol=2225 HTTP/1.1\r\nHost: x\r\n\r\n";
    game.write_all(get.as_bytes()).await.unwrap();
    let (head, body) = read_message(&mut server).await;
    assert_eq!(head, get);
    assert!(body.is_empty());
    let info = response("200 OK", "application/json", r#"{"name":"A Server"}"#);
    server.write_all(info.as_bytes()).await.unwrap();
    let (head, body) = read_message(&mut game).await;
    assert_eq!(format!("{head}{body}"), info);

    // On the same connection, the offer goes out in tiny pieces and arrives rewritten.
    let post = request(
        "POST /v1/join/16403851536850534224",
        "application/sdp",
        OFFER,
    );
    for piece in post.as_bytes().chunks(7) {
        game.write_all(piece).await.unwrap();
    }
    let (head, body) = read_message(&mut server).await;
    assert!(head.starts_with("POST /v1/join/16403851536850534224 HTTP/1.1\r\n"));
    assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())));
    assert_eq!(body, rewrite_offer(OFFER).0);
    assert!(relay.allows("10.0.0.174".parse().unwrap()));

    let answer = response("200 OK", "application/sdp", ANSWER);
    server.write_all(answer.as_bytes()).await.unwrap();
    let (head, body) = read_message(&mut game).await;
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
    assert_eq!(body, rewrite_answer(ANSWER, relay.candidate).0);
    assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())));
}

#[tokio::test]
async fn a_refused_offer_is_passed_back_as_it_came() {
    let (mut game, mut server, _relay) = start();
    let post = request("POST /v1/join/1", "application/sdp", OFFER);
    game.write_all(post.as_bytes()).await.unwrap();
    read_message(&mut server).await;
    let refusal = response("401 Unauthorized", "application/sdp", ANSWER);
    server.write_all(refusal.as_bytes()).await.unwrap();
    let (head, body) = read_message(&mut game).await;
    assert_eq!(format!("{head}{body}"), refusal);
}

#[tokio::test]
async fn tls_passes_through_untouched() {
    let (mut game, mut server, _relay) = start();
    let hello = [
        0x16, 0x03, 0x01, 0x00, 0xfb, 0x01, 0x00, 0x00, 0xf7, 0x03, 0x03,
    ];
    game.write_all(&hello).await.unwrap();
    let mut received = [0; 11];
    timeout(LIMIT, server.read_exact(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, hello);
    server.write_all(&[0x15, 0x03, 0x03]).await.unwrap();
    let mut alert = [0; 3];
    timeout(LIMIT, game.read_exact(&mut alert))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alert, [0x15, 0x03, 0x03]);
}

#[tokio::test]
async fn chunked_requests_are_left_alone() {
    let (mut game, mut server, _relay) = start();
    let post = format!(
        "POST /v1/join/1 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{OFFER}\r\n0\r\n\r\n",
        OFFER.len()
    );
    game.write_all(post.as_bytes()).await.unwrap();
    game.shutdown().await.unwrap();
    let mut received = String::new();
    timeout(LIMIT, server.read_to_string(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, post);

    // The response to it is not touched either.
    let answer = response("200 OK", "application/sdp", ANSWER);
    server.write_all(answer.as_bytes()).await.unwrap();
    let (head, body) = read_message(&mut game).await;
    assert_eq!(format!("{head}{body}"), answer);
}

#[tokio::test]
async fn closing_either_side_ends_the_rewriter() {
    let (game, game_side) = duplex(1024);
    let (tunnel_side, mut server) = duplex(1024);
    let task = tokio::spawn(rewrite_signaling(game_side, tunnel_side, relay()));
    drop(game);
    let mut rest = Vec::new();
    timeout(LIMIT, server.read_to_end(&mut rest))
        .await
        .unwrap()
        .unwrap();
    drop(server);
    timeout(LIMIT, task).await.unwrap().unwrap();
}

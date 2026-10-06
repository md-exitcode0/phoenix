//! WebSocket bridge (plan 015 phase 0) — the gateway for faces that can't
//! reach a unix socket: the canvas webview, browsers, mobile, and (later)
//! Windows builds, where localhost TCP is the portable transport.
//!
//! Deliberately a dumb translator, not a second server: each WS connection
//! carries ONE `WireRequest` (first text frame) and streams back
//! `WireResponse` lines as text frames — the exact newline-JSON protocol the
//! unix socket speaks, relayed 1:1 over a fresh unix connection. Dispatch
//! logic stays single-sourced in `daemon::handle_connection`; this module can
//! never drift from it.
//!
//! Security: binds 127.0.0.1 only, and every handshake must present the
//! gateway token (`~/.phoenix/gateway.token`, created 0600 on first boot)
//! either as `Authorization: Bearer <token>` or — because browsers cannot set
//! WS headers — as a `?token=<token>` query parameter.

use anyhow::{Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixStream};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use zeroize::Zeroizing;

use super::daemon::{glog, socket_path, WireRequest, WireResponse, MAX_WIRE_REQUEST_BYTES};

/// Default port. Overridable with `PHOENIX_WS_PORT`; the config dashboard
/// gets a proper knob when the GUI lands.
pub const DEFAULT_WS_PORT: u16 = 7469; // "PHNX" on a phone keypad

pub fn ws_port() -> u16 {
    std::env::var("PHOENIX_WS_PORT")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_WS_PORT)
}

/// Read the gateway token, creating it on first use. The token is the ONLY
/// thing standing between localhost processes and the agent — never log it.
pub fn gateway_token() -> Result<String> {
    let path = crate::config::phoenix_home().join("gateway.token");
    gateway_token_at(&path)
}

fn gateway_token_at(path: &std::path::Path) -> Result<String> {
    crate::config::private_io::read_modify_write_private(path, |current| {
        let existing = current
            .map(String::from_utf8_lossy)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let token = existing.unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        Ok((token.clone(), token.into_bytes()))
    })
    .with_context(|| format!("failed to create gateway token at {}", path.display()))
}

/// Constant-shape token check for the handshake: header first (native
/// clients), then query param (browsers).
fn token_matches(presented: &str, expected: &str) -> bool {
    bool::from(presented.as_bytes().ct_eq(expected.as_bytes()))
}

fn handshake_authorized(request: &Request, token: &str) -> bool {
    if let Some(value) = request.headers().get("authorization") {
        if let Ok(value) = value.to_str() {
            if let Some(presented) = value.strip_prefix("Bearer ") {
                return token_matches(presented.trim(), token);
            }
        }
    }
    if let Some(query) = request.uri().query() {
        for pair in query.split('&') {
            if let Some(presented) = pair.strip_prefix("token=") {
                return token_matches(presented, token);
            }
        }
    }
    false
}

/// Accept WS connections and relay each to the unix socket. Never fatal to
/// the daemon: a bridge that can't bind logs and stays down (the unix socket
/// keeps working), because a busy port must not kill the gateway.
pub async fn run_ws_bridge() {
    let port = ws_port();
    let token = match gateway_token() {
        Ok(token) => token,
        Err(error) => {
            glog(&format!(
                "ws bridge: token unavailable, bridge DOWN ({error:#})"
            ));
            return;
        }
    };
    // SO_REUSEADDR before bind: a restarted gateway must reclaim 7469 even
    // while the previous listener's socket sits in TCP TIME_WAIT. Without it,
    // an ordinary restart (crash recovery, the canvas supervisor respawn) hit
    // "Address already in use" and left the WS bridge DOWN for up to ~60s —
    // the canvas, which talks ONLY over this port, could not connect the whole
    // time (observed live 2026-07-18 during a rebuild). Build the socket by
    // hand so the option is set before bind; the plain `TcpListener::bind`
    // convenience never sets it.
    let listener = match bind_reusable(port) {
        Ok(listener) => listener,
        Err(error) => {
            glog(&format!(
                "ws bridge: failed to bind 127.0.0.1:{port}, bridge DOWN ({error})"
            ));
            return;
        }
    };
    glog(&format!(
        "ws bridge listening on ws://127.0.0.1:{port} (token in ~/.phoenix/gateway.token)"
    ));
    loop {
        let Ok((stream, peer)) = listener.accept().await else {
            continue;
        };
        let token = token.clone();
        tokio::spawn(async move {
            if let Err(error) = relay_connection(stream, &token).await {
                glog(&format!(
                    "ws bridge: connection from {peer} ended: {error:#}"
                ));
            }
        });
    }
}

/// Bind the localhost WS-bridge port with SO_REUSEADDR set BEFORE bind, so a
/// restarted gateway reclaims the port during the old listener's TIME_WAIT.
/// `socket2` builds the raw socket; we hand the std listener to tokio.
fn bind_reusable(port: u16) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    TcpListener::from_std(std::net::TcpListener::from(socket))
}

fn inbound_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .max_message_size(Some(MAX_WIRE_REQUEST_BYTES))
        .max_frame_size(Some(MAX_WIRE_REQUEST_BYTES))
}

/// One WS connection = one wire request: authenticate the handshake, take the
/// first text frame as the request line, pump response lines back as frames.
async fn relay_connection(stream: tokio::net::TcpStream, token: &str) -> Result<()> {
    relay_connection_to(stream, token, &socket_path()).await
}

async fn relay_connection_to(
    stream: tokio::net::TcpStream,
    token: &str,
    unix_socket: &std::path::Path,
) -> Result<()> {
    let mut authorized = false;
    let ws = tokio_tungstenite::accept_hdr_async_with_config(
        stream,
        |request: &Request, response: Response| {
            if handshake_authorized(request, token) {
                authorized = true;
                Ok(response)
            } else {
                Err(ErrorResponse::new(Some("unauthorized".to_string())))
            }
        },
        Some(inbound_websocket_config()),
    )
    .await;
    let ws = match ws {
        Ok(ws) if authorized => ws,
        Ok(_) | Err(_) => return Ok(()), // rejected handshake — already answered
    };
    let (mut ws_out, mut ws_in) = ws.split();

    // First text frame is the request. Control frames may precede it.
    let request_line = loop {
        match ws_in.next().await {
            Some(Ok(Message::Text(text))) if text.len() <= MAX_WIRE_REQUEST_BYTES => break text,
            Some(Ok(Message::Text(_))) => {
                let _ = ws_out
                    .send(Message::Text(
                        format!(
                            "{{\"Error\":{{\"message\":\"request exceeds {MAX_WIRE_REQUEST_BYTES} bytes\"}}}}"
                        )
                        .into(),
                    ))
                    .await;
                return Ok(());
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
            Some(Ok(Message::Binary(_))) => {
                let _ = ws_out
                    .send(Message::Text(
                        "{\"Error\":{\"message\":\"text frames only\"}}".into(),
                    ))
                    .await;
                return Ok(());
            }
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Ok(()),
        }
    };

    // The browser frame producer and the WebSocket bridge live in this same
    // gateway process. Sending a live frame through the Unix JSON protocol
    // first forced every JPEG through a costly base64 -> JSON -> socket ->
    // JSON -> base64 decode round trip before WebKit could paint it. Canvas is
    // the latency-sensitive local subscriber, so connect it straight to the
    // in-process newest-frame channel and keep the Unix path below for older
    // clients and bridge tests that deliberately use another socket.
    let binary_browser_stream = request_line.trim() == "\"SubscribeBrowser\"";
    let gateway_socket = socket_path();
    if binary_browser_stream && unix_socket == gateway_socket.as_path() {
        let mut rx = crate::tools::browser_native::frames_subscribe();
        let mut newest = None;
        let (command_tx, mut command_rx) = tokio::sync::mpsc::channel::<Zeroizing<String>>(8);
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel::<Message>(8);
        tokio::spawn(async move {
            while let Some(command) = command_rx.recv().await {
                let response = direct_browser_control_response(command).await;
                if reply_tx.send(response).await.is_err() {
                    break;
                }
            }
        });
        let mut cadence = tokio::time::interval_at(
            tokio::time::Instant::now() + std::time::Duration::from_millis(33),
            std::time::Duration::from_millis(33),
        );
        cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        glog("browser view subscriber attached directly to binary WS lane");
        ws_out
            .send(Message::Text(
                "{\"BrowserStreamReady\":{\"control\":true}}".into(),
            ))
            .await?;
        loop {
            tokio::select! {
                received = rx.recv() => match received {
                    Ok(frame) => {
                        newest = Some(frame);
                        while let Ok(frame) = rx.try_recv() {
                            newest = Some(frame);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = cadence.tick(), if newest.is_some() => {
                    let frame = newest.take().expect("browser cadence checked newest frame");
                    let Some(message) = browser_frame_binary(frame) else { continue };
                    if ws_out.send(message).await.is_err() {
                        break;
                    }
                }
                reply = reply_rx.recv() => {
                    let Some(reply) = reply else { break };
                    if ws_out.send(reply).await.is_err() {
                        break;
                    }
                }
                incoming = ws_in.next() => match incoming {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(Message::Text(command))) => {
                        if command.len() > MAX_WIRE_REQUEST_BYTES {
                            let _ = ws_out.send(Message::Text(
                                format!("{{\"Error\":{{\"message\":\"request exceeds {MAX_WIRE_REQUEST_BYTES} bytes\"}}}}").into()
                            )).await;
                            continue;
                        }
                        if command_tx.send(Zeroizing::new(command.to_string())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => continue,
                }
            }
        }
        return Ok(());
    }

    // Relay to the unix socket: same line-JSON protocol, fresh connection.
    let unix = UnixStream::connect(unix_socket)
        .await
        .context("gateway unix socket unreachable")?;
    let (read_half, mut write_half) = unix.into_split();
    write_half.write_all(request_line.as_bytes()).await?;
    write_half.write_all(b"\n").await?;
    write_half.flush().await?;
    let mut lines = BufReader::new(read_half).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line? {
                    Some(line) => {
                        let outgoing = if binary_browser_stream {
                            browser_frame_message(&line).unwrap_or_else(|| Message::Text(line.into()))
                        } else {
                            Message::Text(line.into())
                        };
                        if ws_out.send(outgoing).await.is_err() {
                            break; // ws client gone
                        }
                    }
                    None => {
                        // Daemon closed the response stream — mirror it.
                        let _ = ws_out.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
            frame = ws_in.next() => {
                match frame {
                    // Protocol is one request per connection; a close (or a
                    // dropped client) tears down the unix side too, which is
                    // how a subscriber detaches.
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => continue, // pings etc.
                }
            }
        }
    }
    Ok(())
}

/// Handle the small, sequential control lane multiplexed onto the live frame
/// WebSocket. The work stays off Tokio's executor because CDP and durable
/// workflow writes are synchronous. Browser commands arrive one at a time
/// from the Canvas queue, and this worker preserves that ordering.
async fn direct_browser_control_response(command: Zeroizing<String>) -> Message {
    let parsed = serde_json::from_str::<WireRequest>(&command);
    let response = match parsed {
        Ok(WireRequest::BrowserInteract {
            instance,
            browser_action,
        }) => match tokio::task::spawn_blocking(move || {
            crate::tools::browser_native::user_interact(&instance, &browser_action)
        })
        .await
        {
            Ok(Ok(receipt)) => WireResponse::BrowserInteraction(receipt),
            Ok(Err(message)) => WireResponse::Error { message },
            Err(error) => WireResponse::Error {
                message: error.to_string(),
            },
        },
        Ok(WireRequest::BrowserSurface { instance, action }) => {
            match tokio::task::spawn_blocking(move || {
                crate::tools::browser_native::browser_surface(&instance, action)
            })
            .await
            {
                Ok(Ok(reply)) => WireResponse::BrowserSurface(reply),
                Ok(Err(message)) => WireResponse::Error { message },
                Err(error) => WireResponse::Error {
                    message: error.to_string(),
                },
            }
        }
        Ok(WireRequest::DesktopWorkspaces) => {
            match tokio::task::spawn_blocking(crate::tools::isolated_desktop::existing_desktops).await {
                Ok(views)=>WireResponse::DesktopWorkspaces(views),
                Err(error)=>WireResponse::Error{message:error.to_string()},
            }
        }
        Ok(WireRequest::DesktopObservation{scope_key,after_ms}) => {
            match tokio::task::spawn_blocking(move ||
                crate::tools::isolated_desktop::latest_observation(&scope_key,after_ms)).await {
                Ok(Ok(observation))=>WireResponse::DesktopObservation(observation),
                Ok(Err(error))=>WireResponse::Error{message:error.to_string()},
                Err(error)=>WireResponse::Error{message:error.to_string()},
            }
        }
        Ok(WireRequest::TeachWorkflow(command)) => {
            match tokio::task::spawn_blocking(move || {
                crate::runtime::workflow_teaching::handle_command(command)
            })
            .await
            {
                Ok(Ok(reply)) => WireResponse::TeachWorkflow(reply),
                Ok(Err(error)) => WireResponse::Error {
                    message: format!("{error:#}"),
                },
                Err(error) => WireResponse::Error {
                    message: error.to_string(),
                },
            }
        }
        Ok(_) => WireResponse::Error {
            message: "the live browser socket accepts browser controls only".to_string(),
        },
        Err(error) => WireResponse::Error {
            message: format!("invalid browser control request: {error}"),
        },
    };
    Message::Text(
        serde_json::to_string(&response)
            .unwrap_or_else(|_| {
                "{\"Error\":{\"message\":\"could not encode browser response\"}}".to_string()
            })
            .into(),
    )
}

/// Convert the high-rate browser lane to a compact binary WebSocket frame.
/// The Unix protocol remains backwards-compatible newline JSON, while the
/// WebView avoids parsing and base64-decoding a multi-hundred-KB JSON string
/// on every paint. Layout: `PHXF`, big-endian u32 header length, UTF-8 JSON
/// metadata, raw JPEG bytes.
fn browser_frame_message(line: &str) -> Option<Message> {
    let WireResponse::BrowserFrame(frame) = serde_json::from_str::<WireResponse>(line).ok()? else {
        return None;
    };
    browser_frame_binary(frame)
}

fn browser_frame_binary(frame: crate::tools::browser_native::BrowserFrame) -> Option<Message> {
    let header = serde_json::to_vec(&serde_json::json!({
        "instance": frame.instance,
        "url": frame.url,
        "w": frame.w,
        "h": frame.h,
    }))
    .ok()?;
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(frame.data.as_bytes())
        .ok()?;
    let mut payload = Vec::with_capacity(8 + header.len() + jpeg.len());
    payload.extend_from_slice(b"PHXF");
    payload.extend_from_slice(&(header.len() as u32).to_be_bytes());
    payload.extend_from_slice(&header);
    payload.extend_from_slice(&jpeg);
    Some(Message::Binary(payload.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with(header: Option<&str>, uri: &str) -> Request {
        let mut builder = Request::builder().uri(uri);
        if let Some(value) = header {
            builder = builder.header("authorization", value);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn handshake_accepts_bearer_header_or_query_token_only() {
        let token = "sekrit";
        assert!(handshake_authorized(
            &request_with(Some("Bearer sekrit"), "ws://x/"),
            token
        ));
        assert!(handshake_authorized(
            &request_with(None, "ws://x/?token=sekrit"),
            token
        ));
        assert!(handshake_authorized(
            &request_with(None, "ws://x/?lane=journal&token=sekrit"),
            token
        ));
        for bad in [
            request_with(None, "ws://x/"),
            request_with(Some("Bearer wrong"), "ws://x/"),
            request_with(Some("sekrit"), "ws://x/"), // missing Bearer scheme
            request_with(None, "ws://x/?token=wrong"),
            request_with(None, "ws://x/?nottoken=sekrit"),
        ] {
            assert!(!handshake_authorized(&bad, token), "{:?}", bad.uri());
        }
    }

    #[test]
    fn browser_frames_cross_the_webview_lane_as_raw_jpeg_binary() {
        let line = serde_json::to_string(&WireResponse::BrowserFrame(
            crate::tools::browser_native::BrowserFrame {
                instance: "agent-phoenix".into(),
                url: "https://example.test/".into(),
                data: std::sync::Arc::from("/9j/2Q=="),
                w: 1600,
                h: 900,
            },
        ))
        .unwrap();
        let Message::Binary(payload) = browser_frame_message(&line).expect("binary frame") else {
            panic!("browser frame stayed on the text lane")
        };
        assert_eq!(&payload[..4], b"PHXF");
        let header_len = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as usize;
        let header: serde_json::Value =
            serde_json::from_slice(&payload[8..8 + header_len]).unwrap();
        assert_eq!(header["instance"], "agent-phoenix");
        assert_eq!(header["w"], 1600);
        assert_eq!(&payload[8 + header_len..], &[0xff, 0xd8, 0xff, 0xd9]);
    }

    #[test]
    fn token_match_requires_the_complete_exact_token() {
        let token = "0123456789abcdef0123456789abcdef";
        assert!(token_matches(token, token));
        assert!(!token_matches("1123456789abcdef0123456789abcdef", token));
        assert!(!token_matches("0123456789abcdef0123456789abcdee", token));
        assert!(!token_matches("0123456789abcdef", token));
        assert!(!token_matches("0123456789abcdef0123456789abcdef00", token));
    }

    #[test]
    fn websocket_parser_caps_frames_and_reassembled_messages() {
        let config = inbound_websocket_config();
        assert_eq!(config.max_frame_size, Some(MAX_WIRE_REQUEST_BYTES));
        assert_eq!(config.max_message_size, Some(MAX_WIRE_REQUEST_BYTES));
    }

    #[tokio::test]
    async fn oversized_authenticated_frame_is_closed_before_unix_relay() {
        let token = "frame-limit-token";
        let tcp = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let nonexistent_socket = dir.path().join("must-not-be-reached.sock");
        let server = tokio::spawn(async move {
            let (stream, _) = tcp.accept().await.unwrap();
            relay_connection_to(stream, token, &nonexistent_socket).await
        });

        let url = format!("ws://127.0.0.1:{port}/?token={token}");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let sent = ws
            .send(Message::Text("x".repeat(MAX_WIRE_REQUEST_BYTES + 1).into()))
            .await;
        if sent.is_ok() {
            let closure = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
                .await
                .expect("oversized frame must be rejected promptly");
            assert!(
                !matches!(closure, Some(Ok(Message::Text(_)))),
                "oversized request must never reach the Unix JSON relay"
            );
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .expect("relay exits after capacity rejection")
            .expect("relay task")
            .expect("capacity rejection is a clean connection close");
    }

    #[test]
    fn concurrent_first_start_creates_one_private_gateway_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.token");
        let tokens = std::thread::scope(|scope| {
            let handles = (0..8)
                .map(|_| {
                    let path = path.clone();
                    scope.spawn(move || gateway_token_at(&path).unwrap())
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert!(!tokens[0].is_empty());
        assert!(tokens.iter().all(|token| token == &tokens[0]));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), tokens[0]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn gateway_token_repairs_permissive_files_and_rejects_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.token");
        std::fs::write(&path, "existing-token\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(gateway_token_at(&path).unwrap(), "existing-token");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let outside = dir.path().join("outside");
        std::fs::write(&outside, "do-not-touch").unwrap();
        let linked = dir.path().join("linked-token");
        symlink(&outside, &linked).unwrap();
        assert!(gateway_token_at(&linked).is_err());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "do-not-touch");
    }

    /// Full round-trip: a mock line-JSON server on a unix socket, the relay
    /// in front of it, and a real WS client speaking through — proves the
    /// frame↔line translation and the auth gate end to end.
    #[tokio::test]
    async fn ws_relay_round_trips_frames_as_lines() {
        use tokio::io::AsyncWriteExt;

        // Mock "daemon": reads one line, answers two lines, closes.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("mock.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            assert_eq!(line, "\"Ping\"");
            write_half.write_all(b"\"Pong\"\n").await.unwrap();
            write_half
                .write_all(b"{\"Event\":{\"Done\":null}}\n")
                .await
                .unwrap();
        });

        // Relay listener on an ephemeral port, pointed at the mock socket.
        let token = "test-token";
        let tcp = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let sock_for_relay = sock.clone();
        tokio::spawn(async move {
            let (stream, _) = tcp.accept().await.unwrap();
            // Inline relay with the mock path (relay_connection uses the real
            // socket_path; the test drives the same body via a copy of its
            // pump with the mock socket).
            let mut authorized = false;
            let ws =
                tokio_tungstenite::accept_hdr_async(stream, |req: &Request, resp: Response| {
                    if handshake_authorized(req, token) {
                        authorized = true;
                        Ok(resp)
                    } else {
                        Err(ErrorResponse::new(Some("unauthorized".to_string())))
                    }
                })
                .await
                .unwrap();
            assert!(authorized);
            let (mut ws_out, mut ws_in) = ws.split();
            let request_line = match ws_in.next().await {
                Some(Ok(Message::Text(text))) => text,
                other => panic!("expected request frame, got {other:?}"),
            };
            let unix = UnixStream::connect(&sock_for_relay).await.unwrap();
            let (read_half, mut write_half) = unix.into_split();
            write_half.write_all(request_line.as_bytes()).await.unwrap();
            write_half.write_all(b"\n").await.unwrap();
            let mut lines = BufReader::new(read_half).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if ws_out.send(Message::Text(line.into())).await.is_err() {
                    break;
                }
            }
            let _ = ws_out.send(Message::Close(None)).await;
        });

        // Real WS client: token in the query string, Ping in, two lines out.
        let url = format!("ws://127.0.0.1:{port}/?token={token}");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        ws.send(Message::Text("\"Ping\"".into())).await.unwrap();
        let first = ws.next().await.unwrap().unwrap();
        assert_eq!(first, Message::Text("\"Pong\"".into()));
        let second = ws.next().await.unwrap().unwrap();
        assert_eq!(second, Message::Text("{\"Event\":{\"Done\":null}}".into()));
    }
}

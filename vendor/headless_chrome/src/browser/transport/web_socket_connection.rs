use std::net::TcpStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;

use anyhow::Result;
use log::{debug, info, trace, warn};
use tungstenite::http::Response;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use url::Url;

use crate::types::{Message, parse_raw_message};

type TungsteniteWebsocketConnection = tungstenite::protocol::WebSocket<MaybeTlsStream<TcpStream>>;

// PHOENIX PATCH (9/9): the reader holds the socket lock for the whole
// blocking read, so every send waited out this timeout, and a busy stream
// (screencast frames) let the reader re-take the lock before any writer.
// Typing is two sends per character, so text went in at about one character a
// second. Keep the blocking window short and let a waiting writer go first.
const READ_TIMEOUT_DURATION: std::time::Duration = std::time::Duration::from_millis(5);
const IDLE_PARK_DURATION: std::time::Duration = std::time::Duration::from_millis(50);

pub struct WebSocketConnection {
    connection: Arc<Mutex<TungsteniteWebsocketConnection>>,
    writers_waiting: Arc<std::sync::atomic::AtomicUsize>,
    thread: std::thread::JoinHandle<()>,
    process_id: Option<u32>,
}

// TODO websocket::sender::Writer is not :Debug...
impl std::fmt::Debug for WebSocketConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        write!(f, "WebSocketConnection {{}}")
    }
}

impl WebSocketConnection {
    pub fn new(
        ws_url: &Url,
        process_id: Option<u32>,
        messages_tx: mpsc::Sender<Message>,
    ) -> Result<Self> {
        let (connection, _) = Self::websocket_connection(ws_url)?;

        let connection = Arc::new(Mutex::new(connection));
        let writers_waiting = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let thread = {
            let sender = connection.clone();
            let writers = writers_waiting.clone();
            std::thread::spawn(move || {
                trace!("Starting msg dispatching loop");
                Self::dispatch_incoming_messages(sender, writers, messages_tx, process_id);
                trace!("Quit loop msg dispatching loop");
            })
        };

        Ok(Self {
            connection,
            writers_waiting,
            thread,
            process_id,
        })
    }

    pub fn shutdown(&self) {
        trace!(
            "Shutting down WebSocket connection for Chrome {:?}",
            self.process_id
        );
        if let Err(err) = self.connection.lock().unwrap().close(None) {
            debug!(
                "Couldn't shut down WS connection for Chrome {:?}: {}",
                self.process_id, err
            );
        }

        self.connection.lock().unwrap().flush().ok();
        self.thread.thread().unpark();
    }

    fn dispatch_incoming_messages(
        receiver: Arc<Mutex<TungsteniteWebsocketConnection>>,
        writers_waiting: Arc<std::sync::atomic::AtomicUsize>,
        messages_tx: mpsc::Sender<Message>,
        process_id: Option<u32>,
    ) {
        loop {
            for _ in 0..2000 {
                if writers_waiting.load(std::sync::atomic::Ordering::Acquire) == 0 {
                    break;
                }
                std::thread::yield_now();
            }
            let message = receiver.lock().unwrap().read();

            match message {
                Err(err) => match err {
                    tungstenite::Error::Io(err) => {
                        if matches!(
                            err.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) {
                            std::thread::park_timeout(IDLE_PARK_DURATION);
                        } else {
                            debug!("WS IO Error for Chrome #{process_id:?}: {err}");
                            break;
                        }
                    }
                    tungstenite::Error::ConnectionClosed
                    | tungstenite::Error::AlreadyClosed
                    | tungstenite::Error::Protocol(
                        tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    ) => break,
                    error => {
                        // PHOENIX PATCH (6/8): this arm PANICKED the reader
                        // thread — a silent socket death (chrome pid alive,
                        // every later call "connection is closed") with no
                        // trace anywhere, since daemon stderr isn't captured.
                        // Any unhandled error means the socket is unusable:
                        // log WHY and break; the phoenix reconnect layer
                        // handles recovery.
                        warn!(
                            "WebSocket reader for Chrome #{process_id:?} stopping on unhandled error: {error:?}"
                        );
                        break;
                    }
                },
                Ok(message) => {
                    if let tungstenite::protocol::Message::Text(message_string) = message {
                        if let Ok(message) = parse_raw_message(&message_string) {
                            if messages_tx.send(message).is_err() {
                                break;
                            }
                        } else {
                            trace!(
                                "Incoming message isn't recognised as event or method response ({} bytes, prefix {:?})",
                                message_string.len(),
                                message_string.chars().take(256).collect::<String>(),
                            );
                        }
                    } else if let tungstenite::protocol::Message::Close(close_frame) = message {
                        // PHOENIX PATCH (7/8): an abnormal close code (e.g.
                        // 1001 Going Away during a heavy navigation) PANICKED
                        // the reader instead of closing it. Either way the
                        // socket is over — log the code and break cleanly.
                        match close_frame {
                            Some(tungstenite::protocol::CloseFrame { code, reason }) => {
                                warn!(
                                    "Chrome #{process_id:?} closed the CDP socket: {code:?} {reason:?}"
                                );
                            }
                            None => {
                                warn!(
                                    "Chrome #{process_id:?} closed the CDP socket (no close frame)"
                                );
                            }
                        }
                        break;
                    } else {
                        // PHOENIX PATCH (8/8): Ping/Pong/Binary/Frame messages
                        // PANICKED the reader ("Got a weird message") — a WS
                        // ping from chrome silently killed the whole transport.
                        // tungstenite already queues the pong reply on read;
                        // just keep reading.
                        let (kind, bytes) = match &message {
                            tungstenite::protocol::Message::Binary(bytes) => {
                                ("binary", bytes.len())
                            }
                            tungstenite::protocol::Message::Ping(bytes) => ("ping", bytes.len()),
                            tungstenite::protocol::Message::Pong(bytes) => ("pong", bytes.len()),
                            tungstenite::protocol::Message::Frame(_) => ("raw frame", 0),
                            tungstenite::protocol::Message::Text(_)
                            | tungstenite::protocol::Message::Close(_) => ("handled", 0),
                        };
                        trace!("Ignoring non-text WebSocket message ({kind}, {bytes} bytes)");
                    }
                }
            }
        }

        info!("Sending shutdown message to message handling loop");
        if messages_tx.send(Message::ConnectionShutdown).is_err() {
            warn!("Couldn't send message to transport loop telling it to shut down");
        }
    }

    pub fn websocket_connection(
        ws_url: &Url,
    ) -> Result<(
        tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
        Response<Option<Vec<u8>>>,
    )> {
        let mut client = tungstenite::client::connect_with_config(
            ws_url.as_str(),
            Some(WebSocketConfig::default().accept_unmasked_frames(true)),
            u8::MAX - 1,
        )?;

        let stream = client.0.get_mut();

        // this should be handled in tungstenite
        let stream = match stream {
            MaybeTlsStream::Plain(s) => s,
            #[cfg(feature = "native-tls")]
            MaybeTlsStream::NativeTls(s) => s.get_mut(),
            #[cfg(feature = "rustls")]
            MaybeTlsStream::Rustls(s) => &mut s.sock,

            _ => todo!(),
        };
        stream.set_read_timeout(Some(READ_TIMEOUT_DURATION))?;

        debug!("Successfully connected to WebSocket: {ws_url}");

        Ok(client)
    }

    pub fn send_message(&self, message_text: &str) -> Result<()> {
        let message = tungstenite::protocol::Message::text(message_text);
        self.writers_waiting.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let mut sender = self.connection.lock().unwrap();
        self.writers_waiting.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        sender.send(message)?;
        self.thread.thread().unpark();
        Ok(())
    }
}

impl Drop for WebSocketConnection {
    fn drop(&mut self) {
        info!("dropping websocket connection");
    }
}

//! The tokio side: accept WebSocket connections on 127.0.0.1, do the
//! `session.hello` handshake, parse requests into the per-connection
//! queues, and write responses and notifications back.
//!
//! Nothing here touches the project: requests wait in [`Shared::state`]
//! until the UI thread drains them ([`crate::ServerHandle::drain`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::discovery::token_eq;
use crate::handler::{Client, ConnId};
use crate::queue::Queues;
use crate::rpc::{Incoming, RpcError, parse_message, response};

/// Inbound messages over this close the socket (1009).
pub(crate) const MAX_INBOUND: usize = 16 << 20;
/// Outbound messages may be this big (snapshots).
const MAX_OUTBOUND: usize = 64 << 20;
/// A connection that doesn't say hello in time is closed.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// `approval_wait_ms` default and maximum.
const DEFAULT_WAIT_MS: u64 = 20_000;
const MAX_WAIT_MS: u64 = 120_000;

/// What the writer task sends.
#[derive(Debug)]
pub(crate) enum Outgoing {
    Text(String),
    Close(CloseCode, &'static str),
}

/// A connection that said hello.
pub(crate) struct Conn {
    pub client: Client,
    pub tx: mpsc::UnboundedSender<Outgoing>,
    pub since: SystemTime,
}

/// Everything the tokio side and the UI thread share.
pub(crate) struct State {
    pub queues: Queues,
    pub conns: BTreeMap<ConnId, Conn>,
    /// Connections that closed, for the UI thread to clean up after.
    pub closed: Vec<ConnId>,
}

/// What `session.hello` reports besides `rev`.
#[derive(Debug, Clone)]
pub(crate) struct HelloInfo {
    pub dir: String,
    pub name: String,
    pub headless: bool,
    pub approval: bool,
}

pub(crate) struct Shared {
    pub token: String,
    pub rev: AtomicU64,
    pub hello: Mutex<HelloInfo>,
    pub state: Mutex<State>,
    /// Approval waits: batch id to the blocked request's reply channel.
    pub waiters: Mutex<HashMap<String, oneshot::Sender<Result<Value, RpcError>>>>,
    pub wake: Option<Arc<dyn Fn() + Send + Sync>>,
    next_conn: AtomicU64,
}

/// Lock, recovering from a poisoned mutex (a panic can't leave these
/// half-written in a way that matters more than losing the server).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub fn new(token: String, hello: HelloInfo, wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        Shared {
            token,
            rev: AtomicU64::new(0),
            hello: Mutex::new(hello),
            state: Mutex::new(State {
                queues: Queues::default(),
                conns: BTreeMap::new(),
                closed: Vec::new(),
            }),
            waiters: Mutex::new(HashMap::new()),
            wake,
            next_conn: AtomicU64::new(1),
        }
    }

    pub fn wake(&self) {
        if let Some(w) = &self.wake {
            w();
        }
    }

    /// Send a message to a connection (dropped if it is gone).
    pub fn send(&self, conn: ConnId, v: &Value) {
        if let Some(c) = lock(&self.state).conns.get(&conn) {
            let _ = c.tx.send(Outgoing::Text(v.to_string()));
        }
    }
}

/// Accept connections until `shutdown` flips.
pub(crate) async fn serve(
    listener: TcpListener,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            r = listener.accept() => {
                if let Ok((stream, _)) = r {
                    tokio::spawn(connection(stream, shared.clone(), shutdown.clone()));
                }
            }
            _ = shutdown.changed() => break,
        }
    }
}

/// Refuse browsers (an `Origin` header) and any path but `/`. (The error
/// type is tungstenite's callback contract.)
#[allow(clippy::result_large_err)]
fn check_request(req: &Request, resp: Response) -> Result<Response, ErrorResponse> {
    let refuse = |status: StatusCode, text: &str| {
        let mut r = ErrorResponse::new(Some(text.to_string()));
        *r.status_mut() = status;
        r
    };
    if req.headers().contains_key("origin") {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "browsers can't connect to the Worlds API",
        ));
    }
    if req.uri().path() != "/" {
        return Err(refuse(StatusCode::NOT_FOUND, "the Worlds API is at /"));
    }
    Ok(resp)
}

async fn connection(stream: TcpStream, shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_INBOUND))
        .max_frame_size(Some(MAX_INBOUND));
    let Ok(ws) =
        tokio_tungstenite::accept_hdr_async_with_config(stream, check_request, Some(config)).await
    else {
        return;
    };
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Outgoing>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            match m {
                Outgoing::Text(s) => {
                    if s.len() > MAX_OUTBOUND {
                        tracing::error!("dropping an outbound message of {} bytes", s.len());
                        continue;
                    }
                    if sink.send(Message::Text(s.into())).await.is_err() {
                        break;
                    }
                }
                Outgoing::Close(code, reason) => {
                    let frame = CloseFrame {
                        code,
                        reason: reason.into(),
                    };
                    let _ = sink.send(Message::Close(Some(frame))).await;
                    let _ = sink.close().await;
                    break;
                }
            }
        }
    });

    let client = match tokio::time::timeout(HELLO_TIMEOUT, hello(&mut source, &tx, &shared)).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            drop(tx);
            let _ = writer.await;
            return;
        }
        Err(_) => {
            let _ = tx.send(Outgoing::Close(
                CloseCode::Policy,
                "no session.hello within 5 s",
            ));
            drop(tx);
            let _ = writer.await;
            return;
        }
    };
    let conn = client.conn;
    {
        let mut st = lock(&shared.state);
        st.queues.add_conn(conn);
        st.conns.insert(
            conn,
            Conn {
                client,
                tx: tx.clone(),
                since: SystemTime::now(),
            },
        );
    }
    shared.wake();

    loop {
        tokio::select! {
            msg = source.next() => match msg {
                Some(Ok(Message::Text(text))) => match parse_message(&text) {
                    Incoming::Request { id, method, params } => {
                        if method == "session.hello" {
                            let e = RpcError::invalid_request("this connection already said hello");
                            let _ = tx.send(Outgoing::Text(response(&id, Err(e)).to_string()));
                            continue;
                        }
                        lock(&shared.state).queues.push(conn, id, method, params);
                        shared.wake();
                    }
                    Incoming::Notification => {}
                    Incoming::Invalid(v) => {
                        let _ = tx.send(Outgoing::Text(v.to_string()));
                    }
                },
                Some(Ok(Message::Binary(_))) => {
                    let _ = tx.send(Outgoing::Close(CloseCode::Unsupported, "binary frames are not supported; send JSON text frames"));
                    break;
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(WsError::Capacity(_))) => {
                    let _ = tx.send(Outgoing::Close(CloseCode::Size, "messages are capped at 16 MiB"));
                    break;
                }
                Some(Err(_)) => break,
            },
            _ = shutdown.changed() => {
                let _ = tx.send(Outgoing::Close(CloseCode::Away, "the server is stopping"));
                break;
            }
        }
    }
    {
        let mut st = lock(&shared.state);
        st.queues.remove_conn(conn);
        st.conns.remove(&conn);
        st.closed.push(conn);
    }
    shared.wake();
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloParams {
    #[allow(dead_code)]
    token: String,
    client: HelloClient,
    approval_wait_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloClient {
    name: String,
    version: String,
}

/// Wait for a valid `session.hello`. Anything else, or a wrong token, gets
/// `unauthorized` and a close (1008); a hello with bad params gets
/// `invalid params` and may be retried.
async fn hello<S>(
    source: &mut S,
    tx: &mpsc::UnboundedSender<Outgoing>,
    shared: &Shared,
) -> Option<Client>
where
    S: futures_util::Stream<Item = Result<Message, WsError>> + Unpin,
{
    let refuse = |id: &Value, msg: &str| {
        let e = RpcError::unauthorized(msg);
        let _ = tx.send(Outgoing::Text(response(id, Err(e)).to_string()));
        let _ = tx.send(Outgoing::Close(CloseCode::Policy, "unauthorized"));
    };
    loop {
        let text = match source.next().await? {
            Ok(Message::Text(t)) => t,
            Ok(Message::Binary(_)) => {
                let _ = tx.send(Outgoing::Close(
                    CloseCode::Unsupported,
                    "binary frames are not supported",
                ));
                return None;
            }
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        };
        let (id, params) = match parse_message(&text) {
            Incoming::Request { id, method, params } if method == "session.hello" => (id, params),
            Incoming::Request { id, .. } => {
                refuse(
                    &id,
                    "say session.hello with the token from .worlds/server.json first",
                );
                return None;
            }
            _ => {
                refuse(
                    &Value::Null,
                    "say session.hello with the token from .worlds/server.json first",
                );
                return None;
            }
        };
        // A missing or wrong token is unauthorized whatever else is wrong.
        let token = params.get("token").and_then(Value::as_str).unwrap_or("");
        if !token_eq(token, &shared.token) {
            refuse(
                &id,
                "wrong token: read it from .worlds/server.json of the open project",
            );
            return None;
        }
        let p: HelloParams = match serde_json::from_value(params) {
            Ok(p) => p,
            Err(e) => {
                let e = RpcError::invalid_params(format!("session.hello: {e}"));
                let _ = tx.send(Outgoing::Text(response(&id, Err(e)).to_string()));
                continue;
            }
        };
        if let Err(msg) = check_name(&p.client.name) {
            let e = RpcError::invalid_params(format!("session.hello: {msg}"));
            let _ = tx.send(Outgoing::Text(response(&id, Err(e)).to_string()));
            continue;
        }
        let wait = p.approval_wait_ms.unwrap_or(DEFAULT_WAIT_MS);
        if wait > MAX_WAIT_MS {
            let e = RpcError::invalid_params(format!(
                "session.hello: approval_wait_ms is at most {MAX_WAIT_MS}"
            ));
            let _ = tx.send(Outgoing::Text(response(&id, Err(e)).to_string()));
            continue;
        }
        let client = Client {
            conn: shared.next_conn.fetch_add(1, Ordering::SeqCst),
            name: p.client.name,
            version: p.client.version,
            session: crate::random_session(),
            approval_wait: Duration::from_millis(wait),
        };
        let info = lock(&shared.hello).clone();
        let result = json!({
            "protocol": crate::PROTOCOL, "server": crate::server_info(),
            "project": { "dir": info.dir, "name": info.name },
            "rev": shared.rev.load(Ordering::SeqCst), "headless": info.headless,
            "approval": info.approval, "session": client.session,
        });
        let _ = tx.send(Outgoing::Text(response(&id, Ok(result)).to_string()));
        return Some(client);
    }
}

/// A client name: 1 to 32 characters of `a-z 0-9 - _ .`.
fn check_name(name: &str) -> Result<(), String> {
    let ok = (1..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.".contains(c));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "client.name {name:?} must be 1 to 32 characters of a-z, 0-9, -, _ and ."
        ))
    }
}

/// Wait for the user's decision on an approval batch, up to `wait`, then
/// reply: the decision's result, or `queued`. Frees the connection after.
pub(crate) async fn await_decision(
    shared: Arc<Shared>,
    conn: ConnId,
    id: Value,
    batch_id: String,
    wait: Duration,
    mut rx: oneshot::Receiver<Result<Value, RpcError>>,
) {
    let decided = tokio::select! {
        r = &mut rx => r.ok(),
        _ = tokio::time::sleep(wait) => None,
    };
    let result = match decided {
        Some(r) => r,
        None => {
            let still_waiting = lock(&shared.waiters).remove(&batch_id).is_some();
            if still_waiting {
                Ok(
                    json!({ "rev": shared.rev.load(Ordering::SeqCst), "status": "queued", "batch_id": batch_id }),
                )
            } else {
                // Decided just now: the result is already in the channel.
                rx.await.unwrap_or_else(|_| {
                    Ok(json!({ "rev": shared.rev.load(Ordering::SeqCst), "status": "queued", "batch_id": batch_id }))
                })
            }
        }
    };
    shared.send(conn, &response(&id, result));
    lock(&shared.state).queues.set_idle(conn);
    shared.wake();
}

#[cfg(test)]
mod tests {
    use super::check_name;

    #[test]
    fn client_names() {
        assert!(check_name("claude-code").is_ok());
        assert!(check_name("a.b_c-9").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("Claude").is_err());
        assert!(check_name(&"a".repeat(33)).is_err());
    }
}

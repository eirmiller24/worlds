//! End to end over a real socket: discovery, handshake, events, the queue
//! across connections, approval mode.

mod common;

use std::sync::mpsc;
use std::time::Duration;

use common::{call, connect, hello, recv, send};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use worlds_server::headless::{self, HeadlessHost};
use worlds_server::{Server, ServerConfig, ServerHandle, ServerInfo};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn fill(x: u32, value: &str) -> Value {
    json!({ "level": "crevasse", "layer": "collision", "rect": [x, 1, 1, 1], "value": value })
}

/// The close code of the next close frame (None when the socket just ends).
async fn close_code(ws: &mut common::Ws) -> Option<u16> {
    use futures_util::StreamExt;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .ok()??
        {
            Ok(Message::Close(Some(f))) => return Some(f.code.into()),
            Ok(Message::Close(None)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
}

#[test]
fn discovery_handshake_events_and_stop() {
    let (_tmp, dir) = common::mini();
    let server = headless::run(&dir, headless::Options::default()).unwrap();
    let info = ServerInfo::read(&dir).unwrap().unwrap();
    assert_eq!(info.port, server.port());
    assert_eq!(info.pid, std::process::id());
    assert_eq!(info.project, dir.to_string_lossy());
    assert!(info.is_live());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(ServerInfo::path(&dir))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    rt().block_on(async {
        // A bad token: unauthorized, then closed with 1008.
        let mut ws = connect(info.port).await;
        let r = hello(&mut ws, &"0".repeat(64), "claude-code").await;
        assert_eq!(r["error"]["code"], -32000);
        assert_eq!(close_code(&mut ws).await, Some(1008));
        // Anything before hello: the same.
        let mut ws = connect(info.port).await;
        send(
            &mut ws,
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "level.list" }),
        )
        .await;
        assert_eq!(recv(&mut ws).await.unwrap()["error"]["code"], -32000);
        assert_eq!(close_code(&mut ws).await, Some(1008));

        let mut ws = connect(info.port).await;
        let r = hello(&mut ws, &info.token, "claude-code").await;
        let res = &r["result"];
        assert_eq!(
            (
                res["protocol"].clone(),
                res["rev"].clone(),
                res["headless"].clone()
            ),
            (json!(1), json!(0), json!(true))
        );
        assert_eq!(res["session"].as_str().unwrap().len(), 8);
        let mut ev = Vec::new();
        call(
            &mut ws,
            1,
            "events.subscribe",
            json!({ "events": ["changed"] }),
            &mut ev,
        )
        .await;
        let r = call(&mut ws, 2, "grid.fill_rect", fill(1, "solid"), &mut ev).await;
        assert_eq!(r["result"]["rev"], 1);
        // The event follows the response.
        let e = recv(&mut ws).await.unwrap();
        assert_eq!(e["method"], "changed");
        assert_eq!(e["params"]["rev"], 1);
        assert_eq!(e["params"]["session"], res["session"]);
        assert_eq!(e["params"]["origin"], json!({ "agent": "claude-code" }));
        assert_eq!(
            e["params"]["files"],
            json!(["worlds/main/levels/crevasse/collision.json"])
        );
        // Framing errors.
        ws.send(Message::Text("[1, 2]".into())).await.unwrap();
        let r = recv(&mut ws).await.unwrap();
        assert_eq!(
            (r["error"]["code"].clone(), r["id"].clone()),
            (json!(-32600), Value::Null)
        );
        ws.send(Message::Text("{oops".into())).await.unwrap();
        assert_eq!(recv(&mut ws).await.unwrap()["error"]["code"], -32700);
        // A notification gets nothing back; the next request still works.
        send(
            &mut ws,
            &json!({ "jsonrpc": "2.0", "method": "level.list" }),
        )
        .await;
        let r = call(&mut ws, 3, "level.list", json!({}), &mut ev).await;
        assert_eq!(r["result"]["rev"], 1);
        ws.send(Message::Binary(vec![1, 2, 3].into()))
            .await
            .unwrap();
        assert_eq!(close_code(&mut ws).await, Some(1003));

        // Browsers (an Origin header) and other paths are refused.
        let mut req = format!("ws://127.0.0.1:{}/", info.port)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("Origin", "https://example.com".parse().unwrap());
        let err = tokio_tungstenite::connect_async(req)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("403"), "{err}");
        let err = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/x", info.port))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("404"), "{err}");
    });
    server.stop();
    assert!(!ServerInfo::path(&dir).exists());
}

#[test]
fn a_second_server_on_a_live_project_is_refused() {
    let (_tmp, dir) = common::mini();
    let server = headless::run(&dir, headless::Options::default()).unwrap();
    // The file names another live process (a child) on a live port: one
    // writer per project, so a second server refuses to start.
    let mut child = std::process::Command::new("sleep")
        .arg("5")
        .spawn()
        .unwrap();
    let mut info = ServerInfo::read(&dir).unwrap().unwrap();
    info.pid = child.id();
    std::fs::write(
        ServerInfo::path(&dir),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();
    let r = Server::start(ServerConfig {
        project_dir: dir.clone(),
        project_name: "Mini".into(),
        headless: true,
        wake: None,
    });
    assert!(
        matches!(r, Err(worlds_server::ServerError::AlreadyRunning { pid, .. }) if pid == child.id()),
        "{:?}",
        r.err()
    );
    let _ = child.kill();
    let _ = child.wait();
    // Stopping leaves a file that isn't ours alone.
    server.stop();
    assert!(ServerInfo::path(&dir).exists());
}

/// Checking `server.json` and publishing it is one step under
/// `.worlds/server.lock`: a server that is still starting (lock held, no
/// file yet) or running in this same process refuses a second one.
#[test]
fn the_project_lock_makes_starting_exclusive() {
    let (_tmp, dir) = common::mini();
    let config = || ServerConfig {
        project_dir: dir.clone(),
        project_name: "Mini".into(),
        headless: true,
        wake: None,
    };
    std::fs::create_dir_all(dir.join(".worlds")).unwrap();
    let starting = std::fs::File::create(dir.join(".worlds/server.lock")).unwrap();
    starting.lock().unwrap();
    let r = Server::start(config());
    assert!(
        matches!(
            r,
            Err(worlds_server::ServerError::AlreadyRunning { pid: 0, .. })
        ),
        "{:?}",
        r.err()
    );
    assert!(!ServerInfo::path(&dir).exists(), "nothing published");
    drop(starting);
    let first = Server::start(config()).unwrap();
    let r = Server::start(config());
    assert!(
        matches!(r, Err(worlds_server::ServerError::AlreadyRunning { pid, port })
            if pid == std::process::id() && port == first.port()),
        "{:?}",
        r.err()
    );
    assert_eq!(ServerInfo::read(&dir).unwrap().unwrap(), *first.info());
    first.stop();
    Server::start(config()).unwrap().stop();
}

#[test]
fn writes_wait_behind_another_connections_batch() {
    let (_tmp, dir) = common::mini();
    let server = headless::run(&dir, headless::Options::default()).unwrap();
    let token = ServerInfo::read(&dir).unwrap().unwrap().token;
    rt().block_on(async {
        let (mut a, mut b, mut c) = (connect(server.port()).await, connect(server.port()).await, connect(server.port()).await);
        hello(&mut a, &token, "agent-a").await;
        hello(&mut b, &token, "agent-b").await;
        hello(&mut c, &token, "agent-c").await;
        let mut ev = Vec::new();
        call(&mut a, 1, "history.begin_batch", json!({ "label": "A: walls" }), &mut ev).await;
        assert_eq!(call(&mut a, 2, "grid.fill_rect", fill(1, "solid"), &mut ev).await["result"]["rev"], 1);
        // B's write parks, and B's read waits behind it.
        send(&mut b, &json!({ "jsonrpc": "2.0", "id": 10, "method": "grid.fill_rect", "params": fill(2, "water") })).await;
        send(&mut b, &json!({ "jsonrpc": "2.0", "id": 11, "method": "level.list", "params": {} })).await;
        // C's read goes ahead and sees the batch so far; A keeps writing.
        assert_eq!(call(&mut c, 1, "level.list", json!({}), &mut ev).await["result"]["rev"], 1);
        assert_eq!(call(&mut a, 3, "grid.fill_rect", fill(3, "solid"), &mut ev).await["result"]["rev"], 2);
        let nothing = tokio::time::timeout(Duration::from_millis(100), recv(&mut b)).await;
        assert!(nothing.is_err(), "B was answered during A's batch: {nothing:?}");
        let end = call(&mut a, 4, "history.end_batch", json!({}), &mut ev).await;
        assert_eq!(end["result"]["entries"], 1);
        let r = recv(&mut b).await.unwrap();
        assert_eq!((r["id"].clone(), r["result"]["rev"].clone()), (json!(10), json!(3)));
        let r = recv(&mut b).await.unwrap();
        assert_eq!((r["id"].clone(), r["result"]["rev"].clone()), (json!(11), json!(3)));
        // A drops mid-batch: its unrecorded writes are reverted.
        call(&mut c, 2, "events.subscribe", json!({ "events": ["changed"] }), &mut ev).await;
        call(&mut a, 5, "history.begin_batch", json!({ "label": "A: more" }), &mut ev).await;
        call(&mut a, 6, "grid.fill_rect", fill(4, "solid"), &mut ev).await;
        drop(a);
        // (Writes inside a batch send no event of their own.)
        let e = recv(&mut c).await.unwrap();
        assert_eq!((e["params"]["kind"].clone(), e["params"]["rev"].clone()), (json!("cancel"), json!(5)));
    });
    server.stop();
}

type Job = Box<dyn FnOnce(&mut ServerHandle, &mut HeadlessHost) + Send>;

/// An editor-like host: a server (not headless) drained on its own thread,
/// with jobs run there as the UI would.
struct Editor {
    jobs: mpsc::Sender<Job>,
    thread: Option<std::thread::JoinHandle<()>>,
    port: u16,
    token: String,
}

impl Editor {
    fn start(dir: &std::path::Path) -> Self {
        let mut host = HeadlessHost::open(dir).unwrap();
        let handle = Server::start(ServerConfig {
            project_dir: dir.to_path_buf(),
            project_name: host.project.name.clone(),
            headless: false,
            wake: None,
        })
        .unwrap();
        let (port, token) = (handle.port(), handle.info().token.clone());
        let (tx, rx) = mpsc::channel::<Job>();
        let thread = std::thread::spawn(move || {
            let mut handle = handle;
            loop {
                match rx.recv_timeout(Duration::from_millis(2)) {
                    Ok(job) => job(&mut handle, &mut host),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                handle.drain(&mut host);
            }
        });
        Editor {
            jobs: tx,
            thread: Some(thread),
            port,
            token,
        }
    }

    fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut ServerHandle, &mut HeadlessHost) -> T + Send + 'static,
    ) -> T {
        let (tx, rx) = mpsc::channel();
        self.jobs
            .send(Box::new(move |h, host| {
                let _ = tx.send(f(h, host));
            }))
            .unwrap();
        rx.recv().unwrap()
    }

    /// Wait until `n` batches are pending; their ids.
    fn pending(&self, n: usize) -> Vec<String> {
        for _ in 0..500 {
            let ids = self.run(|h, _| h.pending().iter().map(|p| p.id.clone()).collect::<Vec<_>>());
            if ids.len() >= n {
                return ids;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("no pending batch");
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        let (tx, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.jobs, tx));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

async fn hello_wait(ws: &mut common::Ws, token: &str, name: &str, wait_ms: u64) -> Value {
    let mut ev = Vec::new();
    call(ws, 0, "session.hello", json!({ "token": token, "client": { "name": name, "version": "1" }, "approval_wait_ms": wait_ms }), &mut ev).await
}

#[test]
fn approval_mode_blocks_then_queues() {
    let (_tmp, dir) = common::mini();
    let ed = Editor::start(&dir);
    ed.run(|h, _| h.set_approval(true));
    let rt = rt();
    // Accepted while the request waits: the plain applied result.
    let mut a = rt.block_on(async {
        let mut a = connect(ed.port).await;
        let r = hello_wait(&mut a, &ed.token, "claude-code", 5000).await;
        assert_eq!(r["result"]["approval"], true);
        send(&mut a, &json!({ "jsonrpc": "2.0", "id": 1, "method": "grid.fill_rect", "params": fill(1, "solid") })).await;
        a
    });
    let ids = ed.pending(1);
    let (changes, rev, entries) = ed.run(move |h, host| {
        let c = h.apply_pending(host, &ids[0]).unwrap();
        (c.len(), h.rev(), host.history.undo_entries().len())
    });
    assert_eq!((changes, rev, entries), (1, 1, 1));
    rt.block_on(async {
        let r = recv(&mut a).await.unwrap();
        assert_eq!((r["result"]["status"].clone(), r["result"]["rev"].clone()), (json!("applied"), json!(1)));
        assert_eq!(r["result"]["label"], "claude-code: Fill rectangle on collision");
        // Rejected while waiting: error rejected.
        send(&mut a, &json!({ "jsonrpc": "2.0", "id": 2, "method": "grid.fill_rect", "params": fill(2, "solid") })).await;
    });
    let ids = ed.pending(1);
    ed.run(move |h, _| h.reject_pending(&ids[0]));
    rt.block_on(async {
        assert_eq!(recv(&mut a).await.unwrap()["error"]["code"], -32003);
        // A refused write is refused at once, nothing queued.
        let mut ev = Vec::new();
        let r = call(&mut a, 3, "grid.fill_rect", fill(2, "lava"), &mut ev).await;
        assert_eq!(r["error"]["code"], -32004);
        let r = call(&mut a, 4, "history.undo", json!({ "any": true }), &mut ev).await;
        assert_eq!(r["error"]["code"], -32005);
        let r = call(&mut a, 5, "project.reload", json!({}), &mut ev).await;
        assert_eq!(r["error"]["code"], -32005);
    });
    assert!(ed.run(|h, _| h.pending().is_empty()));

    // end_batch waits for the decision like a single write: accepted in
    // time, the batch's plain applied result.
    rt.block_on(async {
        let mut ev = Vec::new();
        call(
            &mut a,
            6,
            "history.begin_batch",
            json!({ "label": "claude-code: pair" }),
            &mut ev,
        )
        .await;
        let r = call(&mut a, 7, "grid.fill_rect", fill(6, "solid"), &mut ev).await;
        assert_eq!(r["result"]["status"], "collected");
        call(&mut a, 8, "grid.fill_rect", fill(7, "solid"), &mut ev).await;
        send(
            &mut a,
            &json!({ "jsonrpc": "2.0", "id": 9, "method": "history.end_batch", "params": {} }),
        )
        .await;
    });
    let ids = ed.pending(1);
    let applied = ed.run(move |h, host| h.apply_pending(host, &ids[0]).map(|c| c.len()).ok());
    assert_eq!(applied, Some(1));
    rt.block_on(async {
        let r = recv(&mut a).await.unwrap();
        assert_eq!(r["id"], 9);
        assert_eq!(
            (
                r["result"]["status"].clone(),
                r["result"]["entries"].clone(),
                r["result"]["label"].clone()
            ),
            (json!("applied"), json!(1), json!("claude-code: pair")),
            "{r}"
        );
        assert_eq!(r["result"]["rev"], 2);
    });

    // A short wait: `queued` first, the `batch` notification later.
    let mut b = rt.block_on(async {
        let mut b = connect(ed.port).await;
        hello_wait(&mut b, &ed.token, "quick", 50).await;
        let mut ev = Vec::new();
        let r = call(&mut b, 1, "grid.fill_rect", fill(3, "water"), &mut ev).await;
        assert_eq!(r["result"]["status"], "queued");
        assert_eq!(r["result"]["rev"], 2);
        let id = r["result"]["batch_id"].as_str().unwrap().to_string();
        let st = call(
            &mut b,
            2,
            "history.batch_status",
            json!({ "batch_id": id }),
            &mut ev,
        )
        .await;
        assert_eq!(st["result"]["status"], "queued");
        // Batches in approval mode: writes are collected, end_batch submits.
        call(
            &mut b,
            3,
            "history.begin_batch",
            json!({ "label": "quick: pair" }),
            &mut ev,
        )
        .await;
        let r = call(&mut b, 4, "grid.fill_rect", fill(4, "solid"), &mut ev).await;
        assert_eq!(
            (r["result"]["status"].clone(), r["result"]["rev"].clone()),
            (json!("collected"), json!(2))
        );
        call(&mut b, 5, "grid.fill_rect", fill(5, "solid"), &mut ev).await;
        let r = call(&mut b, 6, "history.end_batch", json!({}), &mut ev).await;
        assert_eq!(r["result"]["status"], "queued");
        b
    });
    let ids = ed.pending(2);
    let applied = {
        let ids = ids.clone();
        ed.run(move |h, host| {
            let first = h.apply_pending(host, &ids[0]).is_ok();
            let second = h.apply_pending(host, &ids[1]).is_ok();
            (first, second, h.rev(), host.history.undo_entries().len())
        })
    };
    assert_eq!(applied, (true, true, 4, 4));
    rt.block_on(async {
        for (id, rev) in [(&ids[0], 3), (&ids[1], 4)] {
            let n = recv(&mut b).await.unwrap();
            assert_eq!(n["method"], "batch");
            assert_eq!(
                n["params"],
                json!({ "batch_id": id, "status": "applied", "rev": rev })
            );
        }
        let mut ev = Vec::new();
        let st = call(
            &mut b,
            7,
            "history.batch_status",
            json!({ "batch_id": ids[1] }),
            &mut ev,
        )
        .await;
        assert_eq!(
            (
                st["result"]["status"].clone(),
                st["result"]["rev_applied"].clone()
            ),
            (json!("applied"), json!(4))
        );
        // Its own entries can still be undone without approval.
        let r = call(&mut b, 8, "history.undo", json!({}), &mut ev).await;
        assert_eq!(r["result"]["steps"][0]["label"], "quick: pair");
    });
}

#[test]
fn save_on_write_saves_after_each_write_outside_a_batch() {
    let (_tmp, dir) = common::mini();
    let server = headless::run(
        &dir,
        headless::Options {
            save_on_write: true,
        },
    )
    .unwrap();
    let token = ServerInfo::read(&dir).unwrap().unwrap().token;
    let file = dir.join("worlds/main/levels/crevasse/collision.json");
    let row1 = || -> String {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        v["rows"][1].as_str().unwrap().to_string()
    };
    rt().block_on(async {
        let mut ws = connect(server.port()).await;
        hello(&mut ws, &token, "claude-code").await;
        let mut ev = Vec::new();
        call(
            &mut ws,
            1,
            "events.subscribe",
            json!({ "events": ["saved"] }),
            &mut ev,
        )
        .await;
        call(
            &mut ws,
            2,
            "history.begin_batch",
            json!({ "label": "pair" }),
            &mut ev,
        )
        .await;
        call(&mut ws, 3, "grid.fill_rect", fill(1, "solid"), &mut ev).await;
        assert_eq!(
            row1(),
            "#..................#",
            "not saved while the batch is open"
        );
        call(&mut ws, 4, "history.end_batch", json!({}), &mut ev).await;
        let saved = recv(&mut ws).await.unwrap();
        assert_eq!(saved["method"], "saved");
        assert_eq!(
            saved["params"]["files"],
            json!(["worlds/main/levels/crevasse/collision.json"])
        );
    });
    assert_eq!(row1(), "##.................#");
    server.stop();
}

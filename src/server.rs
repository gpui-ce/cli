use anyhow::Result;
use axum::{
    Router,
    extract::{Query, State, WebSocketUpgrade, ws::Message},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};
use subsecond_types::JumpTable;
use tokio::sync::{broadcast, watch};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Connection {
    pub aslr_reference: u64,
    pub build_id: u64,
    pub pid: u32,
}

struct StateData {
    pid: AtomicU32,
    build_id: AtomicU64,
    connection: watch::Sender<Option<Connection>>,
    messages: broadcast::Sender<String>,
}

pub struct Server {
    state: Arc<StateData>,
    task: tokio::task::JoinHandle<()>,
    pub port: u16,
}

impl Server {
    pub async fn start(port: u16) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        let port = listener.local_addr()?.port();
        let (connection, _) = watch::channel(None);
        let (messages, _) = broadcast::channel(32);
        let state = Arc::new(StateData {
            pid: AtomicU32::new(0),
            build_id: AtomicU64::new(0),
            connection,
            messages,
        });
        let router = Router::new()
            .route("/_dioxus", get(connect))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                eprintln!("patch server: {error}");
            }
        });
        Ok(Self { state, task, port })
    }

    pub fn expect(&self, pid: u32, build_id: u64) {
        self.state.pid.store(pid, Ordering::SeqCst);
        self.state.build_id.store(build_id, Ordering::SeqCst);
        self.state.connection.send_replace(None);
    }

    pub async fn connection(&self) -> Result<Connection> {
        let mut rx = self.state.connection.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if let Some(connection) = *rx.borrow_and_update() { return Ok::<_, anyhow::Error>(connection) }
                rx.changed().await?;
            }
        }).await.map_err(|_| anyhow::anyhow!("application did not connect to the patch server; enable GPUI-CE's hot-patching feature"))?
    }

    pub fn patch(&self, table: JumpTable, elapsed: u64) -> Result<()> {
        let connection = self
            .state
            .connection
            .borrow()
            .context("application disconnected from the patch server")?;
        self.state.messages.send(
            serde_json::json!({"HotReload": {
                "templates": [], "assets": [], "ms_elapsed": elapsed,
                "jump_table": table, "for_pid": connection.pid, "for_build_id": connection.build_id
            }})
            .to_string(),
        )?;
        Ok(())
    }
}

use anyhow::Context;

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn connect(
    State(state): State<Arc<StateData>>,
    Query(connection): Query<Connection>,
    ws: WebSocketUpgrade,
) -> Response {
    if connection.pid != state.pid.load(Ordering::SeqCst)
        || connection.build_id != state.build_id.load(Ordering::SeqCst)
    {
        return StatusCode::CONFLICT.into_response();
    }
    ws.on_upgrade(move |mut socket| async move {
        let mut messages = state.messages.subscribe();
        state.connection.send_replace(Some(connection));
        loop {
            tokio::select! {
                message = messages.recv() => {
                    if connection.pid != state.pid.load(Ordering::SeqCst) { break }
                    let Ok(message) = message else { break };
                    if socket.send(Message::Text(message.into())).await.is_err() { break }
                },
                message = socket.recv() => {
                    match message {
                        Some(Ok(Message::Ping(payload))) => {
                            if socket.send(Message::Pong(payload)).await.is_err() { break }
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        _ => {},
                    }
                }
            }
        }
        state.connection.send_if_modified(|current| {
            if *current == Some(connection) {
                *current = None;
                true
            } else {
                false
            }
        });
    })
    .into_response()
}

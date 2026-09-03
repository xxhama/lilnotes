//! Read-only MCP (Model Context Protocol) server for local AI agents.
//!
//! Runs *inside* the app process over Streamable HTTP on `127.0.0.1:<port>`
//! (`/mcp`), gated by a bearer token, and is **off by default**. It reuses
//! the app's single DB connection through `Arc<LazyDb>`, so it only serves
//! data once the DB is unlocked (returning users at startup, new users after
//! onboarding's `init_db`).
//!
//! Privacy invariants: loopback bind only; token required on every request;
//! tools never expose audio paths, settings or voiceprints. See `tools.rs`
//! for the tool surface and `auth.rs` for the token gate.
//!
//! Lifecycle: `McpServer` is Tauri-managed state. `apply_settings` is an
//! idempotent reconciler (start / stop / restart on `{enabled, port,
//! token}` changes, no-op otherwise) called from `update_settings`,
//! `regenerate_mcp_token`, `init_db` and app setup. `stop` is called from
//! `shutdown::cleanup_on_exit`.

pub mod auth;
pub mod format;
pub mod tools;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use axum::{middleware, Router};
use rand::RngCore;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;

use crate::db::LazyDb;
use crate::settings::AppSettings;

/// Default loopback port. High and unassigned; user-configurable in Settings.
pub const DEFAULT_MCP_PORT: u16 = 41777;
/// Largest JSON-RPC request body we accept (tool calls are tiny).
const MAX_BODY_BYTES: usize = 1 << 20;
/// How long `stop` waits for the listener task to wind down.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

struct Running {
    port: u16,
    token: String,
    cancel: CancellationToken,
    /// The server task holds the `Sender`; it is dropped when the task
    /// exits, which makes `recv` return — a sync "task finished" signal so
    /// an immediate rebind of the same port cannot race the old listener.
    finished: mpsc::Receiver<()>,
}

/// Tauri-managed MCP server handle.
#[derive(Default)]
pub struct McpServer {
    running: Mutex<Option<Running>>,
    last_error: Mutex<Option<String>>,
}

/// Snapshot for the Settings UI (`mcp_status` command).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub running: bool,
    pub port: Option<u16>,
    /// Full endpoint URL while running, e.g. `http://127.0.0.1:41777/mcp`.
    pub url: Option<String>,
    /// Last start failure (e.g. port in use); cleared on a successful start.
    pub error: Option<String>,
}

pub fn endpoint_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

/// 32 random bytes from `OsRng`, hex — same recipe as the llama-server
/// sidecar API key.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl McpServer {
    /// Bind `127.0.0.1:port` and start serving. Binding is synchronous so
    /// "address in use" is reported to the caller right away; `port = 0`
    /// picks a free port (tests). Returns the bound port.
    pub fn start(&self, db: Arc<LazyDb>, port: u16, token: String) -> Result<u16, String> {
        let mut running = self.running.lock().unwrap();
        if let Some(r) = running.take() {
            stop_running(r);
        }

        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
        let listener = std::net::TcpListener::bind(addr)
            .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("listener setup failed: {e}"))?;
        let bound = listener
            .local_addr()
            .map_err(|e| format!("listener setup failed: {e}"))?
            .port();

        let cancel = CancellationToken::new();
        let router = build_router(db, Arc::new(token.clone()), cancel.child_token());
        let (finished_tx, finished_rx) = mpsc::channel::<()>();
        let task_cancel = cancel.clone();
        tauri::async_runtime::spawn(async move {
            // Keep the sender alive for the whole task; dropping it on exit
            // is the completion signal `stop` waits for.
            let _finished = finished_tx;
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[mcp] listener conversion failed: {e}");
                    return;
                }
            };
            eprintln!("[mcp] serving on {}", endpoint_url(bound));
            if let Err(e) = axum::serve(listener, router)
                .with_graceful_shutdown(task_cancel.cancelled_owned())
                .await
            {
                eprintln!("[mcp] server error: {e}");
            }
            eprintln!("[mcp] stopped");
        });

        *running = Some(Running {
            port: bound,
            token,
            cancel,
            finished: finished_rx,
        });
        *self.last_error.lock().unwrap() = None;
        Ok(bound)
    }

    /// Stop serving (no-op if not running). Bounded wait for the task.
    pub fn stop(&self) {
        let taken = self.running.lock().unwrap().take();
        if let Some(r) = taken {
            stop_running(r);
        }
    }

    /// Reconcile the running state with `settings`. Never fails; start
    /// errors are recorded and surfaced via `status()`.
    pub fn apply_settings(&self, db: Arc<LazyDb>, settings: &AppSettings) {
        let desired = settings
            .mcp_enabled
            .then(|| {
                settings
                    .mcp_token
                    .clone()
                    .filter(|t| !t.is_empty())
                    .map(|t| (settings.mcp_port, t))
            })
            .flatten();

        {
            let running = self.running.lock().unwrap();
            if let (Some(r), Some((port, token))) = (running.as_ref(), desired.as_ref()) {
                if r.port == *port && r.token == *token {
                    return; // already in the desired state
                }
            }
            if running.is_none() && desired.is_none() {
                return;
            }
        }

        match desired {
            Some((port, token)) => match self.start(db, port, token) {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("[mcp] start failed: {e}");
                    *self.last_error.lock().unwrap() = Some(e);
                }
            },
            None => {
                self.stop();
                *self.last_error.lock().unwrap() = None;
            }
        }
    }

    pub fn status(&self) -> McpStatus {
        let running = self.running.lock().unwrap();
        let error = self.last_error.lock().unwrap().clone();
        match running.as_ref() {
            Some(r) => McpStatus {
                running: true,
                port: Some(r.port),
                url: Some(endpoint_url(r.port)),
                error,
            },
            None => McpStatus {
                running: false,
                port: None,
                url: None,
                error,
            },
        }
    }

    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Some(r) = self.running.get_mut().map(Option::take).unwrap_or(None) {
            stop_running(r);
        }
    }
}

fn stop_running(r: Running) {
    r.cancel.cancel();
    if r.finished.recv_timeout(STOP_TIMEOUT).is_err()
        && matches!(r.finished.try_recv(), Err(mpsc::TryRecvError::Empty))
    {
        eprintln!("[mcp] server task did not stop within {STOP_TIMEOUT:?}");
    }
}

/// Start the server if the user enabled it and the DB is unlocked. Called
/// from app setup and after `init_db`. Repairs a missing token.
pub fn start_if_enabled(app: &AppHandle) {
    let Some(lazy) = app.try_state::<Arc<LazyDb>>() else {
        return;
    };
    let Some(db) = lazy.get() else {
        return;
    };
    let mut settings = db.get_settings();
    if !settings.mcp_enabled {
        return;
    }
    if settings.mcp_token.as_deref().is_none_or(str::is_empty) {
        settings.mcp_token = Some(generate_token());
        if let Err(e) = db.set_settings(&settings) {
            eprintln!("[mcp] cannot persist generated token: {e}");
        }
    }
    if let Some(server) = app.try_state::<McpServer>() {
        server.apply_settings(lazy.inner().clone(), &settings);
    }
}

/// `/mcp` → rmcp Streamable HTTP service, behind the bearer gate. Every
/// other path is a 404 from axum (also behind the gate, so unauthenticated
/// probes learn nothing).
pub(crate) fn build_router(
    db: Arc<LazyDb>,
    token: Arc<String>,
    cancel: CancellationToken,
) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_cancellation_token(cancel)
        .with_sse_keep_alive(Some(Duration::from_secs(15)))
        .with_max_request_body_bytes(MAX_BODY_BYTES);
    let service = StreamableHttpService::new(
        move || Ok(tools::LilNotesMcp::new(db.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(token, auth::require_bearer))
}

#[cfg(test)]
mod tests;

//! End-to-end tests: seeded temp DB → real listener on a free port → plain
//! JSON-RPC over HTTP (Streamable HTTP framing parsed by hand), plus
//! in-memory router tests for the auth gate.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use super::{build_router, McpServer};
use crate::asr::Segment;
use crate::db::{Db, LazyDb};

static TEST_ID: AtomicU64 = AtomicU64::new(0);

fn seg(source: &str, speaker: &str, start_ms: u64, text: &str) -> Segment {
    Segment {
        id: 0,
        source: source.into(),
        start_ms,
        end_ms: start_ms + 2_000,
        text: text.into(),
        speaker: Some(speaker.into()),
        kind: "speech".into(),
        deleted: false,
    }
}

/// Temp SQLCipher DB with one finished meeting: Me + SPEAKER_00 (linked to
/// persona "Alice", confirmed), a rename on SPEAKER_01, notes, a summary,
/// and a customer.
fn seeded() -> (Arc<LazyDb>, i64, i64) {
    let dir = std::env::temp_dir().join(format!(
        "lilnotes-mcp-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        TEST_ID.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("test.sqlite3"), &[0x42u8; 32]).unwrap();

    let started = 1_700_000_000_000;
    let mid = db.insert_meeting_started("s1", "Kickoff", started).unwrap();
    db.finalize_meeting(mid, started + 90_000, "mic.wav", "system.wav")
        .unwrap();
    db.replace_segments(
        mid,
        &[
            seg("mic", "Me", 0, "Hello everyone"),
            seg("system", "SPEAKER_00", 5_000, "Hi, Alice here"),
            seg(
                "system",
                "SPEAKER_01",
                65_000,
                "Bob speaking about the roadmap",
            ),
        ],
    )
    .unwrap();
    let pid = db.create_persona("Alice").unwrap();
    db.upsert_link(mid, "SPEAKER_00", Some(pid), Some(0.91))
        .unwrap();
    db.confirm_link(mid, "SPEAKER_00", pid, "Alice", 50)
        .unwrap();
    db.rename_speaker(mid, "SPEAKER_01", Some("Bob")).unwrap();
    db.update_notes(mid, "Follow up on pricing").unwrap();
    db.insert_summary(mid, "test-model", "tmpl", "## TL;DR\nRoadmap agreed.")
        .unwrap();
    let cid = db.create_customer("Acme", Some("Key account")).unwrap();
    db.set_meeting_customer(mid, Some(cid)).unwrap();

    (Arc::new(LazyDb::for_tests(db)), mid, cid)
}

// ---------------------------------------------------------------------------
// Auth gate (in-memory, no socket)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn router_rejects_missing_or_wrong_token() {
    let (db, _, _) = seeded();
    let token = "t0ken";
    let router = build_router(db, Arc::new(token.into()), CancellationToken::new());

    let req = Request::post("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(res.headers().contains_key(header::WWW_AUTHENTICATE));

    let req = Request::post("/mcp")
        .header(header::AUTHORIZATION, "Bearer wrong")
        .body(Body::from("{}"))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Unknown paths are gated too, and 404 once authenticated.
    let req = Request::get("/other")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Live server, JSON-RPC over Streamable HTTP
// ---------------------------------------------------------------------------

struct Rpc {
    client: reqwest::Client,
    url: String,
    token: String,
    session: Option<String>,
}

impl Rpc {
    /// POST one JSON-RPC message. Returns `(status, parsed JSON-RPC message
    /// if any)`; captures `Mcp-Session-Id` from the initialize response.
    async fn post(&mut self, body: Value) -> (StatusCode, Option<Value>) {
        let mut req = self
            .client
            .post(&self.url)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::ACCEPT, "application/json, text/event-stream")
            .json(&body);
        if let Some(sid) = &self.session {
            req = req.header("mcp-session-id", sid);
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        if let Some(sid) = res.headers().get("mcp-session-id") {
            self.session = Some(sid.to_str().unwrap().to_string());
        }
        let ctype = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = res.text().await.unwrap();
        let msg = if ctype.starts_with("text/event-stream") {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .filter_map(|d| serde_json::from_str::<Value>(d).ok())
                .find(|v| v.get("result").is_some() || v.get("error").is_some())
        } else if ctype.starts_with("application/json") {
            serde_json::from_str(&text).ok()
        } else {
            None
        };
        (status, msg)
    }

    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let (status, msg) = self
            .post(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        assert_eq!(status, StatusCode::OK, "{method} status");
        let msg = msg.unwrap_or_else(|| panic!("{method}: no JSON-RPC message in response"));
        assert!(msg.get("error").is_none(), "{method}: {msg}");
        msg["result"].clone()
    }

    async fn tool(&mut self, id: u64, name: &str, args: Value) -> Value {
        self.call(id, "tools/call", json!({"name": name, "arguments": args}))
            .await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serves_tools_over_http_with_bearer_auth() {
    let (db, mid, cid) = seeded();
    let server = McpServer::default();
    let token = super::generate_token();
    let port = server.start(db, 0, token.clone()).unwrap();
    assert!(server.is_running());
    let url = super::endpoint_url(port);

    // No token → 401 before anything reaches rmcp.
    let res = reqwest::Client::new()
        .post(&url)
        .header(header::ACCEPT, "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let mut rpc = Rpc {
        client: reqwest::Client::new(),
        url,
        token,
        session: None,
    };

    let init = rpc
        .call(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }),
        )
        .await;
    assert_eq!(init["serverInfo"]["name"], "lilnotes");
    assert!(init["capabilities"]["tools"].is_object());
    assert!(rpc.session.is_some(), "server should assign a session id");

    let (status, _) = rpc
        .post(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // tools/list: 9 read-only tools.
    let tools = rpc.call(2, "tools/list", json!({})).await;
    let tools = tools["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "get_customer",
            "get_meeting",
            "get_transcript",
            "list_customer_summaries",
            "list_customers",
            "list_meetings",
            "list_personas",
            "list_summaries",
            "search_meetings",
        ]
    );
    for t in tools {
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{}", t["name"]);
    }

    // list_meetings
    let r = rpc.tool(3, "list_meetings", json!({"limit": 5})).await;
    let sc = &r["structuredContent"];
    assert_eq!(sc["total"], 1);
    assert_eq!(sc["meetings"][0]["id"], mid);
    assert_eq!(sc["meetings"][0]["customerId"], cid);
    assert_eq!(sc["meetings"][0]["hasSummary"], true);
    assert_eq!(sc["meetings"][0]["hasNotes"], true);
    // Text block mirrors the structured content for clients that ignore it.
    assert!(r["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("\"total\": 1"));

    // Filters: wrong customer → empty; date window → hit.
    let r = rpc
        .tool(4, "list_meetings", json!({"customer_id": cid + 1}))
        .await;
    assert_eq!(r["structuredContent"]["total"], 0);
    let r = rpc
        .tool(
            5,
            "list_meetings",
            json!({"from_ms": 1_700_000_000_000u64, "to_ms": 1_700_000_000_001u64}),
        )
        .await;
    assert_eq!(r["structuredContent"]["total"], 1);

    // get_transcript: persona + rename resolved, raw labels gone from text.
    let r = rpc
        .tool(6, "get_transcript", json!({"meeting_id": mid}))
        .await;
    let text = r["structuredContent"]["text"].as_str().unwrap();
    assert_eq!(
        text,
        "[0:00] Me: Hello everyone\n[0:05] Alice: Hi, Alice here\n[1:05] Bob: Bob speaking about the roadmap\n"
    );
    assert_eq!(r["structuredContent"]["truncated"], false);

    let r = rpc
        .tool(
            7,
            "get_transcript",
            json!({"meeting_id": mid, "format": "segments", "from_ms": 60_000}),
        )
        .await;
    let segs = r["structuredContent"]["segments"].as_array().unwrap();
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0]["speaker"], "Bob");
    assert_eq!(segs[0]["rawLabel"], "SPEAKER_01");
    assert_eq!(segs[0]["source"], "system");

    // get_meeting
    let r = rpc.tool(8, "get_meeting", json!({"meeting_id": mid})).await;
    let sc = &r["structuredContent"];
    assert_eq!(sc["title"], "Kickoff");
    assert_eq!(sc["notes"], "Follow up on pricing");
    assert_eq!(sc["latestSummary"]["content"], "## TL;DR\nRoadmap agreed.");
    assert_eq!(sc["summaryCount"], 1);
    let parts = sc["participants"].as_array().unwrap();
    let alice = parts
        .iter()
        .find(|p| p["rawLabel"] == "SPEAKER_00")
        .unwrap();
    assert_eq!(alice["displayName"], "Alice");
    assert_eq!(alice["confirmed"], true);
    assert!(
        sc.get("micWav").is_none(),
        "audio paths must never be exposed"
    );

    // search_meetings hits transcript + summary + persona + notes.
    let r = rpc
        .tool(9, "search_meetings", json!({"query": "roadmap"}))
        .await;
    let results = r["structuredContent"]["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    let fields: Vec<&str> = results[0]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["field"].as_str().unwrap())
        .collect();
    assert!(
        fields.contains(&"transcript") && fields.contains(&"summary"),
        "{fields:?}"
    );
    let r = rpc
        .tool(10, "search_meetings", json!({"query": "alice"}))
        .await;
    assert_eq!(
        r["structuredContent"]["results"].as_array().unwrap().len(),
        1
    );

    // Customers + personas + summaries.
    let r = rpc.tool(11, "list_customers", json!({})).await;
    assert_eq!(r["structuredContent"]["customers"][0]["name"], "Acme");
    let r = rpc
        .tool(12, "get_customer", json!({"customer_id": cid}))
        .await;
    assert_eq!(r["structuredContent"]["meetingCount"], 1);
    let r = rpc
        .tool(13, "list_customer_summaries", json!({"customer_id": cid}))
        .await;
    assert_eq!(
        r["structuredContent"]["rollups"].as_array().unwrap().len(),
        0
    );
    let r = rpc.tool(14, "list_personas", json!({})).await;
    assert_eq!(
        r["structuredContent"]["personas"][0]["displayName"],
        "Alice"
    );
    let r = rpc
        .tool(15, "list_summaries", json!({"meeting_id": mid}))
        .await;
    assert_eq!(
        r["structuredContent"]["summaries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Unknown id → tool-level error (isError), not a protocol error.
    let r = rpc
        .tool(16, "get_meeting", json!({"meeting_id": 9999}))
        .await;
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("not found"));

    // Stop → connection refused.
    server.stop();
    assert!(!server.is_running());
    let err = reqwest::Client::new()
        .post(super::endpoint_url(port))
        .header(header::AUTHORIZATION, "Bearer x")
        .body("{}")
        .send()
        .await;
    assert!(err.is_err(), "listener should be closed after stop()");
}

#[test]
fn apply_settings_reconciles_enabled_port_and_token() {
    use crate::settings::AppSettings;

    let (db, _, _) = seeded();
    let server = McpServer::default();
    let mut s = AppSettings {
        mcp_enabled: true,
        mcp_port: 0,
        mcp_token: Some("abc".into()),
        ..AppSettings::default()
    };

    server.apply_settings(db.clone(), &s);
    assert!(server.is_running());
    let port = server.status().port.unwrap();
    assert!(port > 0);

    // Same settings → no restart (port unchanged).
    s.mcp_port = port;
    server.apply_settings(db.clone(), &s);
    assert_eq!(server.status().port, Some(port));

    // Token change → restart on the same port.
    s.mcp_token = Some("def".into());
    server.apply_settings(db.clone(), &s);
    assert_eq!(server.status().port, Some(port));

    // Disabled → stopped, error cleared.
    s.mcp_enabled = false;
    server.apply_settings(db.clone(), &s);
    assert!(!server.is_running());
    assert_eq!(server.status().error, None);

    // Port in use → recorded error, not running.
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    s.mcp_enabled = true;
    s.mcp_port = blocker.local_addr().unwrap().port();
    server.apply_settings(db.clone(), &s);
    assert!(!server.is_running());
    assert!(server.status().error.unwrap().contains("cannot listen"));
    drop(blocker);
}

#[test]
fn generated_tokens_are_64_hex_and_unique() {
    let a = super::generate_token();
    let b = super::generate_token();
    assert_eq!(a.len(), 64);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

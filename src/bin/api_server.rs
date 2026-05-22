use axum::{
    routing::post,
    Router,
    Json,
    extract::State,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use hca::ChromeBrowser;

#[derive(Deserialize)]
struct TokenRequest {
    site_url: String,
    site_key: String,
}

#[derive(Serialize)]
struct ExecutionMetrics {
    total_time_ms: u128,
    script_time_ms: u128,
    cached: bool,
    human_readable: String,
}

#[derive(Serialize)]
struct TokenResponse {
    token: Option<String>,
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<ExecutionMetrics>,
}

struct WorkerMessage {
    req: TokenRequest,
    resp_tx: oneshot::Sender<TokenResponse>,
}

struct AppState {
    tx: mpsc::Sender<WorkerMessage>,
}

/// Per-worker state that tracks the cached recaptcha site_key to skip reloading.
struct WorkerState {
    browser: ChromeBrowser,
    cached_site_key: Option<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let num_workers: usize = std::env::var("WORKERS")
        .unwrap_or_else(|_| "5".to_string())
        .parse()
        .unwrap_or(5);

    println!("🔥 Booting with {} workers...", num_workers);

    // Scale the request queue based on worker count
    let (tx, rx) = mpsc::channel::<WorkerMessage>(num_workers * 10);
    let rx = Arc::new(tokio::sync::Mutex::new(rx));

    // Spawn workers
    for i in 0..num_workers {
        let rx_clone = rx.clone();
        tokio::spawn(async move {
            // Stagger startups by 250ms so we don't CPU-spike the server when booting 100 Chromes!
            tokio::time::sleep(tokio::time::Duration::from_millis(i as u64 * 250)).await;
            
            println!("🚀 Starting worker {}/{}", i + 1, num_workers);
            let port = 9222 + i as u16;
            let browser = match ChromeBrowser::new(true, port).await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Worker {} failed to start browser: {}", i, e);
                    return;
                }
            };

            let mut state = WorkerState {
                browser,
                cached_site_key: None,
            };

            // Pre-navigate to about:blank once
            if let Err(e) = state.browser.navigate_to_fast("about:blank").await {
                eprintln!("Worker {} failed initial navigation: {}", i, e);
            }
            println!("✅ Worker {} ready", i);

            loop {
                let mut rx_lock = rx_clone.lock().await;
                let msg = match rx_lock.recv().await {
                    Some(m) => m,
                    None => break,
                };
                drop(rx_lock);

                println!("Worker {} processing request for {}", i, msg.req.site_url);
                let result = process_request(&mut state, &msg.req).await;
                let _ = msg.resp_tx.send(result);
            }
        });
    }

    let state = Arc::new(AppState { tx });

    let app = Router::new()
        .route("/api/v1/generate-token", post(generate_token))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("🌐 API Server listening on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

async fn process_request(worker: &mut WorkerState, req: &TokenRequest) -> TokenResponse {
    let total_start = std::time::Instant::now();

    let is_cached = worker.cached_site_key.as_deref() == Some(&req.site_key);

    let script = if is_cached {
        // ⚡ FAST PATH: recaptcha API is already loaded with this site_key.
        // Just call grecaptcha.execute() directly — no script injection needed.
        format!(r#"
            new Promise((resolve, reject) => {{
                const timer = setTimeout(() => reject('Timeout'), 15000);
                window.grecaptcha.execute('{}', {{action: 'submit'}})
                    .then(token => {{ clearTimeout(timer); resolve(token); }})
                    .catch(e => {{ clearTimeout(timer); reject(e.toString()); }});
            }})
        "#, req.site_key)
    } else {
        // 🐢 COLD PATH: Need to reload page and inject recaptcha API fresh.
        // This only happens on the first request or when site_key changes.

        // Reload the blank page to clear old state
        let _ = worker.browser.execute_script_fast("location.reload();").await;
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        format!(r#"
            new Promise((resolve, reject) => {{
                const TIMEOUT = 25000;
                const timer = setTimeout(() => reject('Timeout: token generation exceeded 25s'), TIMEOUT);
                const siteKey = '{}';

                function doExecute() {{
                    window.grecaptcha.execute(siteKey, {{action: 'submit'}})
                        .then(token => {{ clearTimeout(timer); resolve(token); }})
                        .catch(e => {{ clearTimeout(timer); reject('grecaptcha.execute failed: ' + e.toString()); }});
                }}

                function poll() {{
                    if (typeof window.grecaptcha !== 'undefined' && typeof window.grecaptcha.execute === 'function') {{
                        window.grecaptcha.ready(doExecute);
                    }} else {{
                        setTimeout(poll, 100);
                    }}
                }}

                const s = document.createElement('script');
                s.src = 'https://www.google.com/recaptcha/api.js?render=' + siteKey;
                s.onerror = () => {{ clearTimeout(timer); reject('Failed to load recaptcha api.js'); }};
                document.head.appendChild(s);
                poll();
            }})
        "#, req.site_key)
    };

    let script_start = std::time::Instant::now();

    match worker.browser.execute_script_fast(&script).await {
        Ok(token) => {
            let script_ms = script_start.elapsed().as_millis();
            let total_ms = total_start.elapsed().as_millis();
            if token == "null" || token.is_empty() {
                // Invalidate cache on failure
                worker.cached_site_key = None;
                TokenResponse {
                    token: None,
                    error: Some(format!("Received empty token after {}ms", total_ms)),
                    metrics: None,
                }
            } else {
                // Cache this site_key for future fast-path requests
                worker.cached_site_key = Some(req.site_key.clone());
                let path = if is_cached { "⚡ cached" } else { "🐢 cold" };
                println!("  ✅ [{}] Token in {}ms (script: {}ms, {} chars)", path, total_ms, script_ms, token.len());
                TokenResponse {
                    token: Some(token),
                    error: None,
                    metrics: Some(ExecutionMetrics {
                        total_time_ms: total_ms,
                        script_time_ms: script_ms,
                        cached: is_cached,
                        human_readable: format!("{} | Total: {}ms | Script: {}ms",
                            if is_cached { "⚡ Cached" } else { "🐢 Cold start" },
                            total_ms, script_ms),
                    }),
                }
            }
        },
        Err(e) => {
            worker.cached_site_key = None;
            TokenResponse {
                token: None,
                error: Some(format!("Script execution failed: {}", e)),
                metrics: None,
            }
        }
    }
}

async fn generate_token(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<TokenRequest>,
) -> Json<TokenResponse> {
    let (resp_tx, resp_rx) = oneshot::channel();
    let msg = WorkerMessage {
        req: payload,
        resp_tx,
    };

    if state.tx.send(msg).await.is_err() {
        return Json(TokenResponse {
            token: None,
            error: Some("Server is overloaded or worker pool is down".to_string()),
            metrics: None,
        });
    }

    match resp_rx.await {
        Ok(resp) => Json(resp),
        Err(_) => Json(TokenResponse {
            token: None,
            error: Some("Internal server error: worker failed to respond".to_string()),
            metrics: None,
        }),
    }
}

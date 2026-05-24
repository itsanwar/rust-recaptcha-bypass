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
    #[serde(default = "default_action")]
    action: String,
    /// "v3" (default), "v2", or "v2invisible"
    #[serde(default = "default_captcha_type", alias = "type")]
    captcha_type: String,
    /// Custom API domain: "www.google.com" (default) or "www.recaptcha.net"
    #[serde(default = "default_api_domain")]
    api_domain: String,
}

fn default_action() -> String {
    "submit".to_string()
}

fn default_captcha_type() -> String {
    "v3".to_string()
}

fn default_api_domain() -> String {
    "www.google.com".to_string()
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

#[derive(Deserialize, Debug)]
struct CapSolverTask {
    #[serde(rename = "type")]
    task_type: String,
    #[serde(rename = "websiteURL")]
    website_url: String,
    #[serde(rename = "websiteKey")]
    website_key: String,
    #[serde(rename = "pageAction", default = "default_action")]
    page_action: String,
    /// Custom API domain for recaptcha.net sites
    #[serde(rename = "recaptchaDataSValue", default)]
    recaptcha_data_s_value: Option<String>,
}

#[derive(Deserialize, Debug)]
struct CreateTaskRequest {
    #[serde(rename = "clientKey", default)]
    client_key: String,
    task: CapSolverTask,
}

#[derive(Serialize, Clone)]
struct CapSolverSolution {
    #[serde(rename = "gRecaptchaResponse")]
    g_recaptcha_response: String,
}

#[derive(Serialize, Clone)]
struct CreateTaskResponse {
    #[serde(rename = "errorId")]
    error_id: i32,
    #[serde(rename = "errorCode", skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(rename = "errorDescription", skip_serializing_if = "Option::is_none")]
    error_description: Option<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    solution: Option<CapSolverSolution>,
    #[serde(rename = "taskId", skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GetTaskResultRequest {
    #[serde(rename = "clientKey", default)]
    client_key: String,
    #[serde(rename = "taskId")]
    task_id: String,
}

struct WorkerMessage {
    req: TokenRequest,
    resp_tx: oneshot::Sender<TokenResponse>,
}

struct AppState {
    tx: mpsc::Sender<WorkerMessage>,
    tasks: std::sync::Mutex<std::collections::HashMap<String, CreateTaskResponse>>,
}

/// Per-worker state that tracks the cached recaptcha site_key to skip reloading.
struct WorkerState {
    browser: ChromeBrowser,
    cached_site_key: Option<String>,
    cached_site_url: Option<String>,
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
                cached_site_url: None,
            };

            // Pre-navigate to about:blank once
            if let Err(e) = state.browser.navigate_to_fast("about:blank").await {
                eprintln!("Worker {} failed initial navigation: {}", i, e);
            }
            
            // EXPERT: Pre-warm Google's recaptcha api.js into Chrome's HTTP cache.
            // This downloads api.js once at boot so all future cold-start requests
            // serve it from disk cache (~50ms) instead of downloading fresh (~800ms).
            let prewarm_script = r#"
                new Promise((resolve) => {
                    const s = document.createElement('script');
                    s.src = 'https://www.google.com/recaptcha/api.js?render=explicit';
                    s.onload = () => resolve('cached');
                    s.onerror = () => resolve('failed');
                    (document.head || document.documentElement).appendChild(s);
                    setTimeout(() => resolve('timeout'), 5000);
                })
            "#;
            let _ = state.browser.execute_script_fast(prewarm_script).await;
            println!("✅ Worker {} ready (api.js pre-warmed)", i);

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

    let state = Arc::new(AppState { 
        tx,
        tasks: std::sync::Mutex::new(std::collections::HashMap::new()),
    });

    let app = Router::new()
        .route("/api/v1/generate-token", post(generate_token))
        .route("/createTask", post(create_task))
        .route("/getTaskResult", post(get_task_result))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("🌐 API Server listening on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

async fn process_request(worker: &mut WorkerState, req: &TokenRequest) -> TokenResponse {
    let total_start = std::time::Instant::now();

    let is_cached = worker.cached_site_key.as_deref() == Some(&req.site_key) 
        && worker.cached_site_url.as_deref() == Some(&req.site_url);

    // Detect V2 vs V3
    let is_v2 = req.captcha_type.to_lowercase().contains("v2");
    let api_base = &req.api_domain;

    let script = if is_cached && !is_v2 {
        // ⚡ FAST PATH (V3 only): recaptcha API is already loaded with this site_key.
        format!(r#"
            new Promise((resolve, reject) => {{
                const timer = setTimeout(() => reject('Timeout: execution exceeded 15s'), 15000);
                
                const origError = console.error;
                const origWarn = console.warn;
                const handleError = function(...args) {{
                    const msg = args.join(' ');
                    if (msg.toLowerCase().includes('site key') || msg.toLowerCase().includes('domain') || msg.toLowerCase().includes('invalid')) {{
                        clearTimeout(timer);
                        reject('Google ReCaptcha API Error: ' + msg);
                    }}
                    origError.apply(console, args);
                }};
                console.error = handleError;
                console.warn = handleError;

                const execTimer = setTimeout(() => {{
                    clearTimeout(timer);
                    reject('Google ReCaptcha API Error: Invalid site_key, unauthorized domain, or Google dropped the request.');
                }}, 7000);

                window.grecaptcha.execute('{}', {{action: '{}'}})
                    .then(token => {{ clearTimeout(timer); clearTimeout(execTimer); resolve(token); }})
                    .catch(e => {{ clearTimeout(timer); clearTimeout(execTimer); reject(e.toString()); }});
            }})
        "#, req.site_key, req.action)
    } else {
        // 🐢 COLD PATH: Navigate and inject recaptcha API fresh.
        let _ = worker.browser.navigate_in_place(&req.site_url).await;

        if is_v2 {
            // ===== RECAPTCHA V2 PATH =====
            // Navigate to the actual site URL (Google validates origin against registered domains).
            // V2 uses grecaptcha.render() + execute(widgetId) with size:'invisible'.
            format!(r#"
                new Promise((resolve, reject) => {{
                    const TIMEOUT = 20000;
                    let step = 'init';
                    const timer = setTimeout(() => reject('V2 Timeout at step: ' + step), TIMEOUT);
                    const siteKey = '{site_key}';
                    const apiDomain = '{api_domain}';

                    function injectAndSolve() {{
                        step = 'waiting_for_dom';
                        const target = document.head || document.documentElement;
                        if (!target) {{ setTimeout(injectAndSolve, 50); return; }}

                        // Ensure body exists for widget container
                        if (!document.body) {{
                            const body = document.createElement('body');
                            document.documentElement.appendChild(body);
                        }}

                        // Set custom API endpoint if using recaptcha.net
                        if (apiDomain !== 'www.google.com') {{
                            window.__recaptcha_api = 'https://' + apiDomain + '/recaptcha/api2/';
                        }}

                        step = 'loading_api_js';
                        const s = document.createElement('script');
                        s.src = 'https://' + apiDomain + '/recaptcha/api.js?render=explicit';
                        s.onerror = () => {{ clearTimeout(timer); reject('V2: Failed to load api.js from ' + apiDomain); }};
                        s.onload = () => {{
                            step = 'api_loaded_waiting_grecaptcha';
                            function waitReady() {{
                                if (typeof window.grecaptcha !== 'undefined' && typeof window.grecaptcha.render === 'function') {{
                                    window.grecaptcha.ready(() => {{
                                        try {{
                                            step = 'rendering_widget';
                                            // Create container for the widget
                                            let container = document.getElementById('rc-container');
                                            if (!container) {{
                                                container = document.createElement('div');
                                                container.id = 'rc-container';
                                                document.body.appendChild(container);
                                            }}

                                            const widgetId = window.grecaptcha.render('rc-container', {{
                                                sitekey: siteKey,
                                                callback: (token) => {{
                                                    clearTimeout(timer);
                                                    resolve(token);
                                                }},
                                                'error-callback': () => {{
                                                    clearTimeout(timer);
                                                    reject('V2: Google error-callback (bad site_key or domain)');
                                                }},
                                                'expired-callback': () => {{
                                                    clearTimeout(timer);
                                                    reject('V2: Token expired');
                                                }},
                                                size: 'invisible'
                                            }});

                                            step = 'executing_widget_' + widgetId;
                                            window.grecaptcha.execute(widgetId);

                                            // Fallback: check g-recaptcha-response field after 10s
                                            setTimeout(() => {{
                                                const resp = document.getElementById('g-recaptcha-response');
                                                if (resp && resp.value && resp.value.length > 20) {{
                                                    clearTimeout(timer);
                                                    resolve(resp.value);
                                                }}
                                                step = 'waiting_for_callback_after_execute';
                                            }}, 10000);
                                        }} catch(e) {{
                                            clearTimeout(timer);
                                            reject('V2 render error at step ' + step + ': ' + e.toString());
                                        }}
                                    }});
                                }} else {{
                                    setTimeout(waitReady, 100);
                                }}
                            }}
                            waitReady();
                        }};
                        target.appendChild(s);
                    }}
                    injectAndSolve();
                }})
            "#,
            site_key = req.site_key,
            api_domain = api_base)
        } else {
            // ===== RECAPTCHA V3 PATH =====
            format!(r#"
                new Promise((resolve, reject) => {{
                    const TIMEOUT = 15000;
                    const timer = setTimeout(() => reject('Timeout: token generation exceeded 15s'), TIMEOUT);
                    const siteKey = '{site_key}';
                    const apiDomain = '{api_domain}';

                    const origError = console.error;
                    const origWarn = console.warn;
                    const handleError = function(...args) {{
                        const msg = args.join(' ');
                        if (msg.toLowerCase().includes('site key') || msg.toLowerCase().includes('domain') || msg.toLowerCase().includes('invalid')) {{
                            clearTimeout(timer);
                            reject('Google ReCaptcha API Error: ' + msg);
                        }}
                        origError.apply(console, args);
                    }};
                    console.error = handleError;
                    console.warn = handleError;

                    function doExecute() {{
                        const execTimer = setTimeout(() => {{
                            clearTimeout(timer);
                            reject('Google ReCaptcha API Error: Invalid site_key, unauthorized domain, or Google dropped the request.');
                        }}, 7000);

                        window.grecaptcha.execute(siteKey, {{action: '{action}'}})
                            .then(token => {{ clearTimeout(timer); clearTimeout(execTimer); resolve(token); }})
                            .catch(e => {{ clearTimeout(timer); clearTimeout(execTimer); reject('grecaptcha.execute failed: ' + e.toString()); }});
                    }}

                    function poll() {{
                        if (typeof window.grecaptcha !== 'undefined' && typeof window.grecaptcha.execute === 'function') {{
                            window.grecaptcha.ready(doExecute);
                        }} else {{
                            setTimeout(poll, 100);
                        }}
                    }}

                    function injectScript() {{
                        const target = document.head || document.documentElement;
                        if (target) {{
                            const s = document.createElement('script');
                            s.src = 'https://' + apiDomain + '/recaptcha/api.js?render=' + siteKey;
                            s.onerror = () => {{ clearTimeout(timer); reject('Failed to load recaptcha api.js'); }};
                            target.appendChild(s);
                            poll();
                        }} else {{
                            setTimeout(injectScript, 50);
                        }}
                    }}
                    injectScript();
                }})
            "#,
            site_key = req.site_key,
            action = req.action,
            api_domain = api_base)
        }
    };

    let script_start = std::time::Instant::now();

    match worker.browser.execute_script_fast(&script).await {
        Ok(token) => {
            let script_ms = script_start.elapsed().as_millis();
            let total_ms = total_start.elapsed().as_millis();
            if token == "null" || token.is_empty() {
                // Invalidate cache on failure
                worker.cached_site_key = None;
                worker.cached_site_url = None;
                TokenResponse {
                    token: None,
                    error: Some(format!("Received empty token after {}ms", total_ms)),
                    metrics: None,
                }
            } else {
                // Cache this site_key and site_url for future fast-path requests
                worker.cached_site_key = Some(req.site_key.clone());
                worker.cached_site_url = Some(req.site_url.clone());
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
            worker.cached_site_url = None;
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

async fn create_task(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateTaskRequest>,
) -> Json<CreateTaskResponse> {
    let task_id = format!("{:032x}", rand::random::<u128>());

    // Initialize task state as processing
    {
        let mut tasks = state.tasks.lock().unwrap();
        tasks.insert(task_id.clone(), CreateTaskResponse {
            error_id: 0,
            error_code: None,
            error_description: None,
            status: "processing".to_string(),
            solution: None,
            task_id: Some(task_id.clone()),
        });
    }

    // Auto-detect V2 vs V3 from CapSolver task type
    let captcha_type = if payload.task.task_type.contains("V2") || payload.task.task_type.contains("v2") {
        "v2".to_string()
    } else {
        "v3".to_string()
    };

    // Detect API domain from task type or default
    let api_domain = if payload.task.task_type.contains("recaptcha.net") {
        "www.recaptcha.net".to_string()
    } else {
        "www.google.com".to_string()
    };

    let req = TokenRequest {
        site_url: payload.task.website_url.clone(),
        site_key: payload.task.website_key.clone(),
        action: payload.task.page_action.clone(),
        captcha_type,
        api_domain,
    };

    let (resp_tx, resp_rx) = oneshot::channel();
    let msg = WorkerMessage { req, resp_tx };

    // Send task to worker pool
    let tx_clone = state.tx.clone();
    let state_clone = state.clone();
    let task_id_clone = task_id.clone();

    tokio::spawn(async move {
        if tx_clone.send(msg).await.is_err() {
            let mut tasks = state_clone.tasks.lock().unwrap();
            if let Some(t) = tasks.get_mut(&task_id_clone) {
                t.error_id = 1;
                t.status = "failed".to_string();
                t.error_description = Some("Worker pool overloaded".to_string());
            }
            return;
        }

        match resp_rx.await {
            Ok(resp) => {
                let mut tasks = state_clone.tasks.lock().unwrap();
                if let Some(t) = tasks.get_mut(&task_id_clone) {
                    if let Some(token) = resp.token {
                        t.status = "ready".to_string();
                        t.solution = Some(CapSolverSolution {
                            g_recaptcha_response: token,
                        });
                    } else if let Some(err) = resp.error {
                        t.error_id = 1;
                        t.status = "failed".to_string();
                        t.error_description = Some(err);
                    }
                }
            }
            Err(_) => {
                let mut tasks = state_clone.tasks.lock().unwrap();
                if let Some(t) = tasks.get_mut(&task_id_clone) {
                    t.error_id = 1;
                    t.status = "failed".to_string();
                    t.error_description = Some("Internal worker error".to_string());
                }
            }
        }
    });

    Json(CreateTaskResponse {
        error_id: 0,
        error_code: None,
        error_description: None,
        status: "idle".to_string(), // Initial status
        solution: None,
        task_id: Some(task_id),
    })
}

async fn get_task_result(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<GetTaskResultRequest>,
) -> Json<CreateTaskResponse> {
    let tasks = state.tasks.lock().unwrap();
    if let Some(task_resp) = tasks.get(&payload.task_id) {
        Json(task_resp.clone())
    } else {
        Json(CreateTaskResponse {
            error_id: 1,
            error_code: Some("ERROR_TASK_NOT_FOUND".to_string()),
            error_description: Some("Task not found".to_string()),
            status: "failed".to_string(),
            solution: None,
            task_id: Some(payload.task_id),
        })
    }
}
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

// =============================================================================
// Wit.ai configuration — single hardcoded token per project decision.
// All speech transcription routes through this token.
// =============================================================================
const WIT_AI_TOKEN: &str = "4S3EOT3Y2MWKITCPQ7YBHIIDMCLWTMGH";
const WIT_AI_API_VERSION: &str = "20240524";
const MAX_AUDIO_ATTEMPTS: u32 = 3;

/// Cool-down a worker for this long after Google blocks it with the
/// "Try again later — automated queries" message. Lets the IP/fingerprint
/// cool off before we throw the next request at Google.
const BLOCK_COOLDOWN_SECS: u64 = 45;

/// Stealth payload installed via Page.addScriptToEvaluateOnNewDocument.
/// Runs in every new document AND every iframe — including Google's
/// cross-origin bframe — before the page's own JS executes.
const STEALTH_INIT_SCRIPT: &str = r#"
(() => {
    // Remove the most common headless tell.
    try {
        Object.defineProperty(Navigator.prototype, 'webdriver', { get: () => undefined, configurable: true });
    } catch(e) {}
    // Some detectors check the own-property form too.
    try { delete navigator.__proto__.webdriver; } catch(e) {}
    try { delete navigator.webdriver; } catch(e) {}

    // Fake a plausible plugin array (real Chrome has 3-5 plugins; headless has 0).
    try {
        Object.defineProperty(navigator, 'plugins', {
            get: () => {
                const arr = [1, 2, 3, 4, 5].map(i => ({ name: 'Plugin ' + i, filename: 'p' + i + '.dll', description: '' }));
                arr.item = i => arr[i]; arr.namedItem = () => null; arr.refresh = () => {};
                return arr;
            }
        });
    } catch(e) {}

    try {
        Object.defineProperty(navigator, 'languages', { get: () => ['en-US', 'en'] });
    } catch(e) {}

    // window.chrome — headless Chrome has this empty; populate to match real Chrome.
    try {
        if (!window.chrome || !window.chrome.runtime) {
            window.chrome = window.chrome || {};
            window.chrome.runtime = window.chrome.runtime || {
                connect: () => ({ onMessage: { addListener: () => {} }, postMessage: () => {} }),
                sendMessage: () => {},
                onMessage: { addListener: () => {} }
            };
            window.chrome.csi = window.chrome.csi || (() => ({}));
            window.chrome.loadTimes = window.chrome.loadTimes || (() => ({}));
            window.chrome.app = window.chrome.app || { isInstalled: false };
        }
    } catch(e) {}

    // permissions.query — headless returns 'denied' for notifications,
    // but a real Chrome with Notification.permission='default' returns 'prompt'.
    try {
        const origQuery = navigator.permissions && navigator.permissions.query;
        if (origQuery) {
            navigator.permissions.query = (params) => (
                params && params.name === 'notifications'
                    ? Promise.resolve({ state: Notification.permission })
                    : origQuery.call(navigator.permissions, params)
            );
        }
    } catch(e) {}

    // WebGL vendor/renderer — headless reports 'Google Inc.' / 'Google SwiftShader'.
    try {
        const getParam = WebGLRenderingContext.prototype.getParameter;
        WebGLRenderingContext.prototype.getParameter = function(p) {
            if (p === 37445) return 'Intel Inc.';          // UNMASKED_VENDOR_WEBGL
            if (p === 37446) return 'Intel Iris OpenGL Engine'; // UNMASKED_RENDERER_WEBGL
            return getParam.call(this, p);
        };
    } catch(e) {}

    // hardwareConcurrency / deviceMemory — sanity values for a desktop.
    try { Object.defineProperty(navigator, 'hardwareConcurrency', { get: () => 8 }); } catch(e) {}
    try { Object.defineProperty(navigator, 'deviceMemory', { get: () => 8 }); } catch(e) {}
})();
"#;

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
    #[serde(rename = "recaptchaDataSValue", default)]
    #[allow(dead_code)]
    recaptcha_data_s_value: Option<String>,
}

#[derive(Deserialize, Debug)]
struct CreateTaskRequest {
    #[serde(rename = "clientKey", default)]
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

/// Per-worker state — caches the last recaptcha site_key to short-circuit reloads.
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

    println!("Booting with {} workers (wit.ai expert mode)...", num_workers);

    let (tx, rx) = mpsc::channel::<WorkerMessage>(num_workers * 10);
    let rx = Arc::new(tokio::sync::Mutex::new(rx));

    for i in 0..num_workers {
        let rx_clone = rx.clone();
        tokio::spawn(async move {
            // Stagger startups to avoid CPU spike on cold boot.
            tokio::time::sleep(tokio::time::Duration::from_millis(i as u64 * 250)).await;

            println!("Starting worker {}/{}", i + 1, num_workers);
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

            if let Err(e) = state.browser.navigate_to_fast("about:blank").await {
                eprintln!("Worker {} failed initial navigation: {}", i, e);
            }

            // Install stealth as an init script — applies to every new document AND iframe,
            // so Google's bframe also gets the patched navigator.webdriver / chrome.runtime etc.
            // This must happen AFTER target attach (navigate_to_fast establishes the session).
            if let Err(e) = state.browser.add_init_script(STEALTH_INIT_SCRIPT).await {
                eprintln!("Worker {} failed to install stealth init script: {}", i, e);
            }

            // Pre-warm: download api.js into HTTP cache AND open a TLS keep-alive
            // to api.wit.ai so the first real solve doesn't pay DNS+TLS cold cost.
            let prewarm_script = format!(r#"
                Promise.all([
                    new Promise((resolve) => {{
                        const s = document.createElement('script');
                        s.src = 'https://www.google.com/recaptcha/api.js?render=explicit';
                        s.onload = () => resolve('api_cached');
                        s.onerror = () => resolve('api_failed');
                        (document.head || document.documentElement).appendChild(s);
                        setTimeout(() => resolve('api_timeout'), 5000);
                    }}),
                    fetch('https://api.wit.ai/message?q=hi', {{
                        method: 'GET',
                        headers: {{ 'Authorization': 'Bearer {}' }},
                        keepalive: true
                    }}).then(() => 'wit_warm').catch(() => 'wit_cold')
                ]).then(r => r.join('|'))
            "#, WIT_AI_TOKEN);
            let _ = state.browser.execute_script_fast(&prewarm_script).await;
            println!("Worker {} ready (api.js + wit.ai pre-warmed)", i);

            loop {
                let mut rx_lock = rx_clone.lock().await;
                let msg = match rx_lock.recv().await {
                    Some(m) => m,
                    None => break,
                };
                drop(rx_lock);

                println!("Worker {} processing request for {}", i, msg.req.site_url);
                let result = process_request(&mut state, &msg.req).await;

                // If Google blocked this worker, cool down before pulling the next
                // request — and invalidate any cached site context so the next solve
                // forces a fresh navigation (helps shake the fingerprint signal).
                let blocked = result.error.as_deref()
                    .map(|e| e.contains(BLOCK_SENTINEL))
                    .unwrap_or(false);
                let _ = msg.resp_tx.send(result);
                if blocked {{
                    eprintln!("Worker {} hit Google's anti-bot block — cooling down {}s", i, BLOCK_COOLDOWN_SECS);
                    state.cached_site_key = None;
                    state.cached_site_url = None;
                    tokio::time::sleep(tokio::time::Duration::from_secs(BLOCK_COOLDOWN_SECS)).await;
                    // Force a fresh page navigation to reset the JS context.
                    let _ = state.browser.navigate_in_place("about:blank").await;
                }}
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
    println!("API Server listening on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

async fn process_request(worker: &mut WorkerState, req: &TokenRequest) -> TokenResponse {
    let total_start = std::time::Instant::now();

    let is_cached = worker.cached_site_key.as_deref() == Some(&req.site_key)
        && worker.cached_site_url.as_deref() == Some(&req.site_url);

    let is_v2 = req.captcha_type.to_lowercase().contains("v2");
    let api_base = &req.api_domain;

    let script = if is_cached && !is_v2 {
        build_v3_fast_path(&req.site_key, &req.action)
    } else {
        let _ = worker.browser.navigate_in_place(&req.site_url).await;

        if is_v2 {
            build_v2_audio_solver(&req.site_key, api_base)
        } else {
            build_v3_cold_path(&req.site_key, &req.action, api_base)
        }
    };

    let script_start = std::time::Instant::now();

    match worker.browser.execute_script_fast(&script).await {
        Ok(token) => {
            let script_ms = script_start.elapsed().as_millis();
            let total_ms = total_start.elapsed().as_millis();
            if token == "null" || token.is_empty() {
                worker.cached_site_key = None;
                worker.cached_site_url = None;
                TokenResponse {
                    token: None,
                    error: Some(format!("Received empty token after {}ms", total_ms)),
                    metrics: None,
                }
            } else {
                worker.cached_site_key = Some(req.site_key.clone());
                worker.cached_site_url = Some(req.site_url.clone());
                let path = if is_cached { "cached" } else { "cold" };
                println!("  [{}] Token in {}ms (script: {}ms, {} chars)", path, total_ms, script_ms, token.len());
                TokenResponse {
                    token: Some(token),
                    error: None,
                    metrics: Some(ExecutionMetrics {
                        total_time_ms: total_ms,
                        script_time_ms: script_ms,
                        cached: is_cached,
                        human_readable: format!("{} | Total: {}ms | Script: {}ms",
                            if is_cached { "Cached" } else { "Cold start" },
                            total_ms, script_ms),
                    }),
                }
            }
        }
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

// =============================================================================
// V3 fast path: grecaptcha already loaded, just call execute().
// =============================================================================
fn build_v3_fast_path(site_key: &str, action: &str) -> String {
    format!(r#"
        new Promise((resolve, reject) => {{
            const timer = setTimeout(() => reject('Timeout: execution exceeded 15s'), 15000);
            const execTimer = setTimeout(() => {{
                clearTimeout(timer);
                reject('Google ReCaptcha API Error: invalid site_key, unauthorized domain, or request dropped.');
            }}, 7000);

            const origError = console.error;
            console.error = function(...args) {{
                const msg = args.join(' ').toLowerCase();
                if (msg.includes('site key') || msg.includes('domain') || msg.includes('invalid')) {{
                    clearTimeout(timer); clearTimeout(execTimer);
                    reject('Google ReCaptcha API Error: ' + args.join(' '));
                }}
                origError.apply(console, args);
            }};

            window.grecaptcha.execute('{site_key}', {{action: '{action}'}})
                .then(token => {{ clearTimeout(timer); clearTimeout(execTimer); resolve(token); }})
                .catch(e => {{ clearTimeout(timer); clearTimeout(execTimer); reject(e.toString()); }});
        }})
    "#, site_key = site_key, action = action)
}

// =============================================================================
// V3 cold path: inject api.js, wait for grecaptcha, execute.
// =============================================================================
fn build_v3_cold_path(site_key: &str, action: &str, api_domain: &str) -> String {
    format!(r#"
        new Promise((resolve, reject) => {{
            const TIMEOUT = 15000;
            const timer = setTimeout(() => reject('Timeout: token generation exceeded 15s'), TIMEOUT);
            const siteKey = '{site_key}';
            const apiDomain = '{api_domain}';

            const origError = console.error;
            console.error = function(...args) {{
                const msg = args.join(' ').toLowerCase();
                if (msg.includes('site key') || msg.includes('domain') || msg.includes('invalid')) {{
                    clearTimeout(timer);
                    reject('Google ReCaptcha API Error: ' + args.join(' '));
                }}
                origError.apply(console, args);
            }};

            function doExecute() {{
                const execTimer = setTimeout(() => {{
                    clearTimeout(timer);
                    reject('Google ReCaptcha API Error: invalid site_key, unauthorized domain, or request dropped.');
                }}, 7000);

                window.grecaptcha.execute(siteKey, {{action: '{action}'}})
                    .then(token => {{ clearTimeout(timer); clearTimeout(execTimer); resolve(token); }})
                    .catch(e => {{ clearTimeout(timer); clearTimeout(execTimer); reject('grecaptcha.execute failed: ' + e.toString()); }});
            }}

            function poll() {{
                if (typeof window.grecaptcha !== 'undefined' && typeof window.grecaptcha.execute === 'function') {{
                    window.grecaptcha.ready(doExecute);
                }} else {{
                    setTimeout(poll, 50);
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
                    setTimeout(injectScript, 30);
                }}
            }}
            injectScript();
        }})
    "#, site_key = site_key, action = action, api_domain = api_domain)
}

/// Sentinel string embedded in the error message when Google shows the
/// "Try again later — automated queries" block. Workers grep for this
/// and apply BLOCK_COOLDOWN_SECS before processing the next request.
const BLOCK_SENTINEL: &str = "DOSCAPTCHA_BLOCK";

// =============================================================================
// V2 expert audio solver — Wit.ai backed.
//
// Optimizations vs naive impl:
//   1. MutationObserver for iframe & token detection (instant vs 500ms polling)
//   2. Direct audio[src] extraction (skips waiting for download-link element)
//   3. Up to 3 audio challenges — refresh button if Wit.ai transcription fails
//   4. 5s race-timeout per Wit.ai call (refresh sooner if speech API stalls)
//   5. Step tracking — every error includes the exact stage that failed
//   6. Block detection — surfaces "Try again later" immediately so the
//      worker can cool down instead of hanging for the master timeout
//   7. Humanized timing — random sleeps + MouseEvent dispatch + per-char
//      typing make our interaction profile look less obviously scripted
// =============================================================================
fn build_v2_audio_solver(site_key: &str, api_domain: &str) -> String {
    format!(r#"
        new Promise(async (resolve, reject) => {{
            const MASTER_TIMEOUT = 45000;
            const PER_ATTEMPT_TIMEOUT = 12000;
            const MAX_ATTEMPTS = {max_attempts};
            const WIT_TOKEN = '{wit_token}';
            const WIT_VERSION = '{wit_version}';
            const BLOCK_SENTINEL = '{block_sentinel}';
            const siteKey = '{site_key}';
            const apiDomain = '{api_domain}';

            let step = 'init';
            const masterTimer = setTimeout(() => reject('V2 master timeout at step: ' + step), MASTER_TIMEOUT);
            const fail = (msg) => {{ clearTimeout(masterTimer); reject(msg); }};
            const finish = (token) => {{ clearTimeout(masterTimer); resolve(token); }};

            // ----- Human-like timing helpers -----
            const sleep = (ms) => new Promise(r => setTimeout(r, ms));
            const rand = (min, max) => min + Math.floor(Math.random() * (max - min));
            const humanPause = () => sleep(rand(10, 30));

            // ----- DOM utilities -----
            function waitFor(predicate, root, timeoutMs, label) {{
                return new Promise((res, rej) => {{
                    const immediate = predicate();
                    if (immediate) return res(immediate);
                    const observer = new MutationObserver(() => {{
                        const hit = predicate();
                        if (hit) {{ observer.disconnect(); clearTimeout(t); res(hit); }}
                    }});
                    try {{
                        observer.observe(root.body || root.documentElement || root, {{
                            childList: true, subtree: true, attributes: true, attributeFilter: ['src', 'value', 'style', 'class']
                        }});
                    }} catch(e) {{
                        // root may not be a Document — fall back to the safety tick.
                    }}
                    const t = setTimeout(() => {{ observer.disconnect(); rej('waitFor timeout: ' + label); }}, timeoutMs);
                    const tick = setInterval(() => {{
                        const hit = predicate();
                        if (hit) {{ observer.disconnect(); clearInterval(tick); clearTimeout(t); res(hit); }}
                    }}, 20);
                    setTimeout(() => clearInterval(tick), timeoutMs);
                }});
            }}

            // ----- Block detection: Google's "Try again later — automated queries" -----
            function detectBlock(idoc) {{
                if (!idoc) return null;
                const header = idoc.querySelector('.rc-doscaptcha-header-text');
                const body = idoc.querySelector('.rc-doscaptcha-body-text');
                if (header || body) {{
                    const headerText = header ? (header.textContent || '').trim() : '';
                    const bodyText = body ? (body.textContent || '').trim() : '';
                    return (headerText + ' :: ' + bodyText).slice(0, 240);
                }}
                return null;
            }}

            // ----- Human-like click: real mouse events on element center -----
            function humanClick(el, win) {{
                const rect = el.getBoundingClientRect();
                const cx = rect.left + rect.width / 2 + (Math.random() - 0.5) * Math.min(rect.width, 6);
                const cy = rect.top + rect.height / 2 + (Math.random() - 0.5) * Math.min(rect.height, 6);
                const opts = {{ bubbles: true, cancelable: true, view: win, clientX: cx, clientY: cy, button: 0 }};
                el.dispatchEvent(new MouseEvent('mouseover', opts));
                el.dispatchEvent(new MouseEvent('mousemove', opts));
                el.dispatchEvent(new MouseEvent('mousedown', opts));
                el.dispatchEvent(new MouseEvent('mouseup', opts));
                el.dispatchEvent(new MouseEvent('click', opts));
            }}

            // ----- Type the audio response character-by-character with key events -----
            async function humanType(input, text, win) {{
                input.focus();
                input.value = '';
                for (const ch of text) {{
                    const evInit = {{ bubbles: true, cancelable: true, view: win, key: ch, char: ch }};
                    input.dispatchEvent(new KeyboardEvent('keydown', evInit));
                    input.value += ch;
                    input.dispatchEvent(new InputEvent('input', {{ bubbles: true, data: ch, inputType: 'insertText' }}));
                    input.dispatchEvent(new KeyboardEvent('keyup', evInit));
                    await sleep(rand(5, 15));
                }}
                input.dispatchEvent(new Event('change', {{ bubbles: true }}));
            }}

            // ----- Wit.ai transcription -----
            async function transcribe(audioBlob) {{
                step = 'wit_ai_post';
                const controller = new AbortController();
                const witTimeout = setTimeout(() => controller.abort(), 6000);
                try {{
                    const resp = await fetch('https://api.wit.ai/speech?v=' + WIT_VERSION, {{
                        method: 'POST',
                        headers: {{
                            'Authorization': 'Bearer ' + WIT_TOKEN,
                            'Content-Type': 'audio/mpeg3'
                        }},
                        body: audioBlob,
                        signal: controller.signal
                    }});
                    clearTimeout(witTimeout);
                    if (!resp.ok) throw new Error('wit.ai HTTP ' + resp.status);
                    const text = await resp.text();
                    let transcribed = '';
                    for (const line of text.split('\n')) {{
                        const trimmed = line.trim();
                        if (!trimmed || !trimmed.includes('text')) continue;
                        try {{
                            const data = JSON.parse(trimmed);
                            if (data.text) transcribed = data.text;
                        }} catch(e) {{
                            const match = trimmed.match(/"text"\s*:\s*"([^"]+)"/);
                            if (match) transcribed = match[1];
                        }}
                    }}
                    return transcribed.trim();
                }} finally {{
                    clearTimeout(witTimeout);
                }}
            }}

            // ----- Single audio attempt -----
            async function solveOnce(idoc, iwin, attemptIdx) {{
                // Cheap block check up front — if Google blocked us, every other step will hang.
                const earlyBlock = detectBlock(idoc);
                if (earlyBlock) throw new Error(BLOCK_SENTINEL + ': ' + earlyBlock);

                step = 'attempt_' + attemptIdx + '_locate_audio_btn';
                const audioBtn = await waitFor(
                    () => idoc.getElementById('recaptcha-audio-button'),
                    idoc, 5000, 'audio_button'
                );

                if (attemptIdx === 0) {{
                    step = 'attempt_' + attemptIdx + '_click_audio_btn';
                    await sleep(rand(30, 80));  // tiny pause before clicking
                    humanClick(audioBtn, iwin);
                }}

                // After clicking audio, Google often shows the block instead of a clip.
                await sleep(rand(20, 50));
                const postClickBlock = detectBlock(idoc);
                if (postClickBlock) throw new Error(BLOCK_SENTINEL + ': ' + postClickBlock);

                step = 'attempt_' + attemptIdx + '_wait_audio_src';
                const audioInfo = await waitFor(
                    () => {{
                        const block = detectBlock(idoc);
                        if (block) return {{ blocked: block }};
                        const audio = idoc.querySelector('audio[src], #audio-source');
                        if (audio && audio.src) return {{ src: audio.src }};
                        const link = idoc.querySelector('.rc-audiochallenge-tdownload-link');
                        if (link && link.href) return {{ src: link.href }};
                        return null;
                    }},
                    idoc, 6000, 'audio_src'
                );
                if (audioInfo.blocked) throw new Error(BLOCK_SENTINEL + ': ' + audioInfo.blocked);

                step = 'attempt_' + attemptIdx + '_download';
                const audioResp = await fetch(audioInfo.src, {{ credentials: 'omit' }});
                if (!audioResp.ok) throw new Error('audio download HTTP ' + audioResp.status);
                const audioBlob = await audioResp.blob();

                step = 'attempt_' + attemptIdx + '_transcribe';
                const transcribed = await transcribe(audioBlob);
                if (!transcribed) throw new Error('wit.ai returned empty transcription');

                step = 'attempt_' + attemptIdx + '_fill_input';
                const input = idoc.getElementById('audio-response');
                if (!input) throw new Error('audio-response input missing');
                await humanType(input, transcribed, iwin);

                step = 'attempt_' + attemptIdx + '_click_verify';
                const verifyBtn = idoc.getElementById('recaptcha-verify-button');
                if (!verifyBtn) throw new Error('verify button missing');
                await humanPause();  // small "review what I typed" pause
                humanClick(verifyBtn, iwin);

                step = 'attempt_' + attemptIdx + '_await_outcome';
                return await new Promise((res, rej) => {{
                    const outcomeTimer = setTimeout(() => rej('verify outcome timeout'), PER_ATTEMPT_TIMEOUT);
                    const check = setInterval(() => {{
                        const blk = detectBlock(idoc);
                        if (blk) {{
                            clearInterval(check); clearTimeout(outcomeTimer);
                            return rej(new Error(BLOCK_SENTINEL + ': ' + blk));
                        }}
                        const ta = document.getElementById('g-recaptcha-response');
                        if (ta && ta.value && ta.value.length > 20) {{
                            clearInterval(check); clearTimeout(outcomeTimer);
                            return res({{ ok: true, token: ta.value }});
                        }}
                        const errMsg = idoc.querySelector('.rc-audiochallenge-error-message');
                        if (errMsg && errMsg.textContent && errMsg.style.display !== 'none') {{
                            clearInterval(check); clearTimeout(outcomeTimer);
                            return res({{ ok: false, reason: 'wrong_transcription: ' + errMsg.textContent.trim() }});
                        }}
                        const incorrect = idoc.querySelector('.rc-audiochallenge-incorrect-response');
                        if (incorrect && incorrect.textContent && incorrect.style.display !== 'none') {{
                            clearInterval(check); clearTimeout(outcomeTimer);
                            return res({{ ok: false, reason: 'incorrect_response' }});
                        }}
                    }}, 20);
                }});
            }}

            async function refreshChallenge(idoc, iwin) {{
                step = 'refresh_challenge';
                const reload = idoc.getElementById('recaptcha-reload-button');
                if (!reload) throw new Error('reload button missing');
                await sleep(rand(25, 50));
                humanClick(reload, iwin);
                await sleep(rand(50, 100));
            }}

            // ----- Main flow: render widget, then loop attempts -----
            async function run() {{
                step = 'ensure_dom';
                if (!document.documentElement) {{
                    document.appendChild(document.createElement('html'));
                }}
                if (!document.body) {{
                    const body = document.createElement('body');
                    document.documentElement.appendChild(body);
                }}

                step = 'load_api_js';
                if (!window.grecaptcha || !window.grecaptcha.render) {{
                    await new Promise((res, rej) => {{
                        const s = document.createElement('script');
                        s.src = 'https://' + apiDomain + '/recaptcha/api.js?render=explicit';
                        s.onload = res;
                        s.onerror = () => rej('failed to load api.js from ' + apiDomain);
                        (document.head || document.documentElement).appendChild(s);
                    }});
                }}

                step = 'wait_grecaptcha';
                await new Promise(res => {{
                    const check = () => {{
                        if (window.grecaptcha && typeof window.grecaptcha.render === 'function') {{
                            window.grecaptcha.ready(res);
                        }} else {{
                            setTimeout(check, 30);
                        }}
                    }};
                    check();
                }});

                step = 'render_widget';
                let container = document.getElementById('rc-container');
                if (!container) {{
                    container = document.createElement('div');
                    container.id = 'rc-container';
                    document.body.appendChild(container);
                }}

                const widgetId = await new Promise((res, rej) => {{
                    try {{
                        const id = window.grecaptcha.render('rc-container', {{
                            sitekey: siteKey,
                            callback: (token) => finish(token), // High-confidence: bypasses challenge entirely
                            'error-callback': () => fail('V2: Google error-callback (bad site_key or domain)'),
                            'expired-callback': () => fail('V2: Token expired'),
                            size: 'invisible'
                        }});
                        res(id);
                    }} catch(e) {{ rej('render error: ' + e.toString()); }}
                }});

                step = 'execute_widget';
                window.grecaptcha.execute(widgetId);

                step = 'wait_bframe';
                const bframe = await waitFor(
                    () => {{
                        const ifr = document.querySelector('iframe[src*="bframe"]');
                        if (!ifr) return null;
                        const rect = ifr.getBoundingClientRect();
                        // Even invisible iframes have non-zero rect when challenge is presented.
                        return (rect.width > 0 && rect.height > 0) ? ifr : null;
                    }},
                    document, 8000, 'bframe_iframe'
                );

                step = 'wait_bframe_doc';
                let idoc = null;
                let iwin = null;
                for (let i = 0; i < 80; i++) {{
                    try {{
                        iwin = bframe.contentWindow;
                        idoc = iwin.document;
                        if (idoc && (idoc.getElementById('recaptcha-audio-button') || detectBlock(idoc))) break;
                    }} catch(e) {{}}
                    await sleep(20);
                }}
                if (!idoc) throw new Error('cannot access bframe document');

                // Attempt loop with refresh-on-failure
                for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {{
                    try {{
                        const outcome = await solveOnce(idoc, iwin, attempt);
                        if (outcome.ok) return finish(outcome.token);
                        if (attempt < MAX_ATTEMPTS - 1) {{
                            await refreshChallenge(idoc, iwin);
                        }} else {{
                            return fail('V2 exhausted ' + MAX_ATTEMPTS + ' attempts: ' + outcome.reason);
                        }}
                    }} catch(e) {{
                        const errStr = e.message || e.toString();
                        // Block from Google — abort immediately, no point burning more attempts.
                        if (errStr.indexOf(BLOCK_SENTINEL) !== -1) {{
                            return fail(errStr);
                        }}
                        if (attempt < MAX_ATTEMPTS - 1) {{
                            try {{ await refreshChallenge(idoc, iwin); }} catch(_) {{}}
                        }} else {{
                            return fail('V2 last attempt error at ' + step + ': ' + errStr);
                        }}
                    }}
                }}
                fail('V2 fell through attempt loop at step: ' + step);
            }}

            run().catch(e => fail('V2 setup error at ' + step + ': ' + (e.message || e.toString())));
        }})
    "#,
    max_attempts = MAX_AUDIO_ATTEMPTS,
    wit_token = WIT_AI_TOKEN,
    wit_version = WIT_AI_API_VERSION,
    block_sentinel = BLOCK_SENTINEL,
    site_key = site_key,
    api_domain = api_domain)
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

    let captcha_type = if payload.task.task_type.contains("V2") || payload.task.task_type.contains("v2") {
        "v2".to_string()
    } else {
        "v3".to_string()
    };

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
        status: "idle".to_string(),
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

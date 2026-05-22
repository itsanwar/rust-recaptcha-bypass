use anyhow::{anyhow, Result};
use std::process::Command;
use std::time::Duration;
use tokio::time::sleep;
use reqwest;
use serde_json::json;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use base64::Engine;

pub struct ChromeBrowser {
    pub process: std::process::Child,
    port: u16,
    websocket_url: String,
    target_id: Option<String>,
    session_id: Option<String>,
}

impl ChromeBrowser {
    pub async fn new(headless: bool) -> Result<Self> {
        Self::new_with_size(headless, 1280, 1024).await
    }
    
    pub async fn new_with_size(headless: bool, width: u32, height: u32) -> Result<Self> {
        let chrome_path = if cfg!(target_os = "windows") {
            "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"
        } else if cfg!(target_os = "macos") {
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        } else {
            "/usr/bin/google-chrome"
        };
        let port = 9222 + (rand::random::<u32>() % 1000) as u16;
        let user_data_dir = format!("/tmp/chrome_hca_{}", port);
        
        // Clean up any existing profile
        if std::path::Path::new(&user_data_dir).exists() {
            std::fs::remove_dir_all(&user_data_dir)?;
        }
        std::fs::create_dir_all(&user_data_dir)?;
        
        // Launch Chrome with CDP enabled
        let child = Command::new(chrome_path)
            .args(&[
                format!("--remote-debugging-port={}", port),
                format!("--user-data-dir={}", user_data_dir),
                if headless { "--headless" } else { "--headless=false" }.to_string(),
                "--no-sandbox".to_string(),
                "--disable-dev-shm-usage".to_string(),
                "--disable-blink-features=AutomationControlled".to_string(),
                "--disable-features=VizDisplayCompositor".to_string(),
                "--disable-web-security".to_string(),
                "--disable-features=TranslateUI".to_string(),
                "--disable-ipc-flooding-protection".to_string(),
                "--disable-background-timer-throttling".to_string(),
                "--disable-backgrounding-occluded-windows".to_string(),
                "--disable-renderer-backgrounding".to_string(),
                "--disable-plugins-discovery".to_string(),
                "--disable-default-apps".to_string(),
                "--disable-extensions".to_string(),
                "--disable-component-extensions-with-background-pages".to_string(),
                "--disable-background-networking".to_string(),
                "--disable-sync".to_string(),
                "--disable-default-browser-check".to_string(),
                "--disable-features=TranslateUI,BlinkGenPropertyTrees".to_string(),
                format!("--window-size={},{}", width, height).to_string(),
                "--force-device-scale-factor=1".to_string(),
                "--user-agent=Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36".to_string(),
                "about:blank".to_string(),
            ])
            .spawn()?;
        
        // Wait for Chrome to start
        sleep(Duration::from_secs(3)).await;
        
        // Get the browser websocket URL from the HTTP endpoint
        let browser_info_response = reqwest::get(&format!("http://localhost:{}/json", port)).await?;
        let browser_info: serde_json::Value = browser_info_response.json().await?;
        
        if let Some(websocket_url) = browser_info.as_array().and_then(|arr| arr.first()).and_then(|info| info.get("webSocketDebuggerUrl")).and_then(|url| url.as_str()) {
            let websocket_url = websocket_url.to_string();
            
            Ok(Self {
                process: child,
                port,
                websocket_url,
                target_id: None,
                session_id: None,
            })
        } else {
            Err(anyhow!("Failed to get Chrome websocket URL"))
        }
    }
    
    pub async fn navigate_to_fast(&mut self, url: &str) -> Result<()> {
        // Create a new target with the URL
        let create_target_request = json!({
            "id": 1,
            "method": "Target.createTarget",
            "params": {
                "url": url
            }
        });
        
        let response = self.send_message(create_target_request).await?;
        
        if let Some(target_id) = response.get("result").and_then(|r| r.get("targetId")).and_then(|id| id.as_str()) {
            self.target_id = Some(target_id.to_string());
            
            // Establish session for this target
            match self.establish_session().await {
                Ok(_) => println!("✅ Session established successfully"),
                Err(e) => println!("⚠️ Session establishment failed: {}, but continuing...", e),
            }
            
            return Ok(());
        }
        
        Err(anyhow!("Failed to create Chrome target"))
    }
    
    pub async fn navigate_to(&mut self, url: &str) -> Result<()> {
        // Wait a bit more for Chrome to be fully ready
        sleep(Duration::from_secs(2)).await;
        
        // Create a new target with the URL
        let create_target_request = json!({
            "id": 1,
            "method": "Target.createTarget",
            "params": {
                "url": url
            }
        });
        
        let response = self.send_message(create_target_request).await?;
        
        if let Some(target_id) = response.get("result").and_then(|r| r.get("targetId")).and_then(|id| id.as_str()) {
            self.target_id = Some(target_id.to_string());
            
            // Wait for the page to load
            sleep(Duration::from_secs(3)).await;
            
            // Establish session for this target
            match self.establish_session().await {
                Ok(_) => println!("✅ Session established successfully"),
                Err(e) => println!("⚠️ Session establishment failed: {}, but continuing...", e),
            }
            
            return Ok(());
        }
        
        Err(anyhow!("Failed to create Chrome target"))
    }
    
    /// Establish session with the target
    async fn establish_session(&mut self) -> Result<()> {
        if let Some(target_id) = &self.target_id {
            // Connect to WebSocket
            let (ws_stream, _) = connect_async(&self.websocket_url).await?;
            let (mut write, mut read) = ws_stream.split();
            
            // Attach to target
            let attach_request = json!({
                "id": 1,
                "method": "Target.attachToTarget",
                "params": {
                    "targetId": target_id,
                    "flatten": true
                }
            });
            
            write.send(Message::Text(attach_request.to_string())).await?;
            
            // Read attach response
            if let Some(msg) = read.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(attach_response) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(session_id) = attach_response.get("params").and_then(|r| r.get("sessionId")).and_then(|id| id.as_str()) {
                            self.session_id = Some(session_id.to_string());
                            
                            // Enable necessary domains
                            let mut next_id = 2;
                            let domains = ["Page", "Runtime", "Network", "DOM"];
                            for domain in &domains {
                                let enable_request = json!({
                                    "id": next_id,
                                    "method": format!("{}.enable", domain),
                                    "sessionId": session_id
                                });
                                
                                write.send(Message::Text(enable_request.to_string())).await?;
                                let _ = read.next().await; // Consume response
                                next_id += 1;
                            }
                            
                            return Ok(());
                        }
                    }
                }
            }
        }
        
        Err(anyhow!("Failed to establish session"))
    }
    
    pub async fn take_screenshot(&mut self, path: &str) -> Result<()> {
        if let Some(target_id) = &self.target_id {
            // Connect to WebSocket
            let (ws_stream, _) = connect_async(&self.websocket_url).await?;
            let (mut write, mut read) = ws_stream.split();
            
            // Attach to target
            let attach_request = json!({
                "id": 60,
                "method": "Target.attachToTarget",
                "params": {
                    "targetId": target_id,
                    "flatten": true
                }
            });
            
            write.send(Message::Text(attach_request.to_string())).await?;
            
            // Read attach response
            if let Some(msg) = read.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(attach_response) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(session_id) = attach_response.get("params").and_then(|r| r.get("sessionId")).and_then(|id| id.as_str()) {
                            // Enable Page domain
                            let enable_page_request = json!({
                                "id": 61,
                                "method": "Page.enable",
                                "sessionId": session_id
                            });
                            
                            write.send(Message::Text(enable_page_request.to_string())).await?;
                            let _ = read.next().await; // Consume response
                            
                            // Take screenshot
                            let screenshot_request = json!({
                                "id": 62,
                                "method": "Page.captureScreenshot",
                                "params": {
                                    "format": "png"
                                },
                                "sessionId": session_id
                            });
                            
                            write.send(Message::Text(screenshot_request.to_string())).await?;
                            
                            // Read screenshot response
                            if let Some(msg) = read.next().await {
                                if let Ok(Message::Text(text)) = msg {
                                    if let Ok(response) = serde_json::from_str::<serde_json::Value>(&text) {
                                        // Check if this is the Page.enable response (empty result)
                                        if response.get("result").map_or(false, |r| r.as_object().map_or(false, |obj| obj.is_empty())) {
                                            // Read the next message (actual screenshot response)
                                            if let Some(msg2) = read.next().await {
                                                if let Ok(Message::Text(text2)) = msg2 {
                                                    if let Ok(screenshot_response) = serde_json::from_str::<serde_json::Value>(&text2) {
                                                        if let Some(result) = screenshot_response.get("result") {
                                                            if let Some(data) = result.get("data").and_then(|d| d.as_str()) {
                                                                if let Ok(screenshot_bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                                                                    std::fs::write(path, screenshot_bytes)?;
                                                                    return Ok(());
                                                                }
                                                            }
                                                        }
                                                        
                                                        if let Some(error) = screenshot_response.get("error") {
                                                            return Err(anyhow!("Screenshot error: {}", error));
                                                        }
                                                    }
                                                }
                                            }
                                        } else {
                                            // This might be the screenshot response directly
                                            if let Some(result) = response.get("result") {
                                                if let Some(data) = result.get("data").and_then(|d| d.as_str()) {
                                                    if let Ok(screenshot_bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                                                        std::fs::write(path, screenshot_bytes)?;
                                                        return Ok(());
                                                    }
                                                }
                                            }
                                            
                                            if let Some(error) = response.get("error") {
                                                return Err(anyhow!("Screenshot error: {}", error));
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            return Err(anyhow!("Failed to get session ID from attach response"));
                        }
                    }
                }
            }
        }
        
        Err(anyhow!("Failed to take screenshot"))
    }
    
    pub async fn execute_script(&mut self, script: &str) -> Result<String> {
        if let Some(target_id) = &self.target_id {
            // Connect to WebSocket
            let (ws_stream, _) = connect_async(&self.websocket_url).await?;
            let (mut write, mut read) = ws_stream.split();
            
            // Attach to target
            let attach_request = json!({
                "id": 50,
                "method": "Target.attachToTarget",
                "params": {
                    "targetId": target_id,
                    "flatten": true
                }
            });
            
            write.send(Message::Text(attach_request.to_string())).await?;
            
            // Read attach response
            if let Some(msg) = read.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(attach_response) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(session_id) = attach_response.get("params").and_then(|r| r.get("sessionId")).and_then(|id| id.as_str()) {
                            // Enable Runtime domain
                            let enable_runtime_request = json!({
                                "id": 51,
                                "method": "Runtime.enable",
                                "sessionId": session_id
                            });
                            
                            write.send(Message::Text(enable_runtime_request.to_string())).await?;
                            let _ = read.next().await; // Consume response
                            
                            // Enable Input domain for mouse simulation
                            let enable_input_request = json!({
                                "id": 53,
                                "method": "Input.enable",
                                "sessionId": session_id
                            });
                            
                            write.send(Message::Text(enable_input_request.to_string())).await?;
                            
                            // Read all responses until we get past the Input.enable response
                            while let Some(msg) = read.next().await {
                                if let Ok(Message::Text(text)) = msg {
                                    if let Ok(response) = serde_json::from_str::<serde_json::Value>(&text) {
                                        // Check for Input.enable response
                                        if let Some(id) = response.get("id").and_then(|v| v.as_u64()) {
                                            if id == 53 {
                                                break;
                                            }
                                        }
                                        // Check for Runtime.executionContextCreated notification
                                        else if response.get("method").and_then(|m| m.as_str()) == Some("Runtime.executionContextCreated") {
                                            continue; // Skip this notification
                                        }
                                    }
                                }
                            }
                            
                            // Execute the script
                            let evaluate_request = json!({
                                "id": 52,
                                "method": "Runtime.evaluate",
                                "params": {
                                    "expression": script,
                                    "returnByValue": true,
                                    "awaitPromise": true
                                },
                                "sessionId": session_id
                            });
                            
                            write.send(Message::Text(evaluate_request.to_string())).await?;
                            
                            // Wait for the evaluation response (id: 52)
                            while let Some(msg) = read.next().await {
                                if let Ok(Message::Text(text)) = msg {
                                    if let Ok(response) = serde_json::from_str::<serde_json::Value>(&text) {
                                        // We only care about the response to our evaluate request (id: 52)
                                        if response.get("id").and_then(|id| id.as_u64()) == Some(52) {
                                            if let Some(error) = response.get("error") {
                                                return Err(anyhow!("JavaScript execution error: {}", error));
                                            }
                                            
                                            if let Some(result) = response.get("result") {
                                                if let Some(exception_details) = result.get("exceptionDetails") {
                                                    // Try multiple paths to extract a useful error message
                                                    let err_text = exception_details
                                                        .get("exception")
                                                        .and_then(|e| {
                                                            e.get("description").and_then(|d| d.as_str())
                                                                .or_else(|| e.get("value").and_then(|v| v.as_str()))
                                                        })
                                                        .or_else(|| exception_details.get("text").and_then(|t| t.as_str()));
                                                    
                                                    let msg = match err_text {
                                                        Some(text) => text.to_string(),
                                                        None => format!("Exception details: {}", exception_details),
                                                    };
                                                    return Err(anyhow!("JavaScript exception: {}", msg));
                                                }
                                                
                                                if let Some(inner_result) = result.get("result") {
                                                    if let Some(value) = inner_result.get("value") {
                                                        if let Some(text) = value.as_str() {
                                                            return Ok(text.to_string());
                                                        } else if let Some(num) = value.as_f64() {
                                                            return Ok(num.to_string());
                                                        } else if let Some(bool) = value.as_bool() {
                                                            return Ok(bool.to_string());
                                                        } else if value.is_null() {
                                                            return Ok("null".to_string());
                                                        }
                                                    } else {
                                                        // Fallback if there is no primitive value (e.g., object)
                                                        if inner_result.get("type").and_then(|t| t.as_str()) == Some("undefined") {
                                                            return Ok("undefined".to_string());
                                                        }
                                                        if inner_result.get("type").and_then(|t| t.as_str()) == Some("object") {
                                                            if inner_result.get("subtype").and_then(|s| s.as_str()) == Some("null") {
                                                                return Ok("null".to_string());
                                                            }
                                                            // Return stringified object if possible
                                                            if let Some(desc) = inner_result.get("description").and_then(|d| d.as_str()) {
                                                                return Ok(desc.to_string());
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            
                                            // Fallback if we couldn't parse the result
                                            return Ok("null".to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        Ok("null".to_string())
    }
    
    /// Fast script execution — skips Runtime.enable and Input.enable overhead.
    /// Use for simple evaluations where you don't need mouse/input simulation.
    pub async fn execute_script_fast(&mut self, script: &str) -> Result<String> {
        if let Some(target_id) = &self.target_id {
            let (ws_stream, _) = connect_async(&self.websocket_url).await?;
            let (mut write, mut read) = ws_stream.split();
            
            // Attach to target
            let attach_request = json!({
                "id": 50,
                "method": "Target.attachToTarget",
                "params": {
                    "targetId": target_id,
                    "flatten": true
                }
            });
            
            write.send(Message::Text(attach_request.to_string())).await?;
            
            // Read messages until we get the attachToTarget response with a sessionId
            let mut session_id_str = String::new();
            while let Some(msg) = read.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&text) {
                        // The session ID comes in a Target.attachedToTarget event
                        if let Some(sid) = resp.get("params")
                            .and_then(|p| p.get("sessionId"))
                            .and_then(|s| s.as_str())
                        {
                            session_id_str = sid.to_string();
                            break;
                        }
                        // Or it might come directly in the result
                        if let Some(sid) = resp.get("result")
                            .and_then(|r| r.get("sessionId"))
                            .and_then(|s| s.as_str())
                        {
                            session_id_str = sid.to_string();
                            break;
                        }
                    }
                }
            }
            
            if session_id_str.is_empty() {
                return Err(anyhow!("Failed to get session ID"));
            }
            
            // Go straight to Runtime.evaluate — skip Runtime.enable and Input.enable
            let evaluate_request = json!({
                "id": 52,
                "method": "Runtime.evaluate",
                "params": {
                    "expression": script,
                    "returnByValue": true,
                    "awaitPromise": true
                },
                "sessionId": session_id_str
            });
            
            write.send(Message::Text(evaluate_request.to_string())).await?;
            
            // Wait for our response (id: 52), skip any notifications
            while let Some(msg) = read.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(response) = serde_json::from_str::<serde_json::Value>(&text) {
                        if response.get("id").and_then(|id| id.as_u64()) == Some(52) {
                            if let Some(error) = response.get("error") {
                                return Err(anyhow!("JavaScript execution error: {}", error));
                            }
                            
                            if let Some(result) = response.get("result") {
                                if let Some(exception_details) = result.get("exceptionDetails") {
                                    let err_text = exception_details
                                        .get("exception")
                                        .and_then(|e| {
                                            e.get("description").and_then(|d| d.as_str())
                                                .or_else(|| e.get("value").and_then(|v| v.as_str()))
                                        })
                                        .or_else(|| exception_details.get("text").and_then(|t| t.as_str()));
                                    
                                    let msg = match err_text {
                                        Some(text) => text.to_string(),
                                        None => format!("Exception details: {}", exception_details),
                                    };
                                    return Err(anyhow!("JavaScript exception: {}", msg));
                                }
                                
                                if let Some(inner_result) = result.get("result") {
                                    if let Some(value) = inner_result.get("value") {
                                        if let Some(text) = value.as_str() {
                                            return Ok(text.to_string());
                                        } else if let Some(num) = value.as_f64() {
                                            return Ok(num.to_string());
                                        } else if let Some(b) = value.as_bool() {
                                            return Ok(b.to_string());
                                        } else if value.is_null() {
                                            return Ok("null".to_string());
                                        }
                                    }
                                    if inner_result.get("type").and_then(|t| t.as_str()) == Some("undefined") {
                                        return Ok("undefined".to_string());
                                    }
                                }
                            }
                            
                            return Ok("null".to_string());
                        }
                    }
                }
            }
        }
        
        Ok("null".to_string())
    }
    /// Execute JavaScript with retry for bot detection bypass
    pub async fn execute_script_with_retry(&mut self, script: &str, max_retries: u32) -> Result<String> {
        let mut retries = 0;
        
        while retries < max_retries {
            match self.execute_script(script).await {
                Ok(result) => {
                    if result != "null" && !result.is_empty() {
                        return Ok(result);
                    }
                }
                Err(e) => {
                    println!("Retry {} failed: {}", retries + 1, e);
                }
            }
            
            retries += 1;
            if retries < max_retries {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
        }
        
        Err(anyhow!("Script execution failed after {} retries", max_retries))
    }
    
    /// Set custom headers for bot detection bypass
    pub async fn set_headers(&mut self, headers: serde_json::Value) -> Result<()> {
        if let Some(target_id) = &self.target_id {
            // Connect to WebSocket
            let (ws_stream, _) = connect_async(&self.websocket_url).await?;
            let (mut write, mut read) = ws_stream.split();
            
            // Attach to target
            let attach_request = json!({
                "id": 300,
                "method": "Target.attachToTarget",
                "params": {
                    "targetId": target_id,
                    "flatten": true
                }
            });
            
            write.send(Message::Text(attach_request.to_string())).await?;
            
            // Read attach response
            if let Some(msg) = read.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(attach_response) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(session_id) = attach_response.get("params").and_then(|r| r.get("sessionId")).and_then(|id| id.as_str()) {
                            // Enable Network domain
                            let enable_network_request = json!({
                                "id": 301,
                                "method": "Network.enable",
                                "sessionId": session_id
                            });
                            
                            write.send(Message::Text(enable_network_request.to_string())).await?;
                            let _ = read.next().await; // Consume response
                            
                            // Set headers
                            let headers_request = json!({
                                "id": 302,
                                "method": "Network.setExtraHTTPHeaders",
                                "params": {
                                    "headers": headers
                                },
                                "sessionId": session_id
                            });
                            
                            write.send(Message::Text(headers_request.to_string())).await?;
                            let _ = read.next().await; // Consume response
                            
                            return Ok(());
                        }
                    }
                }
            }
        }
        
        Err(anyhow!("Failed to set headers"))
    }
    
    /// Wait for page to load completely
    pub async fn wait_for_page_load(&mut self, timeout_ms: u64) -> Result<()> {
        let start_time = std::time::Instant::now();
        
        while start_time.elapsed().as_millis() < timeout_ms as u128 {
            if let Ok(ready) = self.execute_script("document.readyState").await {
                if ready == "complete" {
                    return Ok(());
                }
            }
            
            // If JavaScript execution fails, just wait and try again
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
        
        // If we timeout, just continue - the page might be loaded anyway
        println!("⚠️ Page load timeout, but continuing...");
        Ok(())
    }
    
    /// Apply bot detection bypass techniques
    pub async fn apply_bot_bypass(&mut self) -> Result<()> {
        println!("🤖 Applying bot detection bypass techniques...");
        
        // Set realistic headers
        let headers = json!({
            "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,image/apng,*/*;q=0.8",
            "Accept-Language": "en-US,en;q=0.9",
            "Accept-Encoding": "gzip, deflate, br",
            "DNT": "1",
            "Connection": "keep-alive",
            "Upgrade-Insecure-Requests": "1",
            "Sec-Fetch-Dest": "document",
            "Sec-Fetch-Mode": "navigate",
            "Sec-Fetch-Site": "none",
            "Sec-Fetch-User": "?1",
            "Cache-Control": "max-age=0"
        });
        
        self.set_headers(headers).await?;
        
        // Inject anti-detection JavaScript
        let anti_detection_script = r#"
        // Remove webdriver traces
        delete navigator.webdriver;
        delete navigator.__proto__.webdriver;
        
        // Override permissions API
        Object.defineProperty(navigator, 'permissions', {
            get: () => ({
                query: () => Promise.resolve({ state: 'granted' })
            })
        });
        
        // Override plugins
        Object.defineProperty(navigator, 'plugins', {
            get: () => [1, 2, 3, 4, 5]
        });
        
        // Override languages
        Object.defineProperty(navigator, 'languages', {
            get: () => ['en-US', 'en']
        });
        
        // Override chrome runtime
        window.chrome = {
            runtime: {}
        };
        
        // Override permissions
        const originalQuery = window.navigator.permissions.query;
        window.navigator.permissions.query = (parameters) => (
            parameters.name === 'notifications' ?
                Promise.resolve({ state: Notification.permission }) :
                originalQuery(parameters)
        );
        "#;
        
        self.execute_script(anti_detection_script).await?;
        
        // Wait a bit for scripts to settle
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
        
        println!("✅ Bot detection bypass applied successfully");
        Ok(())
    }
    
    /// Handle Cloudflare challenges
    pub async fn handle_cloudflare(&mut self, timeout_ms: u64) -> Result<()> {
        println!("🌩️ Checking for Cloudflare challenge...");
        
        let start_time = std::time::Instant::now();
        
        while start_time.elapsed().as_millis() < timeout_ms as u128 {
            // Check if Cloudflare challenge is present
            if let Ok(cf_check) = self.execute_script(
                "!!document.querySelector('.cf-browser-verification') || !!document.querySelector('#challenge-form')"
            ).await {
                if cf_check == "true" {
                    println!("⏳ Cloudflare challenge detected, waiting...");
                    tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;
                    continue;
                }
            }
            
            // Check if page is loaded
            if let Ok(ready) = self.execute_script("document.readyState").await {
                if ready == "complete" {
                    // Check for Cloudflare success
                    if let Ok(cf_success) = self.execute_script(
                        "!document.querySelector('.cf-browser-verification') && !document.querySelector('#challenge-form')"
                    ).await {
                        if cf_success == "true" {
                            println!("✅ Cloudflare challenge passed");
                            return Ok(());
                        }
                    }
                }
            }
            
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
        
        println!("⚠️ Cloudflare challenge timeout, proceeding anyway");
        Ok(())
    }
    
    /// Bypass Google reCAPTCHA v3
    pub async fn bypass_google_recaptcha3(&mut self) -> Result<()> {
        println!("🔐 Bypassing Google reCAPTCHA v3...");
        
        // Wait for reCAPTCHA to load
        tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;
        
        // Inject reCAPTCHA bypass script
        let recaptcha_bypass_script = r#"
        (function() {
            // Override reCAPTCHA v3 token generation
            if (typeof grecaptcha !== 'undefined') {
                // Store original execute method
                const originalExecute = grecaptcha.execute;
                
                // Override execute method to always return a valid token
                grecaptcha.execute = function(sitekey, options) {
                    // Generate a fake but valid-looking token
                    const fakeToken = '03AGdBq26B9y-8K2n4pXrQaZbVcWdEfGhIjKlMnOpQrStUvWxYz1234567890';
                    
                    // Return promise that resolves with fake token
                    return Promise.resolve(fakeToken);
                };
                
                // Override ready method
                grecaptcha.ready = function(callback) {
                    // Execute callback immediately
                    callback();
                };
                
                // Override getResponse method
                grecaptcha.getResponse = function(opt_widget_id) {
                    return '03AGdBq26B9y-8K2n4pXrQaZbVcWdEfGhIjKlMnOpQrStUvWxYz1234567890';
                };
            }
            
            // Intercept and modify reCAPTCHA network requests
            const originalFetch = window.fetch;
            window.fetch = function(url, options) {
                if (url && url.includes('recaptcha')) {
                    // Modify reCAPTCHA requests
                    if (options && options.body) {
                        try {
                            const body = JSON.parse(options.body);
                            if (body.token) {
                                body.token = '03AGdBq26B9y-8K2n4pXrQaZbVcWdEfGhIjKlMnOpQrStUvWxYz1234567890';
                                options.body = JSON.stringify(body);
                            }
                        } catch (e) {
                            // Ignore JSON parsing errors
                        }
                    }
                }
                return originalFetch.apply(this, arguments);
            };
            
            // Override XMLHttpRequest for reCAPTCHA
            const originalXHROpen = XMLHttpRequest.prototype.open;
            const originalXHRSend = XMLHttpRequest.prototype.send;
            
            XMLHttpRequest.prototype.open = function(method, url, async, user, pass) {
                this._recaptchaUrl = url;
                return originalXHROpen.apply(this, arguments);
            };
            
            XMLHttpRequest.prototype.send = function(data) {
                if (this._recaptchaUrl && this._recaptchaUrl.includes('recaptcha')) {
                    try {
                        if (data && typeof data === 'string') {
                            const parsedData = JSON.parse(data);
                            if (parsedData.token) {
                                parsedData.token = '03AGdBq26B9y-8K2n4pXrQaZbVcWdEfGhIjKlMnOpQrStUvWxYz1234567890';
                                data = JSON.stringify(parsedData);
                            }
                        }
                    } catch (e) {
                        // Ignore JSON parsing errors
                    }
                }
                return originalXHRSend.apply(this, arguments);
            };
            
            // Auto-click reCAPTCHA if present
            const recaptchaElements = document.querySelectorAll('[class*="recaptcha"], [id*="recaptcha"]');
            recaptchaElements.forEach(element => {
                if (element.style.display !== 'none' && element.offsetParent !== null) {
                    element.click();
                }
            });
            
            // Remove reCAPTCHA badges and elements
            const badges = document.querySelectorAll('.grecaptcha-badge');
            badges.forEach(badge => badge.remove());
            
            console.log('reCAPTCHA v3 bypass injected successfully');
        })();
        "#;
        
        self.execute_script(recaptcha_bypass_script).await?;
        
        // Wait for bypass to take effect
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
        
        // Check if reCAPTCHA is present and try to solve it
        if let Ok(has_recaptcha) = self.execute_script(
            "!!document.querySelector('.g-recaptcha') || !!document.querySelector('[class*=\"recaptcha\"]') || typeof grecaptcha !== 'undefined'"
        ).await {
            if has_recaptcha == "true" {
                println!("🔍 reCAPTCHA detected, attempting to bypass...");
                
                // Try to find and click any reCAPTCHA elements
                let click_script = r#"
                const recaptchaElements = document.querySelectorAll('[class*="recaptcha"], [id*="recaptcha"], .g-recaptcha');
                let clicked = false;
                recaptchaElements.forEach(element => {
                    if (element.style.display !== 'none' && element.offsetParent !== null) {
                        element.click();
                        clicked = true;
                    }
                });
                clicked;
                "#;
                
                if let Ok(clicked) = self.execute_script(click_script).await {
                    if clicked == "true" {
                        println!("✅ Clicked reCAPTCHA elements");
                    }
                }
                
                // Wait for any reCAPTCHA processing
                tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;
                
                // Try to submit any forms that might be waiting for reCAPTCHA
                let submit_script = r#"
                const forms = document.querySelectorAll('form');
                let submitted = false;
                forms.forEach(form => {
                    const submitButton = form.querySelector('button[type="submit"], input[type="submit"]');
                    if (submitButton && !submitButton.disabled) {
                        submitButton.click();
                        submitted = true;
                    }
                });
                submitted;
                "#;
                
                if let Ok(submitted) = self.execute_script(submit_script).await {
                    if submitted == "true" {
                        println!("✅ Submitted forms after reCAPTCHA bypass");
                    }
                }
            }
        }
        
        println!("✅ reCAPTCHA v3 bypass completed");
        Ok(())
    }
    
    async fn send_message(&mut self, request: serde_json::Value) -> Result<serde_json::Value> {
        // Connect to WebSocket
        let (ws_stream, _) = connect_async(&self.websocket_url).await?;
        let (mut write, mut read) = ws_stream.split();
        
        // Send message
        write.send(Message::Text(request.to_string())).await?;
        
        // Read response
        if let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(response) = serde_json::from_str::<serde_json::Value>(&text) {
                        return Ok(response);
                    }
                }
                _ => {}
            }
        }
        
        Err(anyhow!("Failed to get response"))
    }
    
    /// Simulate mouse movement to specific coordinates
    pub async fn move_mouse_to(&mut self, x: f64, y: f64) -> Result<()> {
        let session_id = self.session_id.clone();
        
        if let Some(session_id) = session_id {
            let input_request = json!({
                "id": 60,
                "method": "Input.dispatchMouseEvent",
                "params": {
                    "type": "mouseMoved",
                    "x": x,
                    "y": y
                },
                "sessionId": session_id
            });
            
            self.send_message(input_request).await?;
        }
        Ok(())
    }

    /// Simulate mouse click at specific coordinates
    pub async fn click_at(&mut self, x: f64, y: f64) -> Result<()> {
        let session_id = self.session_id.clone();
        
        if let Some(session_id) = session_id {
            // Mouse down
            let mouse_down_request = json!({
                "id": 61,
                "method": "Input.dispatchMouseEvent",
                "params": {
                    "type": "mousePressed",
                    "x": x,
                    "y": y,
                    "button": "left",
                    "clickCount": 1
                },
                "sessionId": session_id
            });
            
            self.send_message(mouse_down_request).await?;
            
            // Small delay between down and up
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            
            // Mouse up
            let mouse_up_request = json!({
                "id": 62,
                "method": "Input.dispatchMouseEvent",
                "params": {
                    "type": "mouseReleased",
                    "x": x,
                    "y": y,
                    "button": "left",
                    "clickCount": 1
                },
                "sessionId": session_id
            });
            
            self.send_message(mouse_up_request).await?;
        }
        Ok(())
    }

    /// Find element and click it with mouse simulation
    pub async fn click_element(&mut self, selector: &str) -> Result<bool> {
        let script = format!(r#"
        (function() {{
            const element = document.querySelector('{}');
            if (element && element.offsetParent !== null) {{
                const rect = element.getBoundingClientRect();
                return {{
                    found: true,
                    x: rect.left + rect.width / 2,
                    y: rect.top + rect.height / 2,
                    width: rect.width,
                    height: rect.height
                }};
            }}
            return {{ found: false }};
        }})()
        "#, selector);

        if let Ok(result) = self.execute_script(&script).await {
            if let Ok(element_info) = serde_json::from_str::<serde_json::Value>(&result) {
                if let Some(found) = element_info.get("found").and_then(|v| v.as_bool()) {
                    if found {
                        if let Some(x) = element_info.get("x").and_then(|v| v.as_f64()) {
                            if let Some(y) = element_info.get("y").and_then(|v| v.as_f64()) {
                                println!("🖱️  Clicking element at ({}, {})", x, y);
                                self.move_mouse_to(x, y).await?;
                                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                self.click_at(x, y).await?;
                                return Ok(true);
                            }
                        }
                    }
                }
            }
        }
        Ok(false)
    }

    /// Find and click checkbox or radio button
    pub async fn click_checkbox(&mut self, selector: &str) -> Result<bool> {
        let script = format!(r#"
        (function() {{
            const element = document.querySelector('{}');
            if (element && element.offsetParent !== null) {{
                // Check if it's already checked
                const wasChecked = element.checked;
                
                const rect = element.getBoundingClientRect();
                return {{
                    found: true,
                    x: rect.left + rect.width / 2,
                    y: rect.top + rect.height / 2,
                    width: rect.width,
                    height: rect.height,
                    wasChecked: wasChecked,
                    type: element.type || 'unknown'
                }};
            }}
            return {{ found: false }};
        }})()
        "#, selector);

        if let Ok(result) = self.execute_script(&script).await {
            if let Ok(element_info) = serde_json::from_str::<serde_json::Value>(&result) {
                if let Some(found) = element_info.get("found").and_then(|v| v.as_bool()) {
                    if found {
                        if let Some(x) = element_info.get("x").and_then(|v| v.as_f64()) {
                            if let Some(y) = element_info.get("y").and_then(|v| v.as_f64()) {
                                if let Some(was_checked) = element_info.get("wasChecked").and_then(|v| v.as_bool()) {
                                    if let Some(element_type) = element_info.get("type").and_then(|v| v.as_str()) {
                                        println!("🖱️  Clicking {} at ({}, {}) - was checked: {}", element_type, x, y, was_checked);
                                        
                                        // Move mouse to element
                                        self.move_mouse_to(x, y).await?;
                                        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
                                        
                                        // Click the element
                                        self.click_at(x, y).await?;
                                        
                                        // Wait a moment for the click to register
                                        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                                        
                                        return Ok(true);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(false)
    }
    
    pub async fn quit(&mut self) -> Result<()> {
        // Kill Chrome process
        if let Err(e) = self.process.kill() {
            println!("Warning: Failed to kill Chrome process: {}", e);
        }
        
        // Clean up user data directory
        if std::path::Path::new(&format!("/tmp/chrome_hca_{}", self.port)).exists() {
            std::fs::remove_dir_all(&format!("/tmp/chrome_hca_{}", self.port))?;
        }
        
        Ok(())
    }
}

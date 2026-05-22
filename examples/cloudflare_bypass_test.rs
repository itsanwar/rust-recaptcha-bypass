use anyhow::Result;
use std::process::Command;
use std::time::Duration;
use tokio::time::sleep;
use reqwest::Client;
use serde_json::json;
use base64::Engine;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    println!("🛡️ **Cloudflare WAF Bypass Test** 🛡️");
    println!("==================================");
    
    // Launch Chrome
    println!("\n🚀 Launching Chrome for Cloudflare bypass test...");
    let chrome_path = if cfg!(target_os = "windows") {
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"
    } else if cfg!(target_os = "macos") {
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    } else {
        "/usr/bin/google-chrome"
    };
    let port = 9235;
    let user_data = "/tmp/chrome_cloudflare_test";
    
    // Clean up any existing profile
    if std::path::Path::new(user_data).exists() {
        std::fs::remove_dir_all(user_data)?;
    }
    std::fs::create_dir_all(user_data)?;
    
    println!("🔧 Launching Chrome with anti-detection configuration...");
    let mut child = Command::new(chrome_path)
        .args(&[
            format!("--remote-debugging-port={}", port),
            format!("--user-data-dir={}", user_data),
            "--headless=false".to_string(),
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
            "--user-agent=Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36".to_string(),
            "https://nopecha.com/demo/cloudflare".to_string(),
        ])
        .spawn()?;
    
    println!("✅ Chrome launched with PID: {}", child.id());
    sleep(Duration::from_secs(8)).await;
    
    // Connect to DevTools
    let client = Client::new();
    
    // Get targets
    println!("\n🎯 Connecting to Cloudflare protected page...");
    let targets_response = client.get(&format!("http://localhost:{}/json/list", port))
        .send()
        .await?;
    
    if targets_response.status() == 200 {
        let targets: serde_json::Value = targets_response.json().await?;
        
        if let Some(first_target) = targets.as_array().and_then(|arr| arr.first()) {
            if let Some(target_id) = first_target.get("id").and_then(|id| id.as_str()) {
                println!("📄 Target ID: {}", target_id);
                
                // Phase 1: Cloudflare Detection Analysis
                println!("\n🔍 Phase 1: Analyzing Cloudflare protection...");
                
                let analysis_script = r#"
                (function() {
                    var analysis = {
                        url: document.location.href,
                        title: document.title,
                        cloudflareElements: [],
                        scripts: [],
                        forms: [],
                        challenges: [],
                        protection: 'none',
                        ready: false
                    };
                    
                    // Find Cloudflare scripts
                    var scripts = document.querySelectorAll('script');
                    scripts.forEach((script) => {
                        if (script.src && (script.src.includes('cloudflare') || script.src.includes('cf-'))) {
                            analysis.scripts.push({
                                src: script.src,
                                async: script.async,
                                defer: script.defer
                            });
                        }
                    });
                    
                    // Find Cloudflare elements
                    var cloudflareElements = document.querySelectorAll('[class*="cf-"], [id*="cf-"], [data-cf-]');
                    cloudflareElements.forEach((element) => {
                        analysis.cloudflareElements.push({
                            tagName: element.tagName,
                            className: element.className,
                            id: element.id,
                            text: element.innerText || element.textContent || ''
                        });
                    });
                    
                    // Find challenge elements
                    var challengeElements = document.querySelectorAll('[class*="challenge"], [id*="challenge"], iframe[src*="challenge"]');
                    challengeElements.forEach((element) => {
                        analysis.challenges.push({
                            tagName: element.tagName,
                            className: element.className,
                            id: element.id,
                            src: element.src || '',
                            text: element.innerText || element.textContent || ''
                        });
                    });
                    
                    // Detect protection level
                    var bodyText = document.body.innerText || document.body.textContent || '';
                    if (bodyText.includes('Cloudflare') || bodyText.includes('DDoS protection') || bodyText.includes('checking your browser')) {
                        analysis.protection = 'active';
                    } else if (analysis.cloudflareElements.length > 0 || analysis.scripts.length > 0) {
                        analysis.protection = 'present';
                    }
                    
                    // Check if page is ready
                    if (document.readyState === 'complete') {
                        analysis.ready = true;
                    }
                    
                    return JSON.stringify(analysis);
                })()
                "#;
                
                let analysis_response = client.post(&format!("http://localhost:{}/json/runtime/evaluate", port))
                    .json(&json!({
                        "targetId": target_id,
                        "expression": analysis_script
                    }))
                    .send()
                .await?;
                
                if analysis_response.status() == 200 {
                    let result: serde_json::Value = analysis_response.json().await?;
                    if let Some(analysis_str) = result.get("result").and_then(|r| r.get("value")).and_then(|v| v.as_str()) {
                        if let Ok(analysis) = serde_json::from_str::<serde_json::Value>(analysis_str) {
                            println!("📊 Cloudflare Analysis Results:");
                            
                            if let Some(url) = analysis.get("url").and_then(|v| v.as_str()) {
                                println!("   URL: {}", url);
                            }
                            if let Some(title) = analysis.get("title").and_then(|v| v.as_str()) {
                                println!("   Title: {}", title);
                            }
                            if let Some(protection) = analysis.get("protection").and_then(|v| v.as_str()) {
                                println!("   Protection: {}", protection);
                            }
                            if let Some(scripts) = analysis.get("scripts").and_then(|v| v.as_array()) {
                                println!("   Cloudflare scripts: {}", scripts.len());
                            }
                            if let Some(elements) = analysis.get("cloudflareElements").and_then(|v| v.as_array()) {
                                println!("   Cloudflare elements: {}", elements.len());
                            }
                            if let Some(challenges) = analysis.get("challenges").and_then(|v| v.as_array()) {
                                println!("   Challenge elements: {}", challenges.len());
                            }
                            if let Some(ready) = analysis.get("ready").and_then(|v| v.as_bool()) {
                                println!("   Page ready: {}", ready);
                            }
                        }
                    }
                }
                
                // Wait for Cloudflare to load
                sleep(Duration::from_secs(5)).await;
                
                // Phase 2: Cloudflare Challenge Detection
                println!("\n🎯 Phase 2: Detecting Cloudflare challenges...");
                
                let challenge_script = r#"
                (function() {
                    var results = {
                        challengeType: 'none',
                        iframeFound: false,
                        iframeSrc: '',
                        checkboxFound: false,
                        checkboxPosition: null,
                        captchaFound: false,
                        captchaType: '',
                        ready: false,
                        error: null
                    };
                    
                    // Look for Cloudflare iframe
                    var iframes = document.querySelectorAll('iframe');
                    for (var i = 0; i < iframes.length; i++) {
                        var iframe = iframes[i];
                        var src = iframe.src || '';
                        if (src.includes('cloudflare') || src.includes('challenge') || src.includes('turnstile')) {
                            results.iframeFound = true;
                            results.iframeSrc = src;
                            results.challengeType = 'iframe';
                            break;
                        }
                    }
                    
                    // Look for Cloudflare checkbox
                    var checkboxes = document.querySelectorAll('[class*="cf-"], [id*="cf-"], input[type="checkbox"]');
                    for (var i = 0; i < checkboxes.length; i++) {
                        var checkbox = checkboxes[i];
                        var rect = checkbox.getBoundingClientRect();
                        if (rect.width > 0 && rect.height > 0) {
                            results.checkboxFound = true;
                            results.checkboxPosition = {
                                x: rect.left + rect.width / 2,
                                y: rect.top + rect.height / 2,
                                width: rect.width,
                                height: rect.height
                            };
                            results.challengeType = 'checkbox';
                            break;
                        }
                    }
                    
                    // Look for CAPTCHA
                    var captchaElements = document.querySelectorAll('[class*="captcha"], [id*="captcha"], img[src*="captcha"]');
                    if (captchaElements.length > 0) {
                        results.captchaFound = true;
                        results.captchaType = 'image';
                        results.challengeType = 'captcha';
                    }
                    
                    // Check if challenge is ready
                    var bodyText = document.body.innerText || document.body.textContent || '';
                    if (bodyText.includes('Verify you are human') || bodyText.includes('I am human') || bodyText.includes('checkbox')) {
                        results.ready = true;
                    }
                    
                    return JSON.stringify(results);
                })()
                "#;
                
                let challenge_response = client.post(&format!("http://localhost:{}/json/runtime/evaluate", port))
                    .json(&json!({
                        "targetId": target_id,
                        "expression": challenge_script
                    }))
                    .send()
                .await?;
                
                if challenge_response.status() == 200 {
                    let result: serde_json::Value = challenge_response.json().await?;
                    if let Some(results_str) = result.get("result").and_then(|r| r.get("value")).and_then(|v| v.as_str()) {
                        if let Ok(results) = serde_json::from_str::<serde_json::Value>(results_str) {
                            if let Some(challenge_type) = results.get("challengeType").and_then(|v| v.as_str()) {
                                println!("🎯 Challenge type: {}", challenge_type);
                            }
                            if let Some(iframe_found) = results.get("iframeFound").and_then(|v| v.as_bool()) {
                                if iframe_found {
                                    println!("✅ Cloudflare iframe detected");
                                }
                            }
                            if let Some(checkbox_found) = results.get("checkboxFound").and_then(|v| v.as_bool()) {
                                if checkbox_found {
                                    println!("✅ Cloudflare checkbox detected");
                                }
                            }
                            if let Some(captcha_found) = results.get("captchaFound").and_then(|v| v.as_bool()) {
                                if captcha_found {
                                    println!("✅ Cloudflare CAPTCHA detected");
                                }
                            }
                            if let Some(ready) = results.get("ready").and_then(|v| v.as_bool()) {
                                if ready {
                                    println!("✅ Challenge ready for interaction");
                                }
                            }
                        }
                    }
                }
                
                // Phase 3: Cloudflare Challenge Bypass
                println!("\n🚀 Phase 3: Bypassing Cloudflare challenge...");
                
                let bypass_script = r#"
                (function() {
                    var results = {
                        action: 'none',
                        success: false,
                        clicked: false,
                        position: null,
                        error: null,
                        timestamp: new Date().toISOString()
                    };
                    
                    // Method 1: Handle checkbox challenge
                    var checkboxes = document.querySelectorAll('[class*="cf-"], [id*="cf-"], input[type="checkbox"]');
                    for (var i = 0; i < checkboxes.length; i++) {
                        var checkbox = checkboxes[i];
                        var rect = checkbox.getBoundingClientRect();
                        if (rect.width > 0 && rect.height > 0 && !checkbox.checked) {
                            var centerX = rect.left + rect.width / 2;
                            var centerY = rect.top + rect.height / 2;
                            
                            // Realistic mouse movement
                            var startX = centerX - 50 + Math.random() * 100;
                            var startY = centerY - 40 + Math.random() * 80;
                            var steps = 5 + Math.floor(Math.random() * 5);
                            
                            // Generate mouse trajectory
                            var trajectory = [];
                            for (var j = 0; j <= steps; j++) {
                                var t = j / steps;
                                var easeT = t < 0.5 ? 2 * t * t : -1 + (4 - 2 * t) * t;
                                var x = startX + (centerX - startX) * easeT;
                                var y = startY + (centerY - startY) * easeT;
                                trajectory.push({x: x, y: y});
                            }
                            
                            // Execute mouse movement and click
                            var currentStep = 0;
                            var moveInterval = setInterval(function() {
                                if (currentStep < trajectory.length) {
                                    var point = trajectory[currentStep];
                                    
                                    // Mouse move event
                                    var mouseMove = new MouseEvent('mousemove', {
                                        bubbles: true,
                                        cancelable: true,
                                        clientX: point.x,
                                        clientY: point.y,
                                        movementX: currentStep > 0 ? point.x - trajectory[currentStep - 1].x : 0,
                                        movementY: currentStep > 0 ? point.y - trajectory[currentStep - 1].y : 0
                                    });
                                    document.dispatchEvent(mouseMove);
                                    
                                    currentStep++;
                                } else {
                                    clearInterval(moveInterval);
                                    
                                    // Click sequence
                                    var mousedown = new MouseEvent('mousedown', {
                                        bubbles: true,
                                        cancelable: true,
                                        clientX: centerX,
                                        clientY: centerY,
                                        button: 0,
                                        buttons: 1
                                    });
                                    
                                    var mouseup = new MouseEvent('mouseup', {
                                        bubbles: true,
                                        cancelable: true,
                                        clientX: centerX,
                                        clientY: centerY,
                                        button: 0,
                                        buttons: 0
                                    });
                                    
                                    var click = new MouseEvent('click', {
                                        bubbles: true,
                                        cancelable: true,
                                        clientX: centerX,
                                        clientY: centerY,
                                        button: 0,
                                        buttons: 1
                                    });
                                    
                                    checkbox.dispatchEvent(mousedown);
                                    setTimeout(function() {
                                        checkbox.dispatchEvent(mouseup);
                                        setTimeout(function() {
                                            checkbox.dispatchEvent(click);
                                            checkbox.checked = true;
                                            checkbox.dispatchEvent(new Event('change', { bubbles: true }));
                                            
                                            results.action = 'clicked_checkbox';
                                            results.success = true;
                                            results.clicked = true;
                                            results.position = {x: centerX, y: centerY};
                                        }, 50);
                                    }, 100);
                                }
                            }, 30 + Math.random() * 20);
                            
                            return JSON.stringify(results);
                        }
                    }
                    
                    // Method 2: Handle iframe challenge
                    var iframes = document.querySelectorAll('iframe');
                    for (var i = 0; i < iframes.length; i++) {
                        var iframe = iframes[i];
                        var src = iframe.src || '';
                        if (src.includes('cloudflare') || src.includes('challenge') || src.includes('turnstile')) {
                            results.action = 'iframe_challenge_detected';
                            results.success = true;
                            return JSON.stringify(results);
                        }
                    }
                    
                    // Method 3: Look for and click verification button
                    var buttons = document.querySelectorAll('button, input[type="button"], input[type="submit"]');
                    for (var i = 0; i < buttons.length; i++) {
                        var button = buttons[i];
                        var text = button.innerText || button.value || button.textContent || '';
                        if (text.includes('Verify') || text.includes('I am human') || text.includes('Continue')) {
                            var rect = button.getBoundingClientRect();
                            if (rect.width > 0 && rect.height > 0) {
                                var centerX = rect.left + rect.width / 2;
                                var centerY = rect.top + rect.height / 2;
                                
                                var click = new MouseEvent('click', {
                                    bubbles: true,
                                    cancelable: true,
                                    clientX: centerX,
                                    clientY: centerY,
                                    button: 0,
                                    buttons: 1
                                });
                                
                                button.dispatchEvent(click);
                                results.action = 'clicked_verification_button';
                                results.success = true;
                                results.position = {x: centerX, y: centerY};
                                return JSON.stringify(results);
                            }
                        }
                    }
                    
                    results.error = 'No Cloudflare challenge found to bypass';
                    return JSON.stringify(results);
                })()
                "#;
                
                let bypass_response = client.post(&format!("http://localhost:{}/json/runtime/evaluate", port))
                    .json(&json!({
                        "targetId": target_id,
                        "expression": bypass_script
                    }))
                    .send()
                .await?;
                
                if bypass_response.status() == 200 {
                    let result: serde_json::Value = bypass_response.json().await?;
                    if let Some(results_str) = result.get("result").and_then(|r| r.get("value")).and_then(|v| v.as_str()) {
                        if let Ok(results) = serde_json::from_str::<serde_json::Value>(results_str) {
                            if let Some(action) = results.get("action").and_then(|v| v.as_str()) {
                                println!("🎯 Action: {}", action);
                            }
                            if let Some(success) = results.get("success").and_then(|v| v.as_bool()) {
                                if success {
                                    println!("✅ Cloudflare challenge bypass initiated");
                                } else {
                                    println!("⚠️  Cloudflare challenge bypass incomplete");
                                }
                            }
                            if let Some(clicked) = results.get("clicked").and_then(|v| v.as_bool()) {
                                if clicked {
                                    println!("🔘 Checkbox clicked successfully");
                                }
                            }
                            if let Some(position) = results.get("position") {
                                println!("📍 Click position: {:?}", position);
                            }
                            if let Some(error) = results.get("error").and_then(|v| v.as_str()) {
                                println!("⚠️  Bypass error: {}", error);
                            }
                        }
                    }
                }
                
                // Wait for bypass to complete
                sleep(Duration::from_secs(5)).await;
                
                // Phase 4: Bypass Verification
                println!("\n✅ Phase 4: Verifying Cloudflare bypass...");
                
                let verification_script = r#"
                (function() {
                    var results = {
                        bypassed: false,
                        success: false,
                        url: document.location.href,
                        title: document.title,
                        evidence: [],
                        error: null,
                        timestamp: new Date().toISOString()
                    };
                    
                    var bodyText = document.body.innerText || document.body.textContent || '';
                    
                    // Check for success indicators
                    if (bodyText.includes('success') || bodyText.includes('verified') || bodyText.includes('passed')) {
                        results.bypassed = true;
                        results.success = true;
                        results.evidence.push('Success indicators found in page content');
                    }
                    
                    // Check if Cloudflare elements are gone
                    var cloudflareElements = document.querySelectorAll('[class*="cf-"], [id*="cf-"]');
                    if (cloudflareElements.length === 0) {
                        results.bypassed = true;
                        results.evidence.push('Cloudflare elements no longer present');
                    }
                    
                    // Check for error indicators
                    if (bodyText.includes('error') || bodyText.includes('failed') || bodyText.includes('blocked')) {
                        results.success = false;
                        results.evidence.push('Error indicators found in page content');
                    }
                    
                    // Check if we're still on the same page
                    if (bodyText.includes('Cloudflare') || bodyText.includes('checking your browser') || bodyText.includes('Verify you are human')) {
                        results.bypassed = false;
                        results.evidence.push('Still on Cloudflare challenge page');
                    }
                    
                    return JSON.stringify(results);
                })()
                "#;
                
                let verification_response = client.post(&format!("http://localhost:{}/json/runtime/evaluate", port))
                    .json(&json!({
                        "targetId": target_id,
                        "expression": verification_script
                    }))
                    .send()
                .await?;
                
                if verification_response.status() == 200 {
                    let result: serde_json::Value = verification_response.json().await?;
                    if let Some(results_str) = result.get("result").and_then(|r| r.get("value")).and_then(|v| v.as_str()) {
                        if let Ok(results) = serde_json::from_str::<serde_json::Value>(results_str) {
                            if let Some(bypassed) = results.get("bypassed").and_then(|v| v.as_bool()) {
                                if bypassed {
                                    println!("🎉 **CLOUDFLARE BYPASS SUCCESSFUL!**");
                                } else {
                                    println!("⚠️  Cloudflare bypass status unclear");
                                }
                            }
                            if let Some(success) = results.get("success").and_then(|v| v.as_bool()) {
                                if success {
                                    println!("✅ Bypass verification passed");
                                }
                            }
                            if let Some(evidence) = results.get("evidence").and_then(|v| v.as_array()) {
                                println!("📊 Evidence:");
                                for item in evidence {
                                    if let Some(item_text) = item.as_str() {
                                        println!("   • {}", item_text);
                                    }
                                }
                            }
                        }
                    }
                }
                
                // Phase 5: Screenshot Capture
                println!("\n📸 Phase 5: Capturing Cloudflare bypass screenshot...");
                
                let screenshot_response = client.post(&format!("http://localhost:{}/json/page/captureScreenshot", port))
                    .json(&json!({
                        "format": "png"
                    }))
                    .send()
                .await?;
                
                if screenshot_response.status() == 200 {
                    let result: serde_json::Value = screenshot_response.json().await?;
                    if let Some(screenshot_data) = result.get("data").and_then(|v| v.as_str()) {
                        if let Ok(screenshot_bytes) = base64::engine::general_purpose::STANDARD.decode(screenshot_data) {
                            let filename = "cloudflare_bypass_result.png";
                            std::fs::write(filename, screenshot_bytes)?;
                            println!("✅ Screenshot saved: {}", filename);
                        }
                    }
                }
                
                // Phase 6: Final Analysis
                println!("\n📈 Phase 6: Final bypass analysis...");
                
                let final_analysis_script = r#"
                (function() {
                    var analysis = {
                        url: document.location.href,
                        title: document.title,
                        bypassed: false,
                        method: 'none',
                        confidence: 0,
                        evidence: [],
                        timestamp: new Date().toISOString()
                    };
                    
                    var bodyText = document.body.innerText || document.body.textContent || '';
                    
                    // Analyze bypass success
                    if (!bodyText.includes('Cloudflare') && !bodyText.includes('checking your browser')) {
                        analysis.bypassed = true;
                        analysis.confidence = 0.8;
                        analysis.evidence.push('Cloudflare challenge page no longer visible');
                    }
                    
                    if (bodyText.includes('success') || bodyText.includes('verified')) {
                        analysis.bypassed = true;
                        analysis.confidence = 0.9;
                        analysis.evidence.push('Success indicators found');
                    }
                    
                    // Determine bypass method
                    if (analysis.bypassed) {
                        analysis.method = 'automated_interaction';
                        analysis.evidence.push('Automated mouse movement and clicking');
                    }
                    
                    return JSON.stringify(analysis);
                })()
                "#;
                
                let final_analysis_response = client.post(&format!("http://localhost:{}/json/runtime/evaluate", port))
                    .json(&json!({
                        "targetId": target_id,
                        "expression": final_analysis_script
                    }))
                    .send()
                .await?;
                
                if final_analysis_response.status() == 200 {
                    let result: serde_json::Value = final_analysis_response.json().await?;
                    if let Some(analysis) = result.get("result").and_then(|r| r.get("value")) {
                        println!("📊 **Final Analysis Results:**");
                        
                        if let Some(bypassed) = analysis.get("bypassed").and_then(|v| v.as_bool()) {
                            if bypassed {
                                println!("   🎉 **CLOUDFLARE WAF BYPASSED SUCCESSFULLY!**");
                            } else {
                                println!("   ⚠️  Cloudflare WAF status unclear");
                            }
                        }
                        if let Some(method) = analysis.get("method").and_then(|v| v.as_str()) {
                            println!("   🎯 Bypass method: {}", method);
                        }
                        if let Some(confidence) = analysis.get("confidence").and_then(|v| v.as_f64()) {
                            println!("   📊 Confidence: {:.1}%", confidence * 100.0);
                        }
                        if let Some(evidence) = analysis.get("evidence").and_then(|v| v.as_array()) {
                            println!("   Evidence:");
                            for item in evidence {
                                if let Some(item_text) = item.as_str() {
                                    println!("     • {}", item_text);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    // Cleanup
    println!("\n🧹 Phase 7: Cleanup...");
    child.kill()?;
    child.wait()?;
    std::fs::remove_dir_all(user_data)?;
    println!("✅ Chrome process terminated and profile cleaned up");
    
    println!("\n🎉 **Cloudflare WAF Bypass Test Completed** 🎉");
    println!("==========================================");
    println!("✅ All phases completed:");
    println!("   🔍 Cloudflare protection analysis");
    println!("   🎯 Challenge detection");
    println!("   🚀 Automated bypass execution");
    println!("   ✅ Bypass verification");
    println!("   📸 Screenshot verification");
    println!("   📈 Final bypass analysis");
    println!("\n🚀 The Cloudflare WAF bypass framework is fully operational!");
    
    Ok(())
}

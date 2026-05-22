use hca::create_browser;
use anyhow::Result;
use std::fs;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    println!("🔍 **BrowserScan Real Bot Detection Test** 🔍");
    println!("============================================");
    
    // Create docs directory
    fs::create_dir_all("docs/screenshots")?;
    
    // Create browser instance
    let headless = true;
    let mut browser = ChromeBrowser::new(headless, 9222).await?;
    
    // Navigate to BrowserScan bot detection page
    println!("\n🌐 Navigating to BrowserScan bot detection...");
    browser.navigate_to("https://www.browserscan.net/bot-detection").await?;
    
    // Wait for page to load
    browser.wait_for_page_load(10000).await?;
    
    // Apply bot bypass techniques
    println!("\n🤖 Applying bot detection bypass...");
    browser.apply_bot_bypass().await?;
    
    // Wait for the test to run
    println!("\n⏳ Waiting for BrowserScan test to complete...");
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    
    // Check if test results are visible
    let check_script = r#"
    (function() {
        // Look for test results
        const results = {
            hasResults: false,
            score: null,
            status: null,
            details: []
        };
        
        // Check for various result elements
        const scoreElements = document.querySelectorAll('[class*="score"], [class*="result"], [data-score]');
        scoreElements.forEach(el => {
            const text = el.textContent || el.innerText;
            if (text && text.match(/[0-9.]+/)) {
                results.score = text.match(/[0-9.]+/)?.[0];
                results.hasResults = true;
            }
        });
        
        // Check for status indicators
        const statusElements = document.querySelectorAll('[class*="status"], [class*="human"], [class*="bot"]');
        statusElements.forEach(el => {
            const text = el.textContent || el.innerText;
            if (text && (text.includes('Human') || text.includes('Bot'))) {
                results.status = text;
                results.hasResults = true;
            }
        });
        
        // Look for result sections
        const resultSections = document.querySelectorAll('[class*="result"], [class*="test"], [class*="detection"]');
        resultSections.forEach(section => {
            const text = section.textContent || section.innerText;
            if (text && text.length > 10) {
                results.details.push(text.substring(0, 100));
            }
        });
        
        return JSON.stringify(results);
    })();
    "#;
    
    if let Ok(results_str) = browser.execute_script(check_script).await {
        if let Ok(results) = serde_json::from_str::<serde_json::Value>(&results_str) {
            println!("📊 Test Results Analysis:");
            if let Some(has_results) = results.get("hasResults").and_then(|v| v.as_bool()) {
                if has_results {
                    if let Some(score) = results.get("score").and_then(|v| v.as_str()) {
                        println!("   Score: {}", score);
                    }
                    if let Some(status) = results.get("status").and_then(|v| v.as_str()) {
                        println!("   Status: {}", status);
                    }
                } else {
                    println!("   ⏳ Test still running or results not yet visible");
                }
            }
        }
    }
    
    // Take screenshot of the results
    println!("\n📸 Capturing BrowserScan test results...");
    browser.take_screenshot("docs/screenshots/browserscan_bot_detection.png").await?;
    
    // Try to scroll down to see more results
    println!("\n📜 Scrolling to see more results...");
    let scroll_script = r#"
    window.scrollTo(0, document.body.scrollHeight / 2);
    return "Scrolled to middle";
    "#;
    browser.execute_script(scroll_script).await?;
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    
    // Take another screenshot
    browser.take_screenshot("docs/screenshots/browserscan_bot_detection_scrolled.png").await?;
    
    // Close browser
    browser.quit().await?;
    
    println!("\n🎉 **BrowserScan Test Completed** 🎉");
    println!("===================================");
    println!("✅ Screenshots saved:");
    println!("   📸 docs/screenshots/browserscan_bot_detection.png");
    println!("   📸 docs/screenshots/browserscan_bot_detection_scrolled.png");
    
    Ok(())
}

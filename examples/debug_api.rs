use hca::ChromeBrowser;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let mut browser = ChromeBrowser::new(true).await?;
    println!("Browser started");
    browser.navigate_to_fast("https://example.com").await?;
    println!("Navigated");
    
    // Wait for basic structure
    let _ = browser.wait_for_page_load(15000).await;
    
    let site_key = "6LfB5_IbAAAAAMCtsjEHEHKqcB9iQocwwxTiihJu";
    let script = format!(r#"
        new Promise((resolve, reject) => {{
            try {{
                const siteKey = '{}';
                if (typeof window.grecaptcha === 'undefined') {{
                    const script = document.createElement('script');
                    script.src = 'https://www.google.com/recaptcha/api.js?render=' + siteKey;
                    script.onload = () => {{
                        window.grecaptcha.ready(() => {{
                            window.grecaptcha.execute(siteKey, {{action: 'submit'}}).then(resolve).catch(e => reject(e.toString()));
                        }});
                    }};
                    script.onerror = () => reject('Failed to load recaptcha script');
                    document.head.appendChild(script);
                }} else {{
                    window.grecaptcha.ready(() => {{
                        window.grecaptcha.execute(siteKey, {{action: 'submit'}}).then(resolve).catch(e => reject(e.toString()));
                    }});
                }}
            }} catch(e) {{
                reject(e.toString());
            }}
        }})
    "#, site_key);
    
    println!("Executing script directly via CDP...");
    
    // We can't access `send_message` easily, let's just use `execute_script` but add a simpler script to test.
    // Wait, let's just inject the script via `execute_script` and see if there's a console log we can fetch.
    let debug_script = format!(r#"
        {}
        .then(res => "SUCCESS: " + res)
        .catch(err => "ERROR: " + err)
    "#, script);
    
    match browser.execute_script(&debug_script).await {
        Ok(res) => println!("Result: {}", res),
        Err(e) => println!("Error: {}", e),
    }
    
    Ok(())
}

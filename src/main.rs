use clap::{Arg, Command};
use std::time::Duration;
use tracing::{info, warn, error};

use hca::{ChromeBrowser, WebScraper};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let matches = Command::new("HCA")
        .version("0.1.0")
        .about("A Rust-based headless Chrome automation tool")
        .arg(
            Arg::new("url")
                .short('u')
                .long("url")
                .value_name("URL")
                .help("Target URL to scrape or automate")
                .required(true),
        )
        .arg(
            Arg::new("screenshot")
                .short('s')
                .long("screenshot")
                .value_name("FILE")
                .help("Take screenshot and save to file"),
        )
        .arg(
            Arg::new("headless")
                .short('h')
                .long("headless")
                .help("Run in headless mode")
                .action(clap::ArgAction::SetTrue),
        )
        .get_matches();

    let url = matches.get_one::<String>("url").unwrap();
    let headless = matches.get_flag("headless");

    info!("Starting HCA with URL: {}", url);

    // Create browser instance
    let mut browser = ChromeBrowser::new(headless).await?;
    info!("Browser created successfully");

    // Navigate to URL
    browser.navigate_to(url).await?;
    info!("Navigated to: {}", url);

    // Wait for page to load
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Scrape content
    let mut scraper = WebScraper::new(&mut browser);
    match scraper.scrape_page_content().await {
        Ok(content) => {
            info!("Page Title: {}", content.title);
            info!("Links found: {}", content.links.len());
            info!("Images found: {}", content.images.len());
            info!("Forms found: {}", content.forms.len());
        }
        Err(e) => warn!("Failed to scrape content: {}", e),
    }

    // Take screenshot if requested
    if let Some(filename) = matches.get_one::<String>("screenshot") {
        info!("Taking screenshot: {}", filename);
        if let Err(e) = browser.take_screenshot(filename).await {
            error!("Failed to take screenshot: {}", e);
        } else {
            info!("Screenshot saved successfully");
        }
    }

    // Cleanup
    browser.quit().await?;
    info!("Browser closed successfully");

    Ok(())
}

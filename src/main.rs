use color_eyre::{eyre::eyre, Result};
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use indicatif::{ProgressBar, ProgressStyle};
use futures_util::StreamExt;
use ratatui::{
    prelude::*,
    widgets::{List, ListItem, ListState, Paragraph},
    crossterm::{
        execute,
        event::{self, Event, KeyCode, KeyEventKind},
        terminal::{enable_raw_mode, disable_raw_mode, Clear, ClearType},
    },
};

const DEFAULT_PANEL_URL: &str = "https://mc.bloom.host";

#[derive(Debug, Deserialize)]
struct ListResponse<T> {
    data: Vec<Item<T>>,
    #[serde(default)]
    meta: Option<Meta>,
}

#[derive(Debug, Deserialize)]
struct Item<T> {
    attributes: T,
}

#[derive(Debug, Deserialize)]
struct Meta {
    pagination: Option<Pagination>,
}

#[derive(Debug, Deserialize)]
struct Pagination {
    current_page: u32,
    total_pages: u32,
}

#[derive(Debug, Deserialize)]
struct Server {
    identifier: String,
    uuid: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct BackupDownloadLinkResponse {
    attributes: Attributes,
}

#[derive(Debug, Deserialize)]
struct Attributes {
    url: String,
}

#[derive(Debug, Deserialize)]
struct Backup {
    uuid: String,
    name: String,
    created_at: String,
    #[serde(default)]
    bytes: u64,
    #[serde(default)]
    is_successful: Option<bool>,
    #[serde(default)]
    completed_at: Option<String>,
}

impl Backup {
    /// Only finished, successful backups can be downloaded.
    fn is_downloadable(&self) -> bool {
        self.completed_at.is_some() && self.is_successful != Some(false)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    if let Err(e) = run().await {
        eprintln!("Error: {:?}", e);
        println!("Press Enter to exit...");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
    }
    Ok(())
}


async fn run() -> Result<()> {
    color_eyre::install()?;

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin);

    // Environment variables allow unattended use (e.g. from a scheduled task).
    let api_key = match std::env::var("PANEL_API_KEY") {
        Ok(key) if !key.trim().is_empty() => key.trim().to_string(),
        _ => {
            println!("Enter your Pterodactyl client API key (Account > API Credentials, starts with ptlc_):");
            let mut api_key = String::new();
            reader.read_line(&mut api_key).await?;
            api_key.trim().to_string()
        }
    };
    if api_key.is_empty() {
        return Err(eyre!("No API key entered"));
    }

    let panel_url = match std::env::var("PANEL_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            println!("Enter your panel URL, e.g. panel.lagless.gg (or press Enter for default mc.bloom.host):");
            let mut panel_url = String::new();
            reader.read_line(&mut panel_url).await?;
            panel_url
        }
    };
    let panel_url = normalize_panel_url(&panel_url)?;

    execute!(std::io::stdout(), Clear(ClearType::All))?;

    // rustls is used instead of the OS TLS stack: Windows 10's SChannel has no
    // TLS 1.3, which panels such as panel.lagless.gg require.
    let client = Client::builder()
        .user_agent(concat!("ClumsyLoader/", env!("CARGO_PKG_VERSION")))
        .use_rustls_tls()
        .build()?;

    // Fetch servers
    let servers: Vec<Server> = fetch_all(&client, &format!("{}/api/client", panel_url), &api_key).await?;
    if servers.is_empty() {
        println!("No servers found for this API key on {}.", panel_url);
        return Ok(());
    }

    // Select a server from the UI
    let selected_server = select_from_list("Servers:", &servers, display_server)?;
    let selected_server_uuid = &servers[selected_server].uuid;
    let selected_server_short_uuid = &servers[selected_server].identifier;

    // Fetch backups for the selected server
    let backups: Vec<Backup> = fetch_all(
        &client,
        &format!("{}/api/client/servers/{}/backups", panel_url, selected_server_uuid),
        &api_key,
    )
    .await?
    .into_iter()
    .filter(Backup::is_downloadable)
    .collect();
    if backups.is_empty() {
        println!("No completed backups found for the selected server.");
        return Ok(());
    }

    execute!(std::io::stdout(), Clear(ClearType::All))?;

    // Select a backup from the UI (display name + date)
    let selected_backup = select_from_list("Backups:", &backups, display_backup)?;

    let backup_url = generate_backup_dl_link(&client, &panel_url, &api_key, selected_server_short_uuid, &backups[selected_backup].uuid).await?;

    download_backup(&client, &backup_url, &backups[selected_backup].uuid, backups[selected_backup].bytes).await?;

    Ok(())
}

/// Accepts "panel.example.com", "https://panel.example.com/", or even a URL
/// copied from the browser ("https://panel.example.com/server/abcd1234") and
/// returns just the origin, e.g. "https://panel.example.com".
fn normalize_panel_url(input: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(DEFAULT_PANEL_URL.to_string());
    }
    let with_scheme = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{}", input)
    };
    let url = Url::parse(&with_scheme).map_err(|e| eyre!("Invalid panel URL '{}': {}", input, e))?;
    let host = url.host_str().ok_or_else(|| eyre!("Invalid panel URL '{}': no host", input))?;
    Ok(match url.port() {
        Some(port) => format!("{}://{}:{}", url.scheme(), host, port),
        None => format!("{}://{}", url.scheme(), host),
    })
}

/// Sends an authenticated GET to the panel API and decodes the JSON body,
/// turning HTTP errors into readable messages instead of JSON decode errors.
async fn api_get<T: DeserializeOwned>(client: &Client, url: &str, api_key: &str) -> Result<T> {
    let response = client.get(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| eyre!("Could not reach {}: {}", url, error_chain(&e)))?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => "\nCheck that you used a *client* API key (Account > API Credentials, starts with ptlc_), not an application key, and that it hasn't expired or been restricted by IP.",
            404 => "\nCheck the panel URL - it should be the address you open the panel at in your browser.",
            429 => "\nThe panel is rate limiting requests; wait a minute and try again.",
            _ => "",
        };
        return Err(eyre!("{} returned HTTP {}: {}{}", url, status, excerpt(&body), hint));
    }

    serde_json::from_str(&body).map_err(|e| {
        eyre!(
            "{} did not return the expected Pterodactyl API response ({}). Is this a Pterodactyl panel URL?\nResponse: {}",
            url, e, excerpt(&body)
        )
    })
}

/// Fetches every page of a paginated Pterodactyl list endpoint.
async fn fetch_all<T: DeserializeOwned>(client: &Client, base_url: &str, api_key: &str) -> Result<Vec<T>> {
    let mut items = Vec::new();
    let mut page = 1;
    loop {
        let url = format!("{}?per_page=50&page={}", base_url, page);
        let response: ListResponse<T> = api_get(client, &url, api_key).await?;
        let got = response.data.len();
        items.extend(response.data.into_iter().map(|i| i.attributes));

        match response.meta.and_then(|m| m.pagination) {
            Some(p) if p.current_page < p.total_pages && got > 0 => page += 1,
            _ => break,
        }
    }
    Ok(items)
}

async fn generate_backup_dl_link(client: &Client, url: &str, api_key: &str, server_uuid: &str, backup_uuid: &str) -> Result<String> {
    let response: BackupDownloadLinkResponse = api_get(
        client,
        &format!("{}/api/client/servers/{}/backups/{}/download", url, server_uuid, backup_uuid),
        api_key,
    )
    .await?;
    Ok(response.attributes.url)
}

async fn download_backup(client: &Client, url: &str, backup_uuid: &str, backup_bytes: u64) -> Result<()> {
    let response = client.get(url)
        .send()
        .await
        .map_err(|e| eyre!("Could not reach the backup download server: {}", error_chain(&e)))?;

    let status = response.status();
    let is_html = response.headers().get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .map_or(false, |v| v.starts_with("text/html"));
    if !status.is_success() || is_html {
        let error_message = response.text().await?;
        return Err(eyre!("Failed to download backup (HTTP {}): {}", status, excerpt(&error_message)));
    }

    let mut downloaded = 0;
    let total_size = if backup_bytes > 0 {
        backup_bytes
    } else {
        response.content_length().unwrap_or(0)
    };

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("#>-"),
    );
    pb.set_position(downloaded);
    pb.reset_eta();

    let backup_file_name = format!("{}.tar.gz", backup_uuid);
    let mut backup_file = File::create(&backup_file_name).await?;

    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        downloaded += chunk.len() as u64;
        pb.set_position(downloaded);
        backup_file.write_all(&chunk).await?;
    }
    backup_file.flush().await?;

    pb.finish_with_message("Download complete");
    println!("Saved backup to {}", backup_file_name);
    Ok(())
}

/// Shortens a response body for error messages.
fn excerpt(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "(empty response)".to_string();
    }
    let mut short: String = body.chars().take(300).collect();
    if short.len() < body.len() {
        short.push_str("...");
    }
    short
}

/// Flattens an error and its sources into one line ("a: b: c").
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut message = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        message.push_str(": ");
        message.push_str(&s.to_string());
        source = s.source();
    }
    message
}

fn display_server(server: &Server) -> String {
    server.name.clone()
}

fn display_backup(backup: &Backup) -> String {
    format!("{} - {} ({})", backup.name, backup.created_at, format_size(backup.bytes))
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}

fn select_from_list<Item, DisplayFn>(title: &str, items: &[Item], display_fn: DisplayFn) -> Result<usize>
where
    DisplayFn: Fn(&Item) -> String,
{
    // Enable raw mode for immediate keypress handling
    enable_raw_mode()?;

    // Initialize the terminal
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;

    // Initialize the list state
    let mut state = ListState::default();
    state.select(Some(0));  // Select the first item by default

    loop {
        // Render the UI
        terminal.draw(|frame| {
            let area = frame.area();

            // Create a vertical layout with a title and a list
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints(
                    [
                        Constraint::Length(3),  // Title area
                        Constraint::Min(1),     // List area
                    ]
                    .as_ref(),
                )
                .split(area);

            let title_paragraph = Paragraph::new(title)
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                .alignment(Alignment::Center);
            frame.render_widget(title_paragraph, chunks[0]);

            let list_items: Vec<ListItem> = items
                .iter()
                .map(|item| ListItem::new(display_fn(item)))
                .collect();

            let list = List::new(list_items)
                .highlight_symbol("> ")
                .highlight_style(Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD));

            frame.render_stateful_widget(list, chunks[1], &mut state);
        })?;

        if let Event::Key(key) = event::read()? {
            // Filter out KeyEventKind::Release events for Windows - https://ratatui.rs/faq/#why-am-i-getting-duplicate-key-events-on-windows
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        disable_raw_mode()?;
                        return Err(color_eyre::eyre::eyre!("Selection aborted"));
                    }
                    KeyCode::Down => {
                        state.select_next();
                    }
                    KeyCode::Up => {
                        state.select_previous();
                    }
                    KeyCode::Enter => {
                        disable_raw_mode()?;
                        return Ok(state.selected().unwrap_or(0));
                    }
                    _ => {}
                }
            }
        }
    }
}

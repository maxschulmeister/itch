//! Generic transport for connections to online markdown platforms.
//!
//! Connector logic lives in the frontend (`src/connections/<kind>/`). This
//! module only stores connection settings and tokens, and sends authenticated
//! requests to the base URL of a connection. The token never enters the webview.

mod secrets;

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::http::{Request, Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeContext, UriSchemeResponder};

/// URI scheme that serves connection images with authentication.
pub const ASSET_PROTOCOL: &str = "itch-conn";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub base_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionResponse {
    pub status: u16,
    pub body: serde_json::Value,
    /// Seconds to wait before the next request, from the `Retry-After` header.
    pub retry_after: Option<f64>,
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        reqwest::Client::builder()
            .user_agent(concat!("itch/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("HTTP client")
    })
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("connections.json"))
}

fn load_configs(app: &AppHandle) -> Result<Vec<ConnectionConfig>, String> {
    match std::fs::read_to_string(config_path(app)?) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn save_configs(app: &AppHandle, configs: &[ConnectionConfig]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(configs).map_err(|e| e.to_string())?;
    std::fs::write(config_path(app)?, text).map_err(|e| e.to_string())
}

/// True when a connection exists. The main window then shows the connections as a library.
pub fn has_connections(app: &AppHandle) -> bool {
    load_configs(app).is_ok_and(|configs| !configs.is_empty())
}

fn find_config(app: &AppHandle, id: &str) -> Result<ConnectionConfig, String> {
    load_configs(app)?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| "Connection not found".to_string())
}

/// Connection ids come from `connections_add`: the kind, a dash, and hex digits.
/// The check keeps an id from naming another path when joined to a folder.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn cache_dir(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    if !is_valid_id(id) {
        return Err("Invalid connection id".to_string());
    }
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("connections").join(id))
}

/// Accept only HTTPS base URLs, or HTTP for a local server.
fn normalize_base_url(raw: &str) -> Result<String, String> {
    let url = url::Url::parse(raw.trim()).map_err(|_| "Enter a valid URL".to_string())?;
    let host = url.host_str().unwrap_or_default();
    let is_local = host == "localhost" || host == "127.0.0.1";
    if url.scheme() != "https" && !(url.scheme() == "http" && is_local) {
        return Err("The URL must start with https://".to_string());
    }
    Ok(url.origin().ascii_serialization())
}

/// Resolve a URL from a document. Returns the absolute URL and whether it is on
/// the connection server (and so gets the token).
fn resolve_url(base_url: &str, raw: &str) -> Result<(String, bool), String> {
    if raw.starts_with('/') && !raw.starts_with("//") {
        return Ok((format!("{base_url}{raw}"), true));
    }
    let url = url::Url::parse(raw).map_err(|_| "Invalid URL".to_string())?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err("Unsupported URL scheme".to_string());
    }
    let same_origin = url.origin().ascii_serialization() == base_url;
    Ok((url.to_string(), same_origin))
}

#[tauri::command]
pub fn connections_list(app: AppHandle) -> Result<Vec<ConnectionConfig>, String> {
    load_configs(&app)
}

#[tauri::command]
pub fn connections_add(
    app: AppHandle,
    kind: String,
    name: String,
    base_url: String,
    token: String,
) -> Result<ConnectionConfig, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("Enter an API key".to_string());
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let config = ConnectionConfig {
        id: format!("{kind}-{nanos:x}"),
        kind,
        name,
        base_url: normalize_base_url(&base_url)?,
    };
    secrets::set(&app, &config.id, token)?;
    let mut configs = load_configs(&app)?;
    configs.push(config.clone());
    save_configs(&app, &configs)?;
    Ok(config)
}

#[tauri::command]
pub fn connections_rename(app: AppHandle, id: String, name: String) -> Result<(), String> {
    let mut configs = load_configs(&app)?;
    let config = configs
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or_else(|| "Connection not found".to_string())?;
    config.name = name;
    save_configs(&app, &configs)
}

#[tauri::command]
pub fn connections_remove(app: AppHandle, id: String) -> Result<(), String> {
    find_config(&app, &id)?;
    let cache = cache_dir(&app, &id)?;
    let configs: Vec<_> = load_configs(&app)?
        .into_iter()
        .filter(|c| c.id != id)
        .collect();
    save_configs(&app, &configs)?;
    secrets::delete(&app, &id)?;
    let _ = std::fs::remove_dir_all(cache);
    Ok(())
}

/// POST a JSON body to `{base_url}{path}` with the connection token.
#[tauri::command]
pub async fn connection_request(
    app: AppHandle,
    id: String,
    path: String,
    body: serde_json::Value,
) -> Result<ConnectionResponse, String> {
    if !path.starts_with('/') || path.starts_with("//") {
        return Err("The request path must start with /".to_string());
    }
    let config = find_config(&app, &id)?;
    let token = secrets::get(&app, &id)?;
    let response = client()
        .post(format!("{}{}", config.base_url, path))
        .bearer_auth(token)
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok());
    let body = response.json().await.unwrap_or(serde_json::Value::Null);
    Ok(ConnectionResponse {
        status,
        body,
        retry_after,
    })
}

/// Upload a file as multipart form data. The token is sent only when the
/// upload URL is on the connection server.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn connection_upload(
    app: AppHandle,
    id: String,
    url: String,
    fields: Vec<(String, String)>,
    file_name: String,
    content_type: String,
    data_base64: String,
) -> Result<u16, String> {
    let config = find_config(&app, &id)?;
    let (url, same_origin) = resolve_url(&config.base_url, &url)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64)
        .map_err(|e| e.to_string())?;
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str(&content_type)
        .map_err(|e| e.to_string())?;
    let mut form = reqwest::multipart::Form::new();
    for (key, value) in fields {
        form = form.text(key, value);
    }
    let mut request = client().post(url).multipart(form.part("file", part));
    if same_origin {
        request = request.bearer_auth(secrets::get(&app, &id)?);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    Ok(response.status().as_u16())
}

/// Fetch a connection image, from the cache when possible.
async fn fetch_asset(app: &AppHandle, id: &str, raw_url: &str) -> Result<(Vec<u8>, String), String> {
    let config = find_config(app, id)?;
    let (url, same_origin) = resolve_url(&config.base_url, raw_url)?;

    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let dir = cache_dir(app, id)?;
    let file = dir.join(format!("{:016x}", hasher.finish()));
    let type_file = file.with_extension("type");
    if let (Ok(bytes), Ok(mime)) = (std::fs::read(&file), std::fs::read_to_string(&type_file)) {
        return Ok((bytes, mime));
    }

    let mut request = client().get(&url);
    if same_origin {
        request = request.bearer_auth(secrets::get(app, id)?);
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = response.bytes().await.map_err(|e| e.to_string())?.to_vec();
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(&file, &bytes);
        let _ = std::fs::write(&type_file, &mime);
    }
    Ok((bytes, mime))
}

/// Handler for `itch-conn://localhost/<encoded "connectionId/url">`.
pub fn handle_asset_request(
    ctx: UriSchemeContext<'_, tauri::Wry>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    let path = request.uri().path().trim_start_matches('/').to_string();
    tauri::async_runtime::spawn(async move {
        let decoded = urlencoding::decode(&path)
            .map(|s| s.into_owned())
            .unwrap_or_default();
        let result = match decoded.split_once('/') {
            Some((id, url)) => fetch_asset(&app, id, url).await,
            None => Err("Invalid asset path".to_string()),
        };
        let response = match result {
            Ok((bytes, mime)) => Response::builder()
                .status(StatusCode::OK)
                .header("content-type", mime)
                .header("cache-control", "max-age=86400")
                .body(bytes),
            Err(error) => Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .header("content-type", "text/plain")
                .body(error.into_bytes()),
        };
        responder.respond(response.expect("valid response"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_must_be_https_or_local_http() {
        assert_eq!(
            normalize_base_url("https://team.getoutline.com/home").unwrap(),
            "https://team.getoutline.com"
        );
        assert_eq!(
            normalize_base_url("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
        assert!(normalize_base_url("http://team.example.com").is_err());
        assert!(normalize_base_url("not a url").is_err());
    }

    #[test]
    fn connection_ids_cannot_name_other_paths() {
        assert!(is_valid_id("outline-18e54025f8172260"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("/Users/someone"));
        assert!(!is_valid_id("../.."));
        assert!(!is_valid_id("a/b"));
        assert!(!is_valid_id("C:\\Users"));
    }

    #[test]
    fn token_is_sent_only_to_the_connection_server() {
        let base = "https://team.getoutline.com";
        assert_eq!(
            resolve_url(base, "/api/attachments.redirect?id=1").unwrap(),
            ("https://team.getoutline.com/api/attachments.redirect?id=1".to_string(), true)
        );
        assert!(resolve_url(base, "https://team.getoutline.com/x").unwrap().1);
        assert!(!resolve_url(base, "https://evil.example.com/x").unwrap().1);
        assert!(!resolve_url(base, "//evil.example.com/x").is_ok_and(|(_, auth)| auth));
        assert!(resolve_url(base, "file:///etc/passwd").is_err());
    }
}

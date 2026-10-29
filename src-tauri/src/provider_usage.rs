use std::{
    collections::HashMap,
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Serialize)]
pub struct ProviderUsage {
    pub id: String,
    pub name: String,
    pub status: String,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub note: Option<String>,
    pub error: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    pub used_percent: Option<f64>,
    pub resets_at: Option<i64>,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub unit: Option<String>,
    pub details: Vec<UsageDetail>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageDetail {
    pub label: String,
    pub used: f64,
}

pub fn tray_remaining(provider: &ProviderUsage, now: i64) -> Option<f64> {
    let updated = provider.updated_at?;
    let age = now - updated;
    if age < 0 || age > TRAY_STALE_SECS {
        return None;
    }
    let windows = provider
        .windows
        .iter()
        .filter(|window| {
            matches!(
                window.label.as_str(),
                "5-hour limit" | "Weekly limit" | "Weekly · all models"
            )
        })
        .collect::<Vec<_>>();
    // An elapsed window is unknown until a fresh provider response confirms it.
    if windows
        .iter()
        .any(|w| w.resets_at.is_some_and(|reset| reset <= now))
    {
        return None;
    }
    windows
        .iter()
        .filter_map(|w| w.used_percent)
        .filter(|used| used.is_finite())
        .map(|used| (100.0 - used).clamp(0.0, 100.0))
        .reduce(f64::min)
}

struct ParsedUsage {
    windows: Vec<UsageWindow>,
    plan: Option<String>,
    note: Option<String>,
}

// Credentials never enter a serializable type, logs, or the dashboard response.
struct ProviderCredential {
    id: String,
    name: String,
    token: String,
    url: &'static str,
    plan: Option<String>,
}

struct CacheEntry {
    fingerprint: Vec<u8>,
    checked_at: Instant,
    next_allowed: Instant,
    consecutive_errors: u32,
    usage: ProviderUsage,
}

type UsageCache = HashMap<String, CacheEntry>;

// The Claude usage endpoint answers 429 with `retry-after: 0` even when cold, so the
// backoff below is the only protection the app controls.
const SUCCESS_TTL_SECS: u64 = 300;
const FORCE_FLOOR_SECS: u64 = 60;
const ERROR_BACKOFF_BASE_SECS: u64 = 300;
const MAX_ERROR_BACKOFF_SECS: u64 = 1800;
pub const TRAY_FRESH_SECS: i64 = 600;
pub const TRAY_STALE_SECS: i64 = 3600;

/// Last-known tray values stay displayable for an hour even after a failed check;
/// `tray_value_is_stale` decides whether they still get the fresh presentation.
pub fn tray_value_is_stale(provider: &ProviderUsage, now: i64) -> bool {
    provider.status != "ready"
        || provider.error.is_some()
        || provider
            .updated_at
            .is_none_or(|updated| now - updated > TRAY_FRESH_SECS)
}

fn error_backoff_secs(retry_after: Option<u64>, consecutive_errors: u32) -> u64 {
    let doubling = ERROR_BACKOFF_BASE_SECS
        .checked_shl(consecutive_errors.min(3))
        .unwrap_or(MAX_ERROR_BACKOFF_SECS);
    doubling
        .min(MAX_ERROR_BACKOFF_SECS)
        .max(retry_after.unwrap_or(0))
}

fn should_check_now(entry: &CacheEntry, force: bool) -> bool {
    if force {
        entry.checked_at.elapsed() >= Duration::from_secs(FORCE_FLOOR_SECS)
    } else {
        Instant::now() >= entry.next_allowed
    }
}
static CACHE: OnceLock<Mutex<UsageCache>> = OnceLock::new();

#[cfg_attr(feature = "desktop-app", tauri::command)]
pub async fn get_provider_usage(force: Option<bool>) -> Result<Vec<ProviderUsage>, String> {
    let force = force.unwrap_or(false);
    let credentials = tokio::task::spawn_blocking(discover_credentials)
        .await
        .map_err(|_| "Could not read local provider credentials.".to_string())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not initialize usage requests.".to_string())?;
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .await;
    let results =
        futures::future::join_all(credentials.into_iter().map(|(id, name, credential)| {
            let previous = cache.get(&id);
            let client = &client;
            async move {
                let credential = match credential {
                    Ok(c) => c,
                    Err(message) => return (id.clone(), None, empty_usage(&id, &name, &message)),
                };
                let fingerprint = Sha256::digest(credential.token.as_bytes()).to_vec();
                let previous = previous.filter(|entry| entry.fingerprint == fingerprint);
                if let Some(entry) = previous {
                    // Coalesce dashboard windows and manual refreshes; back off after errors.
                    if !should_check_now(entry, force) {
                        return (id, None, entry.usage.clone());
                    }
                }
                let prior_errors = previous.map(|e| e.consecutive_errors).unwrap_or(0);
                let (usage, next_allowed, consecutive_errors) = match fetch_usage(client, &credential).await {
                    Ok(parsed) => (
                        ProviderUsage {
                            id: credential.id,
                            name: credential.name,
                            status: "ready".into(),
                            plan: parsed.plan.or(credential.plan),
                            windows: parsed.windows,
                            note: parsed.note,
                            error: None,
                            updated_at: Some(chrono::Utc::now().timestamp()),
                        },
                        Instant::now() + Duration::from_secs(SUCCESS_TTL_SECS),
                        0,
                    ),
                    Err(error) => {
                        let mut usage = previous
                            .map(|e| e.usage.clone())
                            .unwrap_or_else(|| empty_usage(&id, &name, ""));
                        usage.status = "error".into();
                        usage.error = Some(error.message);
                        let errors = prior_errors + 1;
                        let wait =
                            error_backoff_secs(error.retry_after, errors.saturating_sub(1));
                        (usage, Instant::now() + Duration::from_secs(wait), errors)
                    }
                };
                (id, Some((fingerprint, next_allowed, consecutive_errors)), usage)
            }
        }))
        .await;
    let mut usages = Vec::new();
    for (id, fingerprint, usage) in results {
        if let Some((fingerprint, next_allowed, consecutive_errors)) = fingerprint {
            cache.insert(
                id,
                CacheEntry {
                    fingerprint,
                    checked_at: Instant::now(),
                    next_allowed,
                    consecutive_errors,
                    usage: usage.clone(),
                },
            );
        } else if usage.status == "not_connected" {
            cache.remove(&id);
        }
        usages.push(usage);
    }
    Ok(usages)
}

fn empty_usage(id: &str, name: &str, message: &str) -> ProviderUsage {
    ProviderUsage {
        id: id.into(),
        name: name.into(),
        status: "not_connected".into(),
        plan: None,
        windows: vec![],
        note: None,
        error: Some(message.into()),
        updated_at: None,
    }
}

struct FetchError {
    message: String,
    retry_after: Option<u64>,
}

impl From<String> for FetchError {
    fn from(message: String) -> Self {
        FetchError {
            message,
            retry_after: None,
        }
    }
}

async fn fetch_usage(
    client: &reqwest::Client,
    credential: &ProviderCredential,
) -> Result<ParsedUsage, FetchError> {
    let mut request = client
        .get(credential.url)
        .header("Accept", "application/json");
    if credential.id == "claude" {
        request = request
            .bearer_auth(&credential.token)
            .header("anthropic-beta", "oauth-2025-04-20");
    } else {
        request = request.header("Authorization", &credential.token);
    }
    let response = request
        .send()
        .await
        .map_err(|_| FetchError::from("Usage request failed. Check your connection and retry.".to_string()))?;
    if !response.status().is_success() {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let message = match response.status().as_u16() {
            401 | 403 => {
                "Session expired or access denied. Sign in again in the provider app.".into()
            }
            429 => "Provider rate limited this check. Retrying with a longer wait.".into(),
            code => format!("Provider returned HTTP {code}. Automatic retry in five minutes."),
        };
        return Err(FetchError { message, retry_after });
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|_| FetchError::from("Provider returned an unreadable usage response.".to_string()))?;
    if credential.id == "claude" {
        parse_claude(&payload).map_err(FetchError::from)
    } else {
        parse_zcode(&payload).map_err(FetchError::from)
    }
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite() && *v >= 0.0)
}

fn reset_time(value: &Value) -> Option<i64> {
    if let Some(raw) = value.as_i64().filter(|v| *v > 0) {
        return Some(if raw > 100_000_000_000 {
            raw / 1000
        } else {
            raw
        });
    }
    value
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
}

fn humanize(value: &str) -> String {
    value
        .split(['_', '-'])
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn window(id: &str, label: String, value: &Value, percent_key: &str) -> UsageWindow {
    UsageWindow {
        id: id.into(),
        label,
        used_percent: number(&value[percent_key]),
        resets_at: reset_time(&value["resets_at"]),
        used: number(&value["used_dollars"]),
        limit: number(&value["limit_dollars"]),
        unit: value
            .get("limit_dollars")
            .filter(|v| v.is_number())
            .map(|_| "USD".into()),
        details: vec![],
    }
}

fn parse_claude(payload: &Value) -> Result<ParsedUsage, String> {
    let object = payload.as_object().ok_or("Unrecognized usage response.")?;
    let mut windows = Vec::new();
    let mut canonical_session = false;
    let mut canonical_weekly = false;
    if let Some(limits) = payload["limits"].as_array() {
        for (index, limit) in limits.iter().enumerate() {
            let Some(kind) = limit["kind"].as_str() else {
                continue;
            };
            let label = match kind {
                "session" => {
                    canonical_session = true;
                    "5-hour limit".into()
                }
                "weekly_all" => {
                    canonical_weekly = true;
                    "Weekly · all models".into()
                }
                "weekly_scoped" => {
                    let model = limit
                        .pointer("/scope/model/display_name")
                        .and_then(Value::as_str);
                    let surface = limit
                        .pointer("/scope/surface/display_name")
                        .and_then(Value::as_str)
                        .or_else(|| limit.pointer("/scope/surface").and_then(Value::as_str));
                    format!("Weekly · {}", model.or(surface).unwrap_or("scoped limit"))
                }
                _ => humanize(kind),
            };
            windows.push(window(&format!("{kind}-{index}"), label, limit, "percent"));
        }
    }
    for (key, value) in object {
        if !value.is_object()
            || key == "extra_usage"
            || value.get("utilization").is_none() && key != "five_hour" && key != "seven_day"
        {
            continue;
        }
        if (key == "five_hour" && canonical_session) || (key == "seven_day" && canonical_weekly) {
            continue;
        }
        let label = match key.as_str() {
            "five_hour" => "5-hour limit".into(),
            "seven_day" => "Weekly · all models".into(),
            _ if key.starts_with("seven_day_") => format!("Weekly · {}", humanize(&key[10..])),
            _ if number(&value["limit_dollars"]).is_some() => {
                "Additional spending allowance".into()
            }
            _ => humanize(key),
        };
        windows.push(window(key, label, value, "utilization"));
    }
    let extra = &payload["extra_usage"];
    let note = if extra["is_enabled"] == false {
        Some("Extra usage is disabled.".into())
    } else if extra["is_enabled"] == true {
        let mut w = window(
            "extra_usage",
            "Extra usage · monthly".into(),
            extra,
            "utilization",
        );
        let divisor = 10_f64.powi(extra["decimal_places"].as_i64().unwrap_or(2).clamp(0, 6) as i32);
        w.used = number(&extra["used_credits"]).map(|n| n / divisor);
        w.limit = number(&extra["monthly_limit"]).map(|n| n / divisor);
        w.unit = Some(extra["currency"].as_str().unwrap_or("USD").into());
        windows.push(w);
        None
    } else {
        None
    };
    if windows.is_empty() && extra.is_null() {
        return Err("The provider did not return any usage limits for this account.".into());
    }
    Ok(ParsedUsage {
        windows,
        plan: None,
        note,
    })
}

fn parse_zcode(payload: &Value) -> Result<ParsedUsage, String> {
    if payload["success"] == false || payload.get("code").is_some_and(|c| c != 200 && c != 0) {
        return Err("The provider could not return quota. Open Zcode and check your Coding Plan connection.".into());
    }
    let data = &payload["data"];
    let limits = data["limits"]
        .as_array()
        .ok_or("The provider did not return quota windows.")?;
    let mut windows = Vec::new();
    for (index, limit) in limits.iter().enumerate() {
        let Some(kind) = limit["type"].as_str() else {
            continue;
        };
        let period = match (limit["unit"].as_i64(), limit["number"].as_i64()) {
            (Some(3), Some(n)) => format!("{n}-hour"),
            (Some(6), Some(1)) => "Weekly".into(),
            (Some(5), Some(1)) => "Monthly".into(),
            (Some(4), Some(1)) => "Daily".into(),
            _ => "".into(),
        };
        let label = match kind {
            "TOKENS_LIMIT" => format!(
                "{} limit",
                if period.is_empty() { "Token" } else { &period }
            ),
            "TIME_LIMIT" => format!(
                "{} tools",
                if period.is_empty() {
                    "Monthly"
                } else {
                    &period
                }
            ),
            _ => humanize(kind),
        };
        let mut w = window(&format!("{kind}-{index}"), label, limit, "percentage");
        w.resets_at = reset_time(&limit["nextResetTime"]);
        w.used = number(&limit["currentValue"]);
        w.limit = number(&limit["usage"]);
        w.unit = (kind == "TIME_LIMIT").then(|| "calls".into());
        if w.used_percent.is_none() {
            if let (Some(used), Some(total)) = (w.used, w.limit.filter(|v| *v > 0.0)) {
                w.used_percent = Some(used / total * 100.0);
            }
        }
        if let Some(details) = limit["usageDetails"].as_array() {
            w.details = details
                .iter()
                .filter_map(|d| {
                    Some(UsageDetail {
                        label: d["modelCode"].as_str()?.into(),
                        used: number(&d["usage"])?,
                    })
                })
                .collect();
        }
        windows.push(w);
    }
    Ok(ParsedUsage {
        windows,
        plan: data["level"].as_str().map(str::to_owned),
        note: limits
            .is_empty()
            .then(|| "No quota windows reported for this Coding Plan.".into()),
    })
}

fn read_json(path: &std::path::Path) -> Result<Value, String> {
    let bytes =
        std::fs::read(path).map_err(|_| "Local credentials could not be read.".to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "Local credentials could not be parsed.".into())
}

fn claude_credential() -> Result<ProviderCredential, String> {
    let home = dirs::home_dir().ok_or("Home directory unavailable.")?;
    let sources_path = home.join(".codex-switcher/provider-sources.json");
    let sources = if sources_path.exists() {
        Some(read_json(&sources_path)?)
    } else {
        None
    };
    let remote_host = sources.as_ref().and_then(|s| s["claude_ssh_host"].as_str());
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    #[allow(unused_mut)]
    let mut credentials = if let Some(host) = remote_host {
        Some(read_remote_claude_credentials(host)?)
    } else {
        read_json(&config.join(".credentials.json")).ok()
    };
    #[cfg(target_os = "macos")]
    if remote_host.is_none()
        && credentials
            .as_ref()
            .and_then(|c| c.pointer("/claudeAiOauth/accessToken"))
            .is_none()
    {
        if let Ok(output) = std::process::Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .output()
        {
            if output.status.success() {
                credentials = serde_json::from_slice(&output.stdout).ok();
            }
        }
    }
    let oauth = credentials.as_ref().and_then(|c| c.get("claudeAiOauth"));
    let token = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            oauth
                .and_then(|o| o["accessToken"].as_str())
                .map(str::to_owned)
        })
        .filter(|s| !s.trim().is_empty())
        .ok_or("Sign in to Claude Code on the machine running this dashboard, then refresh.")?;
    Ok(ProviderCredential {
        id: "claude".into(),
        name: remote_host
            .map(|host| format!("Claude Code · {host}"))
            .unwrap_or_else(|| "Claude Code".into()),
        token,
        url: "https://api.anthropic.com/api/oauth/usage",
        plan: oauth
            .and_then(|o| o["subscriptionType"].as_str())
            .map(str::to_owned),
    })
}

fn valid_ssh_alias(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 255
        && host.as_bytes()[0].is_ascii_alphanumeric()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn read_remote_claude_credentials(host: &str) -> Result<Value, String> {
    if !valid_ssh_alias(host) {
        return Err("Invalid Claude SSH host alias in provider-sources.json.".into());
    }
    let output = std::process::Command::new("/usr/bin/ssh")
        .args(["-n", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=5",
            "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=1", host,
            "python3 -c 'import json,pathlib; print(json.dumps({\"claudeAiOauth\":json.loads((pathlib.Path.home()/\".claude/.credentials.json\").read_text()).get(\"claudeAiOauth\")}))'"])
        .stdin(std::process::Stdio::null())
        .output().map_err(|_| "Could not start the configured SSH credential reader.".to_string())?;
    if !output.status.success() {
        return Err("Could not read the remote Claude login. Check the configured SSH host and sign in there.".into());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "Remote Claude credentials could not be parsed.".into())
}

fn decrypt_zcode(value: &str) -> Result<String, String> {
    decrypt_zcode_with_secret(value, &zcode_credential_secret())
}

fn decrypt_zcode_with_secret(value: &str, secret: &str) -> Result<String, String> {
    let Some(payload) = value.strip_prefix("enc:v1:") else {
        return Ok(value.into());
    };
    let parts = payload
        .split('.')
        .map(|p| URL_SAFE_NO_PAD.decode(p))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Zcode credentials could not be decoded.")?;
    if parts.len() != 3 || parts[0].len() != 12 || parts[1].len() != 16 {
        return Err("Unrecognized Zcode credential format.".into());
    }
    let key = Sha256::digest(secret.as_bytes());
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| "Could not initialize credential reader.")?;
    let ciphertext = [parts[2].as_slice(), parts[1].as_slice()].concat();
    let plaintext = cipher.decrypt(Nonce::from_slice(&parts[0]), ciphertext.as_ref())
        .map_err(|_| "Zcode credentials could not be unlocked. Run the dashboard under the same user and ZCODE_CREDENTIAL_SECRET as Zcode.")?;
    String::from_utf8(plaintext).map_err(|_| "Invalid Zcode credential encoding.".into())
}

fn zcode_credential_secret() -> String {
    std::env::var("ZCODE_CREDENTIAL_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let platform = if cfg!(target_os = "macos") {
                "darwin"
            } else if cfg!(windows) {
                "win32"
            } else {
                "linux"
            };
            let home = dirs::home_dir().unwrap_or_default();
            let user = std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" })
                .unwrap_or_else(|_| "unknown".into());
            format!(
                "zcode-credential-fallback:{platform}:{}:{user}",
                home.display()
            )
        })
}

type DiscoveredCredential = (String, String, Result<ProviderCredential, String>);

fn discover_credentials() -> Vec<DiscoveredCredential> {
    let mut result = vec![("claude".into(), "Claude Code".into(), claude_credential())];
    let path = std::env::var_os("AI_USAGE_ZCODE_CREDENTIALS_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".zcode/v2/credentials.json")
        });
    let credentials = read_json(&path).ok();
    for (family, env_key, name, url) in [
        (
            "zai",
            "ZAI_API_KEY",
            "Zcode · Z.ai",
            "https://api.z.ai/api/monitor/usage/quota/limit",
        ),
        (
            "bigmodel",
            "BIGMODEL_API_KEY",
            "Zcode · BigModel",
            "https://open.bigmodel.cn/api/monitor/usage/quota/limit",
        ),
    ] {
        let stored = credentials.as_ref().and_then(Value::as_object).and_then(|c| c.iter().find(|(key, _)|
            key.starts_with(&format!("account-provider:coding-plan:account:{family}-individual-coding-plan:account:")) && key.ends_with(":api-key")
        )).and_then(|(_, value)| value.as_str());
        let env = std::env::var(env_key).ok().filter(|s| !s.trim().is_empty());
        if family == "bigmodel" && stored.is_none() && env.is_none() {
            continue;
        }
        let token = env.map(Ok).or_else(|| stored.map(decrypt_zcode))
            .unwrap_or_else(|| Err("Connect your personal Coding Plan in Zcode on the machine running this dashboard, then refresh. An API key can also be supplied through ZAI_API_KEY or BIGMODEL_API_KEY.".into()));
        let id = format!("zcode-{family}");
        let credential = token.and_then(|token| {
            if token.trim().is_empty() {
                return Err("The Coding Plan credential is empty. Reconnect it in Zcode.".into());
            }
            Ok(ProviderCredential {
                id: id.clone(),
                name: name.into(),
                token,
                url,
                plan: None,
            })
        });
        result.push((id, name.into(), credential));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn remote_credential_host_only_accepts_ssh_aliases() {
        assert!(valid_ssh_alias("harness"));
        assert!(valid_ssh_alias("my-host.local"));
        for host in [
            "",
            "-oProxyCommand=bad",
            "host;bad",
            "host\nother",
            "user@host",
            "host name",
        ] {
            assert!(!valid_ssh_alias(host));
        }
    }

    #[test]
    fn tray_summary_uses_the_tighter_plan_window_and_marks_stale_values() {
        let parsed = parse_claude(&json!({"five_hour":{"utilization":20},"seven_day":{"utilization":65},"extra_usage":{"is_enabled":true,"utilization":99}})).unwrap();
        let mut provider = ProviderUsage {
            id: "claude".into(),
            name: "Claude Code".into(),
            status: "ready".into(),
            plan: None,
            windows: parsed.windows,
            note: None,
            error: None,
            updated_at: Some(1000),
        };
        assert_eq!(tray_remaining(&provider, 1001), Some(35.0));
        assert!(!tray_value_is_stale(&provider, 1001));
        // A failed follow-up check keeps the last-known value displayable for an hour.
        provider.status = "error".into();
        provider.error = Some("Provider rate limited this check.".into());
        assert_eq!(tray_remaining(&provider, 1001), Some(35.0));
        assert!(tray_value_is_stale(&provider, 1001));
        assert_eq!(tray_remaining(&provider, 4600), Some(35.0));
        assert_eq!(tray_remaining(&provider, 4601), None);
        provider.status = "ready".into();
        provider.error = None;
        provider.windows[0].resets_at = Some(1000);
        assert_eq!(tray_remaining(&provider, 1001), None);
    }

    #[test]
    fn error_backoff_doubles_and_honors_retry_after() {
        assert_eq!(error_backoff_secs(None, 0), 300);
        assert_eq!(error_backoff_secs(None, 1), 600);
        assert_eq!(error_backoff_secs(None, 2), 1200);
        assert_eq!(error_backoff_secs(None, 3), 1800);
        assert_eq!(error_backoff_secs(None, 8), 1800);
        assert_eq!(error_backoff_secs(Some(900), 0), 900);
        assert_eq!(error_backoff_secs(Some(7200), 0), 7200);
        assert_eq!(error_backoff_secs(Some(0), 1), 600);
    }

    #[test]
    fn force_refresh_only_bypasses_the_backoff_after_the_floor() {
        let entry = CacheEntry {
            fingerprint: vec![0],
            checked_at: Instant::now(),
            next_allowed: Instant::now() + Duration::from_secs(300),
            consecutive_errors: 0,
            usage: empty_usage("claude", "Claude Code", ""),
        };
        assert!(!should_check_now(&entry, false));
        assert!(!should_check_now(&entry, true));
        let aged = CacheEntry {
            checked_at: entry.checked_at - Duration::from_secs(61),
            ..entry
        };
        assert!(!should_check_now(&aged, false));
        assert!(should_check_now(&aged, true));
    }

    #[test]
    fn claude_preserves_all_windows_without_duplicate_session_and_weekly() {
        let usage = parse_claude(&json!({
            "five_hour": {"utilization": 36, "resets_at": "2026-10-07T00:59:59Z"},
            "seven_day": {"utilization": 65},
            "seven_day_opus": null,
            "new_pool": {"utilization": 18, "limit_dollars": 250, "used_dollars": 45},
            "limits": [
                {"kind":"session", "percent":36, "resets_at":"2026-10-07T00:59:59Z"},
                {"kind":"weekly_all", "percent":65},
                {"kind":"weekly_scoped", "percent":0, "scope":{"model":{"display_name":"Fable"}}}
            ],
            "extra_usage":{"is_enabled":false}
        }))
        .unwrap();
        assert_eq!(usage.windows.len(), 4);
        assert!(usage
            .windows
            .iter()
            .any(|w| w.label == "Weekly · Fable" && w.used_percent == Some(0.0)));
        assert!(usage.windows.iter().any(|w| w.limit == Some(250.0)));
        assert_eq!(usage.note.as_deref(), Some("Extra usage is disabled."));
    }

    #[test]
    fn zai_distinguishes_hour_week_and_tool_quotas() {
        let usage = parse_zcode(&json!({"code":200,"success":true,"data":{"level":"max","limits":[
            {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":82,"nextResetTime":1791331453837_i64},
            {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":47},
            {"type":"TIME_LIMIT","unit":5,"number":1,"percentage":4,"usage":4000,"currentValue":169,"remaining":3831,
             "usageDetails":[{"modelCode":"search-prime","usage":160}]}
        ]}})).unwrap();
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].label, "5-hour limit");
        assert_eq!(usage.windows[1].label, "Weekly limit");
        assert_eq!(usage.windows[2].label, "Monthly tools");
        assert_eq!(usage.windows[2].limit, Some(4000.0));
        assert_eq!(usage.windows[2].used, Some(169.0));
        assert_eq!(usage.windows[0].resets_at, Some(1791331453));
        assert_eq!(usage.windows[2].details[0].used, 160.0);
    }

    #[test]
    fn missing_usage_is_unknown_not_zero_and_overages_are_preserved() {
        let usage =
            parse_claude(&json!({"five_hour":{"resets_at":null},"seven_day":{"utilization":112}}))
                .unwrap();
        assert_eq!(usage.windows[0].used_percent, None);
        assert_eq!(usage.windows[1].used_percent, Some(112.0));
        assert!(parse_claude(&json!({"error":"bad"})).is_err());
        assert!(parse_zcode(&json!({"code":401,"data":{"limits":[]}})).is_err());
        assert!(parse_zcode(&json!({"code":200,"data":{}})).is_err());
    }

    #[test]
    fn encrypted_zcode_credentials_require_the_matching_key_and_authentication_tag() {
        let secret = "test-credential-secret";
        let key = Sha256::digest(secret.as_bytes());
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let iv = [7u8; 12];
        let encrypted = cipher
            .encrypt(Nonce::from_slice(&iv), b"test-api-key".as_ref())
            .unwrap();
        let (body, tag) = encrypted.split_at(encrypted.len() - 16);
        let encoded = format!(
            "enc:v1:{}.{}.{}",
            URL_SAFE_NO_PAD.encode(iv),
            URL_SAFE_NO_PAD.encode(tag),
            URL_SAFE_NO_PAD.encode(body)
        );
        assert_eq!(
            decrypt_zcode_with_secret(&encoded, secret).unwrap(),
            "test-api-key"
        );
        assert!(decrypt_zcode_with_secret(&encoded, "wrong-key").is_err());
        let tampered = format!(
            "enc:v1:{}.{}.{}",
            URL_SAFE_NO_PAD.encode(iv),
            URL_SAFE_NO_PAD.encode([0u8; 16]),
            URL_SAFE_NO_PAD.encode(body)
        );
        assert!(decrypt_zcode_with_secret(&tampered, secret).is_err());
        assert!(decrypt_zcode_with_secret("enc:v1:bad", secret).is_err());
    }

    #[test]
    fn extra_usage_converts_minor_currency_units_and_keeps_missing_limits_unknown() {
        let usage = parse_claude(&json!({"extra_usage":{"is_enabled":true,"decimal_places":2,"currency":"USD","used_credits":1234,"monthly_limit":5000,"utilization":24.68}})).unwrap();
        assert_eq!(usage.windows[0].used, Some(12.34));
        assert_eq!(usage.windows[0].limit, Some(50.0));
        assert_eq!(usage.windows[0].resets_at, None);
    }

    #[tokio::test]
    async fn provider_http_errors_do_not_expose_raw_responses_or_credentials() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/usage", server.server_addr());
        let worker = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            assert_eq!(
                request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .unwrap()
                    .value
                    .as_str(),
                "test-api-key"
            );
            request
                .respond(
                    tiny_http::Response::from_string("secret-response-body").with_status_code(401),
                )
                .unwrap();
        });
        let credential = ProviderCredential {
            id: "zcode-zai".into(),
            name: "Zcode".into(),
            token: "test-api-key".into(),
            url: Box::leak(url.into_boxed_str()),
            plan: None,
        };
        let error = fetch_usage(&reqwest::Client::new(), &credential)
            .await
            .err()
            .unwrap();
        assert!(error.message.contains("Sign in again"));
        assert!(!error.message.contains("test-api-key"));
        assert!(!error.message.contains("secret-response-body"));
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn rate_limited_responses_carry_the_retry_after_delay() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/usage", server.server_addr());
        let worker = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            request
                .respond(
                    tiny_http::Response::from_string("rate limited")
                        .with_status_code(429)
                        .with_header(
                            "Retry-After: 42"
                                .parse::<tiny_http::Header>()
                                .unwrap(),
                        ),
                )
                .unwrap();
        });
        let credential = ProviderCredential {
            id: "claude".into(),
            name: "Claude Code".into(),
            token: "test-api-key".into(),
            url: Box::leak(url.into_boxed_str()),
            plan: None,
        };
        let error = fetch_usage(&reqwest::Client::new(), &credential)
            .await
            .err()
            .unwrap();
        assert_eq!(error.retry_after, Some(42));
        assert!(error.message.contains("rate limited"));
        worker.join().unwrap();
    }
}

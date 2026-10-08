//! Opt-in harness sync followed by a verified restart of the whole T3 service.
use crate::types::{AccountsStore, AuthDotJson, StoredAccount};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

const HELPER: &str = include_str!("t3_sync.py");

pub fn configured_host() -> Result<Option<String>> {
    let path = super::get_config_dir()?.join("provider-sources.json");
    if !path.exists() {
        return Ok(None);
    }
    let settings: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let Some(value) = settings.get("codex_t3_ssh_host") else {
        return Ok(None);
    };
    let host = value
        .as_str()
        .context("codex_t3_ssh_host must be an SSH alias")?;
    validate_host(host)?;
    Ok(Some(host.to_string()))
}

fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || host.len() > 253
        || host.starts_with('-')
        || !host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-@".contains(&c))
    {
        bail!("Invalid T3 SSH host alias in provider-sources.json");
    }
    Ok(())
}

async fn request(host: &str, payload: Value) -> Result<Value> {
    validate_host(host)?;
    let command = format!("python3 -c '{}'", HELPER.replace('\'', "'\\''"));
    let mut child = tokio::process::Command::new("/usr/bin/ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
            "--",
            host,
            &command,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("Could not connect to harness for T3 account sync")?;
    let mut input = child.stdin.take().context("SSH input unavailable")?;
    let output = tokio::time::timeout(Duration::from_secs(240), async move {
        input.write_all(&serde_json::to_vec(&payload)?).await?;
        input.shutdown().await?;
        drop(input);
        child.wait_with_output().await.map_err(anyhow::Error::from)
    })
    .await
    .context("Harness sync timed out; verify the remote login before retrying")??;
    let reply: Value = serde_json::from_slice(&output.stdout)
        .context("No confirmation from harness; verify its login before retrying")?;
    if let Some(error) = reply.get("error").and_then(Value::as_str) {
        bail!("{host}: {error}");
    }
    if !output.status.success() {
        bail!("Harness account sync failed");
    }
    reply
        .get("result")
        .cloned()
        .context("Missing harness confirmation")
}

#[derive(serde::Deserialize)]
pub struct State {
    pub auth: AuthDotJson,
    revision: String,
}

pub async fn probe(host: &str) -> Result<State> {
    Ok(serde_json::from_value(
        request(host, json!({"action":"probe"})).await?,
    )?)
}

pub async fn apply(host: &str, account: &StoredAccount, state: &State) -> Result<()> {
    request(host, json!({"action":"apply", "auth":super::switcher::create_auth_json(account)?, "revision":state.revision})).await?;
    Ok(())
}

/// Preserve a newer remote refresh-token rotation in the saved matching account.
pub fn preserve_tokens(
    store: &mut AccountsStore,
    remote: &AuthDotJson,
    local: Option<&AuthDotJson>,
) -> bool {
    let Some(tokens) = &remote.tokens else {
        return false;
    };
    if local.is_some_and(|local| {
        local
            .tokens
            .as_ref()
            .is_some_and(|t| t.account_id == tokens.account_id)
            && local.last_refresh >= remote.last_refresh
    }) {
        return false;
    }
    let original = store.active_account_id.clone();
    let mut active_changed = false;
    for account in store.accounts.clone() {
        if let crate::types::AuthData::ChatGPT { access_token, .. } = &account.auth_data {
            if token_issued_at(access_token)
                .zip(token_issued_at(&tokens.access_token))
                .is_some_and(|(saved, remote)| saved > remote)
            {
                continue;
            }
        }
        store.active_account_id = Some(account.id);
        if super::storage::sync_active_account_tokens(store, remote) {
            active_changed = store.active_account_id == original;
            break;
        }
    }
    store.active_account_id = original;
    active_changed
}

fn token_issued_at(token: &str) -> Option<i64> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims.get("iat").and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_cannot_inject_commands_or_options() {
        for host in [
            "",
            "-oProxyCommand=bad",
            "harness;bad",
            "$(bad)",
            "harness\n",
        ] {
            assert!(validate_host(host).is_err());
        }
        assert!(validate_host("harness").is_ok());
    }
    #[test]
    fn older_remote_copy_cannot_overwrite_newer_saved_inactive_tokens() {
        use base64::Engine;
        let token = |iat| {
            format!(
                "h.{}.s",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(json!({"iat":iat}).to_string())
            )
        };
        let mut account = StoredAccount::new_api_key("fixture".into(), "unused".into());
        account.auth_data = crate::types::AuthData::ChatGPT {
            id_token: "fixture".into(),
            access_token: token(200),
            refresh_token: "new-rotation".into(),
            account_id: Some("one".into()),
        };
        let mut store = AccountsStore::default();
        store.active_account_id = Some("another-account".into());
        store.accounts.push(account);
        let remote: AuthDotJson = serde_json::from_value(json!({"tokens":{"id_token":"fixture","access_token":token(100),"refresh_token":"consumed","account_id":"one"},"last_refresh":"2026-10-08T17:00:00Z"})).unwrap();
        preserve_tokens(&mut store, &remote, None);
        let crate::types::AuthData::ChatGPT { refresh_token, .. } = &store.accounts[0].auth_data
        else {
            panic!()
        };
        assert_eq!(refresh_token, "new-rotation");
    }
    #[test]
    fn newer_remote_rotation_is_preserved_without_changing_selection() {
        let mut account = StoredAccount::new_api_key("fixture".into(), "unused".into());
        account.auth_data = crate::types::AuthData::ChatGPT {
            id_token: "fixture".into(),
            access_token: "old".into(),
            refresh_token: "old".into(),
            account_id: Some("one".into()),
        };
        let mut store = AccountsStore::default();
        store.active_account_id = Some(account.id.clone());
        store.accounts.push(account.clone());
        let remote: AuthDotJson = serde_json::from_value(json!({"tokens":{"id_token":"fixture","access_token":"new","refresh_token":"rotated","account_id":"one"},"last_refresh":"2026-10-08T17:00:00Z"})).unwrap();
        assert!(preserve_tokens(&mut store, &remote, None));
        assert_eq!(store.active_account_id, Some(account.id));
        assert!(!preserve_tokens(&mut store, &remote, Some(&remote)));
        let crate::types::AuthData::ChatGPT { refresh_token, .. } = &store.accounts[0].auth_data
        else {
            panic!()
        };
        assert_eq!(refresh_token, "rotated");
    }
}

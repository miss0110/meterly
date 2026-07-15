//! Which account each source is logged in as, read from local auth files
//! (no network). Shown on the cards so it's clear whose usage is being
//! measured — the two CLIs can be on entirely different accounts.

use base64::Engine as _;
use serde_json::Value;

fn home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// Claude Code account: `~/.claude.json` → `oauthAccount`. Returns
/// "email · <plan>" (e.g. team org name), or just the email.
pub fn claude_account() -> Option<String> {
    let content = std::fs::read_to_string(home()?.join(".claude.json")).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    let o = v.get("oauthAccount")?;
    let email = o.get("emailAddress")?.as_str()?;
    let tag = match o.get("organizationType").and_then(Value::as_str) {
        Some("claude_team") => o
            .get("organizationName")
            .and_then(Value::as_str)
            .unwrap_or("Team")
            .to_string(),
        Some("claude_enterprise") => o
            .get("organizationName")
            .and_then(Value::as_str)
            .unwrap_or("Enterprise")
            .to_string(),
        _ => {
            let t = o.get("userRateLimitTier").and_then(Value::as_str).unwrap_or("");
            if t.contains("max") {
                "Max".into()
            } else if t.contains("pro") {
                "Pro".into()
            } else {
                String::new()
            }
        }
    };
    Some(if tag.is_empty() {
        email.to_string()
    } else {
        format!("{email} · {tag}")
    })
}

/// Codex account: `~/.codex/auth.json` → the `id_token` JWT's `email` claim.
/// Returns "email · ChatGPT".
pub fn codex_account() -> Option<String> {
    let content = std::fs::read_to_string(home()?.join(".codex/auth.json")).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    let id_token = v.get("tokens")?.get("id_token")?.as_str()?;
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let email = claims.get("email")?.as_str()?;
    Some(format!("{email} · ChatGPT"))
}

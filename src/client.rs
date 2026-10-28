use anyhow::{Context, Result, anyhow, bail};
use reqwest::Method;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::redirect::Policy;
use serde_json::Value;
use std::time::Duration;

/// Connection settings, read from the environment.
///
/// - `PROXMOX_URL`: base URL, e.g. `https://192.168.1.10:8006`
/// - `PROXMOX_TOKEN`: API token, `user@realm!tokenid=secret`
/// - `PROXMOX_INSECURE`: set to `1` to accept self-signed certificates
/// - `PROXMOX_ALLOW_WRITE`: set to `1` to enable tools that modify Proxmox
pub struct ProxmoxClient {
    http: reqwest::Client,
    base_url: String,
    /// `PVEAPIToken=...`, marked sensitive so it never shows up in debug output.
    auth: HeaderValue,
    allow_write: bool,
}

/// Accept only identifiers that are safe to put in a URL path segment
/// (node names, storage IDs, snapshot names).
pub fn segment(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.len() <= 64
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok { Ok(s) } else { bail!("invalid identifier: {s:?}") }
}

/// Accept only a Proxmox volume ID such as `local:iso/debian.iso`: a storage
/// ID, a colon, then a relative path. Rejects everything that means something
/// in a URL (`?`, `#`, `%`) or in a Proxmox property string (`,`, `=`, spaces).
pub fn volid(s: &str) -> Result<&str> {
    let ok = s.len() <= 255
        && !s.contains("..")
        && s.split_once(':')
            .is_some_and(|(storage, rest)| segment(storage).is_ok() && !rest.is_empty())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '+'));
    if ok { Ok(s) } else { bail!("invalid volume ID: {s:?}") }
}

/// Accept only a task ID as Proxmox builds it
/// (`UPID:node:pid:pstart:starttime:type:id:user@realm!token:`).
pub fn upid(s: &str) -> Result<&str> {
    let ok = s.starts_with("UPID:")
        && s.len() <= 256
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '@' | '!' | '-' | '_' | '.'));
    if ok { Ok(s) } else { bail!("invalid task ID: {s:?}") }
}

/// Accept only an `https://` URL without whitespace or control characters.
pub fn https_url(s: &str) -> Result<&str> {
    let ok = s.len() <= 2048
        && s.len() > "https://".len()
        && s.starts_with("https://")
        && !s.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok { Ok(s) } else { bail!("only https:// URLs are accepted: {s:?}") }
}

impl ProxmoxClient {
    pub fn from_env() -> Result<Self> {
        let base_url = std::env::var("PROXMOX_URL").context("PROXMOX_URL is not set")?;
        let token = std::env::var("PROXMOX_TOKEN").context("PROXMOX_TOKEN is not set")?;
        if !base_url.starts_with("https://") {
            bail!("PROXMOX_URL must start with https:// (the API token would travel in clear text)");
        }
        let mut auth = HeaderValue::from_str(&format!("PVEAPIToken={token}"))
            .context("PROXMOX_TOKEN contains characters that are not allowed in a header")?;
        auth.set_sensitive(true);
        let flag = |name| std::env::var(name).is_ok_and(|v| v == "1");
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(flag("PROXMOX_INSECURE"))
            // The API never needs a redirect; following one could send the token elsewhere.
            .redirect(Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            auth,
            allow_write: flag("PROXMOX_ALLOW_WRITE"),
        })
    }

    async fn send(&self, method: Method, path: &str, params: &[(&str, String)]) -> Result<Value> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}/api2/json{path}", self.base_url))
            .header(AUTHORIZATION, self.auth.clone());
        req = if method == Method::POST || method == Method::PUT {
            req.form(params)
        } else {
            req.query(params)
        };
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let detail: String = resp.text().await.unwrap_or_default().chars().take(300).collect();
            bail!("Proxmox returned {status} for {method} {path}: {detail}");
        }
        let mut body: Value = resp.json().await?;
        Ok(body["data"].take())
    }

    /// GET `/api2/json{path}` and return the `data` field of the response.
    pub async fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        self.send(Method::GET, path, params).await
    }

    /// Modifying request; refused unless `PROXMOX_ALLOW_WRITE=1`.
    ///
    /// If Proxmox answers with a task ID, wait up to `wait_secs` for it to finish.
    pub async fn write(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        wait_secs: u64,
    ) -> Result<String> {
        if !self.allow_write {
            bail!("write operations are disabled: set PROXMOX_ALLOW_WRITE=1 to enable them");
        }
        let data = self.send(method, path, params).await?;
        match data.as_str() {
            Some(upid) if upid.starts_with("UPID:") => self.wait_task(upid, wait_secs).await,
            _ => Ok(serde_json::to_string_pretty(&data)?),
        }
    }

    /// Poll a task until it stops or `wait_secs` elapse.
    pub async fn wait_task(&self, upid: &str, wait_secs: u64) -> Result<String> {
        let upid = self::upid(upid)?;
        let node = segment(upid.split(':').nth(1).unwrap_or_default())?;
        let status_path = format!("/nodes/{node}/tasks/{upid}/status");
        for _ in 0..=wait_secs / 2 {
            let status = self.get(&status_path, &[]).await?;
            if status["status"] == "stopped" {
                if status["exitstatus"] == "OK" {
                    return Ok(format!("Task completed OK ({upid})"));
                }
                let warnings = status["exitstatus"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("WARNINGS"));
                let log = self
                    .get(&format!("/nodes/{node}/tasks/{upid}/log"), &[("limit", "500".into())])
                    .await
                    .unwrap_or_default();
                let tail: Vec<&str> = log
                    .as_array()
                    .map(|a| a.iter().filter_map(|l| l["t"].as_str()).collect())
                    .unwrap_or_default();
                let tail = tail[tail.len().saturating_sub(10)..].join("\n");
                // The operation went through; Proxmox only attached warnings to it.
                if warnings {
                    return Ok(format!(
                        "Task completed with warnings ({upid}):\n{}",
                        tail.lines()
                            .filter(|l| l.starts_with("WARN"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                }
                return Err(anyhow!("Task failed ({}): {tail}", status["exitstatus"]));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Ok(format!(
            "Task still running after {wait_secs}s, check it with get_task_status: {upid}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_rejects_path_tricks() {
        assert!(segment("pve-1").is_ok());
        for bad in ["", "..", "a/b", "a?b", "a#b", "a b", "a%2Fb", "a,b", "a=b"] {
            assert!(segment(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn volid_accepts_real_volumes_only() {
        for good in [
            "local:iso/debian-12.iso",
            "local:vztmpl/debian-12-standard_12.7-1_amd64.tar.zst",
            "local:backup/vzdump-lxc-100-2026_10_28-12_00_00.tar.zst",
        ] {
            assert!(volid(good).is_ok(), "{good:?} should be accepted");
        }
        for bad in [
            "iso/debian.iso",
            "local:",
            "local:../etc/passwd",
            "local:iso/a?x=1",
            "local:iso/a#b",
            "local:iso/a%2F..%2Fb",
            "local:iso/a,media=disk",
            "local:iso/a b",
            "bad/storage:iso/a.iso",
        ] {
            assert!(volid(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn upid_accepts_real_task_ids_only() {
        assert!(upid("UPID:pve:0000A1B2:00123456:65F00000:vzdump:100:root@pam!mcp:").is_ok());
        for bad in [
            "pve",
            "UPID:pve:/../../storage",
            "UPID:pve:1?x=1",
            "UPID:pve:1#x",
            "UPID:pve:1 2",
            "UPID:pve:..",
        ] {
            assert!(upid(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn https_url_requires_https() {
        assert!(https_url("https://cdimage.debian.org/x.iso").is_ok());
        for bad in ["http://a.org/x.iso", "https://", "file:///etc/passwd", "https://a.org/x y"] {
            assert!(https_url(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}

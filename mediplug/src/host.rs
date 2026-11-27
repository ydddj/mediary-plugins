use crate::db;
use crate::models::{
    sanitize_source_id, EngineSettings, HostCatalogSource, HostStorePluginView, HostStoreSnapshot,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostStatus {
    pub plugin_enabled: bool,
    pub connected: bool,
    pub pending_scope_approval: bool,
    pub plugins_read_ready: bool,
    pub settings_read_ready: bool,
    pub notifications_ready: bool,
    pub admin_token_configured: bool,
    pub store_auth_ready: bool,
    pub store_auth_mode: String,
    pub store_auth_error: String,
    pub daemon_ready: bool,
    pub active_port: u16,
    pub bridge_catalog_url: String,
    pub bridge_registered: bool,
    pub proxy_url: String,
}

pub fn host_env_proxy_url() -> String {
    for key in [
        "MEDIARY_PLUGIN_PROXY_URL",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
    ] {
        if let Ok(val) = env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

/// Returns `(plugin_enabled, pending_scope_approval, granted_scopes, secret_admin_token)`
pub fn inspect_local_plugin_state(data_dir: &Path) -> (bool, bool, Vec<String>, String) {
    let mut plugin_enabled = env::var("MEDIARY_PLUGIN_ENABLED")
        .ok()
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(true);
    let mut pending_scope = false;
    let mut scopes = Vec::new();
    let mut secret_admin = env::var("MEDIARY_ADMIN_API_TOKEN")
        .unwrap_or_default()
        .trim()
        .to_string();

    let cfg_path = env::var_os("MEDIARY_PLUGIN_CONFIG_PATH")
        .map(PathBuf::from)
        .or_else(|| data_dir.parent().map(|p| p.join("config.json")));

    if let Some(p) = cfg_path.filter(|p| p.is_file()) {
        if let Ok(raw) = fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(b) = v.get("enabled").and_then(Value::as_bool) {
                    plugin_enabled = b;
                }
                if let Some(b) = v.get("pending_scope_approval").and_then(Value::as_bool) {
                    pending_scope = b;
                }
                if let Some(arr) = v.get("granted_scopes").and_then(Value::as_array) {
                    for sc in arr.iter().filter_map(Value::as_str) {
                        scopes.push(sc.trim().to_string());
                    }
                }
                if secret_admin.is_empty() {
                    if let Some(tok) = v
                        .get("secrets")
                        .and_then(|s| s.get("admin_api_token"))
                        .and_then(Value::as_str)
                    {
                        secret_admin = tok.trim().to_string();
                    }
                }
            }
        }
    }

    (plugin_enabled, pending_scope, scopes, secret_admin)
}

pub fn probe_daemon_health(data_dir: &Path, engine: &EngineSettings) -> (bool, u16) {
    let mut ports = Vec::new();
    if let Some(st) = db::read_daemon_state(data_dir) {
        if st.port > 0 {
            ports.push(st.port);
        }
    }
    if engine.listen_port > 0 && !ports.contains(&engine.listen_port) {
        ports.push(engine.listen_port);
    }

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(650))
        .redirects(0)
        .build();

    for port in ports {
        let url = format!("http://127.0.0.1:{port}/health");
        if let Ok(resp) = agent.get(&url).call() {
            if let Ok(raw) = resp.into_string() {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    if v["ok"].as_bool() == Some(true)
                        && v["service"].as_str() == Some("mediplug")
                    {
                        return (true, port);
                    }
                }
            }
        }
    }

    (false, engine.listen_port)
}

pub struct HostClient {
    base_url: Url,
    plugin_token: String,
    admin_token: String,
    agent: ureq::Agent,
}

impl HostClient {
    pub fn from_env_with_admin(data_dir: &Path, configured_admin_token: &str) -> Option<Self> {
        let raw_url = env::var("MEDIARY_PLUGIN_API_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:8118/api".to_string());
        let plugin_token = env::var("MEDIARY_PLUGIN_TOKEN")
            .unwrap_or_default()
            .trim()
            .to_string();
        let (_, _, _, secret_admin) = inspect_local_plugin_state(data_dir);
        let admin_token = if !configured_admin_token.trim().is_empty() {
            configured_admin_token.trim().to_string()
        } else {
            secret_admin
        };

        if plugin_token.is_empty() && admin_token.is_empty() && env::var_os("MEDIARY_PLUGIN_API_URL").is_none() {
            return None;
        }

        let base_url = Url::parse(&format!("{}/", raw_url.trim().trim_end_matches('/'))).ok()?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .redirects(0)
            .build();

        Some(Self {
            base_url,
            plugin_token,
            admin_token,
            agent,
        })
    }

    pub fn has_admin_token(&self) -> bool {
        !self.admin_token.is_empty()
    }

    fn request_with_token(
        &self,
        token: &str,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
        timeout_secs: u64,
    ) -> std::result::Result<Value, (u16, String)> {
        let full = format!(
            "{}/{}",
            self.base_url.as_str().trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut req = self
            .agent
            .request(method, &full)
            .timeout(Duration::from_secs(timeout_secs.max(2)));
        if !token.trim().is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", token.trim()));
        }
        for (k, v) in query {
            req = req.query(k, v);
        }
        let resp = match body {
            Some(payload) => req.send_json(payload.clone()),
            None => req.call(),
        };
        match resp {
            Ok(r) => {
                let raw = r.into_string().unwrap_or_default();
                let text = if raw.trim().is_empty() { "{}" } else { &raw };
                serde_json::from_str(text)
                    .map_err(|e| (500, format!("解析宿主 JSON 响应失败: {e}")))
            }
            Err(ureq::Error::Status(code, r)) => {
                let raw = r.into_string().unwrap_or_default();
                let snippet = raw.chars().take(240).collect::<String>();
                Err((code, snippet))
            }
            Err(e) => Err((0, format!("请求宿主 API 失败: {e}"))),
        }
    }

    /// Calls plugin-scoped endpoints (`/api/plugin/*`, `/api/settings`), preferring `MEDIARY_PLUGIN_TOKEN`.
    pub fn call_plugin_json(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        timeout_secs: u64,
    ) -> Result<Value> {
        let mut tokens = Vec::new();
        if !self.plugin_token.is_empty() {
            tokens.push(self.plugin_token.as_str());
        }
        if !self.admin_token.is_empty() && self.admin_token != self.plugin_token {
            tokens.push(self.admin_token.as_str());
        }
        if tokens.is_empty() {
            tokens.push("");
        }

        let mut last_err = (0u16, "未配置宿主 API 令牌".to_string());
        for tok in tokens {
            match self.request_with_token(tok, method, path, query, body.as_ref(), timeout_secs) {
                Ok(v) => return Ok(v),
                Err((code, msg)) if matches!(code, 401 | 403) => {
                    last_err = (code, msg);
                    continue;
                }
                Err(other) => {
                    last_err = other;
                    break;
                }
            }
        }
        let (code, msg) = last_err;
        if code > 0 {
            Err(anyhow!("HTTP {code}: {msg}"))
        } else {
            Err(anyhow!("{msg}"))
        }
    }

    /// Calls admin endpoints (`/api/plugin-store/*`, `/api/plugins*`), preferring `admin_api_token`
    /// with transparent fallback to `MEDIARY_PLUGIN_TOKEN`.
    pub fn call_store_json(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        timeout_secs: u64,
    ) -> std::result::Result<(Value, &'static str), (u16, String)> {
        let mut candidates: Vec<(&str, &'static str)> = Vec::new();
        if !self.admin_token.is_empty() {
            candidates.push((self.admin_token.as_str(), "admin_token"));
        }
        if !self.plugin_token.is_empty() && self.plugin_token != self.admin_token {
            candidates.push((self.plugin_token.as_str(), "plugin_token"));
        }
        if candidates.is_empty() {
            return Err((
                401,
                "尚未配置 Mediary 管理员 API Token（请在 Mediary「设置 → 安全」查看并填入工作台「全局设置」）"
                    .to_string(),
            ));
        }

        let mut last_err = (401u16, String::new());
        for (tok, mode) in candidates {
            match self.request_with_token(tok, method, path, query, body.as_ref(), timeout_secs) {
                Ok(v) => return Ok((v, mode)),
                Err((code, msg)) if matches!(code, 401 | 403) => {
                    last_err = (code, msg);
                    continue;
                }
                Err(other) => return Err(other),
            }
        }

        let hint = if self.admin_token.is_empty() {
            "管理宿主 /api/plugin-store/sources 需使用管理员 API Token（请在 Mediary「设置 → 安全」复制 API Token 并填入 MediPlug 工作台）"
        } else {
            "管理员 API Token 校验未通过（HTTP 401/403），请在 Mediary「设置 → 安全」核对最新 API Token"
        };
        Err((
            last_err.0,
            if last_err.1.trim().is_empty() {
                hint.to_string()
            } else {
                format!("{hint}（宿主返回: {}）", last_err.1)
            },
        ))
    }

    pub fn host_proxy_url(&self) -> String {
        let env_p = host_env_proxy_url();
        if !env_p.is_empty() {
            return env_p;
        }
        if let Ok(v) = self.call_plugin_json("GET", "/settings", &[], None, 4) {
            for k in ["github_proxy_url", "http_proxy", "proxy_url", "tmdb_proxy_url"] {
                if let Some(p) = v.get(k).and_then(Value::as_str) {
                    if !p.trim().is_empty() {
                        return p.trim().to_string();
                    }
                }
            }
        }
        String::new()
    }

    pub fn inspect_status(&self, data_dir: &Path, engine: &EngineSettings) -> (HostStatus, Vec<HostCatalogSource>) {
        let (plugin_enabled, pending_scope, scopes, _) = inspect_local_plugin_state(data_dir);
        let (daemon_ready, active_port) = probe_daemon_health(data_dir, engine);
        let base_bridge = engine.effective_bridge_base_url(active_port);
        let bridge_catalog_url = format!("{base_bridge}/catalog.json");

        let has_scope = |name: &str| scopes.iter().any(|s| s == name);
        let mut st = HostStatus {
            plugin_enabled,
            connected: true,
            pending_scope_approval: pending_scope,
            plugins_read_ready: has_scope("plugins:read"),
            settings_read_ready: has_scope("settings:read"),
            notifications_ready: has_scope("notifications:send"),
            admin_token_configured: self.has_admin_token(),
            store_auth_ready: false,
            store_auth_mode: String::new(),
            store_auth_error: String::new(),
            daemon_ready,
            active_port,
            bridge_catalog_url: bridge_catalog_url.clone(),
            bridge_registered: false,
            proxy_url: self.host_proxy_url(),
        };

        if self.call_plugin_json("GET", "/plugin/plugins", &[], None, 4).is_ok() {
            st.plugins_read_ready = true;
        }

        let mut sources = Vec::new();
        match self.list_store_sources_with_mode(&base_bridge, &engine.unified_source_id) {
            Ok((list, mode)) => {
                st.store_auth_ready = true;
                st.store_auth_mode = mode.to_string();
                st.bridge_registered = list.iter().any(|s| {
                    s.enabled
                        && (s.url.trim_end_matches('/') == bridge_catalog_url.trim_end_matches('/')
                            || s.id == engine.unified_source_id)
                });
                sources = list;
            }
            Err((_code, msg)) => {
                st.store_auth_error = msg;
            }
        }

        (st, sources)
    }

    pub fn list_store_sources(
        &self,
        bridge_base_url: &str,
        unified_source_id: &str,
    ) -> Result<Vec<HostCatalogSource>> {
        self.list_store_sources_with_mode(bridge_base_url, unified_source_id)
            .map(|(list, _)| list)
            .map_err(|(_, msg)| anyhow!("{msg}"))
    }

    fn list_store_sources_with_mode(
        &self,
        bridge_base_url: &str,
        unified_source_id: &str,
    ) -> std::result::Result<(Vec<HostCatalogSource>, &'static str), (u16, String)> {
        let (v, mode) = self.call_store_json("GET", "/plugin-store/sources", &[], None, 8)?;
        let arr = v
            .get("sources")
            .and_then(Value::as_array)
            .or_else(|| v.get("items").and_then(Value::as_array))
            .or_else(|| v.get("data").and_then(Value::as_array))
            .or_else(|| v.as_array())
            .cloned()
            .unwrap_or_default();

        let clean_base = bridge_base_url.trim_end_matches('/');
        let mut out = Vec::with_capacity(arr.len());
        for row in arr {
            let id = row["id"]
                .as_str()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                continue;
            }
            let url = row["url"]
                .as_str()
                .or_else(|| row["catalog_url"].as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            let builtin = id == "official"
                || row["builtin"].as_bool() == Some(true)
                || row["official"].as_bool() == Some(true);
            let enabled = row
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let is_mediplug_bridge = id == unified_source_id
                || (!clean_base.is_empty() && url.starts_with(clean_base))
                || (url.contains("127.0.0.1:") && (url.ends_with("/catalog.json") || url.contains("/catalog/")))
                || (url.contains("gist.githubusercontent.com") && url.contains("mediplug-"));

            out.push(HostCatalogSource {
                name: row["name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(&id)
                    .trim()
                    .to_string(),
                id,
                url,
                enabled,
                builtin,
                plugin_count: row["plugin_count"]
                    .as_u64()
                    .or_else(|| row["plugins_count"].as_u64())
                    .unwrap_or(0) as u32,
                error: row["error"]
                    .as_str()
                    .or_else(|| row["last_error"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                is_mediplug_bridge,
            });
        }
        Ok((out, mode))
    }

    pub fn upsert_store_source(
        &self,
        raw_id: &str,
        name: &str,
        url: &str,
        enabled: bool,
    ) -> Result<Value> {
        let id = sanitize_source_id(raw_id);
        ensure!(!id.is_empty(), "插件源 ID 不能为空（仅限字母、数字、连字符）");
        ensure!(
            id != "official",
            "official 为 Mediary 官方内置保护命名空间，禁止修改或覆盖"
        );
        let clean_url = url.trim();
        ensure!(
            clean_url.starts_with("http://") || clean_url.starts_with("https://"),
            "插件源 URL 必须以 http:// 或 https:// 开头"
        );
        Url::parse(clean_url).context("插件源 URL 格式不合法")?;
        let clean_name = if name.trim().is_empty() {
            id.as_str()
        } else {
            name.trim()
        };

        let payload = json!({
            "id": id,
            "name": clean_name,
            "url": clean_url,
            "enabled": enabled,
        });

        let existing = self
            .list_store_sources("", "")
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.id == id);

        if existing.is_none() {
            let (res, _) = self
                .call_store_json("POST", "/plugin-store/sources", &[], Some(payload), 10)
                .map_err(|(_, msg)| anyhow!("{msg}"))?;
            return Ok(res);
        }

        let encoded: String = form_urlencoded::byte_serialize(id.as_bytes()).collect();
        let put_path = format!("/plugin-store/sources/{encoded}");

        // 1. Try PUT /api/plugin-store/sources/:id if supported by host
        if let Ok((res, _)) =
            self.call_store_json("PUT", &put_path, &[], Some(payload.clone()), 10)
        {
            if let Ok(after) = self.list_store_sources("", "") {
                if after.iter().any(|s| {
                    s.id == id && s.url == clean_url && s.name == clean_name && s.enabled == enabled
                }) {
                    return Ok(res);
                }
            }
        }

        // 2. Try POST /api/plugin-store/sources (upsert style)
        if let Ok((res, _)) =
            self.call_store_json("POST", "/plugin-store/sources", &[], Some(payload.clone()), 10)
        {
            if let Ok(after) = self.list_store_sources("", "") {
                if after.iter().any(|s| {
                    s.id == id && s.url == clean_url && s.name == clean_name && s.enabled == enabled
                }) {
                    return Ok(res);
                }
            }
        }

        // 3. Fallback for strict INSERT-only POST + DELETE hosts: DELETE then POST
        let _ = self
            .call_store_json("DELETE", &put_path, &[], None, 8)
            .map_err(|(_, msg)| anyhow!("{msg}"))?;
        let (res, _) = self
            .call_store_json("POST", "/plugin-store/sources", &[], Some(payload), 10)
            .map_err(|(_, msg)| anyhow!("{msg}"))?;
        Ok(res)
    }

    pub fn delete_store_source(&self, raw_id: &str) -> Result<Value> {
        let id = raw_id.trim();
        ensure!(!id.is_empty(), "缺少要删除的插件源 ID");
        if id == "official" {
            bail!("official 为官方内置源，禁止删除");
        }
        let encoded: String = form_urlencoded::byte_serialize(id.as_bytes()).collect();
        let (res, _) = self
            .call_store_json(
                "DELETE",
                &format!("/plugin-store/sources/{encoded}"),
                &[],
                None,
                10,
            )
            .map_err(|(_, msg)| anyhow!("{msg}"))?;
        Ok(res)
    }

    pub fn list_installed_map(&self) -> HashMap<String, (String, bool)> {
        let mut map = HashMap::new();
        let v = self
            .call_plugin_json("GET", "/plugin/plugins", &[], None, 6)
            .ok()
            .or_else(|| {
                self.call_store_json("GET", "/plugins", &[], None, 6)
                    .ok()
                    .map(|(x, _)| x)
            });
        let Some(v) = v else {
            return map;
        };
        let arr = v
            .get("plugins")
            .and_then(Value::as_array)
            .or_else(|| v.get("items").and_then(Value::as_array))
            .or_else(|| v.as_array())
            .cloned()
            .unwrap_or_default();
        for row in arr {
            let id = row["id"].as_str().unwrap_or_default().trim().to_string();
            if id.is_empty() {
                continue;
            }
            let ver = row["version"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            let en = row["enabled"].as_bool() == Some(true);
            map.insert(id, (ver, en));
        }
        map
    }

    pub fn fetch_plugin_store(&self) -> Result<HostStoreSnapshot> {
        let installed_map = self.list_installed_map();
        let (v, _) = self
            .call_store_json("GET", "/plugin-store", &[], None, 15)
            .map_err(|(_, msg)| anyhow!("{msg}"))?;

        let arr = v
            .get("plugins")
            .and_then(Value::as_array)
            .or_else(|| v.get("items").and_then(Value::as_array))
            .or_else(|| v.as_array())
            .cloned()
            .unwrap_or_default();
        let conflicts = v
            .get("conflicts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let raw_sources = v
            .get("sources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut plugins = Vec::with_capacity(arr.len());
        let mut updates_available = 0usize;

        for row in arr {
            let id = row["id"].as_str().unwrap_or_default().trim().to_string();
            if id.is_empty() {
                continue;
            }
            let catalog_ver = row["version"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            let (inst_from_map, en_from_map) = installed_map
                .get(&id)
                .cloned()
                .unwrap_or_else(|| (String::new(), false));
            let installed_ver = row["installed_version"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::trim)
                .map(str::to_string)
                .unwrap_or(inst_from_map);
            let installed = row["installed"].as_bool().unwrap_or(!installed_ver.is_empty());
            let enabled = row["enabled"].as_bool().unwrap_or(en_from_map);
            let update_available = row["update_available"]
                .as_bool()
                .or_else(|| row["has_update"].as_bool())
                .unwrap_or_else(|| {
                    installed
                        && !installed_ver.is_empty()
                        && !catalog_ver.is_empty()
                        && version_is_newer(&catalog_ver, &installed_ver)
                });
            if update_available {
                updates_available += 1;
            }

            let source_id = row["source_id"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            let official = row["official"]
                .as_bool()
                .unwrap_or_else(|| source_id == "official");
            let perms = row["permissions"]
                .as_array()
                .or_else(|| row["requested_scopes"].as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();

            plugins.push(HostStorePluginView {
                name: row["name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(&id)
                    .to_string(),
                id,
                version: catalog_ver,
                installed_version: installed_ver,
                installed,
                enabled,
                update_available,
                description: row["description"].as_str().unwrap_or_default().to_string(),
                author: row["author"].as_str().unwrap_or_default().to_string(),
                homepage: row["homepage"].as_str().unwrap_or_default().to_string(),
                source: row["source"].as_str().unwrap_or_default().to_string(),
                source_name: row["source_name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(if official { "官方源" } else { &source_id })
                    .to_string(),
                source_id,
                official,
                min_mediary_version: row["min_mediary_version"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                permissions: perms,
            });
        }

        Ok(HostStoreSnapshot {
            plugins,
            conflicts,
            raw_sources,
            updates_available,
        })
    }

    pub fn install_store_plugin(&self, plugin_id: &str) -> Result<Value> {
        let id = plugin_id.trim();
        ensure!(!id.is_empty(), "缺少要安装的 plugin_id");
        let encoded: String = form_urlencoded::byte_serialize(id.as_bytes()).collect();
        let (res, _) = self
            .call_store_json(
                "POST",
                &format!("/plugin-store/{encoded}/install"),
                &[],
                Some(json!({})),
                120,
            )
            .map_err(|(_, msg)| anyhow!("{msg}"))?;
        Ok(res)
    }

    pub fn notify(&self, title: &str, content: &str) -> Result<()> {
        let payload = json!({
            "title": title.trim(),
            "content": content.trim(),
        });
        self.call_plugin_json("POST", "/plugin/notifications", &[], Some(payload), 8)
            .context("提交主程序通知失败（请确认已授予 notifications:send 权限且宿主已配置通知渠道）")?;
        Ok(())
    }
}

pub fn fast_host_status(data_dir: &Path, engine: &EngineSettings) -> HostStatus {
    let (plugin_enabled, pending_scope, scopes, secret_admin) = inspect_local_plugin_state(data_dir);
    let (daemon_ready, active_port) = probe_daemon_health(data_dir, engine);
    let base_bridge = engine.effective_bridge_base_url(active_port);
    HostStatus {
        plugin_enabled,
        connected: env::var_os("MEDIARY_PLUGIN_API_URL").is_some(),
        pending_scope_approval: pending_scope,
        plugins_read_ready: scopes.iter().any(|s| s == "plugins:read"),
        settings_read_ready: scopes.iter().any(|s| s == "settings:read"),
        notifications_ready: scopes.iter().any(|s| s == "notifications:send"),
        admin_token_configured: !engine.admin_api_token.is_empty() || !secret_admin.is_empty(),
        store_auth_ready: false,
        store_auth_mode: String::new(),
        store_auth_error: String::new(),
        daemon_ready,
        active_port,
        bridge_catalog_url: format!("{base_bridge}/catalog.json"),
        bridge_registered: false,
        proxy_url: host_env_proxy_url(),
    }
}

pub fn version_is_newer(candidate: &str, current: &str) -> bool {
    let parse_nums = |s: &str| -> Vec<u64> {
        s.trim()
            .trim_start_matches(['v', 'V'])
            .split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse::<u64>().ok())
            .collect()
    };
    let a = parse_nums(candidate);
    let b = parse_nums(current);
    let max_len = a.len().max(b.len());
    for i in 0..max_len {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        if av != bv {
            return av > bv;
        }
    }
    candidate.trim().trim_start_matches(['v', 'V']) != current.trim().trim_start_matches(['v', 'V'])
        && a.is_empty()
}

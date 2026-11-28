use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_LISTEN_PORT: u16 = 18119;
pub const DEFAULT_UNIFIED_SOURCE_ID: &str = "mediplug-github-bridge";
pub const DEFAULT_UNIFIED_SOURCE_NAME: &str = "MediPlug GitHub Release 聚合源";
pub const MAX_UNPACKED_ARCHIVE_BYTES: u64 = 250 * 1024 * 1024;

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_datetime_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub fn flex_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?.unwrap_or(Value::Null);
    Ok(match v {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    })
}

pub fn flex_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?.unwrap_or(Value::Null);
    Ok(match v {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
            .unwrap_or(0),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map(|f| f.max(0.0) as u64)
            .unwrap_or(0),
        _ => 0,
    })
}

pub fn flex_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(flex_u64(deserializer)?.min(u32::MAX as u64) as u32)
}

pub fn flex_u16<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(flex_u64(deserializer)?.min(u16::MAX as u64) as u16)
}

pub fn mask_secret(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 10 {
        return "******".to_string();
    }
    let head: String = chars[..5].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}******{tail}")
}

pub fn is_masked_or_empty(s: &str) -> bool {
    let t = s.trim();
    t.is_empty() || t.contains("******")
}

pub fn sanitize_source_id(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if (ch == '-' || ch == '_' || ch == '/' || ch == '.' || ch == ' ') && !out.is_empty() {
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        }
    }
    let cleaned = out.trim_matches('-').to_string();
    if cleaned.len() > 60 {
        cleaned[..60].trim_matches('-').to_string()
    } else {
        cleaned
    }
}

pub fn is_valid_sha256(s: &str) -> bool {
    let t = s.trim();
    t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSettings {
    #[serde(rename = "$enabled")]
    pub enabled: bool,
    pub auto_sync_releases: bool,
    pub auto_register_bridge: bool,
    pub notify: bool,
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_sync_releases: true,
            auto_register_bridge: true,
            notify: true,
        }
    }
}

/// Exact Mediary v3.4.6 ~ v3.4.8+ `catalog.json` document format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogDocument {
    pub schema_version: u32,
    #[serde(deserialize_with = "null_default")]
    pub plugins: Vec<CatalogPluginEntry>,
}

impl Default for CatalogDocument {
    fn default() -> Self {
        Self {
            schema_version: 1,
            plugins: Vec::new(),
        }
    }
}

/// Exact Mediary v3.4.6 ~ v3.4.8+ `catalog.json` plugin entry format.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogPluginEntry {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub version: String,
    #[serde(deserialize_with = "null_default")]
    pub description: String,
    #[serde(deserialize_with = "null_default")]
    pub author: String,
    #[serde(deserialize_with = "null_default")]
    pub homepage: String,
    #[serde(deserialize_with = "null_default")]
    pub source: String,
    #[serde(deserialize_with = "null_default")]
    pub license: String,
    #[serde(deserialize_with = "flex_u32")]
    pub api_version: u32,
    #[serde(default, skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub min_mediary_version: String,
    #[serde(deserialize_with = "null_default")]
    pub permissions: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub artifacts: BTreeMap<String, CatalogArtifact>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogArtifact {
    #[serde(deserialize_with = "null_default")]
    pub url: String,
    #[serde(deserialize_with = "null_default")]
    pub sha256: String,
    #[serde(deserialize_with = "flex_u64")]
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestRuntime {
    #[serde(deserialize_with = "null_default")]
    pub entrypoint: String,
    #[serde(deserialize_with = "null_default")]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestPlaybackHook {
    #[serde(deserialize_with = "null_default")]
    pub transport: String,
    #[serde(deserialize_with = "null_default")]
    pub socket: String,
    #[serde(deserialize_with = "null_default")]
    pub endpoint: String,
    #[serde(deserialize_with = "null_default")]
    pub path: String,
    pub timeout_ms: Option<u64>,
    pub priority: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestResourceSearch {
    pub source_id: Option<i64>,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub action: String,
    pub transfer_action: Option<String>,
    pub default_selected: Option<bool>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestUi {
    #[serde(deserialize_with = "null_default")]
    pub dir: String,
    #[serde(deserialize_with = "null_default")]
    pub entry: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PluginCapability {
    pub id: String,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ParsedPluginManifest {
    #[serde(deserialize_with = "flex_id")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "flex_id")]
    pub version: String,
    #[serde(deserialize_with = "null_default")]
    pub description: String,
    #[serde(deserialize_with = "null_default")]
    pub author: String,
    #[serde(deserialize_with = "null_default")]
    pub homepage: String,
    #[serde(deserialize_with = "null_default")]
    pub source: String,
    #[serde(deserialize_with = "null_default")]
    pub license: String,
    #[serde(deserialize_with = "flex_u32")]
    pub api_version: u32,
    #[serde(deserialize_with = "null_default")]
    pub min_mediary_version: String,
    #[serde(deserialize_with = "null_default")]
    pub requested_scopes: Vec<String>,
    pub runtime: Option<ManifestRuntime>,
    pub action_runtime: Option<ManifestRuntime>,
    pub playback_hook: Option<ManifestPlaybackHook>,
    pub resource_search: Option<ManifestResourceSearch>,
    pub ui: Option<ManifestUi>,
    #[serde(deserialize_with = "null_default")]
    pub scheduled_actions: Vec<Value>,
}

impl ParsedPluginManifest {
    pub fn capabilities(&self) -> Vec<PluginCapability> {
        let mut caps = Vec::new();
        if let Some(ref hook) = self.playback_hook {
            let transport = hook.transport.trim().to_ascii_lowercase();
            let target = if transport == "http" {
                hook.endpoint.trim()
            } else {
                hook.socket.trim()
            };
            let path = if hook.path.trim().is_empty() {
                "/resolve"
            } else {
                hook.path.trim()
            };
            let timeout_str = hook
                .timeout_ms
                .map(|ms| format!(" · {ms}ms"))
                .unwrap_or_default();
            caps.push(PluginCapability {
                id: "playback_hook".to_string(),
                label: "playback_hook (+/prefetch)".to_string(),
                detail: format!(
                    "{transport}: {target} · {path}{timeout_str}（v3.4.8+ 同通道自动转发 POST /prefetch，4s 超时、404 自动回退）"
                ),
            });
        }
        if let Some(ref rs) = self.resource_search {
            let sid = rs
                .source_id
                .map(|v| v.to_string())
                .unwrap_or_else(|| "未设置".to_string());
            let act = rs.action.trim();
            let transfer = rs
                .transfer_action
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("-");
            let timeout_str = rs
                .timeout_seconds
                .map(|s| format!(" · {s}s"))
                .unwrap_or_default();
            caps.push(PluginCapability {
                id: "resource_search".to_string(),
                label: "resource_search".to_string(),
                detail: format!("source_id={sid} · action={act} · transfer={transfer}{timeout_str}"),
            });
        }
        if let Some(ref ui) = self.ui {
            let dir = if ui.dir.trim().is_empty() {
                "./ui"
            } else {
                ui.dir.trim()
            };
            let entry = if ui.entry.trim().is_empty() {
                "index.html"
            } else {
                ui.entry.trim()
            };
            caps.push(PluginCapability {
                id: "ui".to_string(),
                label: "ui".to_string(),
                detail: format!("工作台 {dir}/{entry}"),
            });
        }
        if !self.scheduled_actions.is_empty() {
            caps.push(PluginCapability {
                id: "scheduled_actions".to_string(),
                label: format!("定时任务({})", self.scheduled_actions.len()),
                detail: format!("声明 {} 项宿主定时任务", self.scheduled_actions.len()),
            });
        }
        if let Some(ref rt) = self.runtime {
            if !rt.entrypoint.trim().is_empty() {
                caps.push(PluginCapability {
                    id: "runtime".to_string(),
                    label: "runtime".to_string(),
                    detail: format!("常驻后台进程 {}", rt.entrypoint.trim()),
                });
            }
        }
        caps
    }

    pub fn compat_note(&self) -> String {
        let min_ver = if self.min_mediary_version.trim().is_empty() {
            "3.4.6"
        } else {
            self.min_mediary_version.trim()
        };
        let mut notes = vec![format!("兼容 Mediary v{min_ver} ~ v3.4.8+")];
        if self.playback_hook.is_some() {
            notes.push("支持 v3.4.8+ playback_hook 同通道 POST /prefetch 预热转发（4s 独立超时、404 自动回退）".to_string());
        }
        if self.resource_search.is_some() {
            notes.push("符合 v3.4.8 resource_search 扩展规范（action_runtime + source_id <= -2）".to_string());
        }
        notes.join(" · ")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveInspection {
    pub filename: String,
    pub arch: String,
    pub compressed_size: u64,
    pub unpacked_size: u64,
    pub sha256: String,
    pub has_plugin_json: bool,
    pub has_plugin_bin: bool,
    pub plugin_bin_executable: bool,
    pub forbidden_entries: Vec<String>,
    pub valid_for_mediary_346: bool,
    pub valid_for_mediary_348: bool,
    pub compat_note: String,
    pub capabilities: Vec<PluginCapability>,
    pub issues: Vec<String>,
    pub warnings: Vec<String>,
    pub manifest: Option<ParsedPluginManifest>,
    pub catalog_entry: Option<CatalogPluginEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GithubRepoBridge {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    /// Normalized `owner/repo`
    #[serde(deserialize_with = "null_default")]
    pub repo: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    /// Optional pinned tag; empty means latest release
    #[serde(deserialize_with = "null_default")]
    pub pinned_tag: String,
    pub include_prerelease: bool,
    /// `"unified"` (merged into `/catalog.json`) or `"dedicated"` (registered as `/catalog/<id>.json`)
    #[serde(deserialize_with = "null_default")]
    pub register_mode: String,
    #[serde(deserialize_with = "null_default")]
    pub asset_keyword: String,
    pub enabled: bool,
    #[serde(deserialize_with = "null_default")]
    pub last_release_tag: String,
    #[serde(deserialize_with = "null_default")]
    pub last_release_url: String,
    pub last_synced_at: i64,
    #[serde(deserialize_with = "null_default")]
    pub last_error: String,
    #[serde(deserialize_with = "null_default")]
    pub plugins: Vec<CatalogPluginEntry>,
}

impl Default for GithubRepoBridge {
    fn default() -> Self {
        Self {
            id: String::new(),
            repo: String::new(),
            name: String::new(),
            pinned_tag: String::new(),
            include_prerelease: false,
            register_mode: "unified".to_string(),
            asset_keyword: String::new(),
            enabled: true,
            last_release_tag: String::new(),
            last_release_url: String::new(),
            last_synced_at: 0,
            last_error: String::new(),
            plugins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    #[serde(deserialize_with = "null_default")]
    pub admin_api_token: String,
    #[serde(deserialize_with = "null_default")]
    pub listen_host: String,
    #[serde(deserialize_with = "flex_u16")]
    pub listen_port: u16,
    #[serde(deserialize_with = "null_default")]
    pub public_base_url: String,
    #[serde(deserialize_with = "null_default")]
    pub unified_source_id: String,
    #[serde(deserialize_with = "null_default")]
    pub unified_source_name: String,
    #[serde(deserialize_with = "null_default")]
    pub github_token: String,
    #[serde(deserialize_with = "null_default")]
    pub proxy_url: String,
    #[serde(deserialize_with = "null_default")]
    pub github_mirror: String,
    pub deep_inspect_archive: bool,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            admin_api_token: String::new(),
            listen_host: "127.0.0.1".to_string(),
            listen_port: DEFAULT_LISTEN_PORT,
            public_base_url: String::new(),
            unified_source_id: DEFAULT_UNIFIED_SOURCE_ID.to_string(),
            unified_source_name: DEFAULT_UNIFIED_SOURCE_NAME.to_string(),
            github_token: String::new(),
            proxy_url: String::new(),
            github_mirror: String::new(),
            deep_inspect_archive: true,
        }
    }
}

impl EngineSettings {
    pub fn normalized(mut self) -> Self {
        let host = self.listen_host.trim();
        self.listen_host = if matches!(host, "0.0.0.0" | "127.0.0.1" | "::1" | "localhost") {
            host.to_string()
        } else {
            "127.0.0.1".to_string()
        };
        if self.listen_port < 1024 {
            self.listen_port = DEFAULT_LISTEN_PORT;
        }
        self.public_base_url = self.public_base_url.trim().trim_end_matches('/').to_string();
        let sid = sanitize_source_id(&self.unified_source_id);
        self.unified_source_id = if sid.is_empty() || sid == "official" {
            DEFAULT_UNIFIED_SOURCE_ID.to_string()
        } else {
            sid
        };
        if self.unified_source_name.trim().is_empty() {
            self.unified_source_name = DEFAULT_UNIFIED_SOURCE_NAME.to_string();
        } else {
            self.unified_source_name = self.unified_source_name.trim().to_string();
        }
        self.proxy_url = self.proxy_url.trim().to_string();
        self.github_mirror = self.github_mirror.trim().to_string();
        self.admin_api_token = self.admin_api_token.trim().to_string();
        self.github_token = self.github_token.trim().to_string();
        self
    }

    pub fn effective_bridge_base_url(&self, actual_port: u16) -> String {
        if !self.public_base_url.is_empty() {
            return self.public_base_url.clone();
        }
        let port = if actual_port > 0 {
            actual_port
        } else {
            self.listen_port
        };
        format!("http://127.0.0.1:{port}")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub engine: EngineSettings,
    #[serde(deserialize_with = "null_default")]
    pub github_repos: Vec<GithubRepoBridge>,
    /// Optional manually-hosted catalog entries built via the Builder workbench tab
    #[serde(deserialize_with = "null_default")]
    pub hosted_entries: Vec<CatalogPluginEntry>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonRuntimeState {
    pub pid: u32,
    pub listen_host: String,
    pub port: u16,
    pub started_at: i64,
    pub version: String,
    #[serde(deserialize_with = "null_default")]
    pub shutdown_token: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostCatalogSource {
    #[serde(deserialize_with = "flex_id")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(alias = "catalog_url", deserialize_with = "null_default")]
    pub url: String,
    pub enabled: bool,
    pub builtin: bool,
    #[serde(deserialize_with = "flex_u32")]
    pub plugin_count: u32,
    #[serde(deserialize_with = "null_default")]
    pub error: String,
    #[serde(deserialize_with = "null_default")]
    pub is_mediplug_bridge: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostStorePluginView {
    #[serde(deserialize_with = "flex_id")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "flex_id")]
    pub version: String,
    #[serde(deserialize_with = "flex_id")]
    pub installed_version: String,
    pub installed: bool,
    pub enabled: bool,
    pub update_available: bool,
    #[serde(deserialize_with = "null_default")]
    pub description: String,
    #[serde(deserialize_with = "null_default")]
    pub author: String,
    #[serde(deserialize_with = "null_default")]
    pub homepage: String,
    #[serde(deserialize_with = "null_default")]
    pub source: String,
    #[serde(deserialize_with = "flex_id")]
    pub source_id: String,
    #[serde(deserialize_with = "null_default")]
    pub source_name: String,
    pub official: bool,
    #[serde(deserialize_with = "null_default")]
    pub min_mediary_version: String,
    #[serde(deserialize_with = "null_default")]
    pub permissions: Vec<String>,
    pub compatible: Option<bool>,
    pub available_for_platform: Option<bool>,
    pub playback_hook_approved: Option<bool>,
    #[serde(deserialize_with = "null_default")]
    pub capabilities: Vec<PluginCapability>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostStoreSnapshot {
    pub plugins: Vec<HostStorePluginView>,
    pub conflicts: Vec<Value>,
    pub raw_sources: Vec<Value>,
    pub updates_available: usize,
}

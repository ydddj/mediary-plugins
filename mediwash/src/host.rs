use crate::category;
use crate::db;
use crate::models::{flex_f64, flex_id, flex_u32, flex_u64, now_ts, null_default};
use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::time::Duration;
use url::Url;

#[derive(Debug)]
pub struct HttpStatusError(pub u16, pub String);

impl std::fmt::Display for HttpStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.0, self.1)
    }
}
impl std::error::Error for HttpStatusError {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostStatus {
    pub open_api_connected: bool,
    pub web_cookie_ready: bool,
    pub flowlink_enabled: bool,
    pub proxy_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibrarySource {
    #[serde(deserialize_with = "flex_id", alias = "source_id")]
    pub id: String,
    #[serde(deserialize_with = "null_default", alias = "file_name")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub strm_path: String,
    #[serde(deserialize_with = "null_default")]
    pub cloud_path: String,
    #[serde(deserialize_with = "null_default")]
    pub file_path: String,
    #[serde(deserialize_with = "null_default")]
    pub stream_url: String,
    #[serde(deserialize_with = "flex_f64", alias = "file_size")]
    pub size: f64,
    #[serde(deserialize_with = "flex_f64")]
    pub bitrate: f64,
    #[serde(deserialize_with = "flex_id")]
    pub file_id: String,
    #[serde(deserialize_with = "null_default", alias = "pickcode")]
    pub pick_code: String,
    #[serde(deserialize_with = "null_default")]
    pub sha1: String,
}

impl LibrarySource {
    pub fn effective_pick_code(&self) -> String {
        if !self.pick_code.trim().is_empty() {
            return self.pick_code.trim().to_string();
        }
        if let Some((_, query)) = self.stream_url.split_once('?') {
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if matches!(k.trim(), "pickcode" | "pick_code") && !v.trim().is_empty() {
                        return v.trim().to_string();
                    }
                }
            }
        }
        String::new()
    }

    /// Returns `true` when two `LibrarySource` entries point to the exact same underlying
    /// 115 cloud file (`file_id` or `pick_code`).
    /// Crucial when FlowLink `delete_cloud_on_local_remove` (`webhook_watcher`) is enabled:
    /// after an in-place 115 rename, an old `.strm` and new `.strm` temporarily share the
    /// same `pick_code` until FlowLink's regular sync removes the stale local `.strm`.
    /// Deleting such a source via API would risk deleting the shared 115 cloud file!
    pub fn shares_cloud_identity(&self, other: &Self) -> bool {
        let fid_a = self.file_id.trim();
        let fid_b = other.file_id.trim();
        if !fid_a.is_empty() && fid_a != "0" && fid_a == fid_b {
            return true;
        }
        let pc_a = self.effective_pick_code();
        let pc_b = other.effective_pick_code();
        if !pc_a.is_empty() && pc_a.eq_ignore_ascii_case(&pc_b) {
            return true;
        }
        false
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaStream {
    #[serde(deserialize_with = "flex_id")]
    pub media_source_id: String,
    #[serde(deserialize_with = "null_default")]
    pub stream_type: String,
    #[serde(deserialize_with = "null_default")]
    pub codec: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub language: String,
    #[serde(deserialize_with = "null_default")]
    pub hdr_format: String,
    #[serde(default)]
    pub frame_rate: Value,
    #[serde(deserialize_with = "flex_u32")]
    pub bit_depth: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub width: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub height: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub channels: u32,
    #[serde(deserialize_with = "null_default")]
    pub channel_layout: String,
    #[serde(deserialize_with = "flex_f64")]
    pub bitrate: f64,
    pub is_default: bool,
    pub is_forced: bool,
}

impl MediaStream {
    pub fn fps(&self) -> Option<f64> {
        self.frame_rate
            .as_f64()
            .or_else(|| {
                let text = self.frame_rate.as_str()?.trim();
                if let Some((num, den)) = text.split_once('/') {
                    let d = den.trim().parse::<f64>().ok()?;
                    if d > 0.0 {
                        Some(num.trim().parse::<f64>().ok()? / d)
                    } else {
                        None
                    }
                } else {
                    text.trim_end_matches("fps").trim().parse::<f64>().ok()
                }
            })
            .filter(|&v| v > 0.0 && v < 1000.0)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryItem {
    #[serde(deserialize_with = "flex_id")]
    pub id: String,
    #[serde(deserialize_with = "flex_id")]
    pub library_id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub item_type: String,
    #[serde(deserialize_with = "null_default")]
    pub path: String,
    #[serde(deserialize_with = "null_default")]
    pub sources: Vec<LibrarySource>,
    #[serde(deserialize_with = "null_default")]
    pub versions: Vec<LibrarySource>,
    #[serde(deserialize_with = "null_default")]
    pub streams: Vec<MediaStream>,
    #[serde(deserialize_with = "flex_id")]
    pub tmdb_id: String,
    #[serde(deserialize_with = "flex_id")]
    pub series_tmdb_id: String,
    #[serde(deserialize_with = "flex_u32")]
    pub season: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub season_number: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub episode: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub episode_number: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub production_year: u32,
    #[serde(deserialize_with = "null_default")]
    pub poster_path: String,
    #[serde(deserialize_with = "null_default")]
    pub backdrop_path: String,
}

impl LibraryItem {
    pub fn effective_sources(&self) -> &[LibrarySource] {
        if !self.sources.is_empty() {
            &self.sources
        } else {
            &self.versions
        }
    }

    pub fn effective_season(&self) -> u32 {
        if self.season > 0 {
            self.season
        } else {
            self.season_number
        }
    }

    pub fn effective_episode(&self) -> u32 {
        if self.episode > 0 {
            self.episode
        } else {
            self.episode_number
        }
    }

    pub fn effective_tmdb(&self) -> &str {
        if self.item_type.eq_ignore_ascii_case("episode") && !self.series_tmdb_id.is_empty() {
            &self.series_tmdb_id
        } else {
            &self.tmdb_id
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryPage {
    #[serde(deserialize_with = "null_default")]
    pub items: Vec<LibraryItem>,
    pub start: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Cloud115File {
    #[serde(deserialize_with = "flex_id")]
    pub file_id: String,
    #[serde(deserialize_with = "flex_id")]
    pub parent_id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "flex_u64")]
    pub size: u64,
    #[serde(deserialize_with = "null_default")]
    pub sha1: String,
    #[serde(deserialize_with = "null_default")]
    pub pick_code: String,
    pub is_dir: bool,
    #[serde(deserialize_with = "null_default")]
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Cloud115Page {
    #[serde(deserialize_with = "flex_id")]
    pub cid: String,
    pub offset: usize,
    pub limit: usize,
    pub count: usize,
    #[serde(deserialize_with = "null_default")]
    pub auth_source: String,
    #[serde(deserialize_with = "null_default")]
    pub items: Vec<Cloud115File>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudMediaInfo {
    #[serde(deserialize_with = "null_default")]
    pub status: String,
    pub cached: Option<bool>,
    #[serde(deserialize_with = "null_default")]
    pub container: String,
    #[serde(deserialize_with = "flex_u64")]
    pub duration_ms: u64,
    #[serde(deserialize_with = "flex_f64")]
    pub bitrate: f64,
    #[serde(deserialize_with = "null_default")]
    pub streams: Vec<MediaStream>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PtSite {
    #[serde(deserialize_with = "flex_u64")]
    pub id: u64,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbSearchHit {
    #[serde(deserialize_with = "flex_id")]
    pub id: String,
    #[serde(deserialize_with = "flex_id")]
    pub tmdb_id: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub original_title: String,
    #[serde(deserialize_with = "null_default")]
    pub media_type: String,
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: String,
    #[serde(deserialize_with = "flex_id")]
    pub year: String,
    #[serde(deserialize_with = "null_default")]
    pub release_date: String,
    #[serde(deserialize_with = "null_default")]
    pub first_air_date: String,
    #[serde(deserialize_with = "null_default")]
    pub poster_path: String,
    #[serde(deserialize_with = "null_default")]
    pub overview: String,
}

impl TmdbSearchHit {
    pub fn effective_id(&self) -> &str {
        if !self.tmdb_id.is_empty() && self.tmdb_id != "0" {
            &self.tmdb_id
        } else {
            &self.id
        }
    }

    pub fn effective_title(&self) -> &str {
        if !self.title.trim().is_empty() {
            self.title.trim()
        } else if !self.name.trim().is_empty() {
            self.name.trim()
        } else {
            self.original_title.trim()
        }
    }

    pub fn effective_year(&self) -> u32 {
        for s in [&self.year, &self.release_date, &self.first_air_date] {
            let t = s.trim();
            if t.len() >= 4 {
                if let Ok(y) = t[..4].parse::<u32>() {
                    if (1900..=2100).contains(&y) {
                        return y;
                    }
                }
            }
        }
        0
    }
}

#[allow(dead_code)]
pub fn tmdb_poster_https(poster_path: &str) -> Option<String> {
    let p = poster_path.trim();
    if p.is_empty() {
        return None;
    }
    if p.starts_with("https://") || p.starts_with("http://") {
        return Some(p.to_string());
    }
    if p.starts_with('/') {
        Some(format!("https://image.tmdb.org/t/p/w500{p}"))
    } else {
        Some(format!("https://image.tmdb.org/t/p/w500/{p}"))
    }
}

pub struct HostClient {
    base_url: Url,
    token: String,
    agent: ureq::Agent,
    tmdb_resolve_cache: std::cell::RefCell<std::collections::HashMap<String, Option<String>>>,
    tmdb_details_cache: std::cell::RefCell<std::collections::HashMap<String, Value>>,
    tmdb_db: Option<std::cell::RefCell<Connection>>,
    subpath_cid_cache:
        std::cell::RefCell<std::collections::HashMap<(String, String), Option<String>>>,
}

#[derive(Debug, Clone)]
pub struct OrganizeFileItem {
    pub file_id: String,
    pub parent_id: String,
    pub name: String,
    pub size: u64,
    pub sha1: String,
    pub pick_code: String,
    pub season: u32,
    pub episode: u32,
}

impl HostClient {
    pub fn from_env() -> Result<Self> {
        let raw_url =
            env::var("MEDIARY_PLUGIN_API_URL").context("缺少环境变量 MEDIARY_PLUGIN_API_URL")?;
        let token =
            env::var("MEDIARY_PLUGIN_TOKEN").context("缺少环境变量 MEDIARY_PLUGIN_TOKEN")?;
        ensure!(
            !raw_url.trim().is_empty() && !token.trim().is_empty(),
            "Mediary 插件 API 地址或令牌为空"
        );
        let base_url = Url::parse(&format!("{}/", raw_url.trim().trim_end_matches('/')))?;
        ensure!(
            matches!(base_url.scheme(), "http" | "https"),
            "无效的 Mediary API 协议"
        );
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .redirects(0)
            .build();
        let tmdb_db = Self::open_tmdb_cache_conn().map(std::cell::RefCell::new);
        Ok(Self {
            base_url,
            token: token.trim().to_string(),
            agent,
            tmdb_resolve_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
            tmdb_details_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
            tmdb_db,
            subpath_cid_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
        })
    }

    fn open_tmdb_cache_conn() -> Option<Connection> {
        let data_dir = env::var_os("MEDIARY_PLUGIN_DATA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("data"));
        let db_path = data_dir.join("mediwash.db");
        if !db_path.exists() {
            return None;
        }
        let conn = Connection::open(&db_path).ok()?;
        let _ = conn.busy_timeout(Duration::from_millis(3000));
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tmdb_cache (
                 cache_key TEXT PRIMARY KEY,
                 val_json TEXT NOT NULL,
                 expires_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_tmdb_cache_exp ON tmdb_cache(expires_at);",
        );
        Some(conn)
    }

    fn tmdb_cache_read(&self, key: &str) -> Option<Value> {
        let cell = self.tmdb_db.as_ref()?;
        let conn = cell.borrow();
        let now = now_ts() as i64;
        let raw: Option<String> = conn
            .query_row(
                "SELECT val_json FROM tmdb_cache WHERE cache_key = ?1 AND expires_at > ?2",
                rusqlite::params![key, now],
                |r| r.get(0),
            )
            .ok();
        raw.and_then(|s| serde_json::from_str(&s).ok())
    }

    fn tmdb_cache_write(&self, key: &str, val: &Value, ttl_secs: u64) {
        let Some(cell) = self.tmdb_db.as_ref() else {
            return;
        };
        let conn = cell.borrow();
        let now = now_ts() as i64;
        let exp = now + ttl_secs as i64;
        if let Ok(encoded) = serde_json::to_string(val) {
            let _ = conn.execute(
                "INSERT INTO tmdb_cache (cache_key, val_json, expires_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(cache_key) DO UPDATE SET val_json=excluded.val_json, expires_at=excluded.expires_at",
                rusqlite::params![key, encoded, exp],
            );
            // Opportunistically prune expired entries (low cost with index on expires_at)
            if now % 37 == 0 {
                let _ = conn.execute("DELETE FROM tmdb_cache WHERE expires_at <= ?1", [now]);
            }
        }
    }

    pub fn call_raw(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        timeout_secs: u64,
    ) -> Result<String> {
        let full = format!(
            "{}/{}",
            self.base_url.as_str().trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let build_req = || {
            let mut req = self
                .agent
                .request(method, &full)
                .timeout(Duration::from_secs(timeout_secs.max(2)))
                .set("Authorization", &format!("Bearer {}", self.token));
            for (k, v) in query {
                req = req.query(k, v);
            }
            req
        };
        let send_once = |payload_opt: Option<Value>| match payload_opt {
            Some(payload) => build_req().send_json(payload),
            None => build_req().call(),
        };
        let mut resp = send_once(body.clone());
        if let Err(ureq::Error::Transport(ref t)) = resp {
            if t.kind() == ureq::ErrorKind::ConnectionFailed {
                std::thread::sleep(Duration::from_millis(180));
                resp = send_once(body);
            }
        }
        match resp {
            Ok(r) => Ok(r.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(code, r)) => {
                let raw = r.into_string().unwrap_or_default();
                let mut msg = raw.trim().to_string();
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    for key in ["error", "message", "detail"] {
                        if let Some(s) = v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                            msg = s.to_string();
                            break;
                        }
                    }
                }
                if msg.is_empty() {
                    msg = format!("状态码 {code}");
                }
                Err(anyhow!(HttpStatusError(code, msg)))
            }
            Err(ureq::Error::Transport(t)) => Err(anyhow!("连接 Mediary API 失败: {t}")),
        }
    }

    pub fn call_json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        timeout_secs: u64,
    ) -> Result<T> {
        let raw = self.call_raw(method, path, query, body, timeout_secs)?;
        let text = if raw.trim().is_empty() { "{}" } else { &raw };
        serde_json::from_str(text).with_context(|| format!("解析 Mediary 响应失败 ({path})"))
    }

    pub fn host_status(&self) -> HostStatus {
        let mut st = HostStatus::default();
        let is_ready = |v: &Value| -> bool {
            if let Some(s) = v.get("status").and_then(Value::as_str) {
                if matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "connected" | "ok" | "ready" | "authorized" | "logged_in"
                ) {
                    return true;
                }
            }
            for k in [
                "logged_in",
                "connected",
                "authorized",
                "authenticated",
                "enabled",
                "valid",
                "configured",
                "has_cookie",
            ] {
                if v.get(k).and_then(Value::as_bool) == Some(true) {
                    return true;
                }
            }
            false
        };

        if let Ok(v) = self.call_json::<Value>("GET", "/open115/status", &[], None, 6) {
            if is_ready(&v) {
                st.open_api_connected = true;
            }
        }
        if let Ok(v) = self.call_json::<Value>("GET", "/open115/web-cookie/status", &[], None, 6) {
            if is_ready(&v) {
                st.web_cookie_ready = true;
            }
        }
        if !st.open_api_connected && !st.web_cookie_ready {
            if let Ok(page) = self.open115_list_page("0", 0, 1, true) {
                if page.auth_source.to_ascii_lowercase().contains("open") {
                    st.open_api_connected = true;
                } else {
                    st.web_cookie_ready = true;
                }
            }
        }
        if let Ok(v) = self.call_json::<Value>("GET", "/settings", &[], None, 8) {
            if let Some(p) = v
                .get("tmdb_proxy_url")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            {
                st.proxy_url = p.trim().to_string();
            }
            if v.get("flowlink")
                .and_then(|fl| fl.get("enabled"))
                .and_then(Value::as_bool)
                == Some(true)
                || v.get("flowlink_enabled").and_then(Value::as_bool) == Some(true)
            {
                st.flowlink_enabled = true;
            }
        }
        if let Ok(env_p) = env::var("MEDIARY_PLUGIN_PROXY_URL") {
            if !env_p.trim().is_empty() {
                st.proxy_url = env_p.trim().to_string();
            }
        }
        st
    }

    /// Skips redundant 4-endpoint host sync when categories were already synced within `max_age_secs`.
    pub fn sync_unified_rules_if_stale(&self, db: &mut Connection, max_age_secs: u64) -> Result<()> {
        if let Ok(cfg) = db::load_config(db) {
            if !cfg.host_sync.categories.is_empty()
                && cfg.host_sync.last_synced > 0
                && now_ts().saturating_sub(cfg.host_sync.last_synced) < max_age_secs
            {
                return Ok(());
            }
        }
        self.sync_unified_rules(db)
    }

    /// Synchronizes Mediary's secondary categories, movie/tv rename templates, and global filter words.
    pub fn sync_unified_rules(&self, db: &mut Connection) -> Result<()> {
        let mut movie_fmt = String::new();
        let mut tv_fmt = String::new();
        let mut flowlink_task_name = String::new();
        if let Ok(settings) = self.call_json::<Value>("GET", "/settings", &[], None, 10) {
            let lp = settings.get("library_paths").unwrap_or(&settings);
            movie_fmt = lp["movie_rename_format"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            tv_fmt = lp["tv_rename_format"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if let Some(tn) = settings
                .get("flowlink")
                .and_then(|fl| fl["task_name"].as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                flowlink_task_name = tn.to_string();
            }
        }

        let mut open115_target_folder = String::new();
        let mut open115_target_cid = String::new();
        let mut open115_offline_folder = String::new();
        let mut open115_offline_cid = String::new();
        let mut transfer_source_folder = String::new();
        let mut transfer_source_cid = String::new();
        let mut transfer_target_folder = String::new();
        let mut transfer_target_cid = String::new();
        let mut transfer_task_name = String::new();
        let mut has_flowlink_transfer_template = true;
        let mut protected_set = std::collections::BTreeSet::new();

        let pick_str = |v: &Value, keys: &[&str]| -> String {
            for k in keys {
                if let Some(s) = v[k].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    return s.to_string();
                }
                if let Some(n) = v[k].as_u64().filter(|&n| n > 0) {
                    return n.to_string();
                }
            }
            String::new()
        };
        let mut mark_protected = |cid: &str| {
            let t = cid.trim();
            if !t.is_empty() && t != "0" && t.bytes().all(|b| b.is_ascii_digit()) {
                protected_set.insert(t.to_string());
            }
        };

        if let Ok(ocfg) = self.call_json::<Value>("GET", "/open115/config", &[], None, 8) {
            open115_target_folder = pick_str(&ocfg, &["target_folder"]);
            open115_target_cid = pick_str(&ocfg, &["target_cid"]);
            open115_offline_folder = pick_str(&ocfg, &["offline_folder"]);
            open115_offline_cid = pick_str(&ocfg, &["offline_folder_cid"]);
            transfer_source_folder = pick_str(
                &ocfg,
                &[
                    "transfer_source_folder",
                    "transfer_source_path",
                    "strm_transfer_source_folder",
                    "strm_transfer_source_path",
                ],
            );
            transfer_source_cid = pick_str(
                &ocfg,
                &["transfer_source_cid", "strm_transfer_source_cid"],
            );
            transfer_target_folder = pick_str(
                &ocfg,
                &["transfer_target_folder", "transfer_target_dir", "transfer_movie_folder"],
            );
            transfer_target_cid = pick_str(
                &ocfg,
                &["transfer_target_cid", "transfer_movie_cid", "transfer_tv_cid"],
            );
            transfer_task_name = pick_str(
                &ocfg,
                &["transfer_task_name", "default_transfer_task", "task_name"],
            );
            for k in [
                "target_cid",
                "offline_folder_cid",
                "transfer_source_cid",
                "transfer_target_cid",
                "transfer_movie_cid",
                "transfer_tv_cid",
            ] {
                mark_protected(&pick_str(&ocfg, &[k]));
            }
            if let Some(tasks) = ocfg["transfer_tasks"].as_array() {
                has_flowlink_transfer_template = tasks.iter().any(|t| {
                    !pick_str(t, &["movie_template", "tv_template"]).is_empty()
                }) || !tasks.is_empty();
                for t in tasks {
                    for k in ["source_cid", "target_cid", "movie_cid", "tv_cid"] {
                        mark_protected(&pick_str(t, &[k]));
                    }
                }
                if tasks.is_empty() {
                    transfer_task_name.clear();
                    flowlink_task_name.clear();
                } else {
                    let chosen = tasks
                        .iter()
                        .find(|t| t["is_default"].as_bool() == Some(true))
                        .or_else(|| tasks.first());
                    if let Some(t) = chosen {
                        let chosen_name = pick_str(t, &["name", "task_name"]);
                        if !chosen_name.is_empty() {
                            transfer_task_name = chosen_name;
                        }
                        if transfer_source_folder.is_empty() {
                            transfer_source_folder = pick_str(t, &["source_path", "source_folder"]);
                        }
                        if transfer_source_cid.is_empty() {
                            transfer_source_cid = pick_str(t, &["source_cid"]);
                        }
                        if transfer_target_folder.is_empty() {
                            transfer_target_folder =
                                pick_str(t, &["target_dir", "movie_dir", "tv_dir"]);
                        }
                        if transfer_target_cid.is_empty() {
                            transfer_target_cid =
                                pick_str(t, &["target_cid", "movie_cid", "tv_cid"]);
                        }
                    }
                }
            }
            if transfer_task_name.is_empty()
                && has_flowlink_transfer_template
                && !flowlink_task_name.is_empty()
            {
                transfer_task_name = flowlink_task_name.clone();
            }
            if let Some(utasks) = ocfg["upload_tasks"].as_array() {
                for ut in utasks {
                    mark_protected(&pick_str(ut, &["cloud_cid", "target_cid"]));
                }
            }
            // Auto-discover root library folder CID (e.g. `媒体库`) when FlowLink `transfer_tasks` is empty/disabled!
            if transfer_target_cid.is_empty() {
                let existing_target = db::load_config(db)
                    .ok()
                    .map(|c| c.host_sync.transfer_target_cid)
                    .unwrap_or_default();
                if existing_target.trim().is_empty() {
                    let strm_top = pick_str(&ocfg, &["strm_source_dir", "source_dir"])
                        .trim_matches('/')
                        .split('/')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let mut candidates = Vec::new();
                    if !strm_top.is_empty() {
                        candidates.push(strm_top);
                    }
                    for d in ["媒体库", "影视库", "影视"] {
                        if !candidates.iter().any(|x| x == d) {
                            candidates.push(d.to_string());
                        }
                    }
                    for cand in candidates {
                        if let Ok(Some(cid)) = self.open115_resolve_subpath_cid("0", &[cand.as_str()]) {
                            if !cid.is_empty()
                                && cid != "0"
                                && cid != open115_target_cid
                                && cid != open115_offline_cid
                                && cid != transfer_source_cid
                            {
                                mark_protected(&cid);
                                transfer_target_folder = cand;
                                transfer_target_cid = cid;
                                break;
                            }
                        }
                    }
                }
            }
        }

        let mut filter_keywords = Vec::new();
        if let Ok(fr) = self.call_json::<Value>("GET", "/filter/rules", &[], None, 8) {
            let arr = fr
                .as_array()
                .or_else(|| fr["items"].as_array())
                .or_else(|| fr["rules"].as_array())
                .or_else(|| fr["data"].as_array());
            for entry in arr.into_iter().flatten() {
                if entry["enabled"].as_bool() == Some(false) {
                    continue;
                }
                if let Some(kw) = entry["keyword"]
                    .as_str()
                    .or_else(|| entry["pattern"].as_str())
                    .filter(|s| !s.trim().is_empty())
                {
                    if entry["action"].as_str().is_none_or(|a| a == "exclude" || a == "block") {
                        filter_keywords.push(kw.trim().to_string());
                    }
                }
            }
        }

        // Priority 1: Host API `/settings/category-file`
        // Priority 2: Host API `/filter/categories`
        // Priority 3: Local host `category.yaml` read-only fallback
        let mut synced_cats = None;
        let mut cat_source = String::new();
        if let Ok(cf) = self.call_json::<Value>("GET", "/settings/category-file", &[], None, 8) {
            let content = cf["content"]
                .as_str()
                .or_else(|| cf["data"]["content"].as_str())
                .unwrap_or_default();
            if !content.trim().is_empty() {
                if let Ok(cats) = category::parse_category_yaml(content) {
                    synced_cats = Some(cats);
                    cat_source = "Mediary API · category-file".to_string();
                }
            }
        }
        if synced_cats.is_none() {
            if let Ok(fc) = self.call_json::<Value>("GET", "/filter/categories", &[], None, 8) {
                if let Some(cats) = category::parse_categories_json(&fc) {
                    synced_cats = Some(cats);
                    cat_source = "Mediary API · filter/categories".to_string();
                }
            }
        }
        if synced_cats.is_none() {
            if let Some(local_yaml) = category::read_local_host_category_yaml() {
                if let Ok(cats) = category::parse_category_yaml(&local_yaml) {
                    synced_cats = Some(cats);
                    cat_source = "Mediary 宿主 · category.yaml".to_string();
                }
            }
        }

        db::edit_config(db, false, |cfg| {
            if !movie_fmt.is_empty() {
                cfg.host_sync.movie_rename_format = movie_fmt;
            }
            if !tv_fmt.is_empty() {
                cfg.host_sync.tv_rename_format = tv_fmt;
            }
            if !filter_keywords.is_empty() {
                cfg.host_sync.filter_keywords = filter_keywords;
            }
            if !open115_target_folder.is_empty() {
                cfg.host_sync.open115_target_folder = open115_target_folder;
            }
            if !open115_target_cid.is_empty() && open115_target_cid != "0" {
                cfg.host_sync.open115_target_cid = open115_target_cid;
            }
            if !open115_offline_folder.is_empty() {
                cfg.host_sync.open115_offline_folder = open115_offline_folder;
            }
            if !open115_offline_cid.is_empty() && open115_offline_cid != "0" {
                cfg.host_sync.open115_offline_cid = open115_offline_cid;
            }
            if !transfer_source_folder.is_empty() {
                cfg.host_sync.transfer_source_folder = transfer_source_folder;
            }
            if !transfer_source_cid.is_empty() && transfer_source_cid != "0" {
                cfg.host_sync.transfer_source_cid = transfer_source_cid;
            }
            if !transfer_target_folder.is_empty() {
                cfg.host_sync.transfer_target_folder = transfer_target_folder;
            }
            if !transfer_target_cid.is_empty() && transfer_target_cid != "0" {
                cfg.host_sync.transfer_target_cid = transfer_target_cid;
            }
            if !transfer_task_name.is_empty() {
                cfg.host_sync.transfer_task_name = transfer_task_name;
            } else if !has_flowlink_transfer_template {
                cfg.host_sync.transfer_task_name.clear();
            } else if !flowlink_task_name.is_empty() {
                cfg.host_sync.transfer_task_name = flowlink_task_name;
            }
            cfg.host_sync.has_flowlink_transfer_template = has_flowlink_transfer_template;
            if !protected_set.is_empty() {
                cfg.host_sync.protected_cids = protected_set.into_iter().collect();
            }
            if let Some(cats) = synced_cats {
                cfg.host_sync.categories = cats;
                cfg.host_sync.category_source = cat_source;
            }
            cfg.host_sync.last_synced = now_ts();
            Ok(false)
        })?;
        Ok(())
    }

    pub fn open115_list_page(
        &self,
        cid: &str,
        offset: usize,
        limit: usize,
        show_dir: bool,
    ) -> Result<Cloud115Page> {
        let cid_s = if cid.trim().is_empty() { "0" } else { cid.trim() };
        let lim = limit.clamp(1, 1000);
        let off_s = offset.to_string();
        let lim_s = lim.to_string();
        let page: Cloud115Page = self
            .call_json(
                "GET",
                "/plugin/open115/files",
                &[
                    ("cid", cid_s),
                    ("offset", &off_s),
                    ("limit", &lim_s),
                    ("show_dir", if show_dir { "true" } else { "false" }),
                ],
                None,
                25,
            )
            .context("读取 115 目录失败（请确认已授予 open115:read 且宿主 115 已连接）")?;
        Ok(sanitize_open115_page(cid_s, page))
    }

    /// Lists all files inside `cid` (up to `max_pages * 1000` items) in minimal API calls so batch execution
    /// never triggers N+1 115 QPS throttling on large staging directories.
    /// Returns `(file_map, is_complete)` where `is_complete` indicates every item in `cid` was read.
    pub fn open115_list_dir_files_map_ext(
        &self,
        cid: &str,
        max_pages: usize,
    ) -> Result<(std::collections::BTreeMap<String, Cloud115File>, bool)> {
        let mut out = std::collections::BTreeMap::new();
        let mut offset = 0usize;
        let limit = 1000usize;
        let mut complete = false;
        for _ in 0..max_pages.max(1) {
            let page = self.open115_list_page(cid, offset, limit, true)?;
            let n = page.items.len();
            for mut item in page.items {
                if !item.is_dir && !item.file_id.is_empty() {
                    if item.parent_id.trim().is_empty() {
                        item.parent_id = cid.trim().to_string();
                    }
                    out.insert(item.file_id.clone(), item);
                }
            }
            offset += n;
            if n == 0 || offset >= page.count {
                complete = true;
                break;
            }
        }
        Ok((out, complete))
    }

    #[allow(dead_code)]
    pub fn open115_list_dir_files_map(
        &self,
        cid: &str,
        max_pages: usize,
    ) -> Result<std::collections::BTreeMap<String, Cloud115File>> {
        Ok(self.open115_list_dir_files_map_ext(cid, max_pages)?.0)
    }

    /// Locates a specific file by `file_id` inside `cid` without relying on fragile pagination offsets.
    pub fn open115_find_file_in_cid(&self, cid: &str, file_id: &str) -> Result<Option<Cloud115File>> {
        Ok(self.open115_find_file_in_cid_ext(cid, file_id, "")?.map(|(f, _)| f))
    }

    /// Locates a specific file by `file_id` (or `pick_code`) inside `cid`, returning both the matched file
    /// and all non-directory files encountered in `cid` (for companion subtitle pairing).
    pub fn open115_find_file_in_cid_ext(
        &self,
        cid: &str,
        file_id: &str,
        pick_code: &str,
    ) -> Result<Option<(Cloud115File, Vec<Cloud115File>)>> {
        let mut offset = 0usize;
        let limit = 200usize;
        let mut dir_files = Vec::new();
        let mut matched: Option<Cloud115File> = None;
        for _ in 0..30 {
            let page = self.open115_list_page(cid, offset, limit, true)?;
            for item in &page.items {
                if !item.is_dir {
                    dir_files.push(item.clone());
                }
                if matched.is_none()
                    && ((!file_id.trim().is_empty() && item.file_id == file_id.trim())
                        || (!pick_code.trim().is_empty()
                            && item.pick_code.eq_ignore_ascii_case(pick_code.trim())))
                {
                    let mut hit = item.clone();
                    if hit.parent_id.trim().is_empty() {
                        hit.parent_id = cid.trim().to_string();
                    }
                    matched = Some(hit);
                }
            }
            offset += page.items.len();
            if matched.is_some() || page.items.is_empty() || offset >= page.count {
                break;
            }
        }
        Ok(matched.map(|f| (f, dir_files)))
    }

    /// Resolves a nested directory path (`segments`, e.g. `["电视剧", "日韩剧", "2020-2030", "剧名", "Season 01"]`)
    /// starting from `root_cid` (e.g. `transfer_target_cid` `媒体库`) to its final 115 folder CID.
    /// Caches every directory seen on each listed page in-memory for the lifetime of this `HostClient`
    /// so repeated sibling/parent lookups (locate + prune + batch retry) complete in 0ms!
    pub fn open115_resolve_subpath_cid(
        &self,
        root_cid: &str,
        segments: &[&str],
    ) -> Result<Option<String>> {
        let mut cur = root_cid.trim().to_string();
        if cur.is_empty() || cur == "0" {
            return Ok(None);
        }
        for seg in segments {
            let want = seg.trim();
            if want.is_empty() {
                continue;
            }
            let cache_key = (cur.clone(), want.to_string());
            if let Some(cached) = self.subpath_cid_cache.borrow().get(&cache_key).cloned() {
                match cached {
                    Some(c) if !c.is_empty() => {
                        cur = c;
                        continue;
                    }
                    _ => return Ok(None),
                }
            }
            let mut offset = 0usize;
            let mut next_cid = None;
            for _ in 0..20 {
                let page = self.open115_list_page(&cur, offset, 1000, true)?;
                {
                    let mut cache = self.subpath_cid_cache.borrow_mut();
                    for it in &page.items {
                        if it.is_dir && !it.file_id.is_empty() {
                            let dname = it.name.trim();
                            if !dname.is_empty() {
                                cache.insert(
                                    (cur.clone(), dname.to_string()),
                                    Some(it.file_id.clone()),
                                );
                            }
                        }
                    }
                }
                if let Some(dir) = page.items.iter().find(|it| it.is_dir && it.name.trim() == want) {
                    next_cid = Some(dir.file_id.clone());
                    break;
                }
                offset += page.items.len();
                if page.items.is_empty() || offset >= page.count {
                    break;
                }
            }
            if next_cid.is_none() {
                self.subpath_cid_cache.borrow_mut().insert(cache_key, None);
            }
            match next_cid {
                Some(c) if !c.is_empty() => cur = c,
                _ => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    /// Calls `POST /api/plugin/open115/files/organize` (`open115:write`, Mediary 3.4.6+)
    /// to directly archive winning 115 files into the final library with secondary categories,
    /// rename templates, companion subtitles, `.strm` generation, and incremental library scan.
    #[allow(dead_code)]
    pub fn open115_organize_files(
        &self,
        source_cid: &str,
        target_cid: Option<&str>,
        task_name: Option<&str>,
        media_type: &str,
        tmdb_id: &str,
        files: &[OrganizeFileItem],
    ) -> Result<Value> {
        self.open115_organize_files_with_timeout(
            source_cid, target_cid, task_name, false, media_type, tmdb_id, files, 90,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open115_organize_files_with_timeout(
        &self,
        source_cid: &str,
        target_cid: Option<&str>,
        task_name: Option<&str>,
        prefer_task_target_dir: bool,
        media_type: &str,
        tmdb_id: &str,
        files: &[OrganizeFileItem],
        timeout_secs: u64,
    ) -> Result<Value> {
        db::validate_cid(source_cid)?;
        ensure!(source_cid != "0", "待洗版源目录 CID 不能为根目录 0");
        ensure!(!files.is_empty(), "没有有效的待归档视频文件");

        let clean_target = target_cid
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "0" && s.bytes().all(|b| b.is_ascii_digit()));
        let mut clean_task_name = task_name.map(str::trim).filter(|s| !s.is_empty());
        // When `task_name` (e.g. `"115-895"`) already owns the default library target directory (`媒体库`),
        // omit `target_cid` first so Mediary preserves `task.target_dir = "媒体库"` (hitting `flowlink.db`
        // `cloud_dirs` cache even in 3,400+ folder categories) instead of rewriting it to `__cid_<id>__/`.
        let mut omit_target_cid =
            prefer_task_target_dir && clean_task_name.is_some() && clean_target.is_some();

        let mut last_resp = json!({});
        // Batch up to 30 files per HTTP request (Mediary allows up to 100) for higher throughput.
        for chunk in files.chunks(30) {
            let items_json: Vec<Value> = chunk
                .iter()
                .map(|f| {
                    let mut obj = json!({
                        "file_id": f.file_id.trim(),
                        "name": f.name.trim(),
                        "size": f.size,
                    });
                    if !f.parent_id.trim().is_empty()
                        && f.parent_id.trim().bytes().all(|b| b.is_ascii_digit())
                    {
                        obj["parent_id"] = json!(f.parent_id.trim());
                    }
                    let pc = f.pick_code.trim();
                    if !pc.is_empty() && pc.bytes().all(|b| b.is_ascii_alphanumeric()) {
                        obj["pick_code"] = json!(pc);
                    }
                    let sh = f.sha1.trim();
                    if sh.len() == 40 && sh.bytes().all(|b| b.is_ascii_hexdigit()) {
                        obj["sha1"] = json!(sh.to_ascii_uppercase());
                    }
                    if media_type == "tv" && f.episode > 0 {
                        obj["season"] = json!(f.season.max(1));
                        obj["episode"] = json!(f.episode);
                    }
                    obj
                })
                .collect();

            let mut body = json!({
                "source_cid": source_cid.trim(),
                "conflict_strategy": "overwrite",
                "dry_run": false,
                "files": items_json,
            });
            if !omit_target_cid {
                if let Some(tcid) = clean_target {
                    body["target_cid"] = json!(tcid);
                }
            }
            if let Some(tname) = clean_task_name {
                body["task_name"] = json!(tname);
            }
            if matches!(media_type, "movie" | "tv") {
                body["media_type"] = json!(media_type);
                if let Ok(tid) = tmdb_id.trim().parse::<u64>() {
                    if tid > 0 {
                        body["tmdb_id"] = json!(tid);
                    }
                }
            }

            let t_secs = timeout_secs.clamp(15, 120);
            let mut cur_res: Result<Value> =
                self.call_json("POST", "/plugin/open115/files/organize", &[], Some(body.clone()), t_secs);

            for _ in 0..2 {
                let err_str = match &cur_res {
                    Ok(v) => match ensure_host_ok(v, "115 原生二级分类归档入库") {
                        Ok(()) => break,
                        Err(e) => format!("{e:#}"),
                    },
                    Err(e) => format!("{e:#}"),
                };
                if omit_target_cid
                    && clean_target.is_some()
                    && (err_str.contains("未配置默认归档目标目录") || err_str.contains("target_cid"))
                {
                    omit_target_cid = false;
                    if let Some(tcid) = clean_target {
                        body["target_cid"] = json!(tcid);
                    }
                    cur_res = self.call_json(
                        "POST",
                        "/plugin/open115/files/organize",
                        &[],
                        Some(body.clone()),
                        t_secs,
                    );
                    continue;
                }
                if clean_task_name.is_some() && err_str.contains("没有找到指定转存任务") {
                    clean_task_name = None;
                    omit_target_cid = false;
                    if let Some(obj) = body.as_object_mut() {
                        obj.remove("task_name");
                        if let Some(tcid) = clean_target {
                            obj.insert("target_cid".to_string(), json!(tcid));
                        }
                    }
                    cur_res = self.call_json(
                        "POST",
                        "/plugin/open115/files/organize",
                        &[],
                        Some(body.clone()),
                        t_secs,
                    );
                    continue;
                }
                break;
            }

            let resp = cur_res?;
            ensure_host_ok(&resp, "115 原生二级分类归档入库")?;
            last_resp = resp;
        }
        Ok(last_resp)
    }

    /// Calls `POST /api/plugin/open115/files/move` (`open115:write`)
    /// to move 115 files directly into an existing library folder CID (e.g. TV `Season XX`
    /// when TMDB has not yet listed the latest episode number).
    pub fn open115_move_files(&self, file_ids: &[String], to_cid: &str) -> Result<usize> {
        db::validate_cid(to_cid).context("115 直移缺少有效的目标目录 to_cid")?;
        ensure!(to_cid != "0", "禁止将媒体文件直接移至 115 根目录 0");
        let clean_ids: Vec<String> = file_ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "0" && s.bytes().all(|b| b.is_ascii_digit()))
            .collect();
        ensure!(!clean_ids.is_empty(), "没有有效的 115 文件 ID 可直移");

        for chunk in clean_ids.chunks(100) {
            let resp: Value = self.call_json(
                "POST",
                "/plugin/open115/files/move",
                &[],
                Some(json!({
                    "file_ids": chunk,
                    "to_cid": to_cid.trim(),
                })),
                30,
            )?;
            ensure_host_ok(&resp, "115 直移入库")?;
        }
        Ok(clean_ids.len())
    }

    /// Calls `POST /api/plugin/open115/files/delete` (`open115:write`, Mediary 3.4.6+)
    /// to move rejected/duplicate 115 files directly into 115 Official Recycle Bin.
    pub fn open115_delete_files(&self, parent_cid: &str, file_ids: &[String]) -> Result<usize> {
        db::validate_cid(parent_cid).context("115 回收站删除缺少有效的父目录 parent_cid")?;
        ensure!(parent_cid != "0", "禁止对 115 根目录执行批量回收删除");
        let clean_ids: Vec<String> = file_ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "0" && s.bytes().all(|b| b.is_ascii_digit()))
            .collect();
        ensure!(!clean_ids.is_empty(), "没有有效的 115 文件 ID 可移入回收站");

        for chunk in clean_ids.chunks(100) {
            let resp: Value = self.call_json(
                "POST",
                "/plugin/open115/files/delete",
                &[],
                Some(json!({
                    "parent_cid": parent_cid.trim(),
                    "file_ids": chunk,
                })),
                30,
            )?;
            ensure_host_ok(&resp, "115 移入回收站")?;
        }
        Ok(clean_ids.len())
    }

    pub fn open115_probe_media_info(
        &self,
        file: &Cloud115File,
        allow_probe: bool,
        wait: bool,
        force_refresh: bool,
        timeout_ms: u64,
    ) -> Result<CloudMediaInfo> {
        ensure!(!file.pick_code.trim().is_empty(), "115 文件缺少 pick_code");
        self.call_json(
            "POST",
            "/plugin/open115/media-info",
            &[],
            Some(json!({
                "pick_code": file.pick_code,
                "sha1": file.sha1,
                "name": file.name,
                "size": file.size,
                "allow_probe": allow_probe,
                "wait": wait,
                "force_refresh": force_refresh,
                "timeout_ms": timeout_ms.clamp(1000, 25000),
            })),
            (timeout_ms / 1000).max(8) + 5,
        )
    }

    pub fn resolve_tmdb(
        &self,
        title: &str,
        year: Option<u32>,
        media_type: &str,
    ) -> Result<Option<String>> {
        let cache_key = format!(
            "resolve|{}|{}|{}",
            media_type.trim().to_ascii_lowercase(),
            year.unwrap_or(0),
            title.trim().to_ascii_lowercase()
        );
        if let Some(cached) = self.tmdb_resolve_cache.borrow().get(&cache_key) {
            return Ok(cached.clone());
        }
        if let Some(persisted) = self.tmdb_cache_read(&cache_key) {
            let val = persisted["tmdb_id"]
                .as_str()
                .filter(|s| !s.is_empty() && *s != "0")
                .map(str::to_string);
            self.tmdb_resolve_cache
                .borrow_mut()
                .insert(cache_key, val.clone());
            return Ok(val);
        }
        let mut body = json!({
            "title": title.trim(),
            "media_type": media_type,
        });
        if let Some(y) = year.filter(|&y| y > 1880) {
            body["year"] = json!(y);
        }
        match self.call_json::<Value>("POST", "/tmdb/resolve", &[], Some(body), 18) {
            Ok(v) => {
                let id = v["data"]["tmdb_id"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| v["data"]["tmdb_id"].as_u64().map(|n| n.to_string()))
                    .or_else(|| v["tmdb_id"].as_str().map(str::to_string))
                    .or_else(|| v["tmdb_id"].as_u64().map(|n| n.to_string()))
                    .or_else(|| v["id"].as_u64().map(|n| n.to_string()))
                    .unwrap_or_default();
                let res = (!id.is_empty() && id != "0").then_some(id);
                // Persist hits for 7 days, misses for 30 minutes
                let ttl = if res.is_some() { 7 * 86_400 } else { 1_800 };
                self.tmdb_cache_write(
                    &cache_key,
                    &json!({ "tmdb_id": res.as_deref().unwrap_or("") }),
                    ttl,
                );
                self.tmdb_resolve_cache
                    .borrow_mut()
                    .insert(cache_key, res.clone());
                Ok(res)
            }
            Err(err) => {
                if err
                    .downcast_ref::<HttpStatusError>()
                    .is_some_and(|st| st.0 == 404)
                {
                    self.tmdb_cache_write(&cache_key, &json!({ "tmdb_id": "" }), 1_800);
                    self.tmdb_resolve_cache.borrow_mut().insert(cache_key, None);
                    Ok(None)
                } else {
                    Err(err.context("调用 Mediary TMDB 识别失败（需 catalog:read 权限）"))
                }
            }
        }
    }

    pub fn search_tmdb(&self, query: &str, media_type: &str) -> Result<Vec<TmdbSearchHit>> {
        let mut q = vec![
            ("query", query.trim()),
            ("keyword", query.trim()),
            ("page", "1"),
        ];
        if !media_type.is_empty() && media_type != "all" && media_type != "auto" {
            q.push(("media_type", media_type));
            q.push(("type", media_type));
        }
        let v: Value = self.call_json("GET", "/search/tmdb", &q, None, 20)?;
        Ok(decode_vec(&v))
    }

    pub fn get_tmdb_details(&self, tmdb_id: &str, media_type: &str) -> Result<Value> {
        let mt = if media_type == "tv" { "tv" } else { "movie" };
        let tid = tmdb_id.trim();
        let cache_key = format!("details|{mt}|{tid}");
        if let Some(cached) = self.tmdb_details_cache.borrow().get(&cache_key) {
            return Ok(cached.clone());
        }
        if let Some(persisted) = self.tmdb_cache_read(&cache_key) {
            self.tmdb_details_cache
                .borrow_mut()
                .insert(cache_key, persisted.clone());
            return Ok(persisted);
        }
        let v: Value = self.call_json(
            "GET",
            "/search/tmdb/details",
            &[
                ("tmdb_id", tid),
                ("id", tid),
                ("media_type", mt),
                ("type", mt),
            ],
            None,
            18,
        )?;
        let out = if let Some(d) = v.get("data").filter(|x| x.is_object()) {
            d.clone()
        } else if let Some(d) = v.get("item").filter(|x| x.is_object()) {
            d.clone()
        } else {
            v
        };
        // Persist TMDB details (title, poster, TV seasons) for 24 hours
        self.tmdb_cache_write(&cache_key, &out, 86_400);
        self.tmdb_details_cache
            .borrow_mut()
            .insert(cache_key, out.clone());
        Ok(out)
    }

    pub fn library_page(
        &self,
        start: usize,
        limit: usize,
        media_type: &str,
        library_id: &str,
    ) -> Result<LibraryPage> {
        self.library_page_filtered(start, limit, media_type, library_id, false, "", "")
    }

    #[allow(clippy::too_many_arguments)]
    pub fn library_page_filtered(
        &self,
        start: usize,
        limit: usize,
        media_type: &str,
        library_id: &str,
        has_multiple_versions: bool,
        hdr_format: &str,
        source_kind: &str,
    ) -> Result<LibraryPage> {
        let st = start.to_string();
        let lim = limit.clamp(1, 200).to_string();
        let mut q = vec![
            ("provider", "internal"),
            ("start", st.as_str()),
            ("limit", lim.as_str()),
        ];
        if !media_type.is_empty() && media_type != "all" {
            q.push(("media_type", media_type));
        }
        if !library_id.trim().is_empty() {
            q.push(("library_id", library_id.trim()));
        }
        if has_multiple_versions {
            q.push(("has_multiple_versions", "true"));
        }
        if !hdr_format.trim().is_empty() && hdr_format.trim() != "all" {
            q.push(("hdr_format", hdr_format.trim()));
        }
        if !source_kind.trim().is_empty() && source_kind.trim() != "all" {
            q.push(("source_kind", source_kind.trim()));
        }
        let page: LibraryPage = self.call_json("GET", "/plugin/library/items", &q, None, 25)?;
        Ok(page)
    }

    pub fn library_items_by_tmdb(
        &self,
        tmdb_id: &str,
        media_type: &str,
        episode: Option<(u32, u32)>,
    ) -> Result<LibraryPage> {
        let mut q = vec![
            ("provider", "internal"),
            ("start", "0"),
            ("limit", "50"),
            ("media_type", media_type),
            ("tmdb_id", tmdb_id.trim()),
        ];
        let s_str;
        let e_str;
        if let Some((s, e)) = episode {
            s_str = s.to_string();
            e_str = e.to_string();
            q.push(("season", &s_str));
            q.push(("episode", &e_str));
        }
        self.call_json("GET", "/plugin/library/items", &q, None, 20)
    }

    pub fn library_items_search(&self, keyword: &str, media_type: &str) -> Result<Vec<LibraryItem>> {
        let mut q = vec![
            ("provider", "internal"),
            ("start", "0"),
            ("limit", "30"),
            ("search", keyword.trim()),
            ("keyword", keyword.trim()),
            ("query", keyword.trim()),
        ];
        if !media_type.is_empty() && media_type != "all" {
            q.push(("media_type", media_type));
        }
        let v: Value = self.call_json("GET", "/plugin/library/items", &q, None, 20)?;
        Ok(decode_vec(&v))
    }

    pub fn library_item_detail(&self, item_id: &str) -> Result<LibraryItem> {
        let id = item_id.trim();
        ensure!(!id.is_empty(), "缺少媒体库条目 ID");
        let v: Value = self.call_json(
            "GET",
            &format!("/plugin/library/items/{id}"),
            &[("provider", "internal")],
            None,
            18,
        )?;
        if let Some(item) = v.get("item").filter(|x| x.is_object()) {
            return Ok(serde_json::from_value(item.clone())?);
        }
        Ok(serde_json::from_value(v)?)
    }

    pub fn delete_library_sources(&self, item_id: &str, source_ids: &[String]) -> Result<()> {
        ensure!(!item_id.trim().is_empty(), "缺少媒体库条目 ID");
        ensure!(!source_ids.is_empty(), "缺少待删除源 ID");
        let resp: Value = self.call_json(
            "DELETE",
            &format!("/plugin/library/items/{}/sources", item_id.trim()),
            &[("provider", "internal")],
            Some(json!({ "source_ids": source_ids })),
            25,
        )?;
        ensure_host_ok(&resp, "删除媒体库重复低分源")
    }

    pub fn scan_library_item(&self, item_id: &str) -> Result<()> {
        let resp: Value = self.call_json(
            "POST",
            &format!("/plugin/library/items/{}/scan", item_id.trim()),
            &[("provider", "internal")],
            Some(json!({})),
            20,
        )?;
        ensure_host_ok(&resp, "刷新媒体库条目")
    }

    pub fn list_pt_sites(&self) -> Vec<PtSite> {
        let Ok(v) = self.call_json::<Value>("GET", "/plugin/sites", &[], None, 10) else {
            return Vec::new();
        };
        decode_vec::<PtSite>(&v)
            .into_iter()
            .filter(|s| s.id > 0 && s.enabled != Some(false))
            .collect()
    }

    pub fn search_pt_torrents(
        &self,
        keyword: &str,
        site_ids: &[u64],
        free_only: bool,
    ) -> Result<Vec<Value>> {
        let mut q = vec![("keyword", keyword.trim())];
        let sites_csv;
        if !site_ids.is_empty() {
            sites_csv = site_ids
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",");
            q.push(("site_ids", &sites_csv));
        }
        if free_only {
            q.push(("free_only", "true"));
        }
        let v: Value = self.call_json("GET", "/plugin/torrents", &q, None, 40)?;
        let list = v
            .as_array()
            .or_else(|| v["items"].as_array())
            .or_else(|| v["data"].as_array())
            .or_else(|| v["results"].as_array())
            .cloned()
            .unwrap_or_default();
        Ok(list)
    }

    pub fn create_media_download(&self, payload: Value) -> Result<String> {
        let resp: Value =
            self.call_json("POST", "/plugin/media-downloads", &[], Some(payload), 30)?;
        ensure_host_ok(&resp, "创建 PT 洗版下载任务")?;
        let id = resp["id"]
            .as_u64()
            .filter(|&n| n > 0)
            .map(|n| n.to_string())
            .or_else(|| {
                resp["id"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string())
            })
            .or_else(|| {
                resp["data"]["id"]
                    .as_u64()
                    .filter(|&n| n > 0)
                    .map(|n| n.to_string())
            })
            .context("宿主未返回有效的下载任务 ID")?;
        Ok(id)
    }

    pub fn get_media_download(&self, download_id: &str) -> Result<Value> {
        self.call_json(
            "GET",
            &format!("/plugin/media-downloads/{}", download_id.trim()),
            &[],
            None,
            15,
        )
    }

    pub fn organize_media_download(&self, download_id: &str, conflict_strategy: &str) -> Result<Value> {
        let overwrite = conflict_strategy == "overwrite";
        let strat = if overwrite { "overwrite" } else { "skip" };
        let resp: Value = self.call_json(
            "POST",
            &format!("/plugin/media-downloads/{}/organize", download_id.trim()),
            &[],
            Some(json!({
                "organize_conflict_strategy": strat,
                "conflict_strategy": strat,
                "overwrite": overwrite,
            })),
            35,
        )?;
        ensure_host_ok(&resp, "触发 PT 下载整理入库")?;
        Ok(resp)
    }

    pub fn notify(&self, title: &str, content: &str, image_url: Option<&str>) {
        let mut payload = json!({
            "title": title.trim(),
            "content": content.trim(),
        });
        if let Some(img) = image_url.map(str::trim).filter(|s| !s.is_empty()) {
            payload["image_url"] = json!(img);
        }
        let _ = self.call_raw("POST", "/plugin/notifications", &[], Some(payload), 10);
    }

    pub fn get_notification_templates(&self) -> HostNotificationTemplates {
        let mut tpls = HostNotificationTemplates::default();
        if let Ok(v) = self.call_json::<Value>("GET", "/settings", &[], None, 6) {
            let root = v.get("data").filter(|x| x.is_object()).unwrap_or(&v);
            let nt_opt = root.get("notification_templates").or_else(|| {
                root.get("system_config")
                    .and_then(|sc| sc.get("notification_templates"))
            });
            if let Some(nt) = nt_opt {
                let load_one = |key: &str, target: &mut NotificationTemplateItem| {
                    if let Some(obj) = nt.get(key).filter(|x| x.is_object()) {
                        if let Some(en) = obj.get("enabled").and_then(Value::as_bool) {
                            target.enabled = en;
                        }
                        if let Some(t) = obj
                            .get("title")
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                        {
                            target.title = t.to_string();
                        }
                        if let Some(c) = obj
                            .get("content")
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                        {
                            target.content = c.to_string();
                        }
                    }
                };
                load_one("transfer_completed", &mut tpls.transfer_completed);
                load_one("download_started", &mut tpls.download_started);
                load_one("library_added", &mut tpls.library_added);
            }
        }
        tpls
    }

    pub fn resolve_tmdb_card_image(&self, tmdb_id: &str, media_type: &str) -> Option<String> {
        let tid = tmdb_id.trim();
        if tid.is_empty() || tid == "0" {
            return None;
        }
        let mt = if media_type == "tv" { "tv" } else { "movie" };
        if let Ok(det) = self.get_tmdb_details(tid, mt) {
            let det = det
                .get("details")
                .filter(|x| x.is_object())
                .unwrap_or(&det);
            let bd = det["backdrop_path"].as_str().unwrap_or_default();
            let ps = det["poster_path"].as_str().unwrap_or_default();
            if let Some(url) = tmdb_card_image_https(bd, ps) {
                return Some(url);
            }
        }
        if let Ok(page) = self.library_items_by_tmdb(tid, mt, None) {
            for item in &page.items {
                if let Some(url) = tmdb_card_image_https(&item.backdrop_path, &item.poster_path) {
                    return Some(url);
                }
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct NotificationTemplateItem {
    pub enabled: bool,
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct HostNotificationTemplates {
    pub transfer_completed: NotificationTemplateItem,
    pub download_started: NotificationTemplateItem,
    pub library_added: NotificationTemplateItem,
}

impl Default for HostNotificationTemplates {
    fn default() -> Self {
        Self {
            transfer_completed: NotificationTemplateItem {
                enabled: true,
                title: "{{#if is_success}}{{media_icon}} {{display_name}} ✅整理完成{{/if}}{{#if is_failed}}⚠️ 影视整理失败: {{name}}{{/if}}".into(),
                content: "{{#if is_success}}{{#if category}}🗂 分类: {{category}}\n{{/if}}{{#if year}}📅 年份: {{year}}{{/if}}{{/if}}{{#if is_failed}}下载已完成，但自动整理失败。\n📁 路径: {{path}}\n❌ 错误: {{error}}\n请检查整理规则、路径权限和文件结构。{{/if}}".into(),
            },
            download_started: NotificationTemplateItem {
                enabled: true,
                title: "{{media_icon}} {{display_title}}{{#if season_episode}} {{season_episode}}{{/if}} 🚀{{download_action}}".into(),
                content: "{{#if torrent_title}}📌 种子: {{torrent_title}}\n{{/if}}{{#if site}}🌐 站点: {{site}}\n{{/if}}{{#if size}}📦 大小: {{size}}\n{{/if}}".into(),
            },
            library_added: NotificationTemplateItem {
                enabled: true,
                title: "{{media_icon}} {{name}}{{#if year}} ({{year}}){{/if}}{{#if season_episode}} {{season_episode}}{{/if}} 🎯加入影库".into(),
                content: "{{#if episode_title}}📺 集标题: {{episode_title}}\n{{/if}}".into(),
            },
        }
    }
}

pub fn tmdb_card_image_https(backdrop_path: &str, poster_path: &str) -> Option<String> {
    let normalize = |raw: &str, size: &str| -> Option<String> {
        let t = raw.trim();
        if t.is_empty() {
            return None;
        }
        if t.starts_with("https://") || t.starts_with("http://") {
            return Some(t.to_string());
        }
        if t.starts_with('/') && !t[1..].contains('/') {
            return Some(format!("https://image.tmdb.org/t/p/{size}{t}"));
        }
        None
    };
    normalize(backdrop_path, "w780").or_else(|| normalize(poster_path, "w500"))
}

pub fn render_mediary_template(
    tpl: &str,
    vars: &std::collections::BTreeMap<&str, String>,
) -> String {
    let mut out = tpl.to_string();
    let is_truthy = |key: &str| {
        vars.get(key).is_some_and(|v| {
            let t = v.trim();
            !t.is_empty() && t != "false" && t != "0"
        })
    };
    for _ in 0..32 {
        let if_pos = out.rfind("{{#if ");
        let unless_pos = out.rfind("{{#unless ");
        let (start, is_unless, tag_len, close_tag) = match (if_pos, unless_pos) {
            (Some(a), Some(b)) if b > a => (b, true, 10usize, "{{/unless}}"),
            (Some(a), _) => (a, false, 6usize, "{{/if}}"),
            (None, Some(b)) => (b, true, 10usize, "{{/unless}}"),
            (None, None) => break,
        };
        let rest = &out[start + tag_len..];
        let Some(open_rel_end) = rest.find("}}") else {
            break;
        };
        let key = rest[..open_rel_end].trim();
        let body_start = start + tag_len + open_rel_end + 2;
        let after_open = &out[body_start..];
        let Some(close_rel) = after_open.find(close_tag) else {
            break;
        };
        let inner = &after_open[..close_rel];
        let close_end = body_start + close_rel + close_tag.len();
        let cond = if is_unless { !is_truthy(key) } else { is_truthy(key) };
        let replacement = if cond { inner.to_string() } else { String::new() };
        out.replace_range(start..close_end, &replacement);
    }

    for (k, v) in vars {
        let token = format!("{{{{{k}}}}}");
        if out.contains(&token) {
            out = out.replace(&token, v.trim());
        }
    }
    while let Some(s_idx) = out.find("{{") {
        if let Some(e_rel) = out[s_idx + 2..].find("}}") {
            out.replace_range(s_idx..(s_idx + 2 + e_rel + 2), "");
        } else {
            break;
        }
    }
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out.trim().to_string()
}

/// Prevents 115 OpenAPI's notorious deleted-CID root fallback:
/// when listing a deleted or non-existent directory CID (`requested_cid != "0"`), 115 may silently
/// return items from the root directory (`cid == "0"` / `parent_id == "0"`).
/// Dropping any mismatched `page.cid` or `item.parent_id` guarantees MediWash can NEVER escape
/// the requested directory into the user's cloud drive root (`云下载`, `媒体库`, etc.).
pub(crate) fn sanitize_open115_page(requested_cid: &str, mut page: Cloud115Page) -> Cloud115Page {
    let req = requested_cid.trim();
    if req.is_empty() || req == "0" {
        return page;
    }
    let resp_cid = page.cid.trim();
    if !resp_cid.is_empty() && resp_cid != req {
        page.cid = req.to_string();
        page.count = 0;
        page.items.clear();
        return page;
    }
    let before = page.items.len();
    page.items.retain(|it| {
        let pid = it.parent_id.trim();
        pid.is_empty() || pid == req
    });
    if before > 0 && page.items.is_empty() {
        page.count = 0;
    }
    page
}

/// Recognizes Mediary v3.4.13+'s safety rejection when selected delete source(s) and retained
/// winner source point to the exact same underlying file (`所选版本与保留版本指向同一文件，拒绝删除`).
pub fn is_same_file_delete_refused(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("指向同一文件")
        || (msg.contains("同一文件") && msg.contains("拒绝删除"))
        || msg.to_ascii_lowercase().contains("same underlying file")
}

/// Adapts a rendered `transfer_completed` host template title for MediWash sub-actions
/// (e.g. `🗑️去重回收`, `🗑️回收清理`, `✨洗版删旧完成`) while preserving custom host prefixes/formats.
pub fn format_transfer_template_title(rendered_title: &str, action_badge: &str) -> String {
    let t = rendered_title.trim();
    if t.is_empty() {
        return action_badge.to_string();
    }
    for default_suffix in ["✅整理完成", "✅ 整理完成", "整理完成"] {
        if t.contains(default_suffix) {
            return t.replacen(default_suffix, action_badge, 1);
        }
    }
    if t.contains(action_badge) {
        t.to_string()
    } else {
        format!("{t} · {action_badge}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mediary_notification_template_and_tmdb_card_url_work() {
        let tpls = HostNotificationTemplates::default();
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("is_success", "true".to_string());
        vars.insert("media_icon", "📺".to_string());
        vars.insert("display_name", "宝珠 · S01E18".to_string());
        vars.insert("category", "电视剧/国产剧/2020-2030".to_string());
        vars.insert("year", "2026".to_string());
        let title = render_mediary_template(&tpls.transfer_completed.title, &vars);
        let body = render_mediary_template(&tpls.transfer_completed.content, &vars);
        assert_eq!(title, "📺 宝珠 · S01E18 ✅整理完成");
        assert!(body.contains("🗂 分类: 电视剧/国产剧/2020-2030"));
        assert!(body.contains("📅 年份: 2026"));

        assert_eq!(
            format_transfer_template_title(&title, "🗑️回收清理"),
            "📺 宝珠 · S01E18 🗑️回收清理"
        );
        assert_eq!(
            format_transfer_template_title("[自定义前缀] 宝珠", "✨洗版删旧完成"),
            "[自定义前缀] 宝珠 · ✨洗版删旧完成"
        );
        assert!(is_same_file_delete_refused(&anyhow::anyhow!(
            "删除媒体库重复低分源失败: 所选版本与保留版本指向同一文件，拒绝删除"
        )));
        assert!(!is_same_file_delete_refused(&anyhow::anyhow!(
            "删除媒体库重复低分源失败: 网络超时"
        )));

        // Verify deleted-CID root fallback (`cid="0"` or `parent_id="0"`) is 100% blocked
        let root_item = Cloud115File {
            file_id: "3269149014077998904".to_string(),
            parent_id: "0".to_string(),
            name: "云下载".to_string(),
            is_dir: true,
            ..Default::default()
        };
        let echoed_fallback = Cloud115Page {
            cid: "999888777".to_string(),
            count: 1,
            items: vec![root_item.clone()],
            ..Default::default()
        };
        let clean1 = sanitize_open115_page("999888777", echoed_fallback);
        assert!(clean1.items.is_empty());
        assert_eq!(clean1.count, 0);

        let root_cid_fallback = Cloud115Page {
            cid: "0".to_string(),
            count: 1,
            items: vec![Cloud115File {
                parent_id: String::new(),
                ..root_item
            }],
            ..Default::default()
        };
        let clean2 = sanitize_open115_page("999888777", root_cid_fallback);
        assert!(clean2.items.is_empty());
        assert_eq!(clean2.count, 0);

        let valid_page = Cloud115Page {
            cid: "999888777".to_string(),
            count: 1,
            items: vec![Cloud115File {
                file_id: "111".to_string(),
                parent_id: "999888777".to_string(),
                name: "ep01.mkv".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let clean3 = sanitize_open115_page("999888777", valid_page);
        assert_eq!(clean3.items.len(), 1);

        assert_eq!(
            tmdb_card_image_https("/bd123.jpg", "/ps456.jpg").as_deref(),
            Some("https://image.tmdb.org/t/p/w780/bd123.jpg")
        );
        assert_eq!(
            tmdb_card_image_https("", "/ps456.jpg").as_deref(),
            Some("https://image.tmdb.org/t/p/w500/ps456.jpg")
        );
        assert_eq!(tmdb_card_image_https("/api/local/poster", ""), None);

        let skipped_resp = json!({
            "ok": false,
            "data": {
                "moved": 0,
                "skipped": 1,
                "failed": 0,
                "skipped_items": [
                    { "name": "庶女.mp4", "skip_reason": "115 request failed: 该目录名称已存在。" }
                ]
            }
        });
        let err = ensure_host_ok(&skipped_resp, "115 原生二级分类归档入库").unwrap_err();
        assert!(format!("{err:#}").contains("该目录名称已存在"));
    }
}

fn find_nested_error(v: &Value, depth: usize) -> Option<String> {
    if depth > 4 {
        return None;
    }
    match v {
        Value::Object(map) => {
            for k in [
                "error",
                "error_message",
                "skip_reason",
                "reason",
                "message",
                "detail",
                "err",
            ] {
                if let Some(s) = map.get(k).and_then(Value::as_str).map(str::trim) {
                    if !s.is_empty() && !matches!(s, "ok" | "organized" | "success" | "done") {
                        return Some(s.to_string());
                    }
                }
            }
            for k in [
                "data",
                "failed_items",
                "skipped_items",
                "errors",
                "failed",
                "skipped",
                "items",
                "results",
                "logs",
            ] {
                if let Some(child) = map.get(k) {
                    if let Some(found) = find_nested_error(child, depth + 1) {
                        return Some(found);
                    }
                }
            }
            None
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    return Some(s.to_string());
                }
                if let Some(found) = find_nested_error(item, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

pub fn ensure_host_ok(v: &Value, context_label: &str) -> Result<()> {
    let ok = v["ok"] != false
        && v["success"] != false
        && v["state"] != false
        && v["errno"]
            .as_i64()
            .is_none_or(|n| n == 0 || (n == 4100024 && v["already_received"] == true));
    if !ok {
        let detail_msg = find_nested_error(v, 0);
        let msg = detail_msg.unwrap_or_else(|| {
            let scope = v.get("data").filter(|d| d.is_object()).unwrap_or(v);
            let moved = scope["moved"].as_u64();
            let skipped = scope["skipped"].as_u64();
            let failed = scope["failed"].as_u64();
            if moved == Some(0) && (skipped.unwrap_or(0) > 0 || failed.unwrap_or(0) > 0) {
                return format!(
                    "宿主整理跳过或失败（moved=0, skipped={}, failed={}，常见于目标目录同名冲突或已在库）",
                    skipped.unwrap_or(0),
                    failed.unwrap_or(0)
                );
            }
            let s = v.to_string();
            if s != "{}" {
                let preview: String = s.chars().take(300).collect();
                let tail = if s.chars().count() > 300 { "..." } else { "" };
                format!("宿主返回未成功状态 ({preview}{tail})")
            } else {
                "宿主回执未确认成功".to_string()
            }
        });
        return Err(anyhow!("{context_label}失败：{msg}"));
    }
    Ok(())
}

fn decode_vec<T: DeserializeOwned>(v: &Value) -> Vec<T> {
    if let Some(arr) = v.as_array() {
        return arr
            .iter()
            .filter_map(|x| serde_json::from_value(x.clone()).ok())
            .collect();
    }
    for k in ["items", "data", "results", "sites", "list"] {
        if let Some(arr) = v.get(k).and_then(Value::as_array) {
            return arr
                .iter()
                .filter_map(|x| serde_json::from_value(x.clone()).ok())
                .collect();
        }
    }
    Vec::new()
}

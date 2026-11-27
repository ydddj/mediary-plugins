use anyhow::{anyhow, ensure, Context, Result};
use chrono::{NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

const MAX_RANGE_SHA1_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostStatus {
    pub plugin_enabled: bool,
    pub connected: bool,
    pub open_api_connected: bool,
    pub web_cookie_ready: bool,
    pub media_service_read_ready: bool,
    pub media_service_write_ready: bool,
    pub playback_hook_approved: bool,
    pub pending_scope_approval: bool,
    pub socket_ready: bool,
    pub proxy_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolvedItemMedia {
    pub item_id: String,
    pub item_name: String,
    pub item_type: String,
    pub series_id: String,
    pub season_id: String,
    pub series_tmdb_id: i64,
    pub tmdb_id: i64,
    pub season: i32,
    pub episode: i32,
    pub poster_url: String,
    pub file_id: String,
    pub pick_code: String,
    pub sha1: String,
    pub file_size: u64,
    pub strm_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostUserBrief {
    pub user_id: String,
    pub username: String,
    pub is_admin: bool,
    pub is_disabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostActivePlayback {
    pub session_key: String,
    pub user_id: String,
    pub username: String,
    pub item_id: String,
    pub item_name: String,
    pub client: String,
    pub device_id: String,
    pub device_name: String,
    pub user_agent: String,
    pub client_ip: String,
    pub started_at: i64,
    pub last_seen_at: i64,
}

pub fn host_env_proxy_url() -> String {
    for key in ["MEDIARY_PLUGIN_PROXY_URL", "HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"] {
        if let Ok(val) = env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

pub fn inspect_local_plugin_state(data_dir: &Path) -> (bool, bool, bool, bool, bool) {
    // Returns (plugin_enabled, playback_hook_approved, pending_scope_approval, has_ms_read, has_ms_write)
    let mut plugin_enabled = env::var("MEDIARY_PLUGIN_ENABLED")
        .ok()
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(true);
    let mut hook_approved = env::var("MEDIARY_PLUGIN_PLAYBACK_HOOK_APPROVED")
        .ok()
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    let mut pending_scope = false;
    let mut has_ms_read = false;
    let mut has_ms_write = false;

    let cfg_path = env::var_os("MEDIARY_PLUGIN_CONFIG_PATH")
        .map(PathBuf::from)
        .or_else(|| data_dir.parent().map(|p| p.join("config.json")));

    if let Some(p) = cfg_path.filter(|p| p.is_file()) {
        if let Ok(raw) = fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(b) = v.get("enabled").and_then(Value::as_bool) {
                    plugin_enabled = b;
                }
                if let Some(b) = v.get("playback_hook_approved").and_then(Value::as_bool) {
                    hook_approved = b;
                }
                if let Some(b) = v.get("pending_scope_approval").and_then(Value::as_bool) {
                    pending_scope = b;
                }
                if let Some(arr) = v.get("granted_scopes").and_then(Value::as_array) {
                    for sc in arr.iter().filter_map(Value::as_str) {
                        match sc.trim() {
                            "media-service:read" => has_ms_read = true,
                            "media-service:write" => has_ms_write = true,
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    (plugin_enabled, hook_approved, pending_scope, has_ms_read, has_ms_write)
}

pub fn playback_socket_path(data_dir: &Path) -> PathBuf {
    env::var_os("MEDIARY_PLUGIN_PLAYBACK_SOCKET")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("playback-hook.sock"))
}

pub fn fast_host_status(data_dir: &Path) -> HostStatus {
    let (plugin_enabled, hook_approved, pending_scope, ms_read, ms_write) =
        inspect_local_plugin_state(data_dir);
    HostStatus {
        plugin_enabled,
        connected: env::var_os("MEDIARY_PLUGIN_API_URL").is_some(),
        open_api_connected: false,
        web_cookie_ready: false,
        media_service_read_ready: ms_read,
        media_service_write_ready: ms_write,
        playback_hook_approved: hook_approved,
        pending_scope_approval: pending_scope,
        socket_ready: playback_socket_path(data_dir).exists(),
        proxy_url: host_env_proxy_url(),
    }
}

fn parse_host_ts(raw: &Value, fallback: i64) -> i64 {
    if let Some(n) = raw.as_i64() {
        return n;
    }
    let Some(s) = raw.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return fallback;
    };
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.timestamp();
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            if let Some(local_dt) = chrono::Local.from_local_datetime(&ndt).single() {
                return local_dt.timestamp();
            }
            return ndt.and_utc().timestamp();
        }
    }
    fallback
}

pub struct HostClient {
    base_url: Url,
    token: String,
    agent: ureq::Agent,
}

impl HostClient {
    pub fn from_env() -> Option<Self> {
        let raw_url = env::var("MEDIARY_PLUGIN_API_URL").ok()?;
        let token = env::var("MEDIARY_PLUGIN_TOKEN").ok()?;
        if raw_url.trim().is_empty() || token.trim().is_empty() {
            return None;
        }
        let base_url = Url::parse(&format!("{}/", raw_url.trim().trim_end_matches('/'))).ok()?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .redirects(0)
            .build();
        Some(Self {
            base_url,
            token: token.trim().to_string(),
            agent,
        })
    }

    pub fn call_json(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        timeout_secs: u64,
    ) -> Result<Value> {
        let full = format!(
            "{}/{}",
            self.base_url.as_str().trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut req = self
            .agent
            .request(method, &full)
            .timeout(Duration::from_secs(timeout_secs.max(2)))
            .set("Authorization", &format!("Bearer {}", self.token));
        for (k, v) in query {
            req = req.query(k, v);
        }
        let resp = match body {
            Some(payload) => req.send_json(payload),
            None => req.call(),
        };
        match resp {
            Ok(r) => {
                let raw = r.into_string().unwrap_or_default();
                let text = if raw.trim().is_empty() { "{}" } else { &raw };
                Ok(serde_json::from_str(text)?)
            }
            Err(ureq::Error::Status(code, r)) => {
                let raw = r.into_string().unwrap_or_default();
                Err(anyhow!("HTTP {code}: {}", &raw[..raw.len().min(180)]))
            }
            Err(e) => Err(anyhow!("请求宿主 API 失败: {e}")),
        }
    }

    pub fn host_status(&self, data_dir: &Path) -> HostStatus {
        let (plugin_enabled, hook_approved, pending_scope, cfg_ms_read, cfg_ms_write) =
            inspect_local_plugin_state(data_dir);
        let sock_exists = playback_socket_path(data_dir).exists();
        let mut st = HostStatus {
            plugin_enabled,
            connected: true,
            playback_hook_approved: hook_approved,
            pending_scope_approval: pending_scope,
            media_service_read_ready: cfg_ms_read,
            media_service_write_ready: cfg_ms_write,
            socket_ready: sock_exists,
            proxy_url: host_env_proxy_url(),
            ..Default::default()
        };
        let is_ready = |v: &Value| -> bool {
            if let Some(s) = v.get("status").and_then(Value::as_str) {
                if matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "connected" | "ok" | "ready" | "authorized" | "logged_in"
                ) {
                    return true;
                }
            }
            v.get("connected").and_then(Value::as_bool) == Some(true)
        };
        if let Ok(v) = self.call_json("GET", "/open115/status", &[], None, 4) {
            st.open_api_connected = is_ready(&v);
        }
        if let Ok(v) = self.call_json("GET", "/open115/web-cookie/status", &[], None, 4) {
            st.web_cookie_ready = is_ready(&v);
        }
        if st.proxy_url.is_empty() {
            if let Ok(v) = self.call_json("GET", "/settings", &[], None, 4) {
                if let Some(p) = v["tmdb_proxy_url"].as_str().filter(|s| !s.trim().is_empty()) {
                    st.proxy_url = p.trim().to_string();
                }
            }
        }
        if self.call_json("GET", "/media-service/users", &[], None, 4).is_ok() {
            st.media_service_read_ready = true;
        }
        st
    }

    /// Native Mediary 3.4.6 `POST /api/plugin/open115/range-sha1` (<= 4 MiB)
    pub fn try_host_range_sha1(
        &self,
        pickcode: &str,
        file_id: &str,
        file_sha1: &str,
        range_spec: &str,
    ) -> Option<String> {
        let clean_range = range_spec.trim().trim_start_matches("bytes=");
        let (s_str, e_str) = clean_range.split_once('-')?;
        let start: usize = s_str.trim().parse().ok()?;
        let end: usize = e_str.trim().parse().ok()?;
        if end < start || (end - start + 1) > MAX_RANGE_SHA1_BYTES {
            return None;
        }
        let mut body = json!({
            "pick_code": pickcode.trim(),
            "range": format!("{start}-{end}"),
        });
        if !file_id.trim().is_empty() {
            body["file_id"] = json!(file_id.trim());
        }
        if file_sha1.trim().len() == 40 {
            body["sha1"] = json!(file_sha1.trim().to_ascii_uppercase());
        }
        let v = self
            .call_json("POST", "/plugin/open115/range-sha1", &[], Some(body), 8)
            .ok()?;
        v["sha1"]
            .as_str()
            .or_else(|| v["data"]["sha1"].as_str())
            .map(|s| s.trim().to_ascii_uppercase())
            .filter(|s| s.len() == 40)
    }

    /// Native Mediary 3.4.6 `GET /api/media-service/users` (`media-service:read`)
    pub fn list_users(&self) -> Result<Vec<HostUserBrief>> {
        let v = self
            .call_json("GET", "/media-service/users", &[], None, 6)
            .map_err(|e| {
                anyhow!("读取宿主观影用户失败（请确认已在插件配置中勾选授予 media-service:read 权限）: {e}")
            })?;
        let arr = v["users"]
            .as_array()
            .or_else(|| v.as_array())
            .ok_or_else(|| anyhow!("/media-service/users 响应缺少 users 数组"))?;
        let mut out = Vec::with_capacity(arr.len());
        for u in arr {
            let username = u["username"].as_str().unwrap_or_default().trim().to_string();
            if username.is_empty() {
                continue;
            }
            let id = u["id"]
                .as_i64()
                .map(|n| n.to_string())
                .or_else(|| u["id"].as_u64().map(|n| n.to_string()))
                .or_else(|| u["id"].as_str().map(|s| s.trim().to_string()))
                .unwrap_or_default();
            out.push(HostUserBrief {
                user_id: id,
                username,
                is_admin: u["is_admin"].as_bool() == Some(true),
                is_disabled: u["enabled"].as_bool() == Some(false),
            });
        }
        Ok(out)
    }

    /// Native Mediary 3.4.6 `GET /api/media-service/active-playbacks` (`media-service:read`)
    pub fn list_active_playbacks(&self) -> Result<Vec<HostActivePlayback>> {
        let v = self.call_json("GET", "/media-service/active-playbacks", &[], None, 5)?;
        let arr = v["playbacks"]
            .as_array()
            .or_else(|| v.as_array())
            .ok_or_else(|| anyhow!("/media-service/active-playbacks 响应缺少 playbacks 数组"))?;
        let now = crate::models::now_ts();
        let mut out = Vec::with_capacity(arr.len());
        for row in arr {
            let session_key = row["session_key"]
                .as_str()
                .or_else(|| row["session_id"].as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            let username = row["username"].as_str().unwrap_or_default().trim().to_string();
            let user_id = row["user_id"]
                .as_i64()
                .map(|n| n.to_string())
                .or_else(|| row["user_id"].as_str().map(|s| s.trim().to_string()))
                .unwrap_or_default();
            out.push(HostActivePlayback {
                session_key,
                user_id,
                username,
                item_id: row["item_id"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| row["item_id"].as_i64().map(|n| n.to_string()))
                    .unwrap_or_default(),
                item_name: row["item_name"]
                    .as_str()
                    .or_else(|| row["title"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                client: row["client"].as_str().unwrap_or_default().to_string(),
                device_id: row["device_id"].as_str().unwrap_or_default().to_string(),
                device_name: row["device_name"].as_str().unwrap_or_default().to_string(),
                user_agent: row["user_agent"].as_str().unwrap_or_default().to_string(),
                client_ip: row["client_ip"]
                    .as_str()
                    .or_else(|| row["ip"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                started_at: parse_host_ts(&row["started_at"], now),
                last_seen_at: parse_host_ts(&row["last_seen_at"], now),
            });
        }
        Ok(out)
    }

    /// Native Mediary 3.4.6 `PUT /api/media-service/users/:id/enabled` (`media-service:write`)
    pub fn try_set_user_enabled(&self, user_id: &str, enabled: bool) -> bool {
        let id = user_id.trim();
        if id.is_empty() {
            return false;
        }
        self.call_json(
            "PUT",
            &format!("/media-service/users/{id}/enabled"),
            &[],
            Some(json!({ "enabled": enabled })),
            6,
        )
        .is_ok()
    }

    /// Native Mediary 3.4.6 `DELETE /api/media-service/sessions/:key` (`media-service:write`)
    /// Deletes watch session, sends targeted WebSocket Stop, and revokes session ticket for 12h.
    pub fn kick_session(&self, session_key: &str) -> Result<()> {
        let key = session_key.trim();
        ensure!(!key.is_empty(), "缺少宿主 session_key");
        let encoded: String = form_urlencoded::byte_serialize(key.as_bytes()).collect();
        self.call_json(
            "DELETE",
            &format!("/media-service/sessions/{encoded}"),
            &[],
            None,
            6,
        )?;
        Ok(())
    }

    pub fn notify(&self, title: &str, content: &str) {
        let _ = self.notify_with_image(title, content, None);
    }

    pub fn notify_with_image(
        &self,
        title: &str,
        content: &str,
        image_url: Option<&str>,
    ) -> Result<()> {
        let mut payload = json!({
            "title": title.trim(),
            "content": content.trim(),
        });
        if let Some(img) = image_url
            .map(str::trim)
            .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
        {
            payload["image_url"] = json!(img);
        }
        self.call_json("POST", "/plugin/notifications", &[], Some(payload), 8)
            .context("提交主程序通知失败（请确认已授予 notifications:send 权限且宿主已配置通知渠道）")?;
        Ok(())
    }

    /// Resolves an `item_id` (+ optional `media_source_id` / `pick_code_hint`) into `(pick_code, sha1, file_size, item_name, episode info)`
    /// via Mediary's local `/api/plugin/library/items/:id?provider=internal`.
    pub fn resolve_item_media(
        &self,
        item_id: &str,
        media_source_id: &str,
        pick_code_hint: &str,
    ) -> Option<ResolvedItemMedia> {
        let id = item_id.trim();
        if id.is_empty() {
            return None;
        }
        let v = self
            .call_json(
                "GET",
                &format!("/plugin/library/items/{id}"),
                &[("provider", "internal")],
                None,
                4,
            )
            .ok()?;
        let item = v.get("item").filter(|x| x.is_object()).unwrap_or(&v);
        parse_item_to_media(id, item, media_source_id, pick_code_hint)
    }

    /// Resolves a detail-page `item_id` (Movie, Episode, Video, Series, or Season) into a playable media target.
    /// When the user opens a Series or Season detail page, automatically locates the first episode (`S01E01`).
    pub fn resolve_playable_for_detail(
        &self,
        item_id: &str,
        media_source_id: &str,
        pick_code_hint: &str,
    ) -> Option<ResolvedItemMedia> {
        let id = item_id.trim();
        if id.is_empty() {
            return None;
        }
        let v = self
            .call_json(
                "GET",
                &format!("/plugin/library/items/{id}"),
                &[("provider", "internal")],
                None,
                4,
            )
            .ok()?;
        let item = v.get("item").filter(|x| x.is_object()).unwrap_or(&v);
        if let Some(direct) = parse_item_to_media(id, item, media_source_id, pick_code_hint) {
            return Some(direct);
        }

        let item_type = item["item_type"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !matches!(item_type.as_str(), "series" | "tv" | "show" | "season") {
            return None;
        }
        let tmdb = item["tmdb_id"]
            .as_i64()
            .or_else(|| item["series_tmdb_id"].as_i64())
            .unwrap_or(0);
        if tmdb <= 0 {
            return None;
        }
        let tmdb_s = tmdb.to_string();
        let list_v = self
            .call_json(
                "GET",
                "/plugin/library/items",
                &[
                    ("provider", "internal"),
                    ("item_type", "episode"),
                    ("tmdb_id", &tmdb_s),
                    ("limit", "500"),
                ],
                None,
                5,
            )
            .ok()?;
        let items = list_v["items"].as_array()?;
        let mut cands: Vec<(i32, i32, String)> = items
            .iter()
            .filter_map(|row| {
                let eid = row["id"].as_str()?.trim().to_string();
                if eid.is_empty() {
                    return None;
                }
                let s = row["season"].as_i64().unwrap_or(1) as i32;
                let e = row["episode"].as_i64().unwrap_or(1) as i32;
                let s_rank = if s <= 0 { 9999 } else { s };
                Some((s_rank, e.max(0), eid))
            })
            .collect();
        cands.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        for (_, _, eid) in cands.into_iter().take(6) {
            if let Some(ep_media) = self.resolve_item_media(&eid, "", "") {
                if item_type == "season" {
                    if ep_media.season_id == id || ep_media.season_id.is_empty() {
                        return Some(ep_media);
                    }
                } else if ep_media.series_id == id || ep_media.series_id.is_empty() {
                    return Some(ep_media);
                }
            }
        }
        None
    }

    /// Finds the next episode (`SxxE(yy+1)` or next season `S(xx+1)E01`) of the same TV series
    /// via Mediary `/plugin/library/items` API, with local `.strm` sibling directory fallback.
    pub fn find_next_episode_media(&self, cur: &ResolvedItemMedia) -> Option<ResolvedItemMedia> {
        if !cur.item_type.eq_ignore_ascii_case("episode") && cur.episode <= 0 && cur.strm_path.is_empty() {
            return None;
        }
        let (eff_season, eff_episode) = if cur.episode > 0 {
            (cur.season.max(1), cur.episode)
        } else {
            parse_se_from_text(&cur.item_name)
                .or_else(|| parse_se_from_text(&cur.strm_path))
                .unwrap_or((cur.season.max(1), 0))
        };

        let tmdb = if cur.series_tmdb_id > 0 {
            cur.series_tmdb_id
        } else {
            cur.tmdb_id
        };
        if tmdb > 0 && eff_episode > 0 {
            let tmdb_s = tmdb.to_string();
            if let Ok(list_v) = self.call_json(
                "GET",
                "/plugin/library/items",
                &[
                    ("provider", "internal"),
                    ("item_type", "episode"),
                    ("tmdb_id", &tmdb_s),
                    ("limit", "500"),
                ],
                None,
                5,
            ) {
                if let Some(items) = list_v["items"].as_array() {
                    let mut cands: Vec<(i32, i32, String)> = items
                        .iter()
                        .filter_map(|row| {
                            let eid = row["id"].as_str()?.trim().to_string();
                            if eid.is_empty() || eid == cur.item_id {
                                return None;
                            }
                            let s = row["season"].as_i64().unwrap_or(0) as i32;
                            let e = row["episode"].as_i64().unwrap_or(0) as i32;
                            if s <= 0 || e <= 0 {
                                return None;
                            }
                            if (s, e) > (eff_season, eff_episode) {
                                Some((s, e, eid))
                            } else {
                                None
                            }
                        })
                        .collect();
                    cands.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

                    for (_, _, eid) in cands.into_iter().take(4) {
                        if let Some(ep_media) = self.resolve_item_media(&eid, "", "") {
                            if cur.series_id.is_empty()
                                || ep_media.series_id.is_empty()
                                || ep_media.series_id == cur.series_id
                            {
                                return Some(ep_media);
                            }
                        }
                    }
                }
            }
        }

        find_next_strm_sibling(&cur.strm_path, eff_season, eff_episode)
    }
}

fn normalize_tmdb_image_url(raw: Option<&str>) -> String {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return String::new();
    };
    if s.starts_with("http://") || s.starts_with("https://") {
        return s.to_string();
    }
    if s.starts_with('/')
        && !s.starts_with("/strm/")
        && !s.starts_with("/media/")
        && !s.starts_with("/vol")
        && (s.ends_with(".jpg") || s.ends_with(".png") || s.ends_with(".webp"))
    {
        return format!("https://image.tmdb.org/t/p/w500{s}");
    }
    String::new()
}

fn parse_item_to_media(
    id: &str,
    item: &Value,
    media_source_id: &str,
    pick_code_hint: &str,
) -> Option<ResolvedItemMedia> {
    let item_name = item["name"].as_str().unwrap_or_default().to_string();
    let item_type = item["item_type"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let series_id = item["series_id"].as_str().unwrap_or_default().to_string();
    let season_id = item["season_id"]
        .as_str()
        .or_else(|| item["parent_id"].as_str())
        .unwrap_or_default()
        .to_string();
    let series_tmdb_id = item["series_tmdb_id"].as_i64().unwrap_or(0);
    let tmdb_id = item["tmdb_id"].as_i64().unwrap_or(0);
    let season = item["season"].as_i64().unwrap_or(0) as i32;
    let episode = item["episode"].as_i64().unwrap_or(0) as i32;
    let poster_url = {
        let p = normalize_tmdb_image_url(item["poster_path"].as_str());
        if !p.is_empty() {
            p
        } else {
            normalize_tmdb_image_url(item["backdrop_path"].as_str())
        }
    };

    let sources = item["sources"]
        .as_array()
        .or_else(|| item["versions"].as_array())?;
    let ms_id = media_source_id.trim();
    let pc_hint = pick_code_hint.trim();

    let source_matches_ms = |s: &Value| -> bool {
        if ms_id.is_empty() {
            return false;
        }
        s["id"].as_str().map(str::trim) == Some(ms_id)
            || s["source_id"].as_str().map(str::trim) == Some(ms_id)
            || s["media_source_id"].as_str().map(str::trim) == Some(ms_id)
            || s["id"].as_i64().map(|n| n.to_string()).as_deref() == Some(ms_id)
            || s["source_id"].as_i64().map(|n| n.to_string()).as_deref() == Some(ms_id)
    };
    let source_matches_pc = |s: &Value| -> bool {
        if pc_hint.is_empty() {
            return false;
        }
        if let Some(p) = s["pick_code"]
            .as_str()
            .or_else(|| s["pickcode"].as_str())
            .map(str::trim)
            .filter(|x| !x.is_empty())
        {
            if p.eq_ignore_ascii_case(pc_hint) {
                return true;
            }
        }
        let sp = s["strm_path"]
            .as_str()
            .or_else(|| s["file_path"].as_str())
            .unwrap_or_default();
        if !sp.is_empty() {
            let (pc_strm, _) = read_strm_if_local(sp);
            if !pc_strm.is_empty() && pc_strm.eq_ignore_ascii_case(pc_hint) {
                return true;
            }
        }
        false
    };

    let chosen = sources
        .iter()
        .find(|s| source_matches_ms(s))
        .or_else(|| sources.iter().find(|s| source_matches_pc(s)))
        .or_else(|| sources.first())?;

    let strm_p = chosen["strm_path"]
        .as_str()
        .or_else(|| chosen["file_path"].as_str())
        .or_else(|| item["path"].as_str())
        .unwrap_or_default();
    let (mut pc, strm_name) = read_strm_if_local(strm_p);
    if pc.is_empty() {
        if let Some(p) = chosen["pick_code"]
            .as_str()
            .or_else(|| chosen["pickcode"].as_str())
        {
            pc = p.trim().to_string();
        }
    }
    if pc.is_empty() {
        return None;
    }
    let file_id = chosen["file_id"]
        .as_str()
        .map(str::to_string)
        .or_else(|| chosen["file_id"].as_u64().map(|n| n.to_string()))
        .unwrap_or_default();
    let sha1 = chosen["sha1"].as_str().unwrap_or_default().to_ascii_uppercase();
    let size = chosen["size"]
        .as_u64()
        .or_else(|| chosen["file_size"].as_u64())
        .or_else(|| chosen["size"].as_f64().map(|f| f.max(0.0) as u64))
        .unwrap_or(0);
    let final_name = if !strm_name.is_empty() {
        strm_name
    } else if let Some(n) = chosen["name"].as_str().filter(|s| !s.is_empty()) {
        n.to_string()
    } else {
        item_name
    };

    Some(ResolvedItemMedia {
        item_id: id.to_string(),
        item_name: final_name,
        item_type,
        series_id,
        season_id,
        series_tmdb_id,
        tmdb_id,
        season,
        episode,
        poster_url,
        file_id,
        pick_code: pc,
        sha1,
        file_size: size,
        strm_path: strm_p.to_string(),
    })
}

fn parse_se_from_text(s: &str) -> Option<(i32, i32)> {
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'S' || bytes[i] == b's' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < bytes.len() && (bytes[j] == b'E' || bytes[j] == b'e') {
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 {
                    if let (Ok(sn), Ok(ep)) = (
                        s[i + 1..j].parse::<i32>(),
                        s[j + 1..k].parse::<i32>(),
                    ) {
                        return Some((sn, ep));
                    }
                }
            }
        }
    }
    None
}

fn find_next_strm_sibling(
    strm_path: &str,
    cur_season: i32,
    cur_episode: i32,
) -> Option<ResolvedItemMedia> {
    if cur_episode <= 0 {
        return None;
    }
    let clean = strm_path.trim().trim_start_matches("file://");
    let p = Path::new(clean);
    let parent = p.parent()?;
    if !parent.is_dir() {
        return None;
    }
    let scan_dir_for = |dir: &Path, target_s: i32, target_e: i32| -> Option<ResolvedItemMedia> {
        let entries = fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let ep_path = entry.path();
            if ep_path.extension().and_then(|x| x.to_str()) != Some("strm") {
                continue;
            }
            let fname = ep_path.file_name().and_then(|x| x.to_str()).unwrap_or_default();
            if let Some((s, e)) = parse_se_from_text(fname) {
                if s == target_s && e == target_e {
                    let path_str = ep_path.to_string_lossy().to_string();
                    let (pc, strm_name) = read_strm_if_local(&path_str);
                    if !pc.is_empty() {
                        return Some(ResolvedItemMedia {
                            item_name: if strm_name.is_empty() {
                                fname.trim_end_matches(".strm").to_string()
                            } else {
                                strm_name
                            },
                            item_type: "episode".to_string(),
                            season: target_s,
                            episode: target_e,
                            pick_code: pc,
                            strm_path: path_str,
                            ..Default::default()
                        });
                    }
                }
            }
        }
        None
    };

    if let Some(found) = scan_dir_for(parent, cur_season.max(1), cur_episode + 1) {
        return Some(found);
    }
    if let Some(series_dir) = parent.parent() {
        for folder_name in [
            format!("Season {:02}", cur_season.max(1) + 1),
            format!("Season {}", cur_season.max(1) + 1),
        ] {
            let next_season_dir = series_dir.join(folder_name);
            if next_season_dir.is_dir() {
                if let Some(found) = scan_dir_for(&next_season_dir, cur_season.max(1) + 1, 1) {
                    return Some(found);
                }
            }
        }
    }
    None
}

pub fn extract_pickcode_and_name_from_str(s: &str) -> (String, String) {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return (String::new(), String::new());
    }
    if let Ok(u) = Url::parse(trimmed) {
        let mut pc = String::new();
        let mut name = String::new();
        for (k, v) in u.query_pairs() {
            if k.eq_ignore_ascii_case("pickcode") || k.eq_ignore_ascii_case("pick_code") {
                pc = v.trim().to_string();
            } else if k.eq_ignore_ascii_case("name") || k.eq_ignore_ascii_case("filename") {
                name = v.trim().to_string();
            }
        }
        if !pc.is_empty() {
            return (pc, name);
        }
    }
    if let Some(rest) = trimmed.strip_prefix("pickcode:") {
        return (rest.trim().to_string(), String::new());
    }
    for part in trimmed.split(['?', '&']) {
        if let Some(val) = part
            .strip_prefix("pickcode=")
            .or_else(|| part.strip_prefix("pick_code="))
        {
            return (val.trim().to_string(), String::new());
        }
    }
    (String::new(), String::new())
}

fn read_strm_if_local(path_or_url: &str) -> (String, String) {
    let (pc, name) = extract_pickcode_and_name_from_str(path_or_url);
    if !pc.is_empty() {
        return (pc, name);
    }
    let clean_path = path_or_url.trim().trim_start_matches("file://");
    if clean_path.ends_with(".strm") && Path::new(clean_path).is_file() {
        if let Ok(content) = fs::read_to_string(clean_path) {
            if let Some(first_line) = content.lines().map(str::trim).find(|l| !l.is_empty()) {
                return extract_pickcode_and_name_from_str(first_line);
            }
        }
    }
    (String::new(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_se_and_strm_sibling_discovery() {
        assert_eq!(parse_se_from_text("甄嬛传.2011.S01E08.2160p.mkv"), Some((1, 8)));
        assert_eq!(parse_se_from_text("Show.s02e15.strm"), Some((2, 15)));
        assert_eq!(
            normalize_tmdb_image_url(Some("/abc123.jpg")),
            "https://image.tmdb.org/t/p/w500/abc123.jpg"
        );
        assert_eq!(
            normalize_tmdb_image_url(Some("file:///strm/cloud/fanart.jpg")),
            ""
        );

        let base = env::temp_dir().join(format!("mn_strm_test_{}", std::process::id()));
        let s1 = base.join("Season 01");
        let s2 = base.join("Season 02");
        fs::create_dir_all(&s1).unwrap();
        fs::create_dir_all(&s2).unwrap();
        let e1 = s1.join("Demo.S01E01.strm");
        let e2 = s1.join("Demo.S01E02.strm");
        let s2e1 = s2.join("Demo.S02E01.strm");
        fs::write(&e1, "http://127.0.0.1:8118/api/302?pickcode=pc_s01e01&name=Demo.S01E01.mkv\n").unwrap();
        fs::write(&e2, "http://127.0.0.1:8118/api/302?pickcode=pc_s01e02&name=Demo.S01E02.mkv\n").unwrap();
        fs::write(&s2e1, "http://127.0.0.1:8118/api/302?pickcode=pc_s02e01&name=Demo.S02E01.mkv\n").unwrap();

        let n1 = find_next_strm_sibling(&e1.to_string_lossy(), 1, 1).expect("should find S01E02");
        assert_eq!(n1.pick_code, "pc_s01e02");
        assert_eq!((n1.season, n1.episode), (1, 2));

        let n2 = find_next_strm_sibling(&e2.to_string_lossy(), 1, 2).expect("should find S02E01");
        assert_eq!(n2.pick_code, "pc_s02e01");
        assert_eq!((n2.season, n2.episode), (2, 1));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_parse_item_to_media_multi_version_matching() {
        let item = json!({
            "id": "movie_mv_1",
            "name": "Demo Movie",
            "item_type": "movie",
            "sources": [
                {
                    "id": "ms_v1",
                    "name": "Demo.4K.Remux.mkv",
                    "pick_code": "pc_version_one",
                    "sha1": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    "size": 50_000_000_000u64
                },
                {
                    "id": "ms_v2",
                    "name": "Demo.1080p.Sub.mkv",
                    "pick_code": "pc_version_two",
                    "sha1": "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
                    "size": 8_000_000_000u64
                }
            ]
        });

        // 1. Explicit media_source_id selects v2
        let m_by_ms = parse_item_to_media("movie_mv_1", &item, "ms_v2", "").unwrap();
        assert_eq!(m_by_ms.pick_code, "pc_version_two");
        assert_eq!(m_by_ms.sha1, "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        assert_eq!(m_by_ms.file_size, 8_000_000_000u64);

        // 2. Empty media_source_id with v2 pick_code_hint selects v2 (not sources.first()!)
        let m_by_pc = parse_item_to_media("movie_mv_1", &item, "", "pc_version_two").unwrap();
        assert_eq!(m_by_pc.pick_code, "pc_version_two");
        assert_eq!(m_by_pc.sha1, "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        assert_eq!(m_by_pc.item_name, "Demo.1080p.Sub.mkv");

        // 3. Both empty falls back to sources.first()
        let m_default = parse_item_to_media("movie_mv_1", &item, "", "").unwrap();
        assert_eq!(m_default.pick_code, "pc_version_one");
    }
}

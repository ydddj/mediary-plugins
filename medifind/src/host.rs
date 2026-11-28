use crate::models::{flex_id, flex_u32, null_default};
use anyhow::{anyhow, ensure, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::time::Duration;
use url::Url;

pub struct HostClient {
    base_url: Url,
    token: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct HostStatus {
    pub open_api_connected: bool,
    pub web_cookie_ready: bool,
    pub flowlink_enabled: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub proxy_url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub target_folder: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub target_cid: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub offline_folder: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub offline_folder_cid: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub transfer_source_folder: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub transfer_source_cid: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct TMDBMediaItem {
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
    pub backdrop_path: String,
    #[serde(deserialize_with = "null_default")]
    pub overview: String,
    pub vote_average: f64,
    pub popularity: f64,
    #[serde(deserialize_with = "null_default")]
    pub genre_ids: Vec<i32>,
    #[serde(deserialize_with = "null_default")]
    pub origin_country: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub original_language: String,
    #[serde(deserialize_with = "null_default")]
    pub status: String,
    pub next_episode_to_air: Option<Value>,
}

impl TMDBMediaItem {
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

    pub fn effective_type(&self) -> &'static str {
        let t = if !self.media_type.trim().is_empty() {
            self.media_type.trim().to_ascii_lowercase()
        } else {
            self.kind.trim().to_ascii_lowercase()
        };
        if matches!(t.as_str(), "tv" | "series" | "show") {
            "tv"
        } else if t == "movie" {
            "movie"
        } else if !self.first_air_date.trim().is_empty() && self.release_date.trim().is_empty() {
            "tv"
        } else {
            "movie"
        }
    }

    pub fn effective_year(&self) -> String {
        if self.year.len() >= 4 {
            return self.year[..4].to_string();
        }
        for d in [&self.release_date, &self.first_air_date] {
            let trimmed = d.trim();
            if trimmed.len() >= 4 {
                return trimmed[..4].to_string();
            }
        }
        String::new()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TMDBSeasonBrief {
    pub season_number: u32,
    pub episode_count: u32,
    #[serde(deserialize_with = "null_default")]
    pub air_date: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TMDBGenre {
    pub id: i32,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct TMDBDetails {
    #[serde(deserialize_with = "flex_id")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub original_title: String,
    #[serde(deserialize_with = "null_default")]
    pub poster_path: String,
    #[serde(deserialize_with = "null_default")]
    pub backdrop_path: String,
    #[serde(deserialize_with = "null_default")]
    pub overview: String,
    #[serde(deserialize_with = "null_default")]
    pub release_date: String,
    #[serde(deserialize_with = "null_default")]
    pub first_air_date: String,
    pub vote_average: f64,
    pub popularity: f64,
    #[serde(deserialize_with = "null_default")]
    pub genre_ids: Vec<i32>,
    #[serde(skip_serializing, deserialize_with = "null_default")]
    pub genres: Vec<TMDBGenre>,
    #[serde(deserialize_with = "null_default")]
    pub original_language: String,
    #[serde(deserialize_with = "null_default")]
    pub origin_country: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub status: String,
    pub next_episode_to_air: Option<Value>,
    #[serde(skip_serializing, deserialize_with = "null_default")]
    pub seasons: Vec<TMDBSeasonBrief>,
}

impl TMDBDetails {
    pub fn to_media_item(&self, fallback_type: &str) -> TMDBMediaItem {
        let mut gids = self.genre_ids.clone();
        for g in &self.genres {
            if g.id > 0 && !gids.contains(&g.id) {
                gids.push(g.id);
            }
        }
        TMDBMediaItem {
            id: self.id.clone(),
            tmdb_id: self.id.clone(),
            title: self.title.clone(),
            name: self.name.clone(),
            original_title: self.original_title.clone(),
            media_type: fallback_type.to_string(),
            release_date: self.release_date.clone(),
            first_air_date: self.first_air_date.clone(),
            poster_path: self.poster_path.clone(),
            backdrop_path: self.backdrop_path.clone(),
            overview: self.overview.clone(),
            vote_average: self.vote_average,
            popularity: self.popularity,
            genre_ids: gids,
            origin_country: self.origin_country.clone(),
            original_language: self.original_language.clone(),
            status: self.status.clone(),
            next_episode_to_air: self.next_episode_to_air.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TMDBDiscoverPage {
    #[serde(alias = "results", deserialize_with = "null_default")]
    pub items: Vec<TMDBMediaItem>,
    pub page: usize,
    pub total_pages: usize,
    pub total_results: usize,
}

pub fn is_valid_115_target_cid(cid: &str) -> bool {
    let t = cid.trim();
    !t.is_empty() && t != "0" && t.chars().all(|c| c.is_ascii_digit())
}

pub fn tmdb_poster_https(poster_path: &str) -> Option<String> {
    tmdb_image_https(poster_path, "w500")
}

pub fn tmdb_card_image_https(backdrop_path: &str, poster_path: &str) -> Option<String> {
    tmdb_image_https(backdrop_path, "w780").or_else(|| tmdb_image_https(poster_path, "w500"))
}

fn tmdb_image_https(raw_path: &str, size: &str) -> Option<String> {
    let p = raw_path.trim();
    if p.is_empty() {
        return None;
    }
    if p.starts_with("https://") || p.starts_with("http://") {
        return Some(p.to_string());
    }
    if p.starts_with('/') {
        return Some(format!("https://image.tmdb.org/t/p/{}{}", size, p));
    }
    Some(format!("https://image.tmdb.org/t/p/{}/{}", size, p))
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Subscription {
    #[serde(deserialize_with = "flex_id")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "flex_id")]
    pub tmdb_id: String,
    #[serde(deserialize_with = "null_default")]
    pub media_type: String,
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: String,
    #[serde(deserialize_with = "flex_id")]
    pub year: String,
    #[serde(deserialize_with = "flex_u32")]
    pub season: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub expected_episodes: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub collected_episodes: u32,
    #[serde(alias = "state", deserialize_with = "null_default")]
    pub status: String,
}

impl Subscription {
    pub fn effective_title(&self) -> &str {
        if !self.title.trim().is_empty() {
            self.title.trim()
        } else {
            self.name.trim()
        }
    }

    pub fn effective_type(&self) -> &'static str {
        let t = if !self.media_type.trim().is_empty() {
            self.media_type.trim().to_ascii_lowercase()
        } else {
            self.kind.trim().to_ascii_lowercase()
        };
        if matches!(t.as_str(), "tv" | "series" | "show") || self.season > 0 {
            "tv"
        } else {
            "movie"
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LibrarySource {
    #[serde(alias = "size")]
    pub file_size: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LibraryItem {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub item_type: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "flex_id")]
    pub tmdb_id: String,
    #[serde(deserialize_with = "flex_id")]
    pub series_tmdb_id: String,
    #[serde(deserialize_with = "null_default")]
    pub sources: Vec<LibrarySource>,
    #[serde(deserialize_with = "null_default")]
    pub versions: Vec<LibrarySource>,
}

impl LibraryItem {
    pub fn effective_sources(&self) -> &[LibrarySource] {
        if !self.sources.is_empty() {
            &self.sources
        } else {
            &self.versions
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Cloud115File {
    #[serde(deserialize_with = "flex_id")]
    pub file_id: String,
    #[serde(deserialize_with = "flex_id")]
    pub parent_id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    pub size: i64,
    #[serde(deserialize_with = "null_default")]
    pub sha1: String,
    #[serde(deserialize_with = "null_default")]
    pub pick_code: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Open115FilesPage {
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LibraryPage {
    #[serde(deserialize_with = "null_default")]
    pub items: Vec<LibraryItem>,
    pub start: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MediaStatus {
    #[serde(deserialize_with = "flex_id")]
    pub tmdb_id: String,
    #[serde(deserialize_with = "null_default")]
    pub media_type: String,
    pub is_subscribed: bool,
    pub is_fully_collected: bool,
    pub is_partially_collected: bool,
    pub inventory_count: usize,
    #[serde(default)]
    pub collected_episodes_by_season: HashMap<String, Vec<u32>>,
}

impl HostClient {
    pub fn from_env() -> Result<Self> {
        let raw_url = env::var("MEDIARY_PLUGIN_API_URL").context("缺少环境变量 MEDIARY_PLUGIN_API_URL")?;
        let token = env::var("MEDIARY_PLUGIN_TOKEN").context("缺少环境变量 MEDIARY_PLUGIN_TOKEN")?;
        ensure!(!raw_url.trim().is_empty() && !token.trim().is_empty(), "宿主 API 凭证为空");
        let base_url = Url::parse(&format!("{}/", raw_url.trim().trim_end_matches('/')))?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(45))
            .build();
        Ok(Self {
            base_url,
            token: token.trim().to_string(),
            agent,
        })
    }

    fn call_raw(
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
        let mut req = self
            .agent
            .request(method, &full)
            .timeout(Duration::from_secs(timeout_secs))
            .set("Authorization", &format!("Bearer {}", self.token));
        for (k, v) in query {
            req = req.query(k, v);
        }
        let resp = match body {
            Some(payload) => req.send_json(payload),
            None => req.call(),
        };
        match resp {
            Ok(r) => Ok(r.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(code, r)) => {
                let raw = r.into_string().unwrap_or_default();
                let mut msg = raw.trim().to_string();
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
                        if !e.is_empty() {
                            msg = e.to_string();
                        }
                    } else if let Some(m) = v.get("message").and_then(|x| x.as_str()) {
                        if !m.is_empty() {
                            msg = m.to_string();
                        }
                    }
                }
                Err(anyhow!("Mediary API HTTP {} ({} {}): {}", code, method, path, msg))
            }
            Err(ureq::Error::Transport(t)) => Err(anyhow!("请求 Mediary API 失败: {}", t)),
        }
    }

    pub fn get_host_status(&self) -> HostStatus {
        let mut st = HostStatus::default();
        let is_ready = |v: &Value| -> bool {
            if let Some(s) = v.get("status").and_then(|x| x.as_str()) {
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
                if v.get(k).and_then(|x| x.as_bool()) == Some(true) {
                    return true;
                }
            }
            false
        };

        if let Ok(raw) = self.call_raw("GET", "/open115/status", &[], None, 8) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if is_ready(&v) {
                    st.open_api_connected = true;
                }
            }
        }
        if let Ok(raw) = self.call_raw("GET", "/open115/web-cookie/status", &[], None, 8) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if is_ready(&v) {
                    st.web_cookie_ready = true;
                }
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
        if let Ok(raw) = self.call_raw("GET", "/open115/config", &[], None, 8) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                let d = v.get("data").filter(|x| x.is_object()).unwrap_or(&v);
                let str_field = |obj: &Value, k: &str| -> String {
                    match obj.get(k) {
                        Some(Value::String(s)) => s.trim().to_string(),
                        Some(Value::Number(n)) => n.to_string(),
                        _ => String::new(),
                    }
                };
                st.target_folder = str_field(d, "target_folder");
                st.target_cid = str_field(d, "target_cid");
                st.offline_folder = str_field(d, "offline_folder");
                st.offline_folder_cid = str_field(d, "offline_folder_cid");
                st.transfer_source_folder = str_field(d, "transfer_source_folder");
                st.transfer_source_cid = str_field(d, "transfer_source_cid");
                if d.get("transfer_enabled").and_then(|x| x.as_bool()) == Some(true) {
                    st.flowlink_enabled = true;
                }
                if let Some(tasks) = d.get("transfer_tasks").and_then(|x| x.as_array()) {
                    let chosen = tasks
                        .iter()
                        .find(|t| {
                            t.get("enabled").and_then(|x| x.as_bool()) != Some(false)
                                && t.get("is_default").and_then(|x| x.as_bool()) == Some(true)
                        })
                        .or_else(|| {
                            tasks
                                .iter()
                                .find(|t| t.get("enabled").and_then(|x| x.as_bool()) != Some(false))
                        });
                    if let Some(task) = chosen {
                        let scid = str_field(task, "source_cid");
                        let spath = str_field(task, "source_path");
                        if !scid.is_empty() {
                            st.transfer_source_cid = scid;
                        }
                        if !spath.is_empty() {
                            st.transfer_source_folder = spath;
                        }
                        st.flowlink_enabled = true;
                    }
                }
            }
        }
        if let Ok(raw) = self.call_raw("GET", "/settings", &[], None, 8) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(p) = v.get("tmdb_proxy_url").and_then(|x| x.as_str()) {
                    if !p.trim().is_empty() {
                        st.proxy_url = p.trim().to_string();
                    }
                }
                if v.get("flowlink")
                    .and_then(|fl| fl.get("enabled"))
                    .and_then(|x| x.as_bool())
                    == Some(true)
                    || v.get("flowlink_enabled").and_then(|x| x.as_bool()) == Some(true)
                {
                    st.flowlink_enabled = true;
                }
            }
        }
        if let Ok(env_p) = env::var("MEDIARY_PLUGIN_PROXY_URL") {
            if !env_p.trim().is_empty() {
                st.proxy_url = env_p.trim().to_string();
            }
        }
        st
    }

    pub fn search_tmdb(&self, query: &str, media_type: &str, page: usize) -> Result<Vec<TMDBMediaItem>> {
        let page_str = page.max(1).to_string();
        let mut q = vec![
            ("query", query.trim()),
            ("keyword", query.trim()),
            ("page", page_str.as_str()),
        ];
        if !media_type.is_empty() && media_type != "all" && media_type != "全部" {
            q.push(("media_type", media_type));
            q.push(("type", media_type));
        }
        let raw = self.call_raw("GET", "/search/tmdb", &q, None, 20)?;
        decode_list(&raw)
    }

    pub fn discover_tmdb_page(
        &self,
        media_type: &str,
        extra_params: &[(&str, &str)],
        page: usize,
    ) -> Result<TMDBDiscoverPage> {
        let mt = if media_type.eq_ignore_ascii_case("tv") {
            "tv"
        } else {
            "movie"
        };
        let page_s = page.max(1).to_string();
        let mut q: Vec<(&str, &str)> = Vec::with_capacity(extra_params.len() + 2);
        q.push(("media_type", mt));
        q.push(("page", page_s.as_str()));
        for &(k, v) in extra_params {
            if !v.trim().is_empty() {
                q.push((k, v.trim()));
            }
        }
        let raw = self.call_raw("GET", "/plugin/tmdb/discover", &q, None, 25)?;
        let v: Value = serde_json::from_str(&raw)?;
        let inner = v.get("data").filter(|x| x.is_object()).unwrap_or(&v);
        let mut page_res: TMDBDiscoverPage = serde_json::from_value(inner.clone())?;
        for it in &mut page_res.items {
            if it.media_type.is_empty() {
                it.media_type = mt.to_string();
            }
        }
        if page_res.page == 0 {
            page_res.page = page.max(1);
        }
        if page_res.total_pages == 0 {
            page_res.total_pages = 1;
        }
        if page_res.total_results == 0 && !page_res.items.is_empty() {
            page_res.total_results = page_res.items.len();
        }
        Ok(page_res)
    }

    pub fn get_media_statuses(
        &self,
        identities: &[String],
        include_episodes: bool,
    ) -> HashMap<String, MediaStatus> {
        let mut out = HashMap::new();
        if identities.is_empty() {
            return out;
        }
        for chunk in identities.chunks(80) {
            let joined = chunk.join(",");
            let mut q = vec![("items", joined.as_str())];
            if include_episodes {
                q.push(("include_episodes", "true"));
            }
            let Ok(raw) = self.call_raw("GET", "/plugin/media-status", &q, None, 12) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let inner = v.get("data").filter(|x| x.is_object()).unwrap_or(&v);
            if let Some(arr) = inner.get("items").and_then(Value::as_array) {
                for elem in arr {
                    if let Ok(st) = serde_json::from_value::<MediaStatus>(elem.clone()) {
                        let mt = st.media_type.trim().to_ascii_lowercase();
                        let tid = st.tmdb_id.trim();
                        if !mt.is_empty() && !tid.is_empty() {
                            out.insert(format!("{}:{}", mt, tid), st);
                        }
                    }
                }
            } else if let Some(map_val) = inner.get("statuses").filter(|x| x.is_object()) {
                if let Ok(map) = serde_json::from_value::<HashMap<String, MediaStatus>>(map_val.clone())
                {
                    out.extend(map);
                }
            }
        }
        out
    }

    pub fn resolve_tmdb(&self, title: &str, media_type: &str, year: i32) -> Result<TMDBMediaItem> {
        let is_tv = media_type.trim().eq_ignore_ascii_case("tv");
        let years_to_try: Vec<i32> = if year > 0 && !is_tv {
            vec![year, 0]
        } else if year > 0 {
            // TV series on TMDB are indexed by Season 1 premiere year, so try without year first
            vec![0, year]
        } else {
            vec![0]
        };

        let mut fallback_item: Option<TMDBMediaItem> = None;
        let mut last_err = None;
        for yr in years_to_try {
            let mut payload = json!({ "title": title.trim() });
            if !media_type.is_empty() && media_type != "all" {
                payload["media_type"] = json!(media_type);
                payload["type"] = json!(media_type);
            }
            if yr > 0 {
                payload["year"] = json!(yr);
            }
            match self.call_raw("POST", "/tmdb/resolve", &[], Some(payload), 20) {
                Ok(raw) => match decode_tmdb_resolve_raw(&raw) {
                    Ok(item) => {
                        if !item.backdrop_path.is_empty() || !item.poster_path.is_empty() {
                            return Ok(item);
                        }
                        if fallback_item.is_none()
                            && (!item.effective_id().is_empty() || !item.effective_title().is_empty())
                        {
                            fallback_item = Some(item);
                        }
                    }
                    Err(e) => last_err = Some(e),
                },
                Err(e) => last_err = Some(e),
            }
        }
        if let Some(it) = fallback_item {
            return Ok(it);
        }
        Err(last_err.unwrap_or_else(|| anyhow!("未能解析 TMDB 条目: {}", title)))
    }

    pub fn get_tmdb_details(&self, tmdb_id: &str, media_type: &str) -> Result<TMDBDetails> {
        let candidates = tmdb_detail_candidate_types(media_type);
        let last_idx = candidates.len().saturating_sub(1);
        for (idx, mt) in candidates.iter().copied().enumerate() {
            let res = self.call_raw(
                "GET",
                "/search/tmdb/details",
                &[
                    ("tmdb_id", tmdb_id.trim()),
                    ("id", tmdb_id.trim()),
                    ("media_type", mt),
                    ("type", mt),
                ],
                None,
                20,
            );
            match res {
                Ok(raw) => {
                    if let Ok(det) = decode_tmdb_details_raw(&raw) {
                        if !det.id.is_empty() || !det.title.is_empty() || !det.name.is_empty() {
                            return Ok(det);
                        }
                    }
                }
                Err(e) if idx == last_idx => return Err(e),
                Err(_) => continue,
            }
        }
        Err(anyhow!("未能获取 TMDB 详情: {}", tmdb_id))
    }

    pub fn list_subscriptions(&self) -> Result<Vec<Subscription>> {
        let raw = self.call_raw("GET", "/subscriptions", &[], None, 15)?;
        decode_list(&raw)
    }

    pub fn create_subscription(
        &self,
        tmdb_id: i64,
        title: &str,
        media_type: &str,
        season: u32,
        year: i32,
    ) -> Result<()> {
        let mut payload = json!({
            "tmdb_id": tmdb_id,
            "title": title,
            "media_type": media_type,
        });
        if season > 0 {
            payload["season"] = json!(season);
        }
        if year > 0 {
            payload["year"] = json!(year);
        }
        self.call_raw("POST", "/subscriptions", &[], Some(payload), 20)?;
        Ok(())
    }

    pub fn list_library_items_by_tmdb(&self, media_type: &str, tmdb_id: &str) -> Result<Vec<LibraryItem>> {
        let mut q = vec![("limit", "500")];
        if !media_type.is_empty() && media_type != "all" {
            q.push(("media_type", media_type));
        }
        if !tmdb_id.trim().is_empty() {
            q.push(("tmdb_id", tmdb_id.trim()));
        }
        let raw = self.call_raw("GET", "/plugin/library/items", &q, None, 20)?;
        decode_list(&raw)
    }

    pub fn list_library_items_filtered(&self, query: &[(&str, &str)]) -> Result<LibraryPage> {
        let raw = self.call_raw("GET", "/plugin/library/items", query, None, 20)?;
        let v: Value = serde_json::from_str(&raw)?;
        let inner = v.get("data").filter(|x| x.is_object()).unwrap_or(&v);
        Ok(serde_json::from_value(inner.clone())?)
    }

    pub fn list_library_movies_page(&self, start: usize, limit: usize) -> Result<Vec<LibraryItem>> {
        let st_s = start.to_string();
        let lim_s = limit.clamp(1, 200).to_string();
        let page = self.list_library_items_filtered(&[
            ("media_type", "movie"),
            ("start", st_s.as_str()),
            ("limit", lim_s.as_str()),
        ])?;
        Ok(page.items)
    }

    pub fn open115_list_page(
        &self,
        cid: &str,
        offset: usize,
        limit: usize,
        show_dir: bool,
    ) -> Result<Open115FilesPage> {
        let c = if cid.trim().is_empty() { "0" } else { cid.trim() };
        let lim = if limit == 0 || limit > 1000 { 200 } else { limit };
        let off_s = offset.to_string();
        let lim_s = lim.to_string();
        let raw = self.call_raw(
            "GET",
            "/plugin/open115/files",
            &[
                ("cid", c),
                ("offset", &off_s),
                ("limit", &lim_s),
                ("show_dir", if show_dir { "true" } else { "false" }),
            ],
            None,
            25,
        )?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn resolve_115_dir_name(&self, cid: &str, hs: &HostStatus) -> Option<String> {
        let c = cid.trim();
        if !is_valid_115_target_cid(c) {
            return None;
        }
        for (known_cid, known_name) in [
            (hs.transfer_source_cid.as_str(), hs.transfer_source_folder.as_str()),
            (hs.target_cid.as_str(), hs.target_folder.as_str()),
            (hs.offline_folder_cid.as_str(), hs.offline_folder.as_str()),
        ] {
            if known_cid.trim() == c && !known_name.trim().is_empty() {
                return Some(known_name.trim().to_string());
            }
        }
        if let Ok(page) = self.open115_list_page("0", 0, 200, true) {
            for item in page.items {
                if item.is_dir && item.file_id.trim() == c && !item.name.trim().is_empty() {
                    return Some(item.name.trim().to_string());
                }
            }
        }
        None
    }

    pub fn open115_share_snap(
        &self,
        share_url: &str,
        receive_code: &str,
        cid: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value> {
        let mut payload = json!({ "share_url": share_url.trim() });
        if !receive_code.trim().is_empty() {
            payload["receive_code"] = json!(receive_code.trim());
        }
        if !cid.trim().is_empty() && cid.trim() != "0" {
            payload["cid"] = json!(cid.trim());
        }
        if offset > 0 {
            payload["offset"] = json!(offset);
        }
        if limit > 0 {
            payload["limit"] = json!(limit);
        }
        let raw = self.call_raw("POST", "/plugin/open115/share/snap", &[], Some(payload), 35)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn open115_share_receive(
        &self,
        share_url: &str,
        receive_code: &str,
        target_cid: &str,
        file_ids: &[String],
    ) -> Result<Value> {
        let mut payload = json!({
            "share_url": share_url.trim(),
            "target_cid": target_cid.trim(),
        });
        if !receive_code.trim().is_empty() {
            payload["receive_code"] = json!(receive_code.trim());
        }
        if !file_ids.is_empty() {
            payload["file_ids"] = json!(file_ids);
        }
        let raw = self.call_raw("POST", "/plugin/open115/share/receive", &[], Some(payload), 45)?;
        Ok(serde_json::from_str(&raw).unwrap_or(Value::Null))
    }

    pub fn open115_offline(&self, urls: &[String], target_cid: &str) -> Result<(usize, Vec<String>)> {
        let cid = target_cid.trim();
        ensure!(
            is_valid_115_target_cid(cid),
            "115 离线下载目标目录 target_cid 必须为非 0 的数字 CID"
        );
        let clean_urls: Vec<String> = urls
            .iter()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
            .collect();
        ensure!(!clean_urls.is_empty(), "离线下载链接列表不能为空");

        let mut succeeded = 0usize;
        let mut warnings = Vec::new();
        for chunk in clean_urls.chunks(50) {
            let payload = json!({
                "urls": chunk,
                "target_cid": cid,
            });
            let raw = self.call_raw("POST", "/plugin/open115/offline", &[], Some(payload), 45)?;
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                let d = v.get("data").filter(|x| x.is_object()).unwrap_or(&v);
                if let Some(arr) = d
                    .get("results")
                    .or_else(|| d.get("items"))
                    .and_then(Value::as_array)
                {
                    for entry in arr {
                        let ok = entry
                            .get("ok")
                            .or_else(|| entry.get("state"))
                            .and_then(Value::as_bool)
                            .unwrap_or_else(|| {
                                entry.get("errcode").and_then(Value::as_i64).unwrap_or(0) == 0
                                    && entry.get("errno").and_then(Value::as_i64).unwrap_or(0) == 0
                                    && entry.get("error").and_then(Value::as_str).unwrap_or("").is_empty()
                            });
                        if ok {
                            succeeded += 1;
                        } else {
                            let err = entry
                                .get("error")
                                .or_else(|| entry.get("error_msg"))
                                .or_else(|| entry.get("message"))
                                .and_then(Value::as_str)
                                .unwrap_or("115 离线任务创建失败");
                            warnings.push(err.to_string());
                        }
                    }
                    continue;
                }
                if let Some(n) = d
                    .get("submitted")
                    .or_else(|| d.get("succeeded"))
                    .or_else(|| d.get("added"))
                    .and_then(Value::as_u64)
                {
                    succeeded += n as usize;
                    continue;
                }
            }
            succeeded += chunk.len();
        }
        ensure!(
            succeeded > 0,
            "115 离线下载提交失败: {}",
            if warnings.is_empty() {
                "未知错误".to_string()
            } else {
                warnings.join("；")
            }
        );
        Ok((succeeded, warnings))
    }

    pub fn submit_link(&self, link: &str) -> Result<String> {
        let trimmed = link.trim();
        ensure!(!trimmed.is_empty(), "提交链接不能为空");
        let payload = if trimmed.to_ascii_lowercase().starts_with("ed2k://") {
            json!({
                "link": trimmed,
                "offline_target": "transfer",
                "flowlink_move_all_delay_seconds": 10,
            })
        } else {
            json!({ "link": trimmed })
        };
        let raw = self.call_raw("POST", "/link/submit", &[], Some(payload), 45)?;
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            for k in ["message", "notice"] {
                if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
                    if !s.trim().is_empty() {
                        return Ok(s.trim().to_string());
                    }
                }
            }
        }
        Ok("资源已提交 Mediary 转存整理".to_string())
    }

    pub fn trigger_flowlink_move_all(&self) {
        let _ = self.call_raw("POST", "/plugin/flowlink/all", &[], Some(json!({})), 20);
    }

    pub fn notify(&self, title: &str, content: &str) -> Result<()> {
        self.notify_with_image(title, content, None)
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
        if let Some(img) = image_url.map(str::trim).filter(|s| !s.is_empty()) {
            payload["image_url"] = json!(img);
        }
        self.call_raw("POST", "/plugin/notifications", &[], Some(payload), 15)?;
        Ok(())
    }

    pub fn get_notification_templates(&self) -> HostNotificationTemplates {
        let mut tpls = HostNotificationTemplates::default();
        if let Ok(raw) = self.call_raw("GET", "/settings", &[], None, 8) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                let root = v.get("data").filter(|x| x.is_object()).unwrap_or(&v);
                if let Some(nt) = root.get("notification_templates") {
                    let load_one = |key: &str, target: &mut NotificationTemplateItem| {
                        if let Some(obj) = nt.get(key).filter(|x| x.is_object()) {
                            if let Some(en) = obj.get("enabled").and_then(|x| x.as_bool()) {
                                target.enabled = en;
                            }
                            if let Some(t) = obj.get("title").and_then(|x| x.as_str()) {
                                if !t.trim().is_empty() {
                                    target.title = t.to_string();
                                }
                            }
                            if let Some(c) = obj.get("content").and_then(|x| x.as_str()) {
                                if !c.trim().is_empty() {
                                    target.content = c.to_string();
                                }
                            }
                        }
                    };
                    load_one("download_started", &mut tpls.download_started);
                    load_one("subscription_added", &mut tpls.subscription_added);
                }
            }
        }
        tpls
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
    pub download_started: NotificationTemplateItem,
    pub subscription_added: NotificationTemplateItem,
}

impl Default for HostNotificationTemplates {
    fn default() -> Self {
        Self {
            download_started: NotificationTemplateItem {
                enabled: true,
                title: "{{media_icon}} {{display_title}}{{#if season_episode}} {{season_episode}}{{/if}} 🚀{{download_action}}".into(),
                content: "{{#if torrent_title}}📌 种子: {{torrent_title}}\n{{/if}}{{#if site}}🌐 站点: {{site}}\n{{/if}}{{#if size}}📦 大小: {{size}}\n{{/if}}".into(),
            },
            subscription_added: NotificationTemplateItem {
                enabled: true,
                title: "{{media_icon}} {{name}}{{#if year}} ({{year}}){{/if}}{{#if season_label}} {{season_label}}{{/if}} 已添加订阅".into(),
                content: "{{#if secondary_category}}🗂 分类: {{secondary_category}}\n{{/if}}{{#if expected_episodes}}📺 预计集数: {{expected_episodes}}\n{{/if}}{{#if overview}}\n📃 简介:\n{{overview}}{{/if}}".into(),
            },
        }
    }
}

pub fn render_mediary_template(tpl: &str, vars: &HashMap<&str, String>) -> String {
    let mut out = tpl.to_string();
    for _ in 0..32 {
        let Some(start) = out.rfind("{{#if ") else {
            break;
        };
        let rest = &out[start + 6..];
        let Some(open_rel_end) = rest.find("}}") else {
            break;
        };
        let key = rest[..open_rel_end].trim();
        let body_start = start + 6 + open_rel_end + 2;
        let after_open = &out[body_start..];
        let Some(close_rel) = after_open.find("{{/if}}") else {
            break;
        };
        let inner = &after_open[..close_rel];
        let close_end = body_start + close_rel + 7;
        let truthy = vars
            .get(key)
            .map(|v| {
                let t = v.trim();
                !t.is_empty() && t != "false" && t != "0"
            })
            .unwrap_or(false);
        let replacement = if truthy { inner.to_string() } else { String::new() };
        out.replace_range(start..close_end, &replacement);
    }

    for (k, v) in vars {
        let token = format!("{{{{{}}}}}", k);
        if out.contains(&token) {
            out = out.replace(&token, v.trim());
        }
    }
    // Strip any leftover unpopulated {{placeholder}} tokens cleanly
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

fn decode_list<T: DeserializeOwned>(raw: &str) -> Result<Vec<T>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(trimmed)?;
    if let Some(arr) = v.as_array() {
        return Ok(decode_value_slice(arr));
    }
    for k in ["subscriptions", "items", "data", "results", "list"] {
        if let Some(arr) = v.get(k).and_then(Value::as_array) {
            return Ok(decode_value_slice(arr));
        }
    }
    Ok(Vec::new())
}

fn decode_value_slice<T: DeserializeOwned>(arr: &[Value]) -> Vec<T> {
    arr.iter()
        .filter_map(|elem| serde_json::from_value::<T>(elem.clone()).ok())
        .collect()
}

pub fn decode_tmdb_resolve_raw(raw: &str) -> Result<TMDBMediaItem> {
    let v: Value = serde_json::from_str(raw)?;
    for key in ["data", "item", "result"] {
        if let Some(inner) = v.get(key).filter(|x| x.is_object()) {
            if let Ok(item) = serde_json::from_value::<TMDBMediaItem>(inner.clone()) {
                if !item.effective_id().is_empty() || !item.effective_title().is_empty() {
                    return Ok(item);
                }
            }
        }
    }
    Ok(serde_json::from_value(v)?)
}

pub fn tmdb_detail_candidate_types(media_type: &str) -> &'static [&'static str] {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "tv" | "series" => &["tv"],
        "movie" => &["movie"],
        _ => &["movie", "tv"],
    }
}

fn decode_tmdb_details_raw(raw: &str) -> Result<TMDBDetails> {
    let v: Value = serde_json::from_str(raw)?;
    for key in ["details", "data", "item"] {
        if let Some(inner) = v.get(key).filter(|x| x.is_object()) {
            if let Ok(det) = serde_json::from_value::<TMDBDetails>(inner.clone()) {
                if !det.id.is_empty() || !det.title.is_empty() || !det.name.is_empty() {
                    return Ok(det);
                }
            }
        }
    }
    Ok(serde_json::from_value(v)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_mediary_subscriptions_with_null_season() {
        let raw_arr = r#"[
            {
                "id": 1,
                "name": "庆余年",
                "year": 2024,
                "media_type": "tv",
                "tmdb_id": 12345,
                "season": 2,
                "expected_episodes": 36,
                "collected_episodes": 18,
                "state": "active"
            },
            {
                "id": 2,
                "name": "沙丘2",
                "year": 2024,
                "media_type": "movie",
                "tmdb_id": 693134,
                "season": null,
                "expected_episodes": 0,
                "collected_episodes": 1,
                "state": "completed"
            }
        ]"#;
        let subs: Vec<Subscription> = decode_list(raw_arr).unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].effective_title(), "庆余年");
        assert_eq!(subs[0].season, 2);
        assert_eq!(subs[0].status, "active");
        assert_eq!(subs[1].effective_title(), "沙丘2");
        assert_eq!(subs[1].season, 0);
        assert_eq!(subs[1].effective_type(), "movie");
        assert_eq!(subs[1].status, "completed");

        let raw_wrapped = format!(r#"{{"subscriptions": {}}}"#, raw_arr);
        let subs2: Vec<Subscription> = decode_list(&raw_wrapped).unwrap();
        assert_eq!(subs2.len(), 2);

        let tpls = HostNotificationTemplates::default();
        let mut vars = HashMap::new();
        vars.insert("media_icon", "📺".to_string());
        vars.insert("display_title", "密档 (2026)".to_string());
        vars.insert("season_episode", "S01E24".to_string());
        vars.insert("download_action", "选集转存".to_string());
        vars.insert("torrent_title", "密档 S01E24 2160p → 待洗版".to_string());
        vars.insert("site", "TG · 115影视资源分享频道".to_string());
        vars.insert("size", "3.85 GB".to_string());
        let title_out = render_mediary_template(&tpls.download_started.title, &vars);
        let body_out = render_mediary_template(&tpls.download_started.content, &vars);
        assert_eq!(title_out, "📺 密档 (2026) S01E24 🚀选集转存");
        assert!(body_out.contains("📌 种子: 密档 S01E24 2160p → 待洗版"));
        assert!(body_out.contains("🌐 站点: TG · 115影视资源分享频道"));
        assert!(body_out.contains("📦 大小: 3.85 GB"));

        let resolve_json = r#"{"data":{"backdrop_path":"/1t3AMebbgsKMuJY8ybjP6fznXPh.jpg","poster_path":"/e9kdsUK01YSa8Ou5tO72QJ0830X.jpg","title":"大主宰","tmdb_id":"226045","type":"tv","year":2023},"status":"success"}"#;
        let r_item = decode_tmdb_resolve_raw(resolve_json).unwrap();
        assert_eq!(r_item.effective_id(), "226045");
        assert_eq!(r_item.effective_year(), "2023");
        assert_eq!(
            tmdb_card_image_https(&r_item.backdrop_path, &r_item.poster_path).as_deref(),
            Some("https://image.tmdb.org/t/p/w780/1t3AMebbgsKMuJY8ybjP6fznXPh.jpg")
        );

        let details_json = r#"{"details":{"id":1821748,"title":"我没错！","release_date":"2026-05-01","backdrop_path":"/xPw16NMyAtgwub5Du8j3ByAqPxV.jpg","poster_path":"/abc.jpg"}}"#;
        let det = decode_tmdb_details_raw(details_json).unwrap();
        assert_eq!(det.id, "1821748");
        assert_eq!(det.release_date, "2026-05-01");
        assert_eq!(
            tmdb_card_image_https(&det.backdrop_path, &det.poster_path).as_deref(),
            Some("https://image.tmdb.org/t/p/w780/xPw16NMyAtgwub5Du8j3ByAqPxV.jpg")
        );

        assert_eq!(tmdb_detail_candidate_types("tv"), &["tv"]);
        assert_eq!(tmdb_detail_candidate_types("series"), &["tv"]);
        assert_eq!(tmdb_detail_candidate_types("movie"), &["movie"]);
        assert_eq!(tmdb_detail_candidate_types(""), &["movie", "tv"]);
        assert_eq!(tmdb_detail_candidate_types("all"), &["movie", "tv"]);
    }
}

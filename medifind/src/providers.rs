use crate::db::Db;
use crate::models::{normalize_tg_level, AppConfig, ResourceItem};
use crate::rules;
use anyhow::{anyhow, ensure, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::Local;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::io::Read;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const HDHIVE_APP_KEY: &str = "2982528f3cba4c430ce45831b36309d2";

static HDHIVE_LAST_CALL: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));
static AYCLUB_LAST_CALL: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

fn throttle(slot: &Mutex<Option<Instant>>, interval: Duration) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(prev) = *guard {
            let elapsed = prev.elapsed();
            if elapsed < interval {
                std::thread::sleep(interval - elapsed);
            }
        }
        *guard = Some(Instant::now());
    }
}

pub fn build_http_agent(proxy_url: &str, timeout_secs: u64) -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(timeout_secs.max(5)))
        .user_agent(DEFAULT_UA);

    let mut raw_proxy = proxy_url.trim().to_string();
    if raw_proxy.is_empty() {
        for k in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"]
        {
            if let Ok(v) = env::var(k) {
                if !v.trim().is_empty() {
                    raw_proxy = v.trim().to_string();
                    break;
                }
            }
        }
    }
    if !raw_proxy.is_empty() {
        if !raw_proxy.contains("://") {
            raw_proxy = format!("http://{}", raw_proxy);
        }
        if raw_proxy.to_ascii_lowercase().starts_with("socks5h://") {
            raw_proxy = format!("socks5://{}", &raw_proxy["socks5h://".len()..]);
        }
        if let Ok(p) = ureq::Proxy::new(&raw_proxy) {
            builder = builder.proxy(p);
        }
    }
    builder.build()
}

fn read_body_string(resp: ureq::Response, max_bytes: u64) -> String {
    let mut buf = Vec::new();
    let _ = resp.into_reader().take(max_bytes).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn read_body_bytes(resp: ureq::Response, max_bytes: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = resp.into_reader().take(max_bytes).read_to_end(&mut buf);
    buf
}

/* ==================== 1. TMDB Poster Path & Image MIME Helpers ==================== */

pub fn normalize_poster_path(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    if let Some(rest) = s
        .strip_prefix("https://image.tmdb.org/t/p/")
        .or_else(|| s.strip_prefix("https://media.themoviedb.org/t/p/"))
    {
        if let Some(slash) = rest.find('/') {
            return rest[slash..].to_string();
        }
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return String::new();
    }
    if !s.starts_with('/') {
        format!("/{}", s)
    } else {
        s.to_string()
    }
}

pub fn detect_image_mime(raw: &[u8], header_ct: &str) -> Option<String> {
    if header_ct.starts_with("image/") {
        let clean = header_ct.split(';').next().unwrap_or(header_ct).trim();
        if !clean.is_empty() {
            return Some(clean.to_string());
        }
    }
    if raw.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg".into())
    } else if raw.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png".into())
    } else if raw.len() >= 12 && &raw[0..4] == b"RIFF" && &raw[8..12] == b"WEBP" {
        Some("image/webp".into())
    } else if raw.starts_with(b"GIF8") {
        Some("image/gif".into())
    } else {
        None
    }
}

/* ==================== 2. HDHive & NextFind Relay (影巢中转) Dual-Protocol Channel ==================== */

fn hdhive_base(cfg: &AppConfig) -> String {
    let b = cfg.hdhive.base_url.trim().trim_end_matches('/');
    if b.is_empty() {
        "https://hdhive.nextemby.com".into()
    } else {
        b.to_string()
    }
}

pub fn prefers_relay_protocol(cfg: &AppConfig) -> bool {
    let proto = cfg.hdhive.protocol.trim().to_ascii_lowercase();
    if proto == "relay" {
        return true;
    }
    if proto == "official" {
        return false;
    }
    let base = hdhive_base(cfg).to_ascii_lowercase();
    !base.contains("hdhive.")
}

pub fn ensure_hdhive_token(cfg: &mut AppConfig, proxy_url: &str) -> String {
    let token = cfg.hdhive.api_key.trim().to_string();
    if prefers_relay_protocol(cfg) {
        return token;
    }
    let refresh = cfg.hdhive.refresh_token.trim().to_string();
    let now = chrono::Utc::now().timestamp();

    if token.is_empty() && refresh.is_empty() {
        return String::new();
    }
    if !token.is_empty() && (cfg.hdhive.access_expire_at == 0 || cfg.hdhive.access_expire_at > now + 60)
    {
        return token;
    }
    if refresh.is_empty() {
        return token;
    }

    let agent = build_http_agent(proxy_url, 12);
    let url = format!("{}/api/public/openapi/oauth/refresh", hdhive_base(cfg));
    if let Ok(resp) = agent
        .post(&url)
        .set("X-API-Key", HDHIVE_APP_KEY)
        .send_json(json!({ "refresh_token": refresh }))
    {
        let raw = read_body_string(resp, 1 << 20);
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            if v.get("success").and_then(|x| x.as_bool()) == Some(true) {
                if let Some(d) = v.get("data") {
                    if let Some(at) = d.get("access_token").and_then(|x| x.as_str()) {
                        if !at.trim().is_empty() {
                            cfg.hdhive.api_key = at.trim().to_string();
                            if let Some(rt) = d.get("refresh_token").and_then(|x| x.as_str()) {
                                if !rt.trim().is_empty() {
                                    cfg.hdhive.refresh_token = rt.trim().to_string();
                                }
                            }
                            if let Some(exp) = d.get("expires_in").and_then(|x| x.as_i64()) {
                                if exp > 0 {
                                    cfg.hdhive.access_expire_at = now + exp;
                                }
                            }
                            return cfg.hdhive.api_key.clone();
                        }
                    }
                }
            }
        }
    }
    token
}

fn call_get_with_status(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(&str, &str)],
    query: &[(&str, &str)],
    max_bytes: u64,
) -> Result<(u16, String)> {
    let mut req = agent.get(url).set("Accept", "application/json");
    for &(k, v) in headers {
        req = req.set(k, v);
    }
    for &(k, v) in query {
        req = req.query(k, v);
    }
    match req.call() {
        Ok(r) => Ok((r.status(), read_body_string(r, max_bytes))),
        Err(ureq::Error::Status(code, r)) => Ok((code, read_body_string(r, max_bytes.min(1 << 20)))),
        Err(e) => Err(anyhow!("{}", e)),
    }
}

pub fn parse_relay_me_json(raw: &str) -> Result<(String, i32)> {
    let v: Value = serde_json::from_str(raw)?;
    if v.get("connected").and_then(|x| x.as_bool()) == Some(false) {
        let msg = non_empty(&[&str_field(&v, "detail"), &str_field(&v, "message"), "授权未通过"]);
        return Err(anyhow!("影巢中转返回未授权: {}", msg));
    }
    let label = non_empty(&[
        &str_field(&v, "label"),
        &str_field(&v, "username"),
        &str_field(&v, "name"),
        "已授权",
    ]);
    let rem = int_field(&v, "weekly_points_remaining")
        .or_else(|| int_field(&v, "remaining_points"))
        .or_else(|| int_field(&v, "points"))
        .unwrap_or(0) as i32;
    Ok((label, rem))
}

fn sync_me_relay(agent: &ureq::Agent, base: &str, token: &str) -> Result<(u16, Result<(String, i32)>)> {
    let url = format!("{}/v1/me", base);
    let bearer = format!("Bearer {}", token);
    let (status, raw) = call_get_with_status(
        agent,
        &url,
        &[("Authorization", &bearer), ("X-Relay-Key", token)],
        &[],
        1 << 20,
    )?;
    if status != 200 {
        let v: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
        let detail = non_empty(&[&str_field(&v, "detail"), &str_field(&v, "message"), raw.trim()]);
        return Ok((status, Err(anyhow!("影巢中转授权检测失败 (HTTP {}): {}", status, detail))));
    }
    Ok((200, parse_relay_me_json(&raw)))
}

fn sync_me_official(agent: &ureq::Agent, base: &str, token: &str) -> Result<(u16, Result<(String, i32)>)> {
    let url = format!("{}/api/open/me", base);
    let bearer = format!("Bearer {}", token);
    let (status, raw) = call_get_with_status(
        agent,
        &url,
        &[("Authorization", &bearer), ("X-API-Key", HDHIVE_APP_KEY)],
        &[],
        1 << 20,
    )?;
    if status != 200 {
        let v: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
        let detail = non_empty(&[&str_field(&v, "message"), &str_field(&v, "detail"), raw.trim()]);
        return Ok((status, Err(anyhow!("HDHive 用户接口返回 HTTP {}: {}", status, detail))));
    }
    let v: Value = serde_json::from_str(&raw)?;
    if v.get("success").and_then(|x| x.as_bool()) == Some(false) {
        let msg = v.get("message").and_then(|x| x.as_str()).unwrap_or("授权无效");
        return Ok((200, Err(anyhow!("HDHive 返回失败: {}", msg))));
    }
    let mut name = String::new();
    let mut pts = 0i32;
    if let Some(d) = v.get("data") {
        name = d
            .get("username")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| d.get("nickname").and_then(|x| x.as_str()))
            .unwrap_or("")
            .trim()
            .to_string();
        pts = d
            .get("points")
            .and_then(|x| x.as_i64())
            .or_else(|| d.get("balance").and_then(|x| x.as_i64()))
            .unwrap_or(0) as i32;
    }
    Ok((200, Ok((name, pts))))
}

pub fn hdhive_sync_me(cfg: &mut AppConfig, proxy_url: &str) -> Result<u128> {
    let token = ensure_hdhive_token(cfg, proxy_url);
    ensure!(!token.is_empty(), "请先填写 API Key / Token");
    let agent = build_http_agent(proxy_url, 12);
    let base = hdhive_base(cfg);
    let start = Instant::now();

    let try_relay_first = prefers_relay_protocol(cfg);
    if try_relay_first {
        let (st, res) = sync_me_relay(&agent, &base, &token)
            .map_err(|e| anyhow!("连接影巢中转服务失败: {}", e))?;
        if st == 404 {
            let (_, off_res) = sync_me_official(&agent, &base, &token)
                .map_err(|e| anyhow!("请求 HDHive 接口失败: {}", e))?;
            let (name, pts) = off_res?;
            cfg.hdhive.protocol = "official".into();
            cfg.hdhive.username = name;
            cfg.hdhive.points = pts;
            return Ok(start.elapsed().as_millis());
        }
        let (label, rem) = res?;
        cfg.hdhive.protocol = "relay".into();
        cfg.hdhive.username = label;
        cfg.hdhive.points = rem;
        Ok(start.elapsed().as_millis())
    } else {
        let (st, res) = sync_me_official(&agent, &base, &token)
            .map_err(|e| anyhow!("连接 HDHive 服务失败: {}", e))?;
        if st == 404 {
            let (_, rel_res) = sync_me_relay(&agent, &base, &token)
                .map_err(|e| anyhow!("请求影巢中转接口失败: {}", e))?;
            let (label, rem) = rel_res?;
            cfg.hdhive.protocol = "relay".into();
            cfg.hdhive.username = label;
            cfg.hdhive.points = rem;
            return Ok(start.elapsed().as_millis());
        }
        let (name, pts) = res?;
        cfg.hdhive.protocol = "official".into();
        cfg.hdhive.username = name;
        cfg.hdhive.points = pts;
        Ok(start.elapsed().as_millis())
    }
}

pub fn hdhive_checkin(cfg: &mut AppConfig, proxy_url: &str) -> Result<String> {
    let token = ensure_hdhive_token(cfg, proxy_url);
    ensure!(!token.is_empty(), "未配置 HDHive / 影巢中转 API Key");

    if prefers_relay_protocol(cfg) {
        let ms = hdhive_sync_me(cfg, proxy_url)?;
        if cfg.hdhive.protocol == "relay" {
            let msg = format!(
                "影巢中转无需手动签到，已刷新授权：{}（剩余额度 {}，{}ms）",
                cfg.hdhive.username, cfg.hdhive.points, ms
            );
            cfg.hdhive.checked_in_today = true;
            cfg.hdhive.last_checkin_date = Local::now().format("%Y-%m-%d").to_string();
            cfg.hdhive.last_checkin_result = msg.clone();
            return Ok(msg);
        }
    }

    let agent = build_http_agent(proxy_url, 15);
    let url = format!("{}/api/public/openapi/checkin", hdhive_base(cfg));
    let (status, raw) = match agent
        .post(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .set("X-API-Key", HDHIVE_APP_KEY)
        .send_json(json!({}))
    {
        Ok(r) => (r.status(), read_body_string(r, 1 << 20)),
        Err(ureq::Error::Status(code, r)) => (code, read_body_string(r, 1 << 20)),
        Err(e) => return Err(anyhow!("HDHive 签到请求失败: {}", e)),
    };

    if status == 404 {
        let ms = hdhive_sync_me(cfg, proxy_url)?;
        if cfg.hdhive.protocol == "relay" {
            let msg = format!(
                "影巢中转已刷新授权：{}（剩余额度 {}，{}ms）",
                cfg.hdhive.username, cfg.hdhive.points, ms
            );
            cfg.hdhive.checked_in_today = true;
            cfg.hdhive.last_checkin_date = Local::now().format("%Y-%m-%d").to_string();
            cfg.hdhive.last_checkin_result = msg.clone();
            return Ok(msg);
        }
    }

    let v: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
    let msg = v
        .get("message")
        .and_then(|x| x.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("签到完成")
        .to_string();
    let mut already = msg.contains("已签到") || msg.to_ascii_lowercase().contains("already");
    if let Some(d) = v.get("data") {
        if d.get("checked_in").and_then(|x| x.as_bool()) == Some(true)
            || d.get("checked_in_today").and_then(|x| x.as_bool()) == Some(true)
        {
            already = true;
        }
        for k in ["points", "balance", "total_points"] {
            if let Some(p) = d.get(k).and_then(|x| x.as_i64()) {
                cfg.hdhive.points = p as i32;
                break;
            }
        }
    }
    if status != 200 && !already {
        return Err(anyhow!("HDHive 签到返回 HTTP {}: {}", status, msg));
    }
    if v.get("success").and_then(|x| x.as_bool()) == Some(false) && !already {
        return Err(anyhow!("HDHive 签到失败: {}", msg));
    }
    cfg.hdhive.checked_in_today = true;
    cfg.hdhive.last_checkin_date = Local::now().format("%Y-%m-%d").to_string();
    cfg.hdhive.last_checkin_result = msg.clone();
    let _ = hdhive_sync_me(cfg, proxy_url);
    Ok(msg)
}

pub fn search_hdhive(
    db: &Db,
    cfg: &mut AppConfig,
    tmdb_id: &str,
    media_type: &str,
    keyword: &str,
    season_totals: &HashMap<u32, u32>,
    force_refresh: bool,
    proxy_url: &str,
) -> Result<Vec<ResourceItem>> {
    if !cfg.hdhive.enabled {
        return Ok(Vec::new());
    }
    let token = ensure_hdhive_token(cfg, proxy_url);
    if token.is_empty() || tmdb_id.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mtype = normalize_media_type(media_type);
    let is_relay = prefers_relay_protocol(cfg);
    let cache_key = format!(
        "hdhive_{}_{}_{}",
        if is_relay { "relay" } else { "off" },
        mtype,
        tmdb_id.trim()
    );

    if !force_refresh {
        if let Some(cached) = db.get_provider_cache(&cache_key, 12 * 3600) {
            return Ok(enrich_from_probe_cache(db, cached));
        }
    }

    let agent = build_http_agent(proxy_url, 22);
    let base = hdhive_base(cfg);
    let bearer = format!("Bearer {}", token);

    let items = if is_relay {
        let relay_url = format!("{}/v1/resources", base);
        let force_s = if force_refresh { "true" } else { "false" };
        throttle(&HDHIVE_LAST_CALL, Duration::from_millis(450));
        let (st, raw) = call_get_with_status(
            &agent,
            &relay_url,
            &[("Authorization", &bearer), ("X-Relay-Key", &token)],
            &[
                ("tmdb_id", tmdb_id.trim()),
                ("media_type", mtype),
                ("keyword", keyword.trim()),
                ("force_refresh", force_s),
            ],
            4 << 20,
        )
        .map_err(|e| anyhow!("连接影巢中转失败: {}", e))?;

        if st == 404 {
            cfg.hdhive.protocol = "official".into();
            search_hdhive_official_http(&agent, &base, &token, tmdb_id.trim(), mtype, season_totals)?
        } else {
            ensure!(st == 200, "影巢中转搜索返回 HTTP {}", st);
            cfg.hdhive.protocol = "relay".into();
            parse_relay_resources_json(&raw, tmdb_id.trim(), mtype, Some(season_totals))?
        }
    } else {
        match search_hdhive_official_http(&agent, &base, &token, tmdb_id.trim(), mtype, season_totals) {
            Ok(list) => {
                cfg.hdhive.protocol = "official".into();
                list
            }
            Err(e) if e.to_string().contains("HTTP 404") => {
                let relay_url = format!("{}/v1/resources", base);
                let (st, raw) = call_get_with_status(
                    &agent,
                    &relay_url,
                    &[("Authorization", &bearer), ("X-Relay-Key", &token)],
                    &[
                        ("tmdb_id", tmdb_id.trim()),
                        ("media_type", mtype),
                        ("keyword", keyword.trim()),
                    ],
                    4 << 20,
                )?;
                ensure!(st == 200, "影巢中转搜索返回 HTTP {}", st);
                cfg.hdhive.protocol = "relay".into();
                parse_relay_resources_json(&raw, tmdb_id.trim(), mtype, Some(season_totals))?
            }
            Err(e) => return Err(e),
        }
    };

    let _ = db.put_provider_cache(&cache_key, &items);
    Ok(enrich_from_probe_cache(db, items))
}

fn search_hdhive_official_http(
    agent: &ureq::Agent,
    base: &str,
    token: &str,
    tmdb_id: &str,
    mtype: &str,
    season_totals: &HashMap<u32, u32>,
) -> Result<Vec<ResourceItem>> {
    let endpoint = format!("{}/api/open/resources/{}/{}", base, mtype, tmdb_id);
    let mut raw = String::new();
    let mut status_code = 0u16;

    for attempt in 0..3 {
        throttle(&HDHIVE_LAST_CALL, Duration::from_millis(950));
        match agent
            .get(&endpoint)
            .set("Authorization", &format!("Bearer {}", token))
            .set("X-API-Key", HDHIVE_APP_KEY)
            .call()
        {
            Ok(resp) => {
                status_code = resp.status();
                raw = read_body_string(resp, 4 << 20);
                break;
            }
            Err(ureq::Error::Status(429, _)) if attempt < 2 => {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            Err(ureq::Error::Status(code, resp)) => {
                status_code = code;
                raw = read_body_string(resp, 1 << 20);
                break;
            }
            Err(e) => {
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(1200));
                    continue;
                }
                return Err(anyhow!("连接 HDHive 失败: {}", e));
            }
        }
    }

    ensure!(status_code == 200, "HDHive 搜索返回 HTTP {}", status_code);
    parse_hdhive_resources_json(&raw, tmdb_id, mtype, Some(season_totals))
}

pub fn parse_relay_resources_json(
    raw: &str,
    tmdb_id: &str,
    mtype: &str,
    season_totals: Option<&HashMap<u32, u32>>,
) -> Result<Vec<ResourceItem>> {
    let v: Value = serde_json::from_str(raw)?;
    let arr = v
        .get("items")
        .or_else(|| v.get("data"))
        .and_then(Value::as_array);
    let Some(arr) = arr else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(arr.len());
    for (idx, row) in arr.iter().enumerate() {
        let resource_id = non_empty(&[&str_field(row, "resource_id"), &str_field(row, "slug")]);
        if resource_id.is_empty() {
            continue;
        }
        let low_id = resource_id.to_lowercase();
        let is_direct = low_id.starts_with("ed2k://") || low_id.starts_with("magnet:?");
        let (sc0, rc0) = rules::parse_115_share(&resource_id, &str_field(row, "access_code"));
        let (display_slug, pan_type, share_code, receive_code) = if is_direct {
            (
                resource_id.clone(),
                if low_id.starts_with("ed2k://") { "ed2k" } else { "magnet" }.to_string(),
                String::new(),
                String::new(),
            )
        } else if !sc0.is_empty() && low_id.contains("115") {
            (
                rules::format_115_share_url(&sc0, &rc0),
                "115".to_string(),
                sc0,
                rc0,
            )
        } else {
            (resource_id.clone(), "radar".to_string(), String::new(), String::new())
        };

        let title = non_empty(&[&str_field(row, "title"), &str_field(row, "name")]);
        let remark = str_field(row, "remark");
        let tags = join_value_strings(&[
            row.get("video_resolution"),
            row.get("source"),
            row.get("color"),
            row.get("audio"),
            row.get("subtitle_language"),
            row.get("subtitle_type"),
        ]);
        let full_text = format!("{} {} {}", title, remark, tags).trim().to_string();
        let parsed_attrs = rules::parse_movie_attributes(&full_text);
        let row_attrs = row
            .get("movie_attributes")
            .and_then(|m| serde_json::from_value::<crate::models::MovieAttributes>(m.clone()).ok())
            .unwrap_or_default();
        let attrs = rules::merge_movie_attributes(&parsed_attrs, &row_attrs);

        let parsed_eps = if mtype == "tv" {
            let from_row = str_field(row, "parsed_episodes");
            if !from_row.is_empty() {
                from_row
            } else {
                rules::parse_shallow_episodes(&full_text, "tv", season_totals)
            }
        } else {
            String::new()
        };

        let raw_pts = int_field(row, "points")
            .or_else(|| int_field(row, "unlock_points"))
            .unwrap_or(0) as i32;
        let unlocked = row
            .get("is_unlocked")
            .or_else(|| row.get("unlocked"))
            .and_then(Value::as_bool)
            .unwrap_or(raw_pts <= 0 || is_direct);
        let effective_pts = if unlocked { 0 } else { raw_pts.max(0) };

        let share_size = non_empty(&[&str_field(row, "share_size"), &str_field(row, "size")]);
        let created_at = non_empty(&[&str_field(row, "updated_at"), &str_field(row, "created_at")]);
        let sharer_raw = str_field(row, "real_sharer");
        let channel_name = if !sharer_raw.is_empty() {
            format!("影巢中转 · {}", sharer_raw)
        } else {
            "影巢中转".to_string()
        };

        out.push(ResourceItem {
            id: format!("relay-{}-{}", rules::clean_slug(&display_slug), idx),
            clean_slug: rules::clean_slug(&display_slug),
            slug: display_slug,
            title: non_empty(&[&title, &remark, &resource_id]),
            raw_title: full_text,
            remark,
            tmdb_id: tmdb_id.to_string(),
            media_type: mtype.to_string(),
            year: str_field(row, "year"),
            source_type: "hdhive".into(),
            channel_name,
            pan_type,
            share_code,
            receive_code,
            size_bytes: (rules::parse_size_to_gb(&share_size) * 1073741824.0) as i64,
            share_size,
            parsed_episodes: parsed_eps,
            movie_attributes: attrs,
            is_unlocked: unlocked,
            unlock_points: effective_pts,
            created_at,
            ..Default::default()
        });
    }
    Ok(out)
}

pub fn parse_hdhive_resources_json(
    raw: &str,
    tmdb_id: &str,
    mtype: &str,
    season_totals: Option<&HashMap<u32, u32>>,
) -> Result<Vec<ResourceItem>> {
    let v: Value = serde_json::from_str(raw)?;
    if v.get("items").and_then(Value::as_array).is_some() {
        return parse_relay_resources_json(raw, tmdb_id, mtype, season_totals);
    }
    if v.get("success").and_then(|x| x.as_bool()) == Some(false) && v.get("data").is_none() {
        let msg = v.get("message").and_then(|x| x.as_str()).unwrap_or("未知错误");
        return Err(anyhow!("HDHive 接口错误: {}", msg));
    }
    let Some(arr) = v.get("data").and_then(|x| x.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(arr.len());
    for (idx, row) in arr.iter().enumerate() {
        let raw_slug = str_field(row, "slug");
        if raw_slug.is_empty() {
            continue;
        }
        let clean = raw_slug
            .trim_start_matches("hdhive://")
            .trim_start_matches("radar://");
        let low_clean = clean.to_lowercase();
        let is_direct = low_clean.starts_with("ed2k://") || low_clean.starts_with("magnet:?");
        let (display_slug, pan_type) = if is_direct {
            (
                clean.to_string(),
                if low_clean.starts_with("ed2k://") { "ed2k" } else { "magnet" }.to_string(),
            )
        } else {
            let (sc, rc) = rules::parse_115_share(clean, "");
            if !sc.is_empty() && low_clean.contains("115") {
                (rules::format_115_share_url(&sc, &rc), "115".to_string())
            } else {
                (format!("hdhive://{}", clean), "radar".to_string())
            }
        };

        let title = non_empty(&[&str_field(row, "title"), &str_field(row, "name")]);
        let remark = str_field(row, "remark");
        let tags = join_value_strings(&[
            row.get("video_resolution"),
            row.get("source"),
            row.get("color"),
            row.get("audio"),
            row.get("video_encoding"),
            row.get("subtitle_language"),
        ]);
        let full_text = format!("{} {} {}", title, remark, tags).trim().to_string();
        let attrs = rules::parse_movie_attributes(&full_text);
        let parsed_eps = if mtype == "tv" {
            rules::parse_shallow_episodes(&full_text, "tv", season_totals)
        } else {
            String::new()
        };
        let pts = int_field(row, "unlock_points").unwrap_or(0) as i32;
        let mut unlocked = row.get("is_unlocked").and_then(|x| x.as_bool()).unwrap_or(false);
        if pts == 0 && is_direct {
            unlocked = true;
        }
        let effective_pts = if unlocked { 0 } else { pts.max(0) };
        let share_size = non_empty(&[&str_field(row, "share_size"), &str_field(row, "size")]);
        let created_at = non_empty(&[&str_field(row, "updated_at"), &str_field(row, "created_at")]);
        let sharer = row
            .get("user")
            .and_then(|u| u.get("nickname"))
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|n| format!("HDHive · {}", n.trim()))
            .unwrap_or_else(|| "HDHive".to_string());

        out.push(ResourceItem {
            id: format!("hdhive-{}-{}", clean, idx),
            clean_slug: rules::clean_slug(&display_slug),
            slug: display_slug,
            title: non_empty(&[&title, &remark, clean]),
            raw_title: full_text,
            remark,
            tmdb_id: tmdb_id.to_string(),
            media_type: mtype.to_string(),
            source_type: "hdhive".into(),
            channel_name: sharer,
            pan_type,
            size_bytes: (rules::parse_size_to_gb(&share_size) * 1073741824.0) as i64,
            share_size,
            parsed_episodes: parsed_eps,
            movie_attributes: attrs,
            is_unlocked: unlocked,
            unlock_points: effective_pts,
            created_at,
            ..Default::default()
        });
    }
    Ok(out)
}

fn strip_hdhive_resource_prefix(raw: &str) -> &str {
    let no_relay = raw.trim().strip_prefix("relay://").unwrap_or(raw.trim());
    let no_type = no_relay
        .strip_prefix("movie:")
        .or_else(|| no_relay.strip_prefix("tv:"))
        .unwrap_or(no_relay);
    no_type
        .trim_start_matches("hdhive://")
        .trim_start_matches("radar://")
}

pub fn unlock_hdhive(
    db: &Db,
    cfg: &mut AppConfig,
    slug: &str,
    media_type: &str,
    requested_points: i32,
    allow_spend: bool,
    proxy_url: &str,
) -> Result<(String, String, String)> {
    let raw = slug.trim();
    let cache_key = rules::clean_slug(raw);

    if let Some(entry) = db.get_probe_cache(&cache_key) {
        if !entry.share_code.is_empty() {
            let url = rules::format_115_share_url(&entry.share_code, &entry.receive_code);
            return Ok((entry.share_code, entry.receive_code, url));
        }
    }

    let raw_clean = strip_hdhive_resource_prefix(raw);
    let (sc0, rc0) = rules::parse_115_share(raw_clean, "");
    if !sc0.is_empty() && raw_clean.to_lowercase().contains("115") {
        let url = rules::format_115_share_url(&sc0, &rc0);
        return Ok((sc0, rc0, url));
    }

    if requested_points > 0 {
        ensure!(
            allow_spend,
            "当前已关闭自动积分兑换（需 {} 积分），请在工作台手动解锁或开启开关",
            requested_points
        );
        if cfg.hdhive.max_unlock_points > 0 && requested_points > cfg.hdhive.max_unlock_points {
            return Err(anyhow!(
                "所需积分 ({}) 超过设定的单次上限 ({})",
                requested_points,
                cfg.hdhive.max_unlock_points
            ));
        }
    }

    let token = ensure_hdhive_token(cfg, proxy_url);
    ensure!(!token.is_empty(), "请先在「资源渠道」配置 HDHive / 影巢中转 API Key");

    let agent = build_http_agent(proxy_url, 22);
    let base = hdhive_base(cfg);
    let mtype = normalize_media_type(media_type);
    let is_relay = prefers_relay_protocol(cfg);

    let (sc, rc) = if is_relay {
        resolve_via_relay(&agent, &base, &token, raw, raw_clean, mtype, allow_spend, cfg.hdhive.max_unlock_points)?
    } else {
        match unlock_via_official(&agent, &base, &token, raw_clean) {
            Ok(pair) => pair,
            Err(e) if e.to_string().contains("HTTP 404") => {
                cfg.hdhive.protocol = "relay".into();
                resolve_via_relay(&agent, &base, &token, raw, raw_clean, mtype, allow_spend, cfg.hdhive.max_unlock_points)?
            }
            Err(e) => return Err(e),
        }
    };

    let full_url = rules::format_115_share_url(&sc, &rc);
    let now_ts = chrono::Utc::now().timestamp();
    let mut entry = db.get_probe_cache(&cache_key).unwrap_or_default();
    entry.clean_slug = cache_key;
    entry.share_code = sc.clone();
    entry.receive_code = rc.clone();
    entry.share_url = full_url.clone();
    entry.is_unlocked = true;
    entry.is_valid = true;
    entry.updated_ts = now_ts;
    let _ = db.put_probe_cache(&entry);

    let mut by_115 = entry.clone();
    by_115.clean_slug = format!("115:{}", sc.to_lowercase());
    let _ = db.put_probe_cache(&by_115);

    let _ = hdhive_sync_me(cfg, proxy_url);
    Ok((sc, rc, full_url))
}

fn resolve_via_relay(
    agent: &ureq::Agent,
    base: &str,
    token: &str,
    raw_slug: &str,
    raw_clean: &str,
    mtype: &str,
    allow_spend: bool,
    max_points: i32,
) -> Result<(String, String)> {
    throttle(&HDHIVE_LAST_CALL, Duration::from_millis(450));
    let url = format!("{}/v1/resources/resolve", base);
    let resource_id = if raw_slug.starts_with("movie:") || raw_slug.starts_with("tv:") {
        raw_slug.to_string()
    } else {
        format!("{}:hdhive://{}", mtype, raw_clean)
    };
    let max_pts = if max_points > 0 { max_points } else { 50 };
    let payload = json!({
        "resource_id": resource_id,
        "media_type": mtype,
        "allow_paid": allow_spend,
        "max_points": max_pts,
    });

    let (status, resp_raw) = match agent
        .post(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .set("X-Relay-Key", token)
        .send_json(payload)
    {
        Ok(r) => (r.status(), read_body_string(r, 1 << 20)),
        Err(ureq::Error::Status(code, r)) => (code, read_body_string(r, 1 << 20)),
        Err(e) => return Err(anyhow!("请求影巢中转解析失败: {}", e)),
    };

    let parsed: Value = serde_json::from_str(&resp_raw).unwrap_or_else(|_| json!({}));
    let err_msg = non_empty(&[
        &str_field(&parsed, "detail"),
        &str_field(&parsed, "message"),
        &str_field(&parsed, "error"),
    ]);
    ensure!(status == 200, "影巢中转解析失败 (HTTP {}): {}", status, err_msg);

    let data_ref = parsed.get("data").unwrap_or(&parsed);
    let share_url = non_empty(&[
        &str_field(data_ref, "share_url"),
        &str_field(data_ref, "url"),
        &str_field(&parsed, "share_url"),
        &str_field(&parsed, "url"),
    ]);
    let access_code = non_empty(&[
        &str_field(data_ref, "access_code"),
        &str_field(data_ref, "receive_code"),
        &str_field(data_ref, "password"),
        &str_field(&parsed, "access_code"),
        &str_field(&parsed, "receive_code"),
    ]);
    let (sc, mut rc) = rules::parse_115_share(&share_url, &access_code);
    if rc.is_empty() && !access_code.is_empty() {
        rc = access_code;
    }
    ensure!(!sc.is_empty(), "影巢中转未返回有效的 115 分享链接");
    Ok((sc, rc))
}

fn unlock_via_official(
    agent: &ureq::Agent,
    base: &str,
    token: &str,
    raw_clean: &str,
) -> Result<(String, String)> {
    throttle(&HDHIVE_LAST_CALL, Duration::from_millis(950));
    let url = format!("{}/api/open/resources/unlock", base);
    let (status, resp_raw) = match agent
        .post(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .set("X-API-Key", HDHIVE_APP_KEY)
        .send_json(json!({ "slug": raw_clean }))
    {
        Ok(r) => (r.status(), read_body_string(r, 1 << 20)),
        Err(ureq::Error::Status(code, r)) => (code, read_body_string(r, 1 << 20)),
        Err(e) => return Err(anyhow!("请求 HDHive 解锁失败: {}", e)),
    };

    let parsed: Value = serde_json::from_str(&resp_raw).unwrap_or_else(|_| json!({}));
    let err_msg = non_empty(&[&str_field(&parsed, "message"), &str_field(&parsed, "error")]);
    ensure!(status == 200, "HDHive 解锁失败 (HTTP {}): {}", status, err_msg);
    if parsed.get("success").and_then(|x| x.as_bool()) == Some(false) {
        return Err(anyhow!("HDHive 解锁被拒: {}", err_msg));
    }

    let mut share_url = String::new();
    if let Some(d) = parsed.get("data") {
        share_url = str_field(d, "url");
        if share_url.is_empty() {
            let sc = str_field(d, "share_code");
            if !sc.is_empty() {
                share_url = rules::format_115_share_url(&sc, &str_field(d, "receive_code"));
            }
        }
    }
    if share_url.is_empty() {
        let sc = str_field(&parsed, "share_code");
        if !sc.is_empty() {
            share_url = rules::format_115_share_url(&sc, &str_field(&parsed, "receive_code"));
        }
    }
    let (sc, rc) = rules::parse_115_share(&share_url, "");
    ensure!(!sc.is_empty(), "HDHive 解锁响应未包含有效 115 分享链接");
    Ok((sc, rc))
}

fn enrich_from_probe_cache(db: &Db, mut items: Vec<ResourceItem>) -> Vec<ResourceItem> {
    for it in &mut items {
        if let Some(c) = db.get_probe_cache(&it.clean_slug) {
            if !c.share_code.is_empty() {
                it.is_unlocked = true;
                it.share_code = c.share_code;
                it.receive_code = c.receive_code;
                if !c.parsed_episodes.is_empty() {
                    it.parsed_episodes = c.parsed_episodes;
                }
                it.movie_attributes =
                    rules::merge_movie_attributes(&it.movie_attributes, &c.movie_attributes);
            }
        }
    }
    items
}

/* ==================== 3. Telegram Public Scraper & Channel Info ==================== */

static RE_TG_POST_SPLIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<div[^>]+class="[^"]*tgme_widget_message\b[^"]*"[^>]+data-post="([^"]+)"[^>]*>"#).unwrap()
});
static RE_TG_TEXT_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<div[^>]+class="[^"]*tgme_widget_message_text[^"]*"[^>]*>(.*?)</div>"#).unwrap()
});
static RE_TG_TIME_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<time[^>]+datetime="([^"]+)""#).unwrap());
static RE_HREF_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)href="([^"]+)""#).unwrap());
static RE_HTML_BR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>").unwrap());
static RE_HTML_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
static RE_TG_PAGE_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<div[^>]+class="[^"]*tgme_page_title[^"]*"[^>]*>(.*?)</div>"#).unwrap()
});
static RE_TG_HEADER_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<div[^>]+class="[^"]*tgme_header_title[^"]*"[^>]*>(.*?)</div>"#).unwrap()
});
static RE_TG_OG_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<meta[^>]+property="og:title"[^>]+content="([^"]+)""#).unwrap()
});
static RE_TG_PAGE_PHOTO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<img[^>]+class="[^"]*tgme_page_photo_image[^"]*"[^>]+src="([^"]+)""#).unwrap()
});

pub fn normalize_tg_channel(raw: &str) -> String {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix("TG · ") {
        s = rest.trim();
    }
    let low = s.to_ascii_lowercase();

    // 1. Private channel post link: https://t.me/c/2167886055/123 -> -1002167886055
    for prefix in ["https://t.me/c/", "http://t.me/c/", "t.me/c/"] {
        if low.starts_with(prefix) {
            let rest = &s[prefix.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                return format!("-100{}", digits);
            }
        }
    }

    // 2. Private invite link: https://t.me/+hash or https://t.me/joinchat/hash -> https://t.me/+hash
    for prefix in [
        "https://t.me/joinchat/",
        "http://t.me/joinchat/",
        "t.me/joinchat/",
        "https://t.me/+",
        "http://t.me/+",
        "t.me/+",
    ] {
        if low.starts_with(prefix) {
            let rest = &s[prefix.len()..];
            let hash = rest
                .split(&['/', '?', '#', ' '][..])
                .next()
                .unwrap_or("")
                .trim();
            if !hash.is_empty() {
                return format!("https://t.me/+{}", hash);
            }
        }
    }

    // 3. Public t.me URL or @username: strip prefix and cut URL suffix
    for prefix in [
        "https://t.me/s/",
        "http://t.me/s/",
        "https://t.me/",
        "http://t.me/",
        "t.me/s/",
        "t.me/",
        "@",
    ] {
        if low.starts_with(prefix) {
            let mut rest = &s[prefix.len()..];
            if let Some(idx) = rest.find(&['/', '?', '#'][..]) {
                rest = &rest[..idx];
            }
            return rest.trim().to_string();
        }
    }

    // 4. Plain display / Chinese channel title (may contain `/` like `懒狗集中营-115/阿里/...`): keep intact!
    s.trim().to_string()
}

pub fn unescape_html(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&#34;", "\"")
        .replace("&nbsp;", " ")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TGChannelMeta {
    pub identifier: String,
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub logo: String,
}

fn is_bogus_tg_title(title: &str) -> bool {
    let low = title.trim().to_ascii_lowercase();
    low.is_empty()
        || low.contains("telegram – a new era of messaging")
        || low.contains("telegram - a new era of messaging")
        || low.starts_with("telegram: contact")
        || low == "telegram"
}

pub fn parse_tg_channel_meta_html(page_html: &str) -> (String, String) {
    // Reject generic telegram.org landing pages that lack channel/chat metadata blocks
    let has_channel_marker = page_html.contains("tgme_page_title")
        || page_html.contains("tgme_header_title")
        || page_html.contains("tgme_page_extra")
        || page_html.contains("tgme_channel_info")
        || page_html.contains("tgme_widget_message");

    let mut title = String::new();
    if let Some(cap) = RE_TG_PAGE_TITLE.captures(page_html) {
        title = unescape_html(&RE_HTML_TAG.replace_all(&cap[1], ""))
            .trim()
            .to_string();
    }
    if title.is_empty() {
        if let Some(cap) = RE_TG_HEADER_TITLE.captures(page_html) {
            title = unescape_html(&RE_HTML_TAG.replace_all(&cap[1], ""))
                .trim()
                .to_string();
        }
    }
    if title.is_empty() && has_channel_marker {
        if let Some(cap) = RE_TG_OG_TITLE.captures(page_html) {
            title = unescape_html(&cap[1]).trim().to_string();
        }
    }
    if is_bogus_tg_title(&title) || !has_channel_marker {
        return (String::new(), String::new());
    }
    let photo = RE_TG_PAGE_PHOTO
        .captures(page_html)
        .map(|c| unescape_html(&c[1]).trim().to_string())
        .unwrap_or_default();
    (title, photo)
}

pub fn resolve_tg_channel_info(
    data_dir: &std::path::Path,
    cfg: &AppConfig,
    raw_input: &str,
    proxy_url: &str,
) -> Result<TGChannelMeta> {
    if crate::tg_mtproto::has_session_file(data_dir) {
        return crate::tg_mtproto::resolve_channel_mtproto(data_dir, cfg, raw_input, proxy_url);
    }
    fetch_tg_channel_info_public(raw_input, proxy_url)
}

pub fn fetch_tg_channel_info_public(raw_input: &str, proxy_url: &str) -> Result<TGChannelMeta> {
    let trimmed = raw_input.trim();
    ensure!(!trimmed.is_empty(), "请输入频道名称或 @用户名");
    if trimmed.contains("t.me/+") || trimmed.contains("t.me/joinchat/") || trimmed.starts_with('-') {
        return Err(anyhow!(
            "私密频道或数字 ID 订阅需先在上方登录您的 Telegram 账号并确保已加入该频道"
        ));
    }
    let clean = normalize_tg_channel(trimmed);
    if !crate::tg_mtproto::is_valid_public_username(&clean) {
        return Err(anyhow!(
            "当前未登录 Telegram 账号：按中文频道名搜索或识别私密频道，请先在上方填写手机号登录 Telegram（未登录仅支持公开频道英文 @username）"
        ));
    }

    let agent = build_http_agent(proxy_url, 14);
    let url = format!("https://t.me/{}", clean);
    let resp = agent
        .get(&url)
        .set("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .call()
        .map_err(|e| anyhow!("连接 Telegram 失败（请检查系统代理）: {}", e))?;
    let page_html = read_body_string(resp, 2 << 20);
    let (title, photo_url) = parse_tg_channel_meta_html(&page_html);
    ensure!(
        !title.is_empty(),
        "未找到公开频道 @{}（如为私密频道或频道中文名，请先在上方登录 Telegram 账号）",
        clean
    );

    let mut logo = String::new();
    if photo_url.starts_with("http://") || photo_url.starts_with("https://") {
        if let Ok(img_resp) = agent.get(&photo_url).call() {
            let ct = img_resp
                .header("Content-Type")
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let bytes = read_body_bytes(img_resp, 160 * 1024);
            if bytes.len() > 64 {
                if let Some(mime) = detect_image_mime(&bytes, &ct) {
                    logo = format!("data:{};base64,{}", mime, B64.encode(&bytes));
                }
            }
        }
    }
    Ok(TGChannelMeta {
        identifier: clean,
        name: title,
        logo,
    })
}

#[derive(Debug, Clone)]
pub struct RawTGPost {
    pub channel: String,
    pub msg_id: i64,
    pub text: String,
    pub links_blob: String,
    pub created_at: String,
}

pub fn parse_tg_channel_web_html(
    channel: &str,
    page_html: &str,
    min_msg_id: i64,
) -> (Vec<RawTGPost>, i64) {
    let clean_ch = normalize_tg_channel(channel);
    let caps: Vec<_> = RE_TG_POST_SPLIT.captures_iter(page_html).collect();
    if caps.is_empty() {
        return (Vec::new(), min_msg_id);
    }

    let mut posts = Vec::new();
    let mut max_id = min_msg_id;

    for (idx, cap) in caps.iter().enumerate() {
        let m0 = cap.get(0).unwrap();
        let post_attr = &cap[1];
        let msg_id = post_attr
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        if msg_id <= 0 {
            continue;
        }
        if msg_id > max_id {
            max_id = msg_id;
        }
        if min_msg_id > 0 && msg_id <= min_msg_id {
            continue;
        }

        let end_idx = caps
            .get(idx + 1)
            .map(|c| c.get(0).unwrap().start())
            .unwrap_or(page_html.len());
        let chunk = &page_html[m0.end()..end_idx];

        let text_html = RE_TG_TEXT_BLOCK
            .captures(chunk)
            .map(|c| c[1].to_string())
            .unwrap_or_default();
        let mut hrefs = Vec::new();
        for hm in RE_HREF_ATTR.captures_iter(chunk) {
            let u = unescape_html(hm[1].trim());
            if !u.is_empty() && !u.starts_with("https://t.me/") {
                hrefs.push(u);
            }
        }

        let with_nl = RE_HTML_BR.replace_all(&text_html, "\n");
        let no_tags = RE_HTML_TAG.replace_all(&with_nl, "");
        let plain = unescape_html(&no_tags).trim().to_string();
        if plain.is_empty() && hrefs.is_empty() {
            continue;
        }

        let created_at = RE_TG_TIME_ATTR
            .captures(chunk)
            .map(|c| {
                let dt = &c[1];
                if dt.len() >= 19 {
                    dt[..19].replace('T', " ")
                } else {
                    dt.to_string()
                }
            })
            .unwrap_or_default();

        posts.push(RawTGPost {
            channel: clean_ch.clone(),
            msg_id,
            text: plain,
            links_blob: hrefs.join("\n"),
            created_at,
        });
    }
    (posts, max_id)
}

pub fn convert_tg_posts_to_resources(
    posts: &[RawTGPost],
    expand_telegraph: bool,
    agent: Option<&ureq::Agent>,
) -> Vec<ResourceItem> {
    let mut out = Vec::new();
    let mut telegraph_fetched = 0usize;

    for p in posts {
        let mut combined = format!("{}\n{}", p.text, p.links_blob);
        if expand_telegraph {
            if let Some(ag) = agent {
                for tg_url in rules::extract_telegraph_urls(&combined) {
                    if telegraph_fetched >= 4 {
                        break;
                    }
                    telegraph_fetched += 1;
                    if let Ok(resp) = ag.get(&tg_url).call() {
                        let extra = unescape_html(&read_body_string(resp, 2 << 20));
                        combined.push('\n');
                        combined.push_str(&extra);
                    }
                }
            }
        }

        let links = rules::extract_all_resource_links(&combined);
        if links.is_empty() {
            continue;
        }

        let pure_title = rules::extract_pure_title(&p.text);
        let tmdb_id = rules::extract_tmdb_id(&p.text);
        let year = rules::extract_year(&p.text);
        let msg_url = format!("https://t.me/{}/{}", p.channel, p.msg_id);

        let mut ed2k_links = Vec::new();
        for lk in links {
            if lk.pan_type == "ed2k" {
                ed2k_links.push(lk);
                continue;
            }
            let file_pure = rules::extract_pure_title(&lk.file_name);
            let fl = first_line(&p.text);
            let safe_first = if rules::is_suspect_banner_title(&fl) {
                ""
            } else {
                fl.as_str()
            };
            let title = non_empty(&[
                &pure_title,
                &file_pure,
                &lk.file_name,
                safe_first,
                "未命名资源",
            ]);
            let ctx_text = format!("{}\n{}", p.text, lk.file_name);
            let attrs = rules::parse_movie_attributes(&ctx_text);
            let explicit_movie = p.text.contains("类型：电影")
                || p.text.contains("类型: 电影")
                || p.text.contains("🎬 电影")
                || p.text.contains("[电影]")
                || p.text.contains("#电影");
            let eps = if explicit_movie {
                String::new()
            } else {
                rules::parse_shallow_episodes(&ctx_text, "tv", None)
            };
            let mtype = if !eps.is_empty()
                || (!explicit_movie
                    && (p.text.contains("剧集")
                        || p.text.contains("电视剧")
                        || p.text.contains("番剧")
                        || p.text.contains("季集")
                        || p.text.contains('季')
                        || p.text.contains("集全")
                        || p.text.contains("更新至")))
            {
                "tv"
            } else {
                "movie"
            };
            let sz_gb = rules::parse_size_to_gb(&p.text);
            let sz_str = if sz_gb > 0.0 {
                format!("{:.2}GB", sz_gb)
            } else {
                String::new()
            };
            out.push(ResourceItem {
                id: format!("tg-{}-{}-{}", p.channel, p.msg_id, lk.pan_type),
                slug: lk.url,
                clean_slug: lk.clean_key,
                title,
                raw_title: p.text.clone(),
                remark: truncate_chars(&p.text, 180),
                tmdb_id: tmdb_id.clone(),
                media_type: mtype.into(),
                year: year.clone(),
                source_type: "telegram".into(),
                channel_id: normalize_tg_channel(&p.channel).to_lowercase(),
                channel_name: format!("@{}", p.channel),
                pan_type: lk.pan_type,
                share_code: lk.share_code,
                receive_code: lk.receive_code,
                size_bytes: (sz_gb * 1073741824.0) as i64,
                share_size: sz_str,
                parsed_episodes: eps,
                movie_attributes: attrs,
                is_unlocked: true,
                created_at: p.created_at.clone(),
                message_url: msg_url.clone(),
                ..Default::default()
            });
        }

        if !ed2k_links.is_empty() {
            let mut urls = Vec::new();
            let mut names = Vec::new();
            let mut total_bytes = 0i64;
            for el in &ed2k_links {
                urls.push(el.url.clone());
                if !el.file_name.is_empty() {
                    names.push(el.file_name.clone());
                }
                total_bytes += el.size_bytes;
            }
            let joined_names = names.join(" ");
            let first_pure = names
                .first()
                .map(|n| rules::extract_pure_title(n))
                .unwrap_or_default();
            let first_raw = names.first().cloned().unwrap_or_default();
            let title = non_empty(&[&pure_title, &first_pure, &first_raw, "ED2K 资源"]);
            let ctx_text = format!("{} {}", joined_names, p.text);
            let attrs = rules::parse_movie_attributes(&ctx_text);
            let eps = rules::parse_shallow_episodes(&ctx_text, "tv", None);
            let mtype = if !eps.is_empty() || ed2k_links.len() > 1 {
                "tv"
            } else {
                "movie"
            };
            out.push(ResourceItem {
                id: format!("tg-{}-{}-ed2k", p.channel, p.msg_id),
                slug: urls.join("\n"),
                clean_slug: format!("ed2k-bundle:{}:{}", p.channel.to_lowercase(), p.msg_id),
                title,
                raw_title: p.text.clone(),
                remark: format!(
                    "含 {} 个 ED2K 文件 · {}",
                    ed2k_links.len(),
                    truncate_chars(&p.text, 120)
                ),
                tmdb_id,
                media_type: mtype.into(),
                year,
                source_type: "telegram".into(),
                channel_id: normalize_tg_channel(&p.channel).to_lowercase(),
                channel_name: format!("@{}", p.channel),
                pan_type: "ed2k".into(),
                share_size: rules::format_bytes(total_bytes),
                size_bytes: total_bytes,
                parsed_episodes: eps,
                movie_attributes: attrs,
                is_unlocked: true,
                created_at: p.created_at.clone(),
                message_url: msg_url,
                ..Default::default()
            });
        }
    }
    out
}

pub fn scrape_tg_channel(
    channel: &str,
    query: &str,
    mut min_msg_id: i64,
    expand_telegraph: bool,
    proxy_url: &str,
) -> Result<(Vec<ResourceItem>, i64)> {
    let clean_ch = normalize_tg_channel(channel);
    ensure!(!clean_ch.is_empty(), "频道用户名不能为空");

    let mut target_url = format!("https://t.me/s/{}", clean_ch);
    let agent = build_http_agent(proxy_url, 18);
    let mut req = agent
        .get(&target_url)
        .set("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8");
    if !query.trim().is_empty() {
        req = req.query("q", query.trim());
        min_msg_id = 0;
        target_url.push_str("?q=...");
    }
    let resp = req.call().map_err(|e| {
        anyhow!(
            "访问 Telegram 频道 @{} 失败（如国内网络请检查系统代理）: {}",
            clean_ch,
            e
        )
    })?;
    let page_html = read_body_string(resp, 4 << 20);
    let (posts, max_id) = parse_tg_channel_web_html(&clean_ch, &page_html, min_msg_id);
    let items = convert_tg_posts_to_resources(&posts, expand_telegraph, Some(&agent));
    Ok((items, max_id))
}

pub fn sync_all_tg_channels(
    db: &Db,
    cfg: &mut AppConfig,
    force_manual: bool,
    proxy_url: &str,
) -> (usize, HashMap<String, Vec<ResourceItem>>, Vec<String>) {
    let mut by_ch: HashMap<String, Vec<ResourceItem>> = HashMap::new();
    if !cfg.telegram.enabled {
        return (0, by_ch, Vec::new());
    }

    if crate::tg_mtproto::has_session_file(&db.data_dir) {
        match crate::tg_mtproto::sync_channels_mtproto(&db.data_dir, db, cfg, force_manual, proxy_url) {
            Ok(res) => return res,
            Err(e) => {
                return (0, by_ch, vec![e.to_string()]);
            }
        }
    }

    let mut total_added = 0usize;
    let mut errs = Vec::new();
    let now_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    for ch in &mut cfg.telegram.channels {
        ch.level = normalize_tg_level(ch.level);
        if !ch.enabled || (!force_manual && ch.level <= 0) {
            continue;
        }
        let clean_id = normalize_tg_channel(&ch.identifier);
        if clean_id.is_empty() {
            continue;
        }
        if !crate::tg_mtproto::is_valid_public_username(&clean_id) {
            let msg = "私密频道请先登录 Telegram 账号".to_string();
            ch.last_error = msg.clone();
            errs.push(format!("{}: {}", ch.name, msg));
            continue;
        }
        let norm_key = clean_id.to_lowercase();
        ch.last_sync_at = now_str.clone();
        match scrape_tg_channel(
            &clean_id,
            "",
            ch.last_msg_id,
            cfg.telegram.expand_telegraph,
            proxy_url,
        ) {
            Ok((items, max_id)) => {
                ch.last_error.clear();
                if max_id > ch.last_msg_id {
                    ch.last_msg_id = max_id;
                }
                let added = db.insert_tg_items_with_channel(&items, &norm_key).unwrap_or(0);
                if added > 0 {
                    ch.cached_count += added;
                    total_added += added;
                    by_ch.insert(norm_key, items);
                }
            }
            Err(e) => {
                ch.last_error = e.to_string();
                errs.push(format!("@{}: {}", clean_id, e));
            }
        }
    }
    (total_added, by_ch, errs)
}

pub fn search_telegram_scoped(
    db: &Db,
    cfg: &mut AppConfig,
    keyword: &str,
    tmdb_id: &str,
    media_type: &str,
    live_query: bool,
    include_hidden: bool,
    proxy_url: &str,
) -> Vec<ResourceItem> {
    if !cfg.telegram.enabled {
        return Vec::new();
    }
    let mut excluded = HashSet::new();
    for ch in &cfg.telegram.channels {
        let norm = normalize_tg_channel(&ch.identifier).to_lowercase();
        if norm.is_empty() {
            continue;
        }
        if !ch.enabled || (ch.hide_in_discover && !include_hidden) {
            excluded.insert(norm);
        }
    }

    let mut out = db.search_tg_cache(keyword, tmdb_id, &excluded);
    let mut seen: HashSet<String> = out.iter().map(|x| x.clean_slug.clone()).collect();

    if live_query && !keyword.trim().is_empty() {
        if crate::tg_mtproto::has_session_file(&db.data_dir) {
            if let Ok(batches) = crate::tg_mtproto::search_channels_mtproto(
                &db.data_dir,
                cfg,
                keyword,
                include_hidden,
                proxy_url,
            ) {
                for (norm_key, mut items) in batches {
                    let added = db.insert_tg_items_with_channel(&items, &norm_key).unwrap_or(0);
                    if added > 0 {
                        if let Some(ch) = cfg.telegram.channels.iter_mut().find(|c| {
                            normalize_tg_channel(&c.identifier).to_lowercase() == norm_key
                        }) {
                            ch.cached_count += added;
                        }
                    }
                    for mut it in items.drain(..) {
                        if !tmdb_id.trim().is_empty() && it.tmdb_id.is_empty() {
                            it.tmdb_id = tmdb_id.trim().to_string();
                        }
                        if !media_type.is_empty() && it.media_type.is_empty() {
                            it.media_type = media_type.to_string();
                        }
                        if seen.insert(it.clean_slug.clone()) {
                            out.push(it);
                        }
                    }
                }
                return out;
            }
        }

        let mut scraped = 0usize;
        for ch in &mut cfg.telegram.channels {
            if !ch.enabled || (ch.hide_in_discover && !include_hidden) || scraped >= 6 {
                continue;
            }
            let clean_id = normalize_tg_channel(&ch.identifier);
            if !crate::tg_mtproto::is_valid_public_username(&clean_id) {
                continue;
            }
            let norm = clean_id.to_lowercase();
            scraped += 1;
            if let Ok((mut items, _)) = scrape_tg_channel(
                &clean_id,
                keyword,
                0,
                cfg.telegram.expand_telegraph,
                proxy_url,
            ) {
                let added = db.insert_tg_items_with_channel(&items, &norm).unwrap_or(0);
                ch.cached_count += added;
                for mut it in items.drain(..) {
                    if !tmdb_id.trim().is_empty() && it.tmdb_id.is_empty() {
                        it.tmdb_id = tmdb_id.trim().to_string();
                    }
                    if !media_type.is_empty() && it.media_type.is_empty() {
                        it.media_type = media_type.to_string();
                    }
                    if seen.insert(it.clean_slug.clone()) {
                        out.push(it);
                    }
                }
            }
        }
    }
    out
}

pub fn send_tg_bot_message(
    bot_token: &str,
    chat_id: &str,
    html_text: &str,
    proxy_url: &str,
) -> Result<()> {
    let tok = bot_token.trim();
    let cid = chat_id.trim();
    ensure!(!tok.is_empty() && !cid.is_empty(), "请填写 Bot Token 和 Chat ID");
    let agent = build_http_agent(proxy_url, 12);
    let url = format!("https://api.telegram.org/bot{}/sendMessage", tok);
    let resp = agent
        .post(&url)
        .send_json(json!({
            "chat_id": cid,
            "text": html_text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        }))
        .map_err(|e| anyhow!("Telegram Bot 发送失败: {}", e))?;
    let raw = read_body_string(resp, 1 << 20);
    let v: Value = serde_json::from_str(&raw)?;
    ensure!(
        v.get("ok").and_then(|x| x.as_bool()) == Some(true),
        "Telegram Bot 返回失败: {}",
        str_field(&v, "description")
    );
    Ok(())
}

/* ==================== 4. AYClub (NF Token) ==================== */

fn ayclub_endpoint(cfg: &AppConfig) -> String {
    let u = cfg.ayclub.base_url.trim();
    if u.is_empty() {
        "http://api.ayclub.vip:5050/api/next_find".into()
    } else {
        u.to_string()
    }
}

pub fn refresh_ayclub_quota(cfg: &mut AppConfig, proxy_url: &str) -> Result<String> {
    let nf_token = cfg.ayclub.nf_token.trim().to_string();
    let tg_id = cfg.ayclub.tg_user_id.trim().to_string();
    ensure!(
        !nf_token.is_empty() && !tg_id.is_empty(),
        "请填写 AYClub NF Token 与绑定的 TG 用户数字 ID"
    );

    let agent = build_http_agent(proxy_url, 20);
    throttle(&AYCLUB_LAST_CALL, Duration::from_secs(3));
    let url = ayclub_endpoint(cfg);
    let (status, raw) = match agent.post(&url).send_json(json!({
        "tg_id": tg_id,
        "tmdb_id": "278",
        "type": "movie",
        "nf_token": nf_token,
    })) {
        Ok(r) => (r.status(), read_body_string(r, 2 << 20)),
        Err(ureq::Error::Status(code, r)) => (code, read_body_string(r, 1 << 20)),
        Err(e) => return Err(anyhow!("请求 AYClub 失败: {}", e)),
    };

    cfg.ayclub.last_checked_at = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    if status == 429 || status == 401 {
        cfg.ayclub.remaining_times = Some(0);
        if status == 429 {
            cfg.ayclub.exhausted_date = Local::now().format("%Y-%m-%d").to_string();
        }
        cfg.ayclub.last_checked_status = format!("HTTP {} (配额耗尽或未授权)", status);
        return Ok(cfg.ayclub.last_checked_status.clone());
    }
    ensure!(status == 200, "AYClub 返回 HTTP {}", status);

    let v: Value = serde_json::from_str(&raw)?;
    if let Some(times) = int_field(&v, "times") {
        let t = times as i32;
        cfg.ayclub.remaining_times = Some(t);
        if t <= 0 {
            cfg.ayclub.exhausted_date = Local::now().format("%Y-%m-%d").to_string();
        } else {
            cfg.ayclub.exhausted_date.clear();
        }
        cfg.ayclub.last_checked_status = format!("正常（今日剩余 {} 次）", t);
        return Ok(cfg.ayclub.last_checked_status.clone());
    }
    cfg.ayclub.last_checked_status = "连通成功".into();
    Ok(cfg.ayclub.last_checked_status.clone())
}

pub fn search_ayclub(
    db: &Db,
    cfg: &mut AppConfig,
    tmdb_id: &str,
    media_type: &str,
    season_totals: &HashMap<u32, u32>,
    force_refresh: bool,
    proxy_url: &str,
) -> Result<Vec<ResourceItem>> {
    if !cfg.ayclub.enabled {
        return Ok(Vec::new());
    }
    let nf_token = cfg.ayclub.nf_token.trim().to_string();
    let tg_id = cfg.ayclub.tg_user_id.trim().to_string();
    if nf_token.is_empty() || tg_id.is_empty() || tmdb_id.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mtype = normalize_media_type(media_type);
    let cache_key = format!("ayclub_{}_{}", mtype, tmdb_id.trim());
    let cached = db.get_provider_cache(&cache_key, 30 * 86400);
    if !force_refresh {
        if let Some(items) = cached {
            return Ok(items);
        }
    }

    let today = Local::now().format("%Y-%m-%d").to_string();
    if cfg.ayclub.exhausted_date == today {
        return Ok(cached.unwrap_or_default());
    }

    let agent = build_http_agent(proxy_url, 20);
    throttle(&AYCLUB_LAST_CALL, Duration::from_secs(3));
    let url = ayclub_endpoint(cfg);
    let (status, raw) = match agent.post(&url).send_json(json!({
        "tg_id": tg_id,
        "tmdb_id": tmdb_id.trim(),
        "type": mtype,
        "nf_token": nf_token,
    })) {
        Ok(r) => (r.status(), read_body_string(r, 2 << 20)),
        Err(ureq::Error::Status(code, r)) => (code, read_body_string(r, 1 << 20)),
        Err(e) => {
            if let Some(items) = cached {
                return Ok(items);
            }
            return Err(anyhow!("请求 AYClub 失败: {}", e));
        }
    };

    if status == 429 {
        cfg.ayclub.remaining_times = Some(0);
        cfg.ayclub.exhausted_date = today;
        return Ok(cached.unwrap_or_default());
    }
    ensure!(status == 200, "AYClub 返回 HTTP {}", status);

    let (items, maybe_times) = parse_ayclub_json(&raw, tmdb_id.trim(), mtype, Some(season_totals))?;
    if let Some(t) = maybe_times {
        cfg.ayclub.remaining_times = Some(t);
        if t <= 0 {
            cfg.ayclub.exhausted_date = today;
        }
    }
    let _ = db.put_provider_cache(&cache_key, &items);
    Ok(items)
}

fn parse_ayclub_json(
    raw: &str,
    tmdb_id: &str,
    mtype: &str,
    season_totals: Option<&HashMap<u32, u32>>,
) -> Result<(Vec<ResourceItem>, Option<i32>)> {
    let parsed: Value = serde_json::from_str(raw)?;
    let maybe_times = int_field(&parsed, "times").map(|x| x as i32);
    if let Some(st_code) = int_field(&parsed, "state") {
        if st_code != 0 {
            return Ok((Vec::new(), maybe_times));
        }
    }
    let mut rows = None;
    for k in ["data", "resources", "items", "list", "result"] {
        if let Some(arr) = parsed.get(k).and_then(|x| x.as_array()) {
            rows = Some(arr);
            break;
        }
    }
    let Some(arr) = rows else {
        return Ok((Vec::new(), maybe_times));
    };

    let today_str = Local::now().format("%Y-%m-%d").to_string();
    let mut out = Vec::new();
    for (idx, row) in arr.iter().enumerate() {
        let link = non_empty(&[
            &str_field(row, "link"),
            &str_field(row, "url"),
            &str_field(row, "share_link"),
        ]);
        if link.is_empty() {
            continue;
        }
        let low = link.to_lowercase();
        let (pan_type, sc, rc, slug) = if low.starts_with("ed2k://") {
            ("ed2k".to_string(), String::new(), String::new(), link.clone())
        } else if low.starts_with("magnet:?") {
            ("magnet".to_string(), String::new(), String::new(), link.clone())
        } else {
            let (s, r) = rules::parse_115_share(&link, "");
            let u = if !s.is_empty() {
                rules::format_115_share_url(&s, &r)
            } else {
                link.clone()
            };
            ("115".to_string(), s, r, u)
        };

        let name = non_empty(&[
            &str_field(row, "name"),
            &str_field(row, "title"),
            &str_field(row, "filename"),
        ]);
        let notes = non_empty(&[
            &str_field(row, "notes"),
            &str_field(row, "remark"),
            &str_field(row, "description"),
        ]);
        let mut share_size = non_empty(&[&str_field(row, "size"), &str_field(row, "share_size")]);
        if !share_size.is_empty() && share_size.chars().all(|c| c.is_ascii_digit() || c == '.') {
            share_size.push_str("GB");
        }
        let size_bytes = if pan_type == "ed2k" {
            let b: i64 = rules::extract_all_resource_links(&link)
                .iter()
                .map(|x| x.size_bytes)
                .sum();
            if b > 0 && share_size.is_empty() {
                share_size = rules::format_bytes(b);
            }
            b
        } else {
            (rules::parse_size_to_gb(&share_size) * 1073741824.0) as i64
        };

        let full_text = format!("{} {}", name, notes).trim().to_string();
        let attrs = rules::parse_movie_attributes(&full_text);
        let eps = if mtype == "tv" {
            rules::parse_shallow_episodes(&full_text, "tv", season_totals)
        } else {
            String::new()
        };

        out.push(ResourceItem {
            id: format!("ayclub-{}-{}", tmdb_id, idx),
            clean_slug: rules::clean_slug(&slug),
            slug,
            title: non_empty(&[&name, "AYClub 资源"]),
            raw_title: full_text,
            remark: notes,
            tmdb_id: tmdb_id.to_string(),
            media_type: mtype.to_string(),
            source_type: "ayclub".into(),
            channel_name: "AYClub".into(),
            pan_type,
            share_code: sc,
            receive_code: rc,
            share_size,
            size_bytes,
            parsed_episodes: eps,
            movie_attributes: attrs,
            is_unlocked: true,
            created_at: non_empty(&[&str_field(row, "release_date"), &today_str]),
            ..Default::default()
        });
    }
    Ok((out, maybe_times))
}

/* ==================== 5. Lenient RSS 2.0 & Atom Parser ==================== */

static RE_FEED_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<(item|entry)\b[^>]*>(.*?)</(?:item|entry)>").unwrap());
static RE_LINK_HREF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<(?:link|enclosure)\b[^>]*\b(?:href|url)\s*=\s*["']([^"']+)["']"#).unwrap()
});

#[derive(Debug, Clone, Default)]
pub struct RSSItem {
    pub guid: String,
    pub title: String,
    pub pure_title: String,
    pub year: i32,
    pub media_type: String,
    pub direct_links: Vec<rules::ExtractedLink>,
}

fn extract_xml_tag(block: &str, tag: &str) -> String {
    let pat = format!(r"(?is)<{0}(?:\s[^>]*)?>(.*?)</{0}>", regex::escape(tag));
    let Ok(re) = Regex::new(&pat) else {
        return String::new();
    };
    let Some(cap) = re.captures(block) else {
        return String::new();
    };
    let mut inner = cap[1].trim().to_string();
    if let Some(cdata) = inner
        .strip_prefix("<![CDATA[")
        .and_then(|s| s.strip_suffix("]]>"))
    {
        inner = cdata.trim().to_string();
    }
    unescape_html(&inner).trim().to_string()
}

pub fn parse_rss_xml(raw: &str) -> Result<Vec<RSSItem>> {
    let mut out = Vec::new();
    for cap in RE_FEED_ENTRY.captures_iter(raw) {
        let block = &cap[2];
        let title = extract_xml_tag(block, "title");
        if title.is_empty() {
            continue;
        }
        let mut link = extract_xml_tag(block, "link");
        if link.is_empty() {
            if let Some(hc) = RE_LINK_HREF.captures(block) {
                link = unescape_html(hc[1].trim());
            }
        }
        let guid = non_empty(&[
            &extract_xml_tag(block, "guid"),
            &extract_xml_tag(block, "id"),
            &link,
            &title,
        ]);
        let desc = non_empty(&[
            &extract_xml_tag(block, "content:encoded"),
            &extract_xml_tag(block, "description"),
            &extract_xml_tag(block, "summary"),
            &extract_xml_tag(block, "content"),
        ]);
        let pure_title = rules::extract_pure_title(&title);
        let year = rules::extract_year(&title).parse::<i32>().unwrap_or(0);
        let media_type = if !rules::parse_shallow_episodes(&title, "tv", None).is_empty()
            || title.contains('剧')
            || title.contains('季')
        {
            "tv".to_string()
        } else {
            "movie".to_string()
        };
        let direct_links =
            rules::extract_all_resource_links(&format!("{}\n{}\n{}", title, link, desc));

        out.push(RSSItem {
            guid,
            title,
            pure_title,
            year,
            media_type,
            direct_links,
        });
    }
    Ok(out)
}

pub fn fetch_rss_feed(feed_url: &str, proxy_url: &str) -> Result<Vec<RSSItem>> {
    let agent = build_http_agent(proxy_url, 20);
    let resp = agent
        .get(feed_url.trim())
        .set(
            "Accept",
            "application/rss+xml, application/atom+xml, application/xml, text/xml, */*",
        )
        .call()
        .map_err(|e| anyhow!("拉取 RSS 失败: {}", e))?;
    let raw = read_body_string(resp, 4 << 20);
    parse_rss_xml(&raw)
}

pub fn rss_interval_seconds(interval: &str) -> i64 {
    match interval.trim().to_ascii_uppercase().as_str() {
        "30M" => 1800,
        "1H" => 3600,
        "3H" => 3 * 3600,
        "6H" => 6 * 3600,
        "12H" => 12 * 3600,
        _ => 24 * 3600,
    }
}

/* ==================== Helpers ==================== */

fn normalize_media_type(v: &str) -> &'static str {
    if matches!(
        v.trim().to_lowercase().as_str(),
        "tv" | "series" | "show" | "电视剧" | "剧集" | "动漫"
    ) {
        "tv"
    } else {
        "movie"
    }
}

fn str_field(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn int_field(v: &Value, k: &str) -> Option<i64> {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn join_value_strings(vals: &[Option<&Value>]) -> String {
    let mut parts = Vec::new();
    for val in vals.iter().flatten() {
        match val {
            Value::String(s) if !s.trim().is_empty() => parts.push(s.trim().to_string()),
            Value::Array(arr) => {
                for elem in arr {
                    if let Some(s) = elem.as_str() {
                        if !s.trim().is_empty() {
                            parts.push(s.trim().to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    parts.join(" ")
}

fn non_empty(vals: &[&str]) -> String {
    for v in vals {
        if !v.trim().is_empty() {
            return v.trim().to_string();
        }
    }
    String::new()
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| truncate_chars(l, 60))
        .unwrap_or_default()
}

fn truncate_chars(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    let count = trimmed.chars().count();
    if count <= max {
        trimmed.to_string()
    } else {
        let prefix: String = trimmed.chars().take(max).collect();
        format!("{}…", prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tg_html_and_meta_parse() {
        let sample = r#"
        <div class="tgme_widget_message_wrap">
          <div class="tgme_widget_message" data-post="Lsp115/1024">
            <div class="tgme_widget_message_text">名称：破地狱 (2024) 4K REMUX 杜比视界 [tmdbid-1182047]<br/>体积：52.4GB<br/><a href="https://115.com/s/sw778899aabb?password=x9Y8">点此转存115</a></div>
            <time datetime="2026-11-20T12:30:00+00:00"></time>
          </div>
        </div>"#;
        let (posts, max_id) = parse_tg_channel_web_html("@Lsp115", sample, 1000);
        assert_eq!(max_id, 1024);
        let items = convert_tg_posts_to_resources(&posts, false, None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].share_code, "sw778899aabb");
        assert_eq!(items[0].receive_code, "x9Y8");
        assert_eq!(items[0].tmdb_id, "1182047");

        let meta_html = r#"<html><head><meta property="og:title" content="Lsp115"></head><body><img class="tgme_page_photo_image" src="https://cdn4.telesco.pe/file/a.jpg"><div class="tgme_page_title"><span>Lsp115 蓝光频道</span></div></body></html>"#;
        let (title, photo) = parse_tg_channel_meta_html(meta_html);
        assert_eq!(title, "Lsp115 蓝光频道");
        assert_eq!(photo, "https://cdn4.telesco.pe/file/a.jpg");

        let bogus_landing = r#"<html><head><meta property="og:title" content="Telegram – a new era of messaging"></head><body>Telegram landing</body></html>"#;
        let (bad_title, _) = parse_tg_channel_meta_html(bogus_landing);
        assert!(bad_title.is_empty());
    }

    #[test]
    fn test_relay_me_and_resources_parse() {
        let me_raw = r#"{"connected":true,"label":"简单不凡","weekly_points_total":400.0,"weekly_points_used":0.0,"weekly_points_remaining":400.0}"#;
        let (label, rem) = parse_relay_me_json(me_raw).unwrap();
        assert_eq!(label, "简单不凡");
        assert_eq!(rem, 400);

        let res_raw = r#"{
            "status": "ok",
            "authoritative_snapshot": true,
            "items": [
                {
                    "resource_id": "movie:hdhive://174aa1d4b3ce4b47be730d4ebc82c2d1",
                    "title": "肖申克的救赎 4K REMUX",
                    "pan_type": "115",
                    "points": 8.0,
                    "is_unlocked": true,
                    "remark": "杜比视界 国英双语",
                    "share_size": "61.32 GB",
                    "real_sharer": "Tester",
                    "video_resolution": ["4K"],
                    "subtitle_language": ["简中"],
                    "movie_attributes": {"resolution": "4K", "remux": "REMUX", "color": "DV&HDR", "audio": "TRUEHD"}
                },
                {
                    "resource_id": "movie:hdhive://222bb",
                    "title": "肖申克的救赎 WEB-DL",
                    "pan_type": "115",
                    "points": 5.0,
                    "is_unlocked": false,
                    "share_size": "18.5 GB"
                }
            ]
        }"#;
        let items = parse_relay_resources_json(res_raw, "278", "movie", None).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].slug, "movie:hdhive://174aa1d4b3ce4b47be730d4ebc82c2d1");
        assert_eq!(items[0].clean_slug, "174aa1d4b3ce4b47be730d4ebc82c2d1");
        assert!(items[0].is_unlocked);
        assert_eq!(items[0].unlock_points, 0);
        assert_eq!(items[0].movie_attributes.color, "DV&HDR");
        assert!(!items[1].is_unlocked);
        assert_eq!(items[1].unlock_points, 5);
    }

    #[test]
    fn test_rss_and_hdhive_parse() {
        let rss = r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel><item><title><![CDATA[【剧集】藏锋 (2026) S01E01-E16 2160p WEB-DL HDR]]></title><link>https://115.com/s/sw5566778899?password=1234</link><guid>item-1</guid></item></channel></rss>"#;
        let items = parse_rss_xml(rss).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].pure_title, "藏锋");
        assert_eq!(items[0].media_type, "tv");
        assert_eq!(items[0].direct_links.len(), 1);

        let hd_raw = r#"{"success":true,"data":[{"slug":"abc123slug","title":"沙丘2 4K REMUX 杜比视界","remark":"2160P TrueHD 特效中字","share_size":"64.2GB","unlock_points":2,"is_unlocked":false}]}"#;
        let hd_items = parse_hdhive_resources_json(hd_raw, "693134", "movie", None).unwrap();
        assert_eq!(hd_items.len(), 1);
        assert_eq!(hd_items[0].slug, "hdhive://abc123slug");
        assert_eq!(hd_items[0].unlock_points, 2);

        assert_eq!(
            strip_hdhive_resource_prefix("relay://tv:hdhive://abc123"),
            "abc123"
        );
        assert_eq!(
            strip_hdhive_resource_prefix("relay://movie:https://115.com/s/sw123?password=abcd"),
            "https://115.com/s/sw123?password=abcd"
        );
        assert_eq!(
            strip_hdhive_resource_prefix("relay://hdhive://xyz789"),
            "xyz789"
        );
        assert_eq!(
            strip_hdhive_resource_prefix("movie:radar://r001"),
            "r001"
        );
    }
}

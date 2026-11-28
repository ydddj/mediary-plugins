use crate::db;
use crate::host::{self, HostClient, HostUserBrief};
use crate::models::{
    now_ts, today_ymd, Account115, AppConfig, PluginSettings, PolicyTemplate, ResolveDecision,
    ResolveRequest, UserPolicy,
};
use crate::p115::{self, P115Error, UploadCredentials};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const NEGATIVE_SENTINEL: &str = "__MEDINEXT_NEGATIVE__";
const PREFETCH_RENEW_THRESHOLD_SECS: i64 = 300;

static DEVICE_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn acquire_device_mutex(device_id: &str) -> Option<Arc<Mutex<()>>> {
    let key = device_id.trim();
    if key.is_empty() {
        return None;
    }
    let map = DEVICE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() > 2048 {
        guard.retain(|_, v| Arc::strong_count(v) > 1);
    }
    Some(
        guard
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone(),
    )
}

fn md5_short8(s: &str) -> String {
    let digest = md5::compute(s.as_bytes());
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

/// Builds `(exact_key, shared_key)` where:
/// - `exact_key` is `{username}:{pickcode}:{device_id}:{ua_md5}:N` (or `shared_key` when `device_id` is empty)
/// - `shared_key` is `{username}:{pickcode}:{ua_md5}:N` (device-less key bridging `/prefetch` <-> `/resolve` `device_id` differences)
fn build_link_cache_keys(
    username: &str,
    pickcode: &str,
    device_id: &str,
    ua: &str,
) -> (String, String) {
    let ua_hash = md5_short8(ua);
    let shared = format!("{username}:{pickcode}:{ua_hash}:N");
    let dev = device_id.trim();
    if dev.is_empty() {
        (shared.clone(), shared)
    } else {
        (format!("{username}:{pickcode}:{dev}:{ua_hash}:N"), shared)
    }
}

#[allow(clippy::too_many_arguments)]
fn put_cached_link_with_shared(
    conn: &Connection,
    exact_key: &str,
    shared_key: &str,
    pickcode: &str,
    provider: &str,
    mode_label: &str,
    uid: &str,
    url: &str,
    headers: &HashMap<String, String>,
    file_name: &str,
    item_id: &str,
    ttl_secs: u64,
) {
    db::put_cached_link(
        conn, exact_key, pickcode, provider, mode_label, uid, url, headers, file_name, item_id,
        ttl_secs,
    );
    if !shared_key.is_empty() && shared_key != exact_key {
        db::put_cached_link(
            conn, shared_key, pickcode, provider, mode_label, uid, url, headers, file_name,
            item_id, ttl_secs,
        );
    }
}

fn is_valid_http_url(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("http://") || t.starts_with("https://")
}

fn is_host_internal_probe(req: &ResolveRequest) -> bool {
    let ua_low = req.user_agent.trim().to_ascii_lowercase();
    if ua_low.contains("media-screenshot")
        || ua_low.contains("media-probe")
        || ua_low.contains("stream-probe")
        || ua_low.contains("ffprobe")
    {
        return true;
    }
    // Mediary internal tasks accessing `/api/302?pickcode=...` carry `Mediary/<ver>` UA and no viewer `user_id`
    req.user_id.trim().is_empty()
        && req.username.trim().is_empty()
        && ua_low.starts_with("mediary/")
}

fn is_daemon_process() -> bool {
    std::env::args().nth(1).as_deref() == Some("daemon")
}

/// Checks whether `key` is out of cooldown in `kv_store`, and updates its timestamp if allowed.
fn check_and_mark_kv_cooldown(conn: &Connection, key: &str, cooldown_secs: i64) -> bool {
    let now = now_ts();
    let prev: i64 = conn
        .query_row(
            "SELECT updated_at FROM kv_store WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(0);
    if cooldown_secs > 0 && now - prev < cooldown_secs {
        return false;
    }
    let _ = db::set_kv(conn, key, &now.to_string());
    true
}

fn send_host_notification_nonblocking(title: String, content: String, poster_url: String) {
    if is_daemon_process() {
        std::thread::spawn(move || {
            if let Some(h) = HostClient::from_env() {
                let _ = h.notify_with_image(&title, &content, Some(&poster_url));
            }
        });
    } else if let Some(h) = HostClient::from_env() {
        let _ = h.notify_with_image(&title, &content, Some(&poster_url));
    }
}

#[allow(clippy::too_many_arguments)]
fn maybe_notify_playback_started(
    conn: &Connection,
    settings: &PluginSettings,
    cfg: &AppConfig,
    uctx: &EffectiveUserContext,
    req: &ResolveRequest,
    display_name: &str,
    provider: &str,
    mode_label: &str,
    ua: &str,
    poster_url: &str,
    elapsed_ms: u64,
) {
    if req.prefetch
        || !settings.notify
        || !cfg.engine.notify_playback
        || uctx.username.eq_ignore_ascii_case("guest")
    {
        return;
    }
    let media_tag = if !req.item_id.trim().is_empty() {
        req.item_id.trim()
    } else {
        req.pick_code.trim()
    };
    let dedup_key = format!(
        "notify_play:{}:{}:{}",
        uctx.username.to_ascii_lowercase(),
        media_tag,
        req.device_id.trim()
    );
    if !check_and_mark_kv_cooldown(conn, &dedup_key, 1800) {
        return;
    }
    let title_media = if !display_name.trim().is_empty() {
        display_name.trim()
    } else if !req.item_id.trim().is_empty() {
        req.item_id.trim()
    } else {
        req.pick_code.trim()
    };
    let client_disp = if !req.client.trim().is_empty() {
        req.client.trim().to_string()
    } else {
        db::normalize_ua_label(ua)
    };
    let dev_disp = if !req.device_name.trim().is_empty() {
        format!(" ({})", req.device_name.trim())
    } else if !req.device_id.trim().is_empty() {
        format!(" ({})", req.device_id.trim())
    } else {
        String::new()
    };
    let ip_disp = if req.client_ip.trim().is_empty() {
        "-"
    } else {
        req.client_ip.trim()
    };
    let title = format!("▶️ 开始播放 · {title_media}");
    let content = format!(
        "用户：{}\n通道：{mode_label} · {provider}\n客户端：{client_disp}{dev_disp}\nIP：{ip_disp}\n调度耗时：{elapsed_ms} ms",
        uctx.username
    );
    send_host_notification_nonblocking(title, content, poster_url.to_string());
}

#[allow(clippy::too_many_arguments)]
fn maybe_notify_risk_intercept(
    conn: &Connection,
    settings: &PluginSettings,
    cfg: &AppConfig,
    username: &str,
    req: &ResolveRequest,
    ua: &str,
    mode_label: &str,
    reason: &str,
) {
    if req.prefetch || !settings.notify || !cfg.engine.notify_risk_intercept {
        return;
    }
    let dedup_key = format!(
        "notify_risk:{}:{}",
        username.to_ascii_lowercase(),
        mode_label
    );
    if !check_and_mark_kv_cooldown(conn, &dedup_key, 300) {
        return;
    }
    let target_media = if !req.file_name.trim().is_empty() {
        req.file_name.trim()
    } else if !req.item_id.trim().is_empty() {
        req.item_id.trim()
    } else {
        "-"
    };
    let ip_disp = if req.client_ip.trim().is_empty() {
        "-"
    } else {
        req.client_ip.trim()
    };
    let title = format!("🛡️ MediNext 风控拦截 {mode_label}");
    let content = format!(
        "用户：{username}\n媒体：{target_media}\n原因：{reason}\n客户端：{}\nIP：{ip_disp}",
        db::normalize_ua_label(ua)
    );
    send_host_notification_nonblocking(title, content, String::new());
}

/// Mediary 3.4.6 native `playback_hook` rejection contract:
/// Always returns `status: "deny"`. When `prompt_video_url` is a valid http/https URL,
/// attaches `fallback_video_url` so Mediary 302-redirects the player to the prompt video
/// while still treating the decision as a policy rejection (`PLAYBACK_REJECTED`).
fn deny_or_prompt_video(
    code: u16,
    reason: &str,
    prompt_video_url: &str,
    started: Instant,
) -> ResolveDecision {
    let fallback = if is_valid_http_url(prompt_video_url) {
        prompt_video_url.trim().to_string()
    } else {
        String::new()
    };
    let has_fallback = !fallback.is_empty();
    ResolveDecision {
        handled: true,
        status: "deny".to_string(),
        url: String::new(),
        fallback_video_url: fallback,
        ttl_seconds: None,
        status_code: code,
        headers: HashMap::new(),
        provider: if has_fallback {
            "RiskPrompt".to_string()
        } else {
            "RiskGuard".to_string()
        },
        mode_label: if has_fallback {
            "[风控提示流]".to_string()
        } else {
            "[风控拦截]".to_string()
        },
        cache_hit: false,
        elapsed_ms: started.elapsed().as_millis() as u64,
        reason: reason.to_string(),
    }
}

fn pass_decision(reason: &str, started: Instant) -> ResolveDecision {
    ResolveDecision {
        handled: false,
        status: "pass".to_string(),
        url: String::new(),
        fallback_video_url: String::new(),
        ttl_seconds: Some(30),
        status_code: 200,
        headers: HashMap::new(),
        provider: "MediaryHost".to_string(),
        mode_label: "[宿主直连]".to_string(),
        cache_hit: false,
        elapsed_ms: started.elapsed().as_millis() as u64,
        reason: reason.to_string(),
    }
}

/// Cached lookup of Mediary native `/media-service/users` (5-minute TTL in `kv_store`).
fn load_or_fetch_host_users(conn: &Connection, host: Option<&HostClient>) -> Vec<HostUserBrief> {
    let now = now_ts();
    let cached: Option<(String, i64)> = conn
        .query_row(
            "SELECT value, updated_at FROM kv_store WHERE key = 'host_users_cache'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();

    if let Some((raw, updated_at)) = &cached {
        if now - *updated_at <= 300 {
            if let Ok(list) = serde_json::from_str::<Vec<HostUserBrief>>(raw) {
                if !list.is_empty() {
                    return list;
                }
            }
        }
    }

    if let Some(h) = host {
        if let Ok(fresh) = h.list_users() {
            if let Ok(raw) = serde_json::to_string(&fresh) {
                let _ = db::set_kv(conn, "host_users_cache", &raw);
            }
            return fresh;
        }
    }

    cached
        .and_then(|(raw, _)| serde_json::from_str::<Vec<HostUserBrief>>(&raw).ok())
        .unwrap_or_default()
}

pub fn sync_active_sessions_now(conn: &Connection, host: Option<&HostClient>, min_interval_secs: i64) {
    let Some(h) = host else {
        return;
    };
    let now = now_ts();
    if min_interval_secs > 0 {
        let last_sync: i64 = conn
            .query_row(
                "SELECT updated_at FROM kv_store WHERE key = 'last_playbacks_sync'",
                [],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
            .unwrap_or(0);
        if now - last_sync < min_interval_secs {
            return;
        }
    }
    if let Ok(playbacks) = h.list_active_playbacks() {
        db::reconcile_host_playbacks(conn, &playbacks);
        let _ = db::set_kv(conn, "last_playbacks_sync", &now.to_string());
    }
}

/// Terminates a playback session both locally and inside Mediary 3.4.6 (`DELETE /media-service/sessions/:key`),
/// sending WebSocket `Stop` and revoking the session ticket for 12 hours when `media-service:write` is granted.
pub fn kick_session_everywhere(
    conn: &Connection,
    host: Option<&HostClient>,
    local_session_key: &str,
) -> (bool, String) {
    let info = db::get_active_session_info(conn, local_session_key);
    let _ = conn.execute(
        "DELETE FROM active_sessions WHERE session_key = ?1",
        params![local_session_key.trim()],
    );

    let mut host_kicked = false;
    if let Some((username, device_id, mut host_skey)) = info {
        db::invalidate_user_device_links(conn, &username, &device_id);
        if let Some(h) = host {
            if host_skey.is_empty() {
                if let Ok(live) = h.list_active_playbacks() {
                    if let Some(found) = live.into_iter().find(|p| {
                        p.username.eq_ignore_ascii_case(&username)
                            && (!device_id.is_empty() && p.device_id == device_id)
                    }) {
                        host_skey = found.session_key;
                    }
                }
            }
            if !host_skey.is_empty() && h.kick_session(&host_skey).is_ok() {
                host_kicked = true;
            }
        }
    }
    let msg = if host_kicked {
        "已通过宿主下发停止指令并吊销该会话票据 (12h)".to_string()
    } else {
        "已移除本地并发会话并失效直链缓存（若需强制断开播放器，请在插件配置授予 media-service:write）".to_string()
    };
    (host_kicked, msg)
}

/// Resolves user policy + template constraints, mapping Mediary numeric `user_id` -> `username` via `media-service:read` when needed.
struct EffectiveUserContext {
    username: String,
    user_id: String,
    is_admin: bool,
    is_banned: bool,
    expiry_date: String,
    route: String, // "direct" | "load_balance" | "own_cookie"
    forbid_master_fallback: bool,
    max_streams: u32,
    max_devices: u32,
    own_policy: Option<UserPolicy>,
}

fn resolve_user_context(
    conn: &mut Connection,
    cfg: &mut AppConfig,
    host: Option<&HostClient>,
    req: &ResolveRequest,
) -> EffectiveUserContext {
    let mut resolved_username = req.username.trim().to_string();
    let mut host_is_admin = req.is_admin;
    let mut host_is_disabled = false;

    let mut matched_idx: Option<usize> = None;
    if !resolved_username.is_empty() {
        matched_idx = cfg
            .users
            .iter()
            .position(|u| u.username.eq_ignore_ascii_case(&resolved_username));
    }
    if matched_idx.is_none() && !req.user_id.trim().is_empty() {
        matched_idx = cfg
            .users
            .iter()
            .position(|u| !u.emby_id.is_empty() && u.emby_id == req.user_id.trim());
    }

    // Map Mediary numeric `user_id` -> `username` via `media-service/users` cache (critical when migrated from NextEmby!)
    if matched_idx.is_none() && !req.user_id.trim().is_empty() {
        let host_users = load_or_fetch_host_users(conn, host);
        if let Some(hu) = host_users.iter().find(|u| u.user_id == req.user_id.trim()) {
            resolved_username = hu.username.clone();
            host_is_admin = host_is_admin || hu.is_admin;
            host_is_disabled = hu.is_disabled;
            if let Some(idx) = cfg
                .users
                .iter()
                .position(|u| u.username.eq_ignore_ascii_case(&hu.username))
            {
                matched_idx = Some(idx);
                let uid_to_save = hu.user_id.clone();
                let uname_to_match = hu.username.clone();
                if cfg.users[idx].emby_id != uid_to_save {
                    cfg.users[idx].emby_id = uid_to_save.clone();
                    let _ = db::edit_config(conn, false, |c| {
                        if let Some(u) = c
                            .users
                            .iter_mut()
                            .find(|x| x.username.eq_ignore_ascii_case(&uname_to_match))
                        {
                            u.emby_id = uid_to_save;
                            return Ok(true);
                        }
                        Ok(false)
                    });
                }
            }
        }
    }

    let default_tpl = cfg
        .templates
        .first()
        .cloned()
        .unwrap_or_else(PolicyTemplate::default);

    if let Some(idx) = matched_idx {
        let u = &cfg.users[idx];
        let tpl = cfg
            .templates
            .iter()
            .find(|t| t.name == u.template)
            .unwrap_or(&default_tpl);
        let is_admin = u.is_admin || host_is_admin;
        let has_own_cookie = !u.cookies.trim().is_empty() || !u.access_token.trim().is_empty();
        // Admin accounts are exempt from template-inherited `forbid_master_fallback` unless explicitly checked on the user itself.
        let forbid_master_fallback = u.forbid_master_fallback || (!is_admin && tpl.forbid_master_fallback);
        let mut route = if is_admin && !forbid_master_fallback && (u.route_mode.is_empty() || u.route_mode == "direct") {
            "direct".to_string()
        } else {
            match u.route_mode.as_str() {
                "direct" => "direct".to_string(),
                "own_cookie" if has_own_cookie => "own_cookie".to_string(),
                "load_balance" => "load_balance".to_string(),
                _ => {
                    if has_own_cookie && tpl.default_route == "own_cookie" {
                        "own_cookie".to_string()
                    } else if tpl.default_route == "direct" {
                        "direct".to_string()
                    } else {
                        "load_balance".to_string()
                    }
                }
            }
        };
        if forbid_master_fallback && route == "direct" {
            route = if has_own_cookie {
                "own_cookie".to_string()
            } else {
                "load_balance".to_string()
            };
        }
        let max_streams = if u.max_streams_override > 0 {
            u.max_streams_override
        } else {
            tpl.max_streams
        };
        let max_devices = if u.max_devices_override > 0 {
            u.max_devices_override
        } else {
            tpl.max_devices
        };
        return EffectiveUserContext {
            username: u.username.clone(),
            user_id: if !u.emby_id.is_empty() {
                u.emby_id.clone()
            } else {
                req.user_id.clone()
            },
            is_admin,
            is_banned: u.is_banned || u.is_disabled || host_is_disabled,
            expiry_date: u.expiry_date.clone(),
            route,
            forbid_master_fallback,
            max_streams,
            max_devices,
            own_policy: Some(u.clone()),
        };
    }

    if resolved_username.is_empty() {
        resolved_username = if !req.user_id.trim().is_empty() {
            format!("user_{}", req.user_id.trim())
        } else {
            "guest".to_string()
        };
    }
    let forbid_master_fallback = !host_is_admin && default_tpl.forbid_master_fallback;
    let route = if (host_is_admin || cfg.engine.guest_route == "direct") && !forbid_master_fallback {
        "direct".to_string()
    } else {
        "load_balance".to_string()
    };
    EffectiveUserContext {
        username: resolved_username,
        user_id: req.user_id.clone(),
        is_admin: host_is_admin,
        is_banned: host_is_disabled,
        expiry_date: String::new(),
        route,
        forbid_master_fallback,
        max_streams: default_tpl.max_streams,
        max_devices: default_tpl.max_devices,
        own_policy: None,
    }
}

/// Orders healthy LB accounts using physical-UID concurrency + per-client concurrency + call_count + affinity scoring.
pub fn select_lb_candidates(
    conn: &Connection,
    cfg: &AppConfig,
    username: &str,
    device_id: &str,
) -> Vec<Account115> {
    let now = now_ts();
    let states = db::load_lb_states(conn);
    let active_by_prov = db::active_sessions_by_provider(conn, device_id);
    let mut active_by_uid: HashMap<String, i64> = HashMap::new();
    for a in &cfg.lb_accounts {
        let uid = if !a.uid.trim().is_empty() {
            a.uid.trim().to_string()
        } else {
            p115::extract_uid_from_cookie(&a.cookies)
        };
        if !uid.is_empty() {
            let cnt = *active_by_prov.get(&a.name).unwrap_or(&0) as i64;
            *active_by_uid.entry(uid).or_insert(0) += cnt;
        }
    }

    let user_aff = if !username.trim().is_empty() {
        db::get_lb_affinity(conn, &format!("user:{}", username.trim()))
    } else {
        None
    };
    let dev_aff = if !device_id.trim().is_empty() {
        db::get_lb_affinity(conn, &format!("dev:{}", device_id.trim()))
    } else {
        None
    };

    let mut scored: Vec<(i64, Account115)> = cfg
        .lb_accounts
        .iter()
        .filter(|a| {
            a.enabled
                && a.valid
                && (!a.cookies.trim().is_empty() || !a.access_token.trim().is_empty())
                && states
                    .get(&a.name)
                    .is_none_or(|st| st.cooldown_until <= now)
        })
        .map(|a| {
            let uid = if !a.uid.trim().is_empty() {
                a.uid.trim().to_string()
            } else {
                p115::extract_uid_from_cookie(&a.cookies)
            };
            let prov_sess = *active_by_prov.get(&a.name).unwrap_or(&0) as i64;
            let uid_sess = if !uid.is_empty() {
                *active_by_uid.get(&uid).unwrap_or(&prov_sess)
            } else {
                prov_sess
            };
            let calls = states.get(&a.name).map(|s| s.call_count as i64).unwrap_or(0);
            let mut bias = 0i64;
            if dev_aff.as_deref() == Some(a.name.as_str()) {
                bias -= 80_000;
            } else if user_aff.as_deref() == Some(a.name.as_str()) {
                bias -= 50_000;
            }
            let score = uid_sess * 1_000_000 + prov_sess * 100_000 + calls + bias;
            (score, a.clone())
        })
        .collect();

    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.sort_order.cmp(&b.1.sort_order)));
    scored.into_iter().map(|(_, acc)| acc).collect()
}

pub fn resolve_playback(
    data_dir: &Path,
    settings: &PluginSettings,
    mut req: ResolveRequest,
) -> Result<ResolveDecision> {
    let started = Instant::now();

    // Immediately delegate Mediary's own background screenshot / stream-probe tasks (`Mediary/3.4.6 media-screenshot`)
    // back to the host without polluting playback history, active sessions, LB quota, or notifications!
    if is_host_internal_probe(&req) {
        return Ok(pass_decision(
            "宿主内部截图/流信息探测任务，直接透传回宿主处理",
            started,
        ));
    }

    let device_guard_arc = acquire_device_mutex(&req.device_id);
    let _device_lock = device_guard_arc
        .as_ref()
        .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()));

    let mut conn = db::open_db(data_dir)?;
    db::prune_stale_data(&conn);
    let mut cfg = db::load_config(&conn)?;
    let host = HostClient::from_env();

    let uctx = resolve_user_context(&mut conn, &mut cfg, host.as_ref(), &req);
    let ua = if req.user_agent.trim().is_empty() {
        "Mozilla/5.0 115Browser/27.0.5.7".to_string()
    } else {
        req.user_agent.trim().to_string()
    };

    // Fast branch for Detail-Page LB Prefetch (`POST /prefetch` or `{"prefetch": true}`)
    if req.prefetch {
        return resolve_detail_prefetch(
            &mut conn,
            &mut cfg,
            settings,
            host.as_ref(),
            &uctx,
            req,
            &ua,
            started,
        );
    }

    // 1. UA Risk Check
    let db_ua_banned = db::record_ua_and_check_db_ban(&conn, &ua, &uctx.username, &req.client_ip);
    let ua_lower = ua.to_ascii_lowercase();
    let keyword_banned = if cfg.risk.ua_mode == "whitelist" {
        !cfg.risk.allowed_ua_keywords.is_empty()
            && !cfg
                .risk
                .allowed_ua_keywords
                .iter()
                .any(|kw| !kw.trim().is_empty() && ua_lower.contains(&kw.to_ascii_lowercase()))
    } else {
        cfg.risk
            .banned_ua_keywords
            .iter()
            .any(|kw| !kw.trim().is_empty() && ua_lower.contains(&kw.to_ascii_lowercase()))
    };
    if db_ua_banned || keyword_banned {
        let reason = format!("UA 被风控拦截 ({})", db::normalize_ua_label(&ua));
        db::record_playback_history(
            &conn,
            &uctx.username,
            &req.item_id,
            &req.file_name,
            "RiskGuard",
            "[UA拦截]",
            &ua,
            &req.client_ip,
            req.file_size,
            false,
            "deny",
            &reason,
        );
        maybe_notify_risk_intercept(
            &conn,
            settings,
            &cfg,
            &uctx.username,
            &req,
            &ua,
            "[UA拦截]",
            &reason,
        );
        return Ok(deny_or_prompt_video(
            403,
            &reason,
            &cfg.engine.banned_video_url,
            started,
        ));
    }

    // 2. Account Ban & Expiry Check
    if uctx.is_banned {
        let reason = "该观影账号已被封禁或停用";
        db::record_playback_history(
            &conn,
            &uctx.username,
            &req.item_id,
            &req.file_name,
            "RiskGuard",
            "[账号封禁]",
            &ua,
            &req.client_ip,
            req.file_size,
            false,
            "deny",
            reason,
        );
        maybe_notify_risk_intercept(
            &conn,
            settings,
            &cfg,
            &uctx.username,
            &req,
            &ua,
            "[账号封禁]",
            reason,
        );
        return Ok(deny_or_prompt_video(
            403,
            reason,
            &cfg.engine.banned_video_url,
            started,
        ));
    }
    if !uctx.is_admin
        && !uctx.expiry_date.trim().is_empty()
        && uctx.expiry_date.trim() < today_ymd().as_str()
    {
        let reason = format!("账号已于 {} 到期", uctx.expiry_date.trim());
        db::record_playback_history(
            &conn,
            &uctx.username,
            &req.item_id,
            &req.file_name,
            "RiskGuard",
            "[账号到期]",
            &ua,
            &req.client_ip,
            req.file_size,
            false,
            "deny",
            &reason,
        );
        maybe_notify_risk_intercept(
            &conn,
            settings,
            &cfg,
            &uctx.username,
            &req,
            &ua,
            "[账号到期]",
            &reason,
        );
        return Ok(deny_or_prompt_video(
            403,
            &reason,
            &cfg.engine.expired_video_url,
            started,
        ));
    }

    // 3. IP Window, Device Limit & Concurrent Stream Limit (non-admin)
    if !uctx.is_admin {
        if cfg.risk.ip_lock_enabled {
            if let Err(reason) = db::check_and_touch_ip_window(
                &conn,
                &uctx.username,
                &req.client_ip,
                cfg.risk.ip_lock_window_mins,
                cfg.risk.ip_lock_max_ips,
            ) {
                db::record_playback_history(
                    &conn,
                    &uctx.username,
                    &req.item_id,
                    &req.file_name,
                    "RiskGuard",
                    "[异地IP锁]",
                    &ua,
                    &req.client_ip,
                    req.file_size,
                    false,
                    "deny",
                    &reason,
                );
                maybe_notify_risk_intercept(
                    &conn,
                    settings,
                    &cfg,
                    &uctx.username,
                    &req,
                    &ua,
                    "[异地IP锁]",
                    &reason,
                );
                return Ok(deny_or_prompt_video(
                    403,
                    &reason,
                    &cfg.engine.banned_video_url,
                    started,
                ));
            }
        }
        let eff_max_dev = if cfg.risk.enforce_device_limit {
            uctx.max_devices
        } else {
            0
        };
        if let Err(reason) = db::check_and_touch_device(
            &conn,
            &uctx.username,
            &req.device_id,
            &req.device_name,
            &req.client,
            &req.client_ip,
            eff_max_dev,
        ) {
            db::record_playback_history(
                &conn,
                &uctx.username,
                &req.item_id,
                &req.file_name,
                "RiskGuard",
                "[设备限制]",
                &ua,
                &req.client_ip,
                req.file_size,
                false,
                "deny",
                &reason,
            );
            maybe_notify_risk_intercept(
                &conn,
                settings,
                &cfg,
                &uctx.username,
                &req,
                &ua,
                "[设备限制]",
                &reason,
            );
            return Ok(deny_or_prompt_video(
                403,
                &reason,
                &cfg.engine.banned_video_url,
                started,
            ));
        }
        if uctx.max_streams > 0 {
            sync_active_sessions_now(&conn, host.as_ref(), 8);
            let other_streams =
                db::count_other_active_streams_for_user(&conn, &uctx.username, &req.device_id);
            if other_streams >= uctx.max_streams {
                let reason = format!(
                    "并发播放设备数超限 ({}/{})",
                    other_streams + 1,
                    uctx.max_streams
                );
                db::record_playback_history(
                    &conn,
                    &uctx.username,
                    &req.item_id,
                    &req.file_name,
                    "RiskGuard",
                    "[并发超限]",
                    &ua,
                    &req.client_ip,
                    req.file_size,
                    false,
                    "deny",
                    &reason,
                );
                maybe_notify_risk_intercept(
                    &conn,
                    settings,
                    &cfg,
                    &uctx.username,
                    &req,
                    &ua,
                    "[并发超限]",
                    &reason,
                );
                return Ok(deny_or_prompt_video(
                    429,
                    &reason,
                    &cfg.engine.concurrency_video_url,
                    started,
                ));
            }
        }
    } else {
        let _ = db::check_and_touch_device(
            &conn,
            &uctx.username,
            &req.device_id,
            &req.device_name,
            &req.client,
            &req.client_ip,
            0,
        );
    }

    // 4. Resolve pick_code & metadata from local SQLite cache + Mediary `/plugin/library/items/:id`
    let mut known_file_id = String::new();
    let mut resolved_item_meta: Option<host::ResolvedItemMedia> = None;
    if !req.pick_code.trim().is_empty() {
        if let Some((fid, sha, sz, nm)) = db::get_pickcode_meta(&conn, &req.pick_code) {
            known_file_id = fid;
            if req.sha1.len() != 40 && sha.len() == 40 {
                req.sha1 = sha;
            }
            if req.file_size == 0 && sz > 0 {
                req.file_size = sz;
            }
            if req.file_name.is_empty() && !nm.is_empty() {
                req.file_name = nm;
            }
        }
    }

    if !req.item_id.trim().is_empty()
        && (req.pick_code.trim().is_empty()
            || req.sha1.len() != 40
            || req.file_size == 0
            || cfg.engine.enable_next_ep_prewarm
            || cfg.engine.notify_playback)
    {
        if let Some(h) = host.as_ref() {
            if let Some(resolved) =
                h.resolve_item_media(&req.item_id, &req.media_source_id, &req.pick_code)
            {
                let pc_matches = req.pick_code.trim().is_empty()
                    || resolved.pick_code.eq_ignore_ascii_case(req.pick_code.trim());
                if req.pick_code.trim().is_empty() && !resolved.pick_code.is_empty() {
                    req.pick_code = resolved.pick_code.clone();
                }
                if pc_matches {
                    if known_file_id.is_empty() && !resolved.file_id.is_empty() {
                        known_file_id = resolved.file_id.clone();
                    }
                    if req.sha1.len() != 40 && resolved.sha1.len() == 40 {
                        req.sha1 = resolved.sha1.clone();
                    }
                    if req.file_size == 0 && resolved.file_size > 0 {
                        req.file_size = resolved.file_size;
                    }
                    if req.file_name.is_empty() && !resolved.item_name.is_empty() {
                        req.file_name = resolved.item_name.clone();
                    }
                    db::put_pickcode_meta(
                        &conn,
                        &req.pick_code,
                        &known_file_id,
                        &req.sha1,
                        req.file_size,
                        &req.file_name,
                    );
                }
                resolved_item_meta = Some(resolved);
            }
        }
    }
    let poster_url = resolved_item_meta
        .as_ref()
        .map(|m| m.poster_url.clone())
        .unwrap_or_default();

    let pickcode = req.pick_code.trim().to_string();
    if pickcode.is_empty() {
        return Ok(pass_decision("非 115 pickcode 媒体源，交由宿主原生处理", started));
    }

    // 5. Check 1800s Playback Link Cache (exact device key with automatic fallback to device-less shared key)
    let (cache_key, shared_cache_key) =
        build_link_cache_keys(&uctx.username, &pickcode, &req.device_id, &ua);

    if !req.bypass_cache {
        if let Some(hit) = db::get_and_refresh_cached_link_with_fallback(
            &conn,
            &cache_key,
            &shared_cache_key,
            cfg.engine.cache_ttl_secs,
        ) {
            if hit.url == NEGATIVE_SENTINEL {
                if !uctx.forbid_master_fallback {
                    return Ok(pass_decision("处于失败冷却期，交由宿主原生 /api/302 直出", started));
                }
            } else {
                let display_name = if !req.file_name.is_empty() {
                    req.file_name.clone()
                } else {
                    hit.file_name.clone()
                };
                db::touch_active_session(
                    &conn,
                    &req.session_id,
                    &uctx.username,
                    &uctx.user_id,
                    &req.device_id,
                    &req.device_name,
                    &req.client,
                    &ua,
                    &req.client_ip,
                    &req.item_id,
                    &display_name,
                    &hit.provider,
                );
                db::record_playback_history(
                    &conn,
                    &uctx.username,
                    &req.item_id,
                    &display_name,
                    &hit.provider,
                    &hit.mode_label,
                    &ua,
                    &req.client_ip,
                    req.file_size,
                    true,
                    "redirect",
                    "命中 1800s 直链缓存并续期",
                );
                let elapsed_ms = started.elapsed().as_millis() as u64;
                maybe_notify_playback_started(
                    &conn,
                    settings,
                    &cfg,
                    &uctx,
                    &req,
                    &display_name,
                    &hit.provider,
                    &hit.mode_label,
                    &ua,
                    &poster_url,
                    elapsed_ms,
                );
                if cfg.engine.enable_next_ep_prewarm {
                    if let Some((acc, is_lb)) = find_managed_account_by_provider(&cfg, &uctx, &hit.provider) {
                        trigger_next_episode_prewarm(
                            &conn,
                            data_dir,
                            &cfg,
                            resolved_item_meta.clone(),
                            &req,
                            &uctx.username,
                            &ua,
                            acc,
                            is_lb,
                            uctx.forbid_master_fallback,
                        );
                    }
                }
                return Ok(ResolveDecision {
                    handled: true,
                    status: "redirect".to_string(),
                    url: hit.url,
                    fallback_video_url: String::new(),
                    ttl_seconds: Some(60),
                    status_code: 302,
                    headers: hit.headers,
                    provider: hit.provider,
                    mode_label: hit.mode_label,
                    cache_hit: true,
                    elapsed_ms,
                    reason: "缓存命中".to_string(),
                });
            }
        }
    }

    // 6. Fast proxy & cooldown settings (keep hook latency well under Mediary's 1500ms budget)
    let global_proxy = if !cfg.engine.p115_proxy_url.trim().is_empty() {
        cfg.engine.p115_proxy_url.clone()
    } else {
        host::host_env_proxy_url()
    };
    let cooldown_ms = cfg.engine.p115_cooldown_ms.min(250);

    let source_acc = cfg
        .source_accounts
        .iter()
        .find(|a| a.enabled && (!a.cookies.trim().is_empty() || !a.access_token.trim().is_empty()))
        .cloned();

    // Optional Source Account Direct Signing (when no source_accounts configured, `status: "pass"` delegates directly to Mediary's own 115 master account!)
    let try_source_direct =
        |conn: &Connection, r_cur: &ResolveRequest, label: &str| -> Option<ResolveDecision> {
            let src = source_acc.as_ref()?;
            let eff_proxy = if !src.proxy_url.trim().is_empty() {
                src.proxy_url.as_str()
            } else {
                global_proxy.as_str()
            };
            let dl = p115::get_download_link(
                &src.cookies,
                &src.access_token,
                &src.uid,
                &pickcode,
                &ua,
                eff_proxy,
                cooldown_ms,
            )
            .ok()?;
            let fname = if !r_cur.file_name.is_empty() {
                r_cur.file_name.clone()
            } else {
                dl.file_name.clone()
            };
            let fsize = if r_cur.file_size > 0 {
                r_cur.file_size
            } else {
                dl.file_size
            };
            db::put_pickcode_meta(conn, &pickcode, "", &r_cur.sha1, fsize, &fname);
            db::put_cached_link(
                conn,
                &cache_key,
                &pickcode,
                &src.name,
                label,
                &src.uid,
                &dl.url,
                &dl.headers,
                &fname,
                &r_cur.item_id,
                cfg.engine.cache_ttl_secs,
            );
            Some(ResolveDecision {
                handled: true,
                status: "redirect".to_string(),
                url: dl.url,
                fallback_video_url: String::new(),
                ttl_seconds: Some(60),
                status_code: 302,
                headers: dl.headers,
                provider: src.name.clone(),
                mode_label: label.to_string(),
                cache_hit: false,
                elapsed_ms: started.elapsed().as_millis() as u64,
                reason: format!("由源盘账号 {} 直出", src.name),
            })
        };

    // 7. Direct Mode or Small-File Fast-Path (strictly disabled when `forbid_master_fallback` is active)
    let small_limit_bytes = cfg.engine.small_file_direct_mb.saturating_mul(1024 * 1024);
    let is_small_direct = !uctx.forbid_master_fallback
        && small_limit_bytes > 0
        && req.file_size > 0
        && req.file_size <= small_limit_bytes;

    if !uctx.forbid_master_fallback && (uctx.route == "direct" || is_small_direct) {
        let label = if is_small_direct && uctx.route != "direct" {
            "[小文件直连]"
        } else {
            "[源盘直连]"
        };
        if let Some(dec) = try_source_direct(&conn, &req, label) {
            db::touch_active_session(
                &conn,
                &req.session_id,
                &uctx.username,
                &uctx.user_id,
                &req.device_id,
                &req.device_name,
                &req.client,
                &ua,
                &req.client_ip,
                &req.item_id,
                &req.file_name,
                &dec.provider,
            );
            db::record_playback_history(
                &conn,
                &uctx.username,
                &req.item_id,
                &req.file_name,
                &dec.provider,
                &dec.mode_label,
                &ua,
                &req.client_ip,
                req.file_size,
                false,
                "redirect",
                &dec.reason,
            );
            maybe_notify_playback_started(
                &conn,
                settings,
                &cfg,
                &uctx,
                &req,
                &req.file_name,
                &dec.provider,
                &dec.mode_label,
                &ua,
                &poster_url,
                dec.elapsed_ms,
            );
            if cfg.engine.enable_next_ep_prewarm {
                if let Some(src) = source_acc.clone() {
                    trigger_next_episode_prewarm(
                        &conn,
                        data_dir,
                        &cfg,
                        resolved_item_meta.clone(),
                        &req,
                        &uctx.username,
                        &ua,
                        src,
                        false,
                        uctx.forbid_master_fallback,
                    );
                }
            }
            return Ok(dec);
        }
        // Delegate directly to Mediary's native `/api/302` master account!
        let pass_reason = if is_small_direct && uctx.route != "direct" {
            "小文件交由宿主内置 115 主号原生 302 直出"
        } else {
            "源盘直连交由宿主内置 115 主号原生 302 直出"
        };
        db::touch_active_session(
            &conn,
            &req.session_id,
            &uctx.username,
            &uctx.user_id,
            &req.device_id,
            &req.device_name,
            &req.client,
            &ua,
            &req.client_ip,
            &req.item_id,
            &req.file_name,
            "MediaryHost",
        );
        db::record_playback_history(
            &conn,
            &uctx.username,
            &req.item_id,
            &req.file_name,
            "MediaryHost",
            label,
            &ua,
            &req.client_ip,
            req.file_size,
            false,
            "pass",
            pass_reason,
        );
        let pdec = pass_decision(pass_reason, started);
        maybe_notify_playback_started(
            &conn,
            settings,
            &cfg,
            &uctx,
            &req,
            &req.file_name,
            "MediaryHost",
            label,
            &ua,
            &poster_url,
            pdec.elapsed_ms,
        );
        return Ok(pdec);
    }

    // 8. Ensure we have `sha1` & `file_size` before Rapid Transfer (LB or Own Cookie)
    if (req.sha1.len() != 40 || req.file_size == 0) && source_acc.is_some() {
        let src = source_acc.as_ref().unwrap();
        match p115::fetch_file_meta_by_pickcode(src, &pickcode, &global_proxy, cooldown_ms) {
            Ok(meta) => {
                if known_file_id.is_empty() && !meta.file_id.is_empty() {
                    known_file_id = meta.file_id.clone();
                }
                if !meta.sha1.is_empty() {
                    req.sha1 = meta.sha1.clone();
                }
                if meta.size > 0 {
                    req.file_size = meta.size;
                }
                if req.file_name.is_empty() && !meta.name.is_empty() {
                    req.file_name = meta.name.clone();
                }
                db::put_pickcode_meta(
                    &conn,
                    &pickcode,
                    &known_file_id,
                    &req.sha1,
                    req.file_size,
                    &req.file_name,
                );
            }
            Err(P115Error::Risk405(msg)) => {
                if settings.notify {
                    if let Some(h) = host.as_ref() {
                        h.notify(
                            "⚠️ MediNext 源盘触发 405 限制",
                            &format!("源盘账号 {} 读取文件元数据时触发风控: {msg}", src.name),
                        );
                    }
                }
            }
            Err(_) => {}
        }
    }

    // Check non-admin daily transfer cap
    if !uctx.is_admin && cfg.engine.max_daily_transfers > 0 {
        let used = db::count_user_daily_transfers(&conn, &uctx.username);
        if used >= cfg.engine.max_daily_transfers {
            let reason = format!(
                "今日转存次数已达上限 ({used}/{})",
                cfg.engine.max_daily_transfers
            );
            return Ok(deny_or_prompt_video(429, &reason, "", started));
        }
    }

    // Build ordered list of target accounts to attempt
    let mut targets: Vec<(Account115, String, bool)> = Vec::new();
    if uctx.route == "own_cookie" {
        if let Some(up) = uctx.own_policy.as_ref() {
            if !up.cookies.trim().is_empty() || !up.access_token.trim().is_empty() {
                let uid = if !up.p115_uid.is_empty() {
                    up.p115_uid.clone()
                } else {
                    p115::extract_uid_from_cookie(&up.cookies)
                };
                targets.push((
                    Account115 {
                        name: format!("用户:{}", up.username),
                        role: "user".to_string(),
                        cookies: up.cookies.clone(),
                        access_token: up.access_token.clone(),
                        uid,
                        user_key: up.user_key.clone(),
                        cache_path: if up.cache_path.is_empty() {
                            "/EmbyCache".to_string()
                        } else {
                            up.cache_path.clone()
                        },
                        cache_cid: up.cache_cid.clone(),
                        enabled: true,
                        valid: true,
                        ..Default::default()
                    },
                    "[独立秒传]".to_string(),
                    false,
                ));
            }
        }
    } else {
        for lb in select_lb_candidates(&conn, &cfg, &uctx.username, &req.device_id) {
            targets.push((lb, "[负载均衡]".to_string(), true));
        }
    }

    let mut last_err = String::new();

    let src_master_uid = source_acc.as_ref().map(|src| {
        if !src.uid.is_empty() {
            src.uid.clone()
        } else {
            p115::extract_uid_from_cookie(&src.cookies)
        }
    }).unwrap_or_default();

    for (target_acc, mode_label, is_lb) in targets {
        let target_uid = if !target_acc.uid.is_empty() {
            target_acc.uid.clone()
        } else {
            p115::extract_uid_from_cookie(&target_acc.cookies)
        };
        if uctx.forbid_master_fallback
            && is_lb
            && !src_master_uid.is_empty()
            && !target_uid.is_empty()
            && target_uid == src_master_uid
        {
            continue;
        }
        let target_proxy = if !target_acc.proxy_url.trim().is_empty() {
            target_acc.proxy_url.as_str()
        } else {
            global_proxy.as_str()
        };

        // Same-UID fast path (disabled when `forbid_master_fallback` forbids master account streaming)
        if !uctx.forbid_master_fallback && !src_master_uid.is_empty() && !target_uid.is_empty() && target_uid == src_master_uid {
            if let Ok(dl) = p115::get_download_link(
                &target_acc.cookies,
                &target_acc.access_token,
                &target_uid,
                &pickcode,
                &ua,
                target_proxy,
                cooldown_ms,
            ) {
                if is_lb {
                    db::record_lb_selected(
                        &conn,
                        &target_acc.name,
                        &uctx.username,
                        &req.device_id,
                    );
                }
                return finish_success(
                    &conn,
                    data_dir,
                    settings,
                    &cfg,
                    resolved_item_meta.clone(),
                    &poster_url,
                    &cache_key,
                    &pickcode,
                    &uctx,
                    &req,
                    &ua,
                    &target_acc,
                    is_lb,
                    &target_uid,
                    &mode_label,
                    dl,
                    cfg.engine.cache_ttl_secs,
                    started,
                    "同 UID 免转存直出",
                );
            }
        }

        // Step A: Try existing SHA1 pool hit on `target_uid`
        if req.sha1.len() == 40 && !target_uid.is_empty() {
            if let Some(existing_pc) = db::get_sha1_target(&conn, &req.sha1, &target_uid) {
                match p115::get_download_link(
                    &target_acc.cookies,
                    &target_acc.access_token,
                    &target_uid,
                    &existing_pc,
                    &ua,
                    target_proxy,
                    cooldown_ms,
                ) {
                    Ok(dl) => {
                        if is_lb {
                            db::record_lb_selected(
                                &conn,
                                &target_acc.name,
                                &uctx.username,
                                &req.device_id,
                            );
                        }
                        return finish_success(
                            &conn,
                            data_dir,
                            settings,
                            &cfg,
                            resolved_item_meta.clone(),
                            &poster_url,
                            &cache_key,
                            &pickcode,
                            &uctx,
                            &req,
                            &ua,
                            &target_acc,
                            is_lb,
                            &target_uid,
                            "[秒传池复用]",
                            dl,
                            cfg.engine.cache_ttl_secs,
                            started,
                            "命中同账号历史秒传池",
                        );
                    }
                    Err(P115Error::Risk405(msg)) => {
                        handle_405_cooldown(
                            &conn,
                            host.as_ref(),
                            settings,
                            &target_acc.name,
                            is_lb,
                            cfg.engine.risk_405_cooldown_secs,
                            &msg,
                        );
                        last_err = msg;
                        continue;
                    }
                    Err(_) => {
                        db::invalidate_sha1_target(&conn, &req.sha1, &target_uid);
                    }
                }
            }
        }

        // Step B: Execute Rapid Transfer (`initupload`) into target account's cache folder
        if req.sha1.len() != 40 || req.file_size == 0 {
            last_err = "缺少媒体文件的 40 位 SHA1 或大小（请确保条目已由宿主刮削入库或配置可选源盘主号）".to_string();
            break;
        }

        let folder_cid = match resolve_or_cache_folder_cid(
            &mut conn,
            &target_acc,
            &target_uid,
            target_proxy,
            cooldown_ms,
        ) {
            Ok(c) => c,
            Err(P115Error::Risk405(msg)) => {
                handle_405_cooldown(
                    &conn,
                    host.as_ref(),
                    settings,
                    &target_acc.name,
                    is_lb,
                    cfg.engine.risk_405_cooldown_secs,
                    &msg,
                );
                last_err = msg;
                continue;
            }
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };

        let cached_creds =
            get_or_fetch_upload_creds(&conn, &target_acc, &target_uid, target_proxy, cooldown_ms);
        let file_title = if req.file_name.trim().is_empty() {
            format!("{pickcode}.mkv")
        } else {
            req.file_name.clone()
        };

        let rt_res = p115::rapid_transfer(
            &target_acc.cookies,
            &target_acc.access_token,
            cached_creds.as_ref(),
            &folder_cid,
            &file_title,
            req.file_size,
            &req.sha1,
            target_proxy,
            cooldown_ms,
            |sign_check| {
                // Primary: Mediary 3.4.6 native `POST /api/plugin/open115/range-sha1` (zero plugin master Cookie needed!)
                if let Some(h) = host.as_ref() {
                    if let Some(sha) =
                        h.try_host_range_sha1(&pickcode, &known_file_id, &req.sha1, sign_check)
                    {
                        return Ok(sha);
                    }
                }
                // Secondary fallback: optional plugin-local `source_accounts`
                let src = source_acc.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("宿主 open115/range-sha1 未就绪且未配置可选源盘主号")
                })?;
                let src_proxy = if !src.proxy_url.trim().is_empty() {
                    src.proxy_url.as_str()
                } else {
                    global_proxy.as_str()
                };
                let src_dl = p115::get_download_link(
                    &src.cookies,
                    &src.access_token,
                    &src.uid,
                    &pickcode,
                    "",
                    src_proxy,
                    cooldown_ms,
                )
                .map_err(|e| anyhow::anyhow!("获取源盘校验直链失败: {e}"))?;
                p115::fetch_range_sha1_from_url(&src_dl.url, "", sign_check, src_proxy)
            },
        );

        match rt_res {
            Ok(outcome) => {
                db::record_rapid_transfer(
                    &conn,
                    &req.sha1,
                    &target_uid,
                    &target_acc.name,
                    &uctx.username,
                    &outcome.pick_code,
                    &outcome.file_id,
                    &file_title,
                    req.file_size,
                );
                bump_quota_usage(
                    &mut conn,
                    &target_acc.name,
                    is_lb,
                    &uctx.username,
                    req.file_size,
                );

                match p115::get_download_link(
                    &target_acc.cookies,
                    &target_acc.access_token,
                    &target_uid,
                    &outcome.pick_code,
                    &ua,
                    target_proxy,
                    cooldown_ms,
                ) {
                    Ok(dl) => {
                        if is_lb {
                            db::record_lb_selected(
                                &conn,
                                &target_acc.name,
                                &uctx.username,
                                &req.device_id,
                            );
                        }
                        return finish_success(
                            &conn,
                            data_dir,
                            settings,
                            &cfg,
                            resolved_item_meta.clone(),
                            &poster_url,
                            &cache_key,
                            &pickcode,
                            &uctx,
                            &req,
                            &ua,
                            &target_acc,
                            is_lb,
                            &target_uid,
                            &mode_label,
                            dl,
                            cfg.engine.cache_ttl_secs,
                            started,
                            "秒传成功并签发直链",
                        );
                    }
                    Err(P115Error::Risk405(msg)) => {
                        handle_405_cooldown(
                            &conn,
                            host.as_ref(),
                            settings,
                            &target_acc.name,
                            is_lb,
                            cfg.engine.risk_405_cooldown_secs,
                            &msg,
                        );
                        last_err = msg;
                        continue;
                    }
                    Err(e) => {
                        last_err = e.to_string();
                        continue;
                    }
                }
            }
            Err(P115Error::Risk405(msg)) => {
                handle_405_cooldown(
                    &conn,
                    host.as_ref(),
                    settings,
                    &target_acc.name,
                    is_lb,
                    cfg.engine.risk_405_cooldown_secs,
                    &msg,
                );
                last_err = msg;
                continue;
            }
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        }
    }

    // 9. Fallback Handling (`fallback_on_fail` / `fallback_for_guest` — strictly blocked by `forbid_master_fallback`)
    let allow_fallback = !uctx.forbid_master_fallback
        && (cfg.engine.fallback_on_fail
            || (uctx.own_policy.is_none() && cfg.engine.fallback_for_guest));
    if allow_fallback {
        if let Some(dec) = try_source_direct(&conn, &req, "[回源兜底]") {
            db::touch_active_session(
                &conn,
                &req.session_id,
                &uctx.username,
                &uctx.user_id,
                &req.device_id,
                &req.device_name,
                &req.client,
                &ua,
                &req.client_ip,
                &req.item_id,
                &req.file_name,
                &dec.provider,
            );
            db::record_playback_history(
                &conn,
                &uctx.username,
                &req.item_id,
                &req.file_name,
                &dec.provider,
                &dec.mode_label,
                &ua,
                &req.client_ip,
                req.file_size,
                false,
                "redirect",
                &format!("转存未命中 ({last_err})，已自动回源直出"),
            );
            maybe_notify_playback_started(
                &conn,
                settings,
                &cfg,
                &uctx,
                &req,
                &req.file_name,
                &dec.provider,
                &dec.mode_label,
                &ua,
                &poster_url,
                dec.elapsed_ms,
            );
            return Ok(dec);
        }
        if cfg.engine.negative_cache_secs > 0 {
            db::put_cached_link(
                &conn,
                &cache_key,
                &pickcode,
                "MediaryHost",
                "[宿主兜底]",
                "",
                NEGATIVE_SENTINEL,
                &HashMap::new(),
                &req.file_name,
                &req.item_id,
                cfg.engine.negative_cache_secs,
            );
        }
        let detail = if last_err.is_empty() {
            "暂无可用负载号，交由宿主原生 /api/302 兜底".to_string()
        } else {
            format!("{last_err}；已交由宿主原生 /api/302 兜底")
        };
        db::touch_active_session(
            &conn,
            &req.session_id,
            &uctx.username,
            &uctx.user_id,
            &req.device_id,
            &req.device_name,
            &req.client,
            &ua,
            &req.client_ip,
            &req.item_id,
            &req.file_name,
            "MediaryHost",
        );
        db::record_playback_history(
            &conn,
            &uctx.username,
            &req.item_id,
            &req.file_name,
            "MediaryHost",
            "[宿主兜底]",
            &ua,
            &req.client_ip,
            req.file_size,
            false,
            "pass",
            &detail,
        );
        return Ok(pass_decision(&detail, started));
    }

    let (fail_mode, fail_reason) = if uctx.forbid_master_fallback {
        let msg = if last_err.is_empty() {
            "已启用「禁止回退源盘大号」策略，当前暂无可用负载小号".to_string()
        } else {
            format!("已启用「禁止回退源盘大号」策略，负载小号调度失败: {last_err}")
        };
        ("[禁回大号]", msg)
    } else {
        let msg = if last_err.is_empty() {
            "没有可用的 115 负载账号且已关闭失败回源".to_string()
        } else {
            format!("播放调度失败: {last_err}")
        };
        ("[调度失败]", msg)
    };
    db::record_playback_history(
        &conn,
        &uctx.username,
        &req.item_id,
        &req.file_name,
        "RiskGuard",
        fail_mode,
        &ua,
        &req.client_ip,
        req.file_size,
        false,
        "deny",
        &fail_reason,
    );
    if uctx.forbid_master_fallback {
        maybe_notify_risk_intercept(
            &conn,
            settings,
            &cfg,
            &uctx.username,
            &req,
            &ua,
            fail_mode,
            &fail_reason,
        );
    }
    Ok(deny_or_prompt_video(403, &fail_reason, "", started))
}

#[allow(clippy::too_many_arguments)]
fn finish_success(
    conn: &Connection,
    data_dir: &Path,
    settings: &PluginSettings,
    cfg: &AppConfig,
    resolved_item_meta: Option<host::ResolvedItemMedia>,
    poster_url: &str,
    cache_key: &str,
    pickcode: &str,
    uctx: &EffectiveUserContext,
    req: &ResolveRequest,
    ua: &str,
    target_acc: &Account115,
    is_lb: bool,
    uid: &str,
    mode_label: &str,
    dl: p115::DownloadLink115,
    ttl_secs: u64,
    started: Instant,
    reason: &str,
) -> Result<ResolveDecision> {
    let provider = target_acc.name.as_str();
    let fname = if !req.file_name.is_empty() {
        req.file_name.clone()
    } else {
        dl.file_name.clone()
    };
    db::put_cached_link(
        conn,
        cache_key,
        pickcode,
        provider,
        mode_label,
        uid,
        &dl.url,
        &dl.headers,
        &fname,
        &req.item_id,
        ttl_secs,
    );
    db::touch_active_session(
        conn,
        &req.session_id,
        &uctx.username,
        &uctx.user_id,
        &req.device_id,
        &req.device_name,
        &req.client,
        ua,
        &req.client_ip,
        &req.item_id,
        &fname,
        provider,
    );
    db::record_playback_history(
        conn,
        &uctx.username,
        &req.item_id,
        &fname,
        provider,
        mode_label,
        ua,
        &req.client_ip,
        if req.file_size > 0 {
            req.file_size
        } else {
            dl.file_size
        },
        false,
        "redirect",
        reason,
    );
    let elapsed_ms = started.elapsed().as_millis() as u64;
    maybe_notify_playback_started(
        conn,
        settings,
        cfg,
        uctx,
        req,
        &fname,
        provider,
        mode_label,
        ua,
        poster_url,
        elapsed_ms,
    );
    if cfg.engine.enable_next_ep_prewarm {
        trigger_next_episode_prewarm(
            conn,
            data_dir,
            cfg,
            resolved_item_meta,
            req,
            &uctx.username,
            ua,
            target_acc.clone(),
            is_lb,
            uctx.forbid_master_fallback,
        );
    }
    Ok(ResolveDecision {
        handled: true,
        status: "redirect".to_string(),
        url: dl.url,
        fallback_video_url: String::new(),
        ttl_seconds: Some(60),
        status_code: 302,
        headers: dl.headers,
        provider: provider.to_string(),
        mode_label: mode_label.to_string(),
        cache_hit: false,
        elapsed_ms,
        reason: reason.to_string(),
    })
}

fn find_managed_account_by_provider(
    cfg: &AppConfig,
    uctx: &EffectiveUserContext,
    provider_name: &str,
) -> Option<(Account115, bool)> {
    if let Some(lb) = cfg
        .lb_accounts
        .iter()
        .find(|a| a.enabled && a.name == provider_name)
    {
        return Some((lb.clone(), true));
    }
    if let Some(src) = cfg
        .source_accounts
        .iter()
        .find(|a| a.enabled && a.name == provider_name)
    {
        return Some((src.clone(), false));
    }
    if provider_name.starts_with("用户:") {
        if let Some(up) = uctx.own_policy.as_ref() {
            if !up.cookies.trim().is_empty() || !up.access_token.trim().is_empty() {
                let uid = if !up.p115_uid.is_empty() {
                    up.p115_uid.clone()
                } else {
                    p115::extract_uid_from_cookie(&up.cookies)
                };
                return Some((
                    Account115 {
                        name: format!("用户:{}", up.username),
                        role: "user".to_string(),
                        cookies: up.cookies.clone(),
                        access_token: up.access_token.clone(),
                        uid,
                        user_key: up.user_key.clone(),
                        cache_path: if up.cache_path.is_empty() {
                            "/EmbyCache".to_string()
                        } else {
                            up.cache_path.clone()
                        },
                        cache_cid: up.cache_cid.clone(),
                        enabled: true,
                        valid: true,
                        ..Default::default()
                    },
                    false,
                ));
            }
        }
    }
    None
}

/// Detail-Page LB Prefetch (`POST /prefetch` or `{"prefetch": true}`):
/// Pre-transfers Movie/Episode (or Series/Season S01E01) to the user's affinity LB account and
/// pre-signs the 1800s direct link without consuming active stream slots or triggering notifications.
#[allow(clippy::too_many_arguments)]
fn resolve_detail_prefetch(
    conn: &mut Connection,
    cfg: &mut AppConfig,
    settings: &PluginSettings,
    host: Option<&HostClient>,
    uctx: &EffectiveUserContext,
    mut req: ResolveRequest,
    ua: &str,
    started: Instant,
) -> Result<ResolveDecision> {
    if !cfg.engine.enable_detail_prefetch {
        return Ok(pass_decision("详情页小号预热已关闭", started));
    }
    if uctx.is_banned
        || (!uctx.is_admin
            && !uctx.expiry_date.trim().is_empty()
            && uctx.expiry_date.trim() < today_ymd().as_str())
    {
        return Ok(pass_decision("账号受限或到期，跳过预热", started));
    }
    let ua_lower = ua.to_ascii_lowercase();
    if cfg
        .risk
        .banned_ua_keywords
        .iter()
        .any(|kw| !kw.trim().is_empty() && ua_lower.contains(&kw.to_ascii_lowercase()))
    {
        return Ok(pass_decision("黑名单 UA 跳过预热", started));
    }
    if uctx.route == "direct"
        && !cfg
            .source_accounts
            .iter()
            .any(|a| a.enabled && (!a.cookies.trim().is_empty() || !a.access_token.trim().is_empty()))
    {
        return Ok(pass_decision("直连路由且未配插件源盘号，交由宿主原生处理", started));
    }

    let raw_tag = if !req.item_id.trim().is_empty() {
        req.item_id.trim().to_string()
    } else {
        req.pick_code.trim().to_string()
    };
    if raw_tag.is_empty() {
        return Ok(pass_decision("缺少预热目标 item_id 或 pick_code", started));
    }

    if !req.bypass_cache {
        let dedup_key = format!(
            "detail_pre:{}:{}:{}:{}:{}",
            uctx.username.to_ascii_lowercase(),
            req.item_id.trim(),
            req.media_source_id.trim(),
            req.pick_code.trim(),
            md5_short8(ua)
        );
        if !check_and_mark_kv_cooldown(conn, &dedup_key, 30) {
            return Ok(pass_decision("30s 内已完成同版本详情预热", started));
        }
    }

    let mut known_file_id = String::new();
    let has_explicit_source = !req.media_source_id.trim().is_empty() && !req.item_id.trim().is_empty();

    if has_explicit_source {
        // Multi-version switch via PlaybackInfo: always align exact version via `resolve_playable_for_detail`.
        if let Some(h) = host {
            if let Some(resolved) =
                h.resolve_playable_for_detail(&req.item_id, &req.media_source_id, &req.pick_code)
            {
                let switched_pc = !resolved.pick_code.is_empty()
                    && !resolved.pick_code.eq_ignore_ascii_case(req.pick_code.trim());
                req.item_id = resolved.item_id;
                if !resolved.pick_code.is_empty() {
                    req.pick_code = resolved.pick_code;
                }
                if switched_pc {
                    known_file_id = resolved.file_id;
                    req.sha1 = resolved.sha1;
                    req.file_size = resolved.file_size;
                    if !resolved.item_name.is_empty() {
                        req.file_name = resolved.item_name;
                    }
                } else {
                    if known_file_id.is_empty() {
                        known_file_id = resolved.file_id;
                    }
                    if req.sha1.len() != 40 && resolved.sha1.len() == 40 {
                        req.sha1 = resolved.sha1;
                    }
                    if req.file_size == 0 && resolved.file_size > 0 {
                        req.file_size = resolved.file_size;
                    }
                    if req.file_name.is_empty() && !resolved.item_name.is_empty() {
                        req.file_name = resolved.item_name;
                    }
                }
            }
        }
        if !req.pick_code.trim().is_empty() {
            if let Some((fid, sha, sz, nm)) = db::get_pickcode_meta(conn, &req.pick_code) {
                if known_file_id.is_empty() {
                    known_file_id = fid;
                }
                if req.sha1.len() != 40 && sha.len() == 40 {
                    req.sha1 = sha;
                }
                if req.file_size == 0 && sz > 0 {
                    req.file_size = sz;
                }
                if req.file_name.is_empty() && !nm.is_empty() {
                    req.file_name = nm;
                }
            }
            db::put_pickcode_meta(
                conn,
                &req.pick_code,
                &known_file_id,
                &req.sha1,
                req.file_size,
                &req.file_name,
            );
        }
    } else {
        if !req.pick_code.trim().is_empty() {
            if let Some((fid, sha, sz, nm)) = db::get_pickcode_meta(conn, &req.pick_code) {
                known_file_id = fid;
                if req.sha1.len() != 40 && sha.len() == 40 {
                    req.sha1 = sha;
                }
                if req.file_size == 0 && sz > 0 {
                    req.file_size = sz;
                }
                if req.file_name.is_empty() && !nm.is_empty() {
                    req.file_name = nm;
                }
            }
        }
        if (req.pick_code.trim().is_empty() || req.sha1.len() != 40 || req.file_size == 0)
            && !req.item_id.trim().is_empty()
        {
            if let Some(h) = host {
                if let Some(resolved) =
                    h.resolve_playable_for_detail(&req.item_id, "", &req.pick_code)
                {
                    let pc_matches = req.pick_code.trim().is_empty()
                        || resolved.pick_code.eq_ignore_ascii_case(req.pick_code.trim());
                    req.item_id = resolved.item_id;
                    if req.pick_code.trim().is_empty() {
                        req.pick_code = resolved.pick_code;
                    }
                    if pc_matches {
                        if known_file_id.is_empty() {
                            known_file_id = resolved.file_id;
                        }
                        if req.sha1.len() != 40 && resolved.sha1.len() == 40 {
                            req.sha1 = resolved.sha1;
                        }
                        if req.file_size == 0 && resolved.file_size > 0 {
                            req.file_size = resolved.file_size;
                        }
                        if req.file_name.is_empty() && !resolved.item_name.is_empty() {
                            req.file_name = resolved.item_name;
                        }
                        db::put_pickcode_meta(
                            conn,
                            &req.pick_code,
                            &known_file_id,
                            &req.sha1,
                            req.file_size,
                            &req.file_name,
                        );
                    }
                }
            }
        }
    }

    let pickcode = req.pick_code.trim().to_string();
    if pickcode.is_empty() {
        return Ok(pass_decision("未能解析出 115 pick_code，跳过预热", started));
    }

    let small_limit_bytes = cfg.engine.small_file_direct_mb.saturating_mul(1024 * 1024);
    if !uctx.forbid_master_fallback
        && small_limit_bytes > 0
        && req.file_size > 0
        && req.file_size <= small_limit_bytes
    {
        return Ok(pass_decision("小文件走直出通道，跳过小号预热", started));
    }

    let (cache_key, shared_cache_key) =
        build_link_cache_keys(&uctx.username, &pickcode, &req.device_id, ua);

    let mut renewed_near_expiry = false;
    if !req.bypass_cache {
        if let Some(hit) = db::peek_cached_link_with_fallback(conn, &cache_key, &shared_cache_key) {
            if hit.remaining_secs >= PREFETCH_RENEW_THRESHOLD_SECS {
                if cache_key != shared_cache_key
                    && db::peek_cached_link(conn, &shared_cache_key).is_none()
                {
                    db::put_cached_link(
                        conn,
                        &shared_cache_key,
                        &pickcode,
                        &hit.provider,
                        &hit.mode_label,
                        &hit.uid,
                        &hit.url,
                        &hit.headers,
                        &hit.file_name,
                        &req.item_id,
                        hit.remaining_secs.max(60) as u64,
                    );
                }
                return Ok(ResolveDecision {
                    handled: true,
                    status: "redirect".to_string(),
                    url: hit.url,
                    fallback_video_url: String::new(),
                    ttl_seconds: Some(60),
                    status_code: 302,
                    headers: hit.headers,
                    provider: hit.provider,
                    mode_label: "[详情预热·缓存]".to_string(),
                    cache_hit: true,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    reason: format!(
                        "详情页预热命中现有 1800s 直链缓存（剩余 {}s）",
                        hit.remaining_secs
                    ),
                });
            }
            // Existing link has < 300s remaining (e.g. end-of-episode Progress prefetch):
            // proactively re-sign a fresh 1800s link and overwrite cache.
            renewed_near_expiry = true;
        }
    }

    let global_proxy = if !cfg.engine.p115_proxy_url.trim().is_empty() {
        cfg.engine.p115_proxy_url.clone()
    } else {
        host::host_env_proxy_url()
    };
    // Keep per-step cooldown tight inside `/prefetch` so cold rapid-transfer + downurl finishes
    // comfortably within Mediary 3.4.8's independent 4s prefetch timeout.
    let cooldown_ms = cfg.engine.p115_cooldown_ms.min(120);
    let source_acc = cfg
        .source_accounts
        .iter()
        .find(|a| a.enabled && (!a.cookies.trim().is_empty() || !a.access_token.trim().is_empty()))
        .cloned();

    if uctx.route != "direct" && (req.sha1.len() != 40 || req.file_size == 0) && source_acc.is_some() {
        if let Ok(meta) = p115::fetch_file_meta_by_pickcode(
            source_acc.as_ref().unwrap(),
            &pickcode,
            &global_proxy,
            cooldown_ms,
        ) {
            if known_file_id.is_empty() && !meta.file_id.is_empty() {
                known_file_id = meta.file_id;
            }
            if !meta.sha1.is_empty() {
                req.sha1 = meta.sha1;
            }
            if meta.size > 0 {
                req.file_size = meta.size;
            }
            if req.file_name.is_empty() && !meta.name.is_empty() {
                req.file_name = meta.name;
            }
            db::put_pickcode_meta(
                conn,
                &pickcode,
                &known_file_id,
                &req.sha1,
                req.file_size,
                &req.file_name,
            );
        }
    }

    let src_master_uid = source_acc
        .as_ref()
        .map(|src| {
            if !src.uid.is_empty() {
                src.uid.clone()
            } else {
                p115::extract_uid_from_cookie(&src.cookies)
            }
        })
        .unwrap_or_default();

    let mut targets: Vec<(Account115, bool)> = Vec::new();
    if uctx.route == "direct" && !uctx.forbid_master_fallback {
        if let Some(src) = source_acc.clone() {
            targets.push((src, false));
        }
    } else if uctx.route == "own_cookie" {
        if let Some((acc, is_lb)) =
            find_managed_account_by_provider(cfg, uctx, &format!("用户:{}", uctx.username))
        {
            targets.push((acc, is_lb));
        }
    } else {
        for lb in select_lb_candidates(conn, cfg, &uctx.username, &req.device_id) {
            if uctx.forbid_master_fallback && !src_master_uid.is_empty() {
                let lb_uid = if !lb.uid.is_empty() {
                    lb.uid.clone()
                } else {
                    p115::extract_uid_from_cookie(&lb.cookies)
                };
                if !lb_uid.is_empty() && lb_uid == src_master_uid {
                    continue;
                }
            }
            targets.push((lb, true));
        }
    }

    for (idx, (target_acc, is_lb)) in targets.into_iter().enumerate() {
        if idx > 0 && started.elapsed() > Duration::from_millis(3400) {
            break;
        }
        if let Ok((dl, detail)) = prewarm_media_into_account(
            conn,
            host,
            settings,
            cfg,
            source_acc.as_ref(),
            &target_acc,
            is_lb,
            &uctx.username,
            &pickcode,
            &known_file_id,
            &req.sha1,
            req.file_size,
            &req.file_name,
            ua,
            &global_proxy,
            cooldown_ms,
        ) {
            let target_uid = if !target_acc.uid.is_empty() {
                target_acc.uid.clone()
            } else {
                p115::extract_uid_from_cookie(&target_acc.cookies)
            };
            let fname = if !req.file_name.is_empty() {
                req.file_name.clone()
            } else {
                dl.file_name.clone()
            };
            if is_lb {
                db::record_lb_selected(conn, &target_acc.name, &uctx.username, &req.device_id);
            }
            let mode_tag = if renewed_near_expiry {
                "[临期续签预热]"
            } else {
                "[详情预热]"
            };
            let final_detail = if renewed_near_expiry {
                format!("临期直链(<300s)已刷新续签: {detail}")
            } else {
                detail
            };
            put_cached_link_with_shared(
                conn,
                &cache_key,
                &shared_cache_key,
                &pickcode,
                &target_acc.name,
                mode_tag,
                &target_uid,
                &dl.url,
                &dl.headers,
                &fname,
                &req.item_id,
                cfg.engine.cache_ttl_secs,
            );
            db::record_playback_history(
                conn,
                &uctx.username,
                &req.item_id,
                &fname,
                &target_acc.name,
                mode_tag,
                ua,
                &req.client_ip,
                req.file_size,
                false,
                "prewarm",
                &final_detail,
            );
            return Ok(ResolveDecision {
                handled: true,
                status: "redirect".to_string(),
                url: dl.url,
                fallback_video_url: String::new(),
                ttl_seconds: Some(60),
                status_code: 302,
                headers: dl.headers,
                provider: target_acc.name,
                mode_label: mode_tag.to_string(),
                cache_hit: false,
                elapsed_ms: started.elapsed().as_millis() as u64,
                reason: final_detail,
            });
        }
    }

    Ok(pass_decision("详情页小号预热未命中可用账号", started))
}

/// Shared worker for both Detail-Page LB Prefetch and Next-Episode Background Prewarm:
/// Ensures the file is present in `target_acc`'s `sha1_pool` (via rapid_transfer if needed) and signs a 1800s direct link.
#[allow(clippy::too_many_arguments)]
fn prewarm_media_into_account(
    conn: &mut Connection,
    host: Option<&HostClient>,
    settings: &PluginSettings,
    cfg: &AppConfig,
    source_acc: Option<&Account115>,
    target_acc: &Account115,
    is_lb: bool,
    username: &str,
    src_pickcode: &str,
    known_file_id: &str,
    sha1: &str,
    file_size: u64,
    file_name: &str,
    ua: &str,
    global_proxy: &str,
    cooldown_ms: u64,
) -> std::result::Result<(p115::DownloadLink115, String), String> {
    let target_uid = if !target_acc.uid.is_empty() {
        target_acc.uid.clone()
    } else {
        p115::extract_uid_from_cookie(&target_acc.cookies)
    };
    let target_proxy = if !target_acc.proxy_url.trim().is_empty() {
        target_acc.proxy_url.as_str()
    } else {
        global_proxy
    };

    if let Some(src) = source_acc {
        let src_uid = if !src.uid.is_empty() {
            src.uid.clone()
        } else {
            p115::extract_uid_from_cookie(&src.cookies)
        };
        if !target_uid.is_empty() && target_uid == src_uid {
            let dl = p115::get_download_link(
                &target_acc.cookies,
                &target_acc.access_token,
                &target_uid,
                src_pickcode,
                ua,
                target_proxy,
                cooldown_ms,
            )
            .map_err(|e| e.to_string())?;
            return Ok((dl, format!("源盘直连已预签 1800s 直链 ({})", target_acc.name)));
        }
    }

    if sha1.len() == 40 && !target_uid.is_empty() {
        if let Some(existing_pc) = db::get_sha1_target(conn, sha1, &target_uid) {
            match p115::get_download_link(
                &target_acc.cookies,
                &target_acc.access_token,
                &target_uid,
                &existing_pc,
                ua,
                target_proxy,
                cooldown_ms,
            ) {
                Ok(dl) => {
                    return Ok((dl, format!("秒传池已就绪并预签直链 ({})", target_acc.name)));
                }
                Err(P115Error::Risk405(msg)) => {
                    handle_405_cooldown(
                        conn,
                        host,
                        settings,
                        &target_acc.name,
                        is_lb,
                        cfg.engine.risk_405_cooldown_secs,
                        &msg,
                    );
                    return Err(msg);
                }
                Err(_) => {
                    db::invalidate_sha1_target(conn, sha1, &target_uid);
                }
            }
        }
    }

    if sha1.len() != 40 || file_size == 0 {
        return Err("缺少 40 位 SHA1 或文件大小".to_string());
    }

    let folder_cid =
        resolve_or_cache_folder_cid(conn, target_acc, &target_uid, target_proxy, cooldown_ms)
            .map_err(|e| e.to_string())?;
    let cached_creds =
        get_or_fetch_upload_creds(conn, target_acc, &target_uid, target_proxy, cooldown_ms);
    let file_title = if file_name.trim().is_empty() {
        format!("{src_pickcode}.mkv")
    } else {
        file_name.to_string()
    };

    let outcome = p115::rapid_transfer(
        &target_acc.cookies,
        &target_acc.access_token,
        cached_creds.as_ref(),
        &folder_cid,
        &file_title,
        file_size,
        sha1,
        target_proxy,
        cooldown_ms,
        |sign_check| {
            if let Some(h) = host {
                if let Some(s) = h.try_host_range_sha1(src_pickcode, known_file_id, sha1, sign_check)
                {
                    return Ok(s);
                }
            }
            let src = source_acc.ok_or_else(|| {
                anyhow::anyhow!("宿主 open115/range-sha1 未就绪且未配置可选源盘主号")
            })?;
            let src_proxy = if !src.proxy_url.trim().is_empty() {
                src.proxy_url.as_str()
            } else {
                global_proxy
            };
            let src_dl = p115::get_download_link(
                &src.cookies,
                &src.access_token,
                &src.uid,
                src_pickcode,
                "",
                src_proxy,
                cooldown_ms,
            )
            .map_err(|e| anyhow::anyhow!("获取源盘校验直链失败: {e}"))?;
            p115::fetch_range_sha1_from_url(&src_dl.url, "", sign_check, src_proxy)
        },
    )
    .map_err(|e| {
        if let P115Error::Risk405(msg) = &e {
            handle_405_cooldown(
                conn,
                host,
                settings,
                &target_acc.name,
                is_lb,
                cfg.engine.risk_405_cooldown_secs,
                msg,
            );
        }
        e.to_string()
    })?;

    db::record_rapid_transfer(
        conn,
        sha1,
        &target_uid,
        &target_acc.name,
        username,
        &outcome.pick_code,
        &outcome.file_id,
        &file_title,
        file_size,
    );
    bump_quota_usage(conn, &target_acc.name, is_lb, username, file_size);

    let dl = p115::get_download_link(
        &target_acc.cookies,
        &target_acc.access_token,
        &target_uid,
        &outcome.pick_code,
        ua,
        target_proxy,
        cooldown_ms,
    )
    .map_err(|e| e.to_string())?;

    Ok((dl, format!("已预秒传至 {} 并缓存 1800s 直链", target_acc.name)))
}

/// Triggers asynchronous background rapid-transfer + 1800s direct-link pre-signing for the next episode
/// (`SxxE(yy+1)` or `S(xx+1)E01`) onto the exact same `target_acc` currently serving the user!
#[allow(clippy::too_many_arguments)]
fn trigger_next_episode_prewarm(
    conn: &Connection,
    data_dir: &Path,
    cfg: &AppConfig,
    cur_resolved: Option<host::ResolvedItemMedia>,
    req: &ResolveRequest,
    username: &str,
    ua: &str,
    target_acc: Account115,
    is_lb: bool,
    forbid_master_fallback: bool,
) {
    if req.item_id.trim().is_empty() && cur_resolved.is_none() {
        return;
    }
    let target_uid = if !target_acc.uid.is_empty() {
        target_acc.uid.clone()
    } else {
        p115::extract_uid_from_cookie(&target_acc.cookies)
    };
    if target_uid.is_empty() {
        return;
    }
    let item_tag = if !req.item_id.trim().is_empty() {
        req.item_id.trim().to_string()
    } else {
        req.pick_code.trim().to_string()
    };
    let dedup_key = format!(
        "prewarm_next:{}:{}:{}",
        target_uid,
        item_tag,
        md5_short8(ua)
    );
    if !check_and_mark_kv_cooldown(conn, &dedup_key, 300) {
        return;
    }

    let dir_owned: PathBuf = data_dir.to_path_buf();
    let cfg_owned = cfg.clone();
    let cur_item_id = req.item_id.trim().to_string();
    let cur_ms_id = req.media_source_id.trim().to_string();
    let uname = username.to_string();
    let dev_id = req.device_id.trim().to_string();
    let client_ip = req.client_ip.trim().to_string();
    let ua_owned = ua.to_string();

    let worker = move || {
        let Some(h) = HostClient::from_env() else {
            return;
        };
        let Some(cur_ep) =
            cur_resolved.or_else(|| h.resolve_item_media(&cur_item_id, &cur_ms_id, ""))
        else {
            return;
        };
        if !cur_ep.item_type.eq_ignore_ascii_case("episode") && cur_ep.episode <= 0 {
            return;
        }
        let Some(mut next_ep) = h.find_next_episode_media(&cur_ep) else {
            return;
        };
        if next_ep.pick_code.trim().is_empty() {
            return;
        }

        let Ok(mut bg_conn) = db::open_db(&dir_owned) else {
            return;
        };
        let (next_cache_key, next_shared_key) =
            build_link_cache_keys(&uname, next_ep.pick_code.trim(), &dev_id, &ua_owned);
        if db::has_valid_cached_link_with_fallback(
            &bg_conn,
            &next_cache_key,
            &next_shared_key,
            PREFETCH_RENEW_THRESHOLD_SECS,
        ) {
            return;
        }

        if let Some((fid, sha, sz, nm)) = db::get_pickcode_meta(&bg_conn, &next_ep.pick_code) {
            if next_ep.file_id.is_empty() && !fid.is_empty() {
                next_ep.file_id = fid;
            }
            if next_ep.sha1.len() != 40 && sha.len() == 40 {
                next_ep.sha1 = sha;
            }
            if next_ep.file_size == 0 && sz > 0 {
                next_ep.file_size = sz;
            }
            if next_ep.item_name.is_empty() && !nm.is_empty() {
                next_ep.item_name = nm;
            }
        }

        let source_acc = cfg_owned
            .source_accounts
            .iter()
            .find(|a| a.enabled && (!a.cookies.trim().is_empty() || !a.access_token.trim().is_empty()))
            .cloned();
        let is_same_source_uid = source_acc.as_ref().is_some_and(|src| {
            let su = if !src.uid.is_empty() {
                src.uid.clone()
            } else {
                p115::extract_uid_from_cookie(&src.cookies)
            };
            !target_uid.is_empty() && target_uid == su
        });

        let small_limit = cfg_owned.engine.small_file_direct_mb.saturating_mul(1024 * 1024);
        if !forbid_master_fallback
            && !is_same_source_uid
            && small_limit > 0
            && next_ep.file_size > 0
            && next_ep.file_size <= small_limit
        {
            return;
        }

        let global_proxy = if !cfg_owned.engine.p115_proxy_url.trim().is_empty() {
            cfg_owned.engine.p115_proxy_url.clone()
        } else {
            host::host_env_proxy_url()
        };
        let cooldown_ms = cfg_owned.engine.p115_cooldown_ms.min(250);

        if !is_same_source_uid && (next_ep.sha1.len() != 40 || next_ep.file_size == 0) && source_acc.is_some() {
            if let Ok(meta) = p115::fetch_file_meta_by_pickcode(
                source_acc.as_ref().unwrap(),
                &next_ep.pick_code,
                &global_proxy,
                cooldown_ms,
            ) {
                if next_ep.file_id.is_empty() && !meta.file_id.is_empty() {
                    next_ep.file_id = meta.file_id;
                }
                if !meta.sha1.is_empty() {
                    next_ep.sha1 = meta.sha1;
                }
                if meta.size > 0 {
                    next_ep.file_size = meta.size;
                }
                if next_ep.item_name.is_empty() && !meta.name.is_empty() {
                    next_ep.item_name = meta.name;
                }
                db::put_pickcode_meta(
                    &bg_conn,
                    &next_ep.pick_code,
                    &next_ep.file_id,
                    &next_ep.sha1,
                    next_ep.file_size,
                    &next_ep.item_name,
                );
            }
        }

        let dummy_settings = PluginSettings {
            enabled: true,
            auto_clean_quota: false,
            notify: false,
        };
        if let Ok((dl, _)) = prewarm_media_into_account(
            &mut bg_conn,
            Some(&h),
            &dummy_settings,
            &cfg_owned,
            source_acc.as_ref(),
            &target_acc,
            is_lb,
            &uname,
            &next_ep.pick_code,
            &next_ep.file_id,
            &next_ep.sha1,
            next_ep.file_size,
            &next_ep.item_name,
            &ua_owned,
            &global_proxy,
            cooldown_ms,
        ) {
            let fname = if !next_ep.item_name.is_empty() {
                next_ep.item_name.clone()
            } else {
                dl.file_name.clone()
            };
            put_cached_link_with_shared(
                &bg_conn,
                &next_cache_key,
                &next_shared_key,
                &next_ep.pick_code,
                &target_acc.name,
                "[下集预热]",
                &target_uid,
                &dl.url,
                &dl.headers,
                &fname,
                &next_ep.item_id,
                cfg_owned.engine.cache_ttl_secs,
            );
            db::record_playback_history(
                &bg_conn,
                &uname,
                &next_ep.item_id,
                &fname,
                &target_acc.name,
                "[下集预热]",
                &ua_owned,
                &client_ip,
                next_ep.file_size,
                false,
                "prewarm",
                &format!(
                    "播放 S{:02}E{:02} 时后台预秒传并缓存下一集 S{:02}E{:02} 至 {}",
                    cur_ep.season.max(1),
                    cur_ep.episode.max(1),
                    next_ep.season.max(1),
                    next_ep.episode.max(1),
                    target_acc.name
                ),
            );
        }
    };

    if is_daemon_process() {
        std::thread::spawn(worker);
    } else {
        worker();
    }
}

fn handle_405_cooldown(
    conn: &Connection,
    host: Option<&HostClient>,
    settings: &PluginSettings,
    account_name: &str,
    is_lb: bool,
    cooldown_secs: u64,
    msg: &str,
) {
    if is_lb {
        db::mark_lb_cooldown(conn, account_name, cooldown_secs.max(300), msg);
    }
    if settings.notify {
        if let Some(h) = host {
            h.notify(
                "⚠️ MediNext 115 账号风控熔断",
                &format!(
                    "账号 [{account_name}] 触发 405 风控 ({msg})，已自动摘除冷却 {} 分钟并切换备用号。",
                    cooldown_secs.max(300) / 60
                ),
            );
        }
    }
}

fn resolve_or_cache_folder_cid(
    conn: &mut Connection,
    acc: &Account115,
    uid: &str,
    proxy_url: &str,
    cooldown_ms: u64,
) -> std::result::Result<String, P115Error> {
    if !acc.cache_cid.trim().is_empty() && acc.cache_cid.trim() != "0" {
        return Ok(acc.cache_cid.trim().to_string());
    }
    let path = if acc.cache_path.trim().is_empty() {
        "/Next"
    } else {
        acc.cache_path.trim()
    };
    let kv_key = format!("folder_cid:{uid}:{path}");
    if let Some(cached) = db::get_kv(conn, &kv_key).filter(|s| !s.is_empty()) {
        return Ok(cached);
    }
    let cid = p115::ensure_folder_cid(&acc.cookies, &acc.access_token, path, proxy_url, cooldown_ms)?;
    let _ = db::set_kv(conn, &kv_key, &cid);
    Ok(cid)
}

fn get_or_fetch_upload_creds(
    conn: &Connection,
    acc: &Account115,
    uid: &str,
    proxy_url: &str,
    cooldown_ms: u64,
) -> Option<UploadCredentials> {
    if !uid.is_empty() && !acc.user_key.is_empty() {
        return Some(UploadCredentials {
            user_id: uid.to_string(),
            user_key: acc.user_key.clone(),
        });
    }
    if !uid.is_empty() {
        if let Some(raw) = db::get_kv(conn, &format!("ucreds:{uid}")) {
            if let Ok(c) = serde_json::from_str::<UploadCredentials>(&raw) {
                if !c.user_key.is_empty() {
                    return Some(c);
                }
            }
        }
    }
    if !acc.cookies.trim().is_empty() {
        if let Ok((creds, _)) = p115::fetch_upload_credentials(&acc.cookies, proxy_url, cooldown_ms)
        {
            let _ = db::set_kv(
                conn,
                &format!("ucreds:{}", creds.user_id),
                &serde_json::to_string(&creds).unwrap_or_default(),
            );
            return Some(creds);
        }
    }
    None
}

/// Aligns `quota_usage_bytes` across multi-client LB entries sharing the same physical 115 `uid` + `cache_path`.
pub fn sync_same_uid_lb_quotas(cfg: &mut AppConfig) -> bool {
    let mut max_by_group: HashMap<(String, String), u64> = HashMap::new();
    for a in &cfg.lb_accounts {
        let uid = if !a.uid.trim().is_empty() {
            a.uid.trim().to_string()
        } else {
            p115::extract_uid_from_cookie(&a.cookies)
        };
        if uid.is_empty() {
            continue;
        }
        let path = if a.cache_path.trim().is_empty() {
            "/Next".to_string()
        } else {
            a.cache_path.trim().to_string()
        };
        let entry = max_by_group.entry((uid, path)).or_insert(0);
        *entry = (*entry).max(a.quota_usage_bytes);
    }
    let mut changed = false;
    for a in &mut cfg.lb_accounts {
        let uid = if !a.uid.trim().is_empty() {
            a.uid.trim().to_string()
        } else {
            p115::extract_uid_from_cookie(&a.cookies)
        };
        if uid.is_empty() {
            continue;
        }
        let path = if a.cache_path.trim().is_empty() {
            "/Next".to_string()
        } else {
            a.cache_path.trim().to_string()
        };
        if let Some(&mx) = max_by_group.get(&(uid, path)) {
            if a.quota_usage_bytes != mx {
                a.quota_usage_bytes = mx;
                changed = true;
            }
        }
    }
    changed
}

fn bump_quota_usage(
    conn: &mut Connection,
    account_name: &str,
    is_lb: bool,
    username: &str,
    added_bytes: u64,
) {
    if added_bytes == 0 {
        return;
    }
    let _ = db::edit_config(conn, false, |c| {
        if is_lb {
            sync_same_uid_lb_quotas(c);
            let target_group = c
                .lb_accounts
                .iter()
                .find(|a| a.name == account_name)
                .map(|a| {
                    let uid = if !a.uid.trim().is_empty() {
                        a.uid.trim().to_string()
                    } else {
                        p115::extract_uid_from_cookie(&a.cookies)
                    };
                    let path = if a.cache_path.trim().is_empty() {
                        "/Next".to_string()
                    } else {
                        a.cache_path.trim().to_string()
                    };
                    (uid, path)
                });
            if let Some((uid, path)) = target_group {
                let mut hit = false;
                for lb in &mut c.lb_accounts {
                    let lb_uid = if !lb.uid.trim().is_empty() {
                        lb.uid.trim().to_string()
                    } else {
                        p115::extract_uid_from_cookie(&lb.cookies)
                    };
                    let lb_path = if lb.cache_path.trim().is_empty() {
                        "/Next".to_string()
                    } else {
                        lb.cache_path.trim().to_string()
                    };
                    if lb.name == account_name || (!uid.is_empty() && lb_uid == uid && lb_path == path) {
                        lb.quota_usage_bytes = lb.quota_usage_bytes.saturating_add(added_bytes);
                        hit = true;
                    }
                }
                return Ok(hit);
            }
        } else if let Some(u) = c
            .users
            .iter_mut()
            .find(|x| x.username.eq_ignore_ascii_case(username))
        {
            u.quota_usage_bytes = u.quota_usage_bytes.saturating_add(added_bytes);
            return Ok(true);
        }
        Ok(false)
    });
}

// === Scheduled Maintenance: Quota Cleanup & Health Check ===

#[allow(clippy::too_many_arguments)]
pub fn clean_target_quota(
    conn: &mut Connection,
    target_label: &str,
    cookie: &str,
    access_token: &str,
    uid: &str,
    recycle_code: &str,
    proxy_url: &str,
    cooldown_ms: u64,
    current_usage: u64,
    target_limit_bytes: u64,
    force_all: bool,
) -> Result<(usize, u64, bool)> {
    let eff_uid = if !uid.trim().is_empty() {
        uid.trim().to_string()
    } else {
        p115::extract_uid_from_cookie(cookie)
    };
    ensure!(!eff_uid.is_empty(), "[{target_label}] 无法识别账号 UID");

    if !force_all && current_usage <= target_limit_bytes {
        return Ok((0, 0, false));
    }

    let logs = db::list_oldest_transfers_for_uid(conn, &eff_uid, 500);
    if logs.is_empty() {
        let rb_ok =
            p115::clean_recycle_bin(cookie, access_token, recycle_code, proxy_url, cooldown_ms)
                .is_ok();
        return Ok((0, 0, rb_ok));
    }

    let goal_bytes = if force_all {
        0
    } else {
        (target_limit_bytes as f64 * 0.80) as u64
    };

    let mut to_delete_fids = Vec::new();
    let mut to_delete_ids = Vec::new();
    let mut to_delete_sha1s = Vec::new();
    let mut freed_bytes = 0u64;
    let mut projected = current_usage;

    for item in logs {
        if !force_all && projected <= goal_bytes {
            break;
        }
        to_delete_ids.push(item.id);
        if !item.target_file_id.is_empty() {
            to_delete_fids.push(item.target_file_id);
        }
        if !item.sha1.is_empty() {
            to_delete_sha1s.push(item.sha1);
        }
        freed_bytes = freed_bytes.saturating_add(item.file_size);
        projected = projected.saturating_sub(item.file_size);
    }

    let mut deleted_files = 0usize;
    if !to_delete_fids.is_empty() {
        deleted_files =
            p115::delete_115_files(cookie, access_token, &to_delete_fids, proxy_url, cooldown_ms)?;
    }
    db::remove_transfer_logs(conn, &to_delete_ids, &eff_uid, &to_delete_sha1s);
    let rb_ok =
        p115::clean_recycle_bin(cookie, access_token, recycle_code, proxy_url, cooldown_ms).is_ok();

    Ok((deleted_files, freed_bytes, rb_ok))
}

pub fn run_scheduled_tick(conn: &mut Connection, settings: &PluginSettings) -> Result<Value> {
    db::prune_stale_data(conn);
    let host = HostClient::from_env();
    sync_active_sessions_now(conn, host.as_ref(), 0);

    let cfg = db::edit_config(conn, false, |c| Ok(sync_same_uid_lb_quotas(c)))
        .unwrap_or_else(|_| db::load_config(conn).unwrap_or_default());
    let valid_lb_names: Vec<String> = cfg.lb_accounts.iter().map(|a| a.name.clone()).collect();
    db::prune_orphaned_lb_states(conn, &valid_lb_names);
    let cooldown_ms = cfg.engine.p115_cooldown_ms.min(300);
    let today = today_ymd();
    let mut disabled_expired = 0usize;

    if cfg.risk.auto_disable_expired {
        if let Some(h) = host.as_ref() {
            let host_users = load_or_fetch_host_users(conn, Some(h));
            for u in &cfg.users {
                if u.is_admin
                    || u.is_disabled
                    || u.expiry_date.trim().is_empty()
                    || u.expiry_date.trim() >= today.as_str()
                {
                    continue;
                }
                let target_id = host_users
                    .iter()
                    .find(|hu| hu.username.eq_ignore_ascii_case(&u.username))
                    .map(|hu| hu.user_id.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(u.emby_id.as_str());
                if !target_id.is_empty() && h.try_set_user_enabled(target_id, false) {
                    disabled_expired += 1;
                    let uname = u.username.clone();
                    let _ = db::edit_config(conn, false, |c| {
                        if let Some(x) = c
                            .users
                            .iter_mut()
                            .find(|x| x.username.eq_ignore_ascii_case(&uname))
                        {
                            x.is_disabled = true;
                            return Ok(true);
                        }
                        Ok(false)
                    });
                }
            }
        }
    }

    let mut cleaned_accounts = 0usize;
    let mut total_deleted_files = 0usize;
    if settings.auto_clean_quota {
        let mut cleaned_uids: HashSet<String> = HashSet::new();
        for lb in &cfg.lb_accounts {
            if !lb.enabled || !lb.quota_enabled || lb.quota_gb == 0 {
                continue;
            }
            let eff_uid = if !lb.uid.trim().is_empty() {
                lb.uid.trim().to_string()
            } else {
                p115::extract_uid_from_cookie(&lb.cookies)
            };
            if !eff_uid.is_empty() && !cleaned_uids.insert(eff_uid.clone()) {
                continue;
            }
            let limit_bytes = lb.quota_gb.saturating_mul(1024 * 1024 * 1024);
            if lb.quota_usage_bytes <= limit_bytes {
                continue;
            }
            let proxy = if !lb.proxy_url.is_empty() {
                lb.proxy_url.as_str()
            } else {
                cfg.engine.p115_proxy_url.as_str()
            };
            if let Ok((del_cnt, freed, _)) = clean_target_quota(
                conn,
                &lb.name,
                &lb.cookies,
                &lb.access_token,
                &eff_uid,
                &lb.recycle_bin_code,
                proxy,
                cooldown_ms,
                lb.quota_usage_bytes,
                limit_bytes,
                false,
            ) {
                cleaned_accounts += 1;
                total_deleted_files += del_cnt;
                let name = lb.name.clone();
                let _ = db::edit_config(conn, false, |c| {
                    for a in &mut c.lb_accounts {
                        let a_uid = if !a.uid.trim().is_empty() {
                            a.uid.trim().to_string()
                        } else {
                            p115::extract_uid_from_cookie(&a.cookies)
                        };
                        if a.name == name || (!eff_uid.is_empty() && a_uid == eff_uid) {
                            a.quota_usage_bytes = a.quota_usage_bytes.saturating_sub(freed);
                        }
                    }
                    Ok(true)
                });
            }
        }

        for u in &cfg.users {
            if u.route_mode != "own_cookie" || !u.quota_enabled || u.quota_gb == 0 {
                continue;
            }
            let limit_bytes = u.quota_gb.saturating_mul(1024 * 1024 * 1024);
            if u.quota_usage_bytes <= limit_bytes {
                continue;
            }
            if let Ok((del_cnt, freed, _)) = clean_target_quota(
                conn,
                &u.username,
                &u.cookies,
                &u.access_token,
                &u.p115_uid,
                &u.recycle_bin_code,
                &cfg.engine.p115_proxy_url,
                cooldown_ms,
                u.quota_usage_bytes,
                limit_bytes,
                false,
            ) {
                cleaned_accounts += 1;
                total_deleted_files += del_cnt;
                let uname = u.username.clone();
                let _ = db::edit_config(conn, false, |c| {
                    if let Some(x) = c
                        .users
                        .iter_mut()
                        .find(|x| x.username.eq_ignore_ascii_case(&uname))
                    {
                        x.quota_usage_bytes = x.quota_usage_bytes.saturating_sub(freed);
                    }
                    Ok(true)
                });
            }
        }
    }

    let notice = format!(
        "巡检完成：清理超额账号 {cleaned_accounts} 个（释放 {total_deleted_files} 个缓存文件），同步到期停用 {disabled_expired} 人"
    );
    let _ = db::set_kv(conn, "last_tick_result", &notice);
    Ok(json!({
        "ok": true,
        "notice": notice,
        "cleaned_accounts": cleaned_accounts,
        "deleted_files": total_deleted_files,
        "disabled_expired": disabled_expired,
    }))
}

pub fn run_health_check(conn: &mut Connection, settings: &PluginSettings) -> Result<Value> {
    let cfg = db::load_config(conn)?;
    let host = HostClient::from_env();
    let cooldown_ms = cfg.engine.p115_cooldown_ms.min(300);
    let mut checked = 0usize;
    let mut healthy = 0usize;
    let mut invalid_names = Vec::new();
    let mut updates: Vec<(String, bool, String, String, String, bool)> = Vec::new();

    for (is_lb, acc) in cfg
        .source_accounts
        .iter()
        .map(|a| (false, a))
        .chain(cfg.lb_accounts.iter().map(|a| (true, a)))
    {
        if !acc.enabled || acc.cookies.trim().is_empty() {
            continue;
        }
        checked += 1;
        let proxy = if !acc.proxy_url.is_empty() {
            acc.proxy_url.as_str()
        } else {
            cfg.engine.p115_proxy_url.as_str()
        };
        match p115::fetch_upload_credentials(&acc.cookies, proxy, cooldown_ms) {
            Ok((creds, drive_label)) => {
                healthy += 1;
                let _ = db::set_kv(
                    conn,
                    &format!("ucreds:{}", creds.user_id),
                    &serde_json::to_string(&creds).unwrap_or_default(),
                );
                if is_lb {
                    db::mark_lb_cooldown(conn, &acc.name, 0, "");
                }
                updates.push((
                    acc.name.clone(),
                    is_lb,
                    creds.user_id,
                    creds.user_key,
                    drive_label,
                    true,
                ));
            }
            Err(P115Error::AuthInvalid(msg)) => {
                invalid_names.push(format!("{} ({msg})", acc.name));
                updates.push((
                    acc.name.clone(),
                    is_lb,
                    acc.uid.clone(),
                    String::new(),
                    String::new(),
                    false,
                ));
            }
            Err(P115Error::Risk405(msg)) => {
                if is_lb {
                    db::mark_lb_cooldown(
                        conn,
                        &acc.name,
                        cfg.engine.risk_405_cooldown_secs,
                        &msg,
                    );
                }
            }
            Err(_) => {}
        }
    }

    let _ = db::edit_config(conn, false, |c| {
        for (name, is_lb, uid, ukey, dinfo, valid) in updates {
            let list = if is_lb {
                &mut c.lb_accounts
            } else {
                &mut c.source_accounts
            };
            if let Some(a) = list.iter_mut().find(|x| x.name == name) {
                if !uid.is_empty() {
                    a.uid = uid;
                }
                if !ukey.is_empty() {
                    a.user_key = ukey;
                }
                if !dinfo.is_empty() {
                    a.drive_info = dinfo;
                }
                a.valid = valid;
            }
        }
        sync_same_uid_lb_quotas(c);
        Ok(true)
    });

    if !invalid_names.is_empty() && settings.notify {
        if let Some(h) = host.as_ref() {
            h.notify(
                "⚠️ MediNext 115 凭证失效提醒",
                &format!("以下账号需要重新扫码登录：{}", invalid_names.join("、")),
            );
        }
    }

    let notice = format!(
        "号池探活完成：检测 {checked} 个账号，正常 {healthy} 个，失效 {} 个",
        invalid_names.len()
    );
    let _ = db::set_kv(conn, "last_health_result", &notice);
    Ok(json!({
        "ok": true,
        "notice": notice,
        "checked": checked,
        "healthy": healthy,
        "invalid": invalid_names,
    }))
}

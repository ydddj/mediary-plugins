use crate::models::{now_datetime_str, now_ts, AppConfig, PluginSettings};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use url::Url;

pub fn open_db(data_dir: &Path) -> Result<Connection> {
    fs::create_dir_all(data_dir).context("创建插件数据目录失败")?;
    let db_path = data_dir.join("medinext.db");
    let conn = Connection::open(&db_path).context("打开 medinext.db 失败")?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;
         PRAGMA foreign_keys = ON;

         CREATE TABLE IF NOT EXISTS kv_store (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL,
             updated_at INTEGER NOT NULL
         );

         CREATE TABLE IF NOT EXISTS link_cache (
             cache_key TEXT PRIMARY KEY,
             pickcode TEXT NOT NULL DEFAULT '',
             provider TEXT NOT NULL DEFAULT '',
             mode_label TEXT NOT NULL DEFAULT '',
             uid TEXT NOT NULL DEFAULT '',
             url TEXT NOT NULL,
             headers_json TEXT NOT NULL DEFAULT '{}',
             file_name TEXT NOT NULL DEFAULT '',
             item_id TEXT NOT NULL DEFAULT '',
             created_at INTEGER NOT NULL,
             expires_at INTEGER NOT NULL,
             hit_count INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_link_cache_expires ON link_cache(expires_at);
         CREATE INDEX IF NOT EXISTS idx_link_cache_pickcode ON link_cache(pickcode);

         CREATE TABLE IF NOT EXISTS pickcode_meta (
             pickcode TEXT PRIMARY KEY,
             file_id TEXT NOT NULL DEFAULT '',
             sha1 TEXT NOT NULL DEFAULT '',
             size INTEGER NOT NULL DEFAULT 0,
             name TEXT NOT NULL DEFAULT '',
             updated_at INTEGER NOT NULL
         );

         CREATE TABLE IF NOT EXISTS sha1_pool (
             sha1 TEXT NOT NULL,
             target_uid TEXT NOT NULL,
             target_pickcode TEXT NOT NULL,
             target_file_id TEXT NOT NULL DEFAULT '',
             size INTEGER NOT NULL DEFAULT 0,
             created_at INTEGER NOT NULL,
             PRIMARY KEY (sha1, target_uid)
         );

         CREATE TABLE IF NOT EXISTS transfer_log (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             target_uid TEXT NOT NULL,
             account_name TEXT NOT NULL DEFAULT '',
             username TEXT NOT NULL DEFAULT '',
             sha1 TEXT NOT NULL DEFAULT '',
             target_pickcode TEXT NOT NULL DEFAULT '',
             target_file_id TEXT NOT NULL DEFAULT '',
             file_name TEXT NOT NULL DEFAULT '',
             file_size INTEGER NOT NULL DEFAULT 0,
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_transfer_log_uid_time ON transfer_log(target_uid, created_at);
         CREATE INDEX IF NOT EXISTS idx_transfer_log_user_time ON transfer_log(username, created_at);

         CREATE TABLE IF NOT EXISTS lb_state (
             account_name TEXT PRIMARY KEY,
             call_count INTEGER NOT NULL DEFAULT 0,
             cooldown_until INTEGER NOT NULL DEFAULT 0,
             last_error TEXT NOT NULL DEFAULT '',
             updated_at INTEGER NOT NULL DEFAULT 0
         );

         CREATE TABLE IF NOT EXISTS lb_affinity (
             scope_key TEXT PRIMARY KEY,
             account_name TEXT NOT NULL,
             updated_at INTEGER NOT NULL
         );

         CREATE TABLE IF NOT EXISTS active_sessions (
             session_key TEXT PRIMARY KEY,
             host_session_key TEXT NOT NULL DEFAULT '',
             username TEXT NOT NULL,
             user_id TEXT NOT NULL DEFAULT '',
             device_id TEXT NOT NULL DEFAULT '',
             device_name TEXT NOT NULL DEFAULT '',
             client TEXT NOT NULL DEFAULT '',
             ua TEXT NOT NULL DEFAULT '',
             ip TEXT NOT NULL DEFAULT '',
             item_id TEXT NOT NULL DEFAULT '',
             item_name TEXT NOT NULL DEFAULT '',
             provider TEXT NOT NULL DEFAULT '',
             started_at INTEGER NOT NULL,
             last_seen_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_sessions_last_seen ON active_sessions(last_seen_at);

         CREATE TABLE IF NOT EXISTS known_devices (
             username TEXT NOT NULL,
             device_id TEXT NOT NULL,
             device_name TEXT NOT NULL DEFAULT '',
             client TEXT NOT NULL DEFAULT '',
             last_ip TEXT NOT NULL DEFAULT '',
             first_seen_at INTEGER NOT NULL,
             last_seen_at INTEGER NOT NULL,
             is_banned INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (username, device_id)
         );

         CREATE TABLE IF NOT EXISTS ip_events (
             username TEXT NOT NULL,
             ip TEXT NOT NULL,
             last_seen_at INTEGER NOT NULL,
             PRIMARY KEY (username, ip)
         );

         CREATE TABLE IF NOT EXISTS ua_stats (
             ua_name TEXT PRIMARY KEY,
             hit_count INTEGER NOT NULL DEFAULT 0,
             last_username TEXT NOT NULL DEFAULT '',
             last_ip TEXT NOT NULL DEFAULT '',
             last_seen_at INTEGER NOT NULL DEFAULT 0,
             is_banned INTEGER NOT NULL DEFAULT 0
         );

         CREATE TABLE IF NOT EXISTS playback_history (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             username TEXT NOT NULL,
             item_id TEXT NOT NULL DEFAULT '',
             item_name TEXT NOT NULL DEFAULT '',
             provider TEXT NOT NULL DEFAULT '',
             mode_label TEXT NOT NULL DEFAULT '',
             ua TEXT NOT NULL DEFAULT '',
             ip TEXT NOT NULL DEFAULT '',
             file_size INTEGER NOT NULL DEFAULT 0,
             cache_hit INTEGER NOT NULL DEFAULT 0,
             status TEXT NOT NULL,
             detail TEXT NOT NULL DEFAULT '',
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_history_created ON playback_history(created_at DESC);",
    )?;
    // Idempotent schema migration for v1.0.0 -> v2.0.0 databases
    let _ = conn.execute(
        "ALTER TABLE active_sessions ADD COLUMN host_session_key TEXT NOT NULL DEFAULT ''",
        [],
    );
    Ok(conn)
}

pub fn get_kv(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM kv_store WHERE key = ?1",
        params![key],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn set_kv(conn: &Connection, key: &str, val: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO kv_store (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, val, now_ts()],
    )?;
    Ok(())
}

pub fn load_config(conn: &Connection) -> Result<AppConfig> {
    if let Some(raw) = get_kv(conn, "app_config") {
        if let Ok(cfg) = serde_json::from_str::<AppConfig>(&raw) {
            return Ok(cfg);
        }
    }
    let cfg = AppConfig::default();
    save_config(conn, &cfg)?;
    Ok(cfg)
}

pub fn save_config(conn: &Connection, cfg: &AppConfig) -> Result<()> {
    let raw = serde_json::to_string(cfg)?;
    set_kv(conn, "app_config", &raw)
}

pub fn edit_config<F>(conn: &mut Connection, bump_ts: bool, f: F) -> Result<AppConfig>
where
    F: FnOnce(&mut AppConfig) -> Result<bool>,
{
    let tx = conn.transaction()?;
    let mut cfg = if let Some(raw) = tx
        .query_row(
            "SELECT value FROM kv_store WHERE key = 'app_config'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        serde_json::from_str::<AppConfig>(&raw).unwrap_or_default()
    } else {
        AppConfig::default()
    };
    let changed = f(&mut cfg)?;
    if changed || bump_ts {
        cfg.updated_at = now_ts();
        let raw = serde_json::to_string(&cfg)?;
        tx.execute(
            "INSERT INTO kv_store (key, value, updated_at) VALUES ('app_config', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![raw, cfg.updated_at],
        )?;
    }
    tx.commit()?;
    Ok(cfg)
}

// === Link Cache (1800s TTL + CDN t= expiration guard + refresh_link) ===

#[derive(Debug, Clone)]
pub struct CachedLinkRow {
    pub url: String,
    pub provider: String,
    pub mode_label: String,
    pub file_name: String,
    pub uid: String,
    pub headers: HashMap<String, String>,
    pub remaining_secs: i64,
}

fn cdn_url_expiry_ts(url_str: &str) -> Option<i64> {
    let parsed = Url::parse(url_str).ok()?;
    for (k, v) in parsed.query_pairs() {
        if k == "t" {
            if let Ok(ts) = v.parse::<i64>() {
                if ts > 1_600_000_000 {
                    return Some(ts);
                }
            }
        }
    }
    None
}

/// Read-only inspection of a single cache entry without extending its TTL.
/// Returns `remaining_secs` accounting for both `expires_at` and any 115 CDN `t=` deadline.
pub fn peek_cached_link(conn: &Connection, cache_key: &str) -> Option<CachedLinkRow> {
    let now = now_ts();
    let row: Option<(String, String, String, String, String, String, i64)> = conn
        .query_row(
            "SELECT url, provider, mode_label, file_name, headers_json, uid, expires_at
             FROM link_cache WHERE cache_key = ?1",
            params![cache_key],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten();
    let (url, provider, mode_label, file_name, headers_json, uid, expires_at) = row?;
    if expires_at <= now {
        let _ = conn.execute("DELETE FROM link_cache WHERE cache_key = ?1", params![cache_key]);
        return None;
    }
    let mut effective_exp = expires_at;
    if let Some(cdn_exp) = cdn_url_expiry_ts(&url) {
        if cdn_exp <= now + 60 {
            let _ = conn.execute("DELETE FROM link_cache WHERE cache_key = ?1", params![cache_key]);
            return None;
        }
        effective_exp = effective_exp.min(cdn_exp - 45);
    }
    let remaining_secs = (effective_exp - now).max(0);
    if remaining_secs == 0 {
        return None;
    }
    let headers: HashMap<String, String> = serde_json::from_str(&headers_json).unwrap_or_default();
    Some(CachedLinkRow {
        url,
        provider,
        mode_label,
        file_name,
        uid,
        headers,
        remaining_secs,
    })
}

pub fn peek_cached_link_with_fallback(
    conn: &Connection,
    exact_key: &str,
    fallback_key: &str,
) -> Option<CachedLinkRow> {
    if let Some(hit) = peek_cached_link(conn, exact_key) {
        if hit.url != "__MEDINEXT_NEGATIVE__" {
            return Some(hit);
        }
    }
    if !fallback_key.is_empty() && fallback_key != exact_key {
        if let Some(hit) = peek_cached_link(conn, fallback_key) {
            if hit.url != "__MEDINEXT_NEGATIVE__" {
                return Some(hit);
            }
        }
    }
    None
}

pub fn get_and_refresh_cached_link(
    conn: &Connection,
    cache_key: &str,
    ttl_secs: u64,
) -> Option<CachedLinkRow> {
    let hit = peek_cached_link(conn, cache_key)?;
    let now = now_ts();
    let mut new_exp = now + (ttl_secs.max(60) as i64);
    if let Some(cdn_exp) = cdn_url_expiry_ts(&hit.url) {
        new_exp = new_exp.min(cdn_exp - 45);
    }
    if new_exp > now {
        let _ = conn.execute(
            "UPDATE link_cache SET expires_at = ?2, hit_count = hit_count + 1 WHERE cache_key = ?1",
            params![cache_key, new_exp],
        );
    }
    Some(hit)
}

/// Looks up `exact_key` first; on miss, falls back to the device-less shared key
/// (`{username}:{pickcode}:{ua_md5}:N`) because 115 direct links are bound only to
/// `(115 account + pickcode + User-Agent)`, bridging `/prefetch` <-> `/resolve` `device_id` differences.
pub fn get_and_refresh_cached_link_with_fallback(
    conn: &Connection,
    exact_key: &str,
    fallback_key: &str,
    ttl_secs: u64,
) -> Option<CachedLinkRow> {
    if let Some(hit) = get_and_refresh_cached_link(conn, exact_key, ttl_secs) {
        return Some(hit);
    }
    if !fallback_key.is_empty() && fallback_key != exact_key {
        if let Some(hit) = get_and_refresh_cached_link(conn, fallback_key, ttl_secs) {
            if hit.url != "__MEDINEXT_NEGATIVE__" {
                return Some(hit);
            }
        }
    }
    None
}

pub fn has_valid_cached_link_with_fallback(
    conn: &Connection,
    exact_key: &str,
    fallback_key: &str,
    min_remaining_secs: i64,
) -> bool {
    peek_cached_link_with_fallback(conn, exact_key, fallback_key)
        .is_some_and(|row| row.remaining_secs >= min_remaining_secs.max(1))
}

#[allow(dead_code)]
pub fn has_valid_cached_link(conn: &Connection, cache_key: &str) -> bool {
    has_valid_cached_link_with_fallback(conn, cache_key, "", 1)
}

#[allow(clippy::too_many_arguments)]
pub fn put_cached_link(
    conn: &Connection,
    cache_key: &str,
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
    let now = now_ts();
    let mut exp = now + (ttl_secs.max(30) as i64);
    if let Some(cdn_exp) = cdn_url_expiry_ts(url) {
        if cdn_exp > now + 45 {
            exp = exp.min(cdn_exp - 45);
        }
    }
    let h_json = serde_json::to_string(headers).unwrap_or_else(|_| "{}".to_string());
    let _ = conn.execute(
        "INSERT INTO link_cache
         (cache_key, pickcode, provider, mode_label, uid, url, headers_json, file_name, item_id, created_at, expires_at, hit_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)
         ON CONFLICT(cache_key) DO UPDATE SET
             pickcode = excluded.pickcode,
             provider = excluded.provider,
             mode_label = excluded.mode_label,
             uid = excluded.uid,
             url = excluded.url,
             headers_json = excluded.headers_json,
             file_name = excluded.file_name,
             item_id = excluded.item_id,
             expires_at = excluded.expires_at",
        params![
            cache_key, pickcode, provider, mode_label, uid, url, h_json, file_name, item_id, now, exp
        ],
    );
}

// === Pickcode Metadata & SHA1 Rapid-Transfer Pool ===

pub fn get_pickcode_meta(conn: &Connection, pickcode: &str) -> Option<(String, String, u64, String)> {
    conn.query_row(
        "SELECT file_id, sha1, size, name FROM pickcode_meta WHERE pickcode = ?1",
        params![pickcode.trim()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?.max(0) as u64, r.get(3)?)),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn put_pickcode_meta(
    conn: &Connection,
    pickcode: &str,
    file_id: &str,
    sha1: &str,
    size: u64,
    name: &str,
) {
    if pickcode.trim().is_empty() {
        return;
    }
    let _ = conn.execute(
        "INSERT INTO pickcode_meta (pickcode, file_id, sha1, size, name, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(pickcode) DO UPDATE SET
             file_id = CASE WHEN excluded.file_id != '' THEN excluded.file_id ELSE pickcode_meta.file_id END,
             sha1 = CASE WHEN excluded.sha1 != '' THEN excluded.sha1 ELSE pickcode_meta.sha1 END,
             size = CASE WHEN excluded.size > 0 THEN excluded.size ELSE pickcode_meta.size END,
             name = CASE WHEN excluded.name != '' THEN excluded.name ELSE pickcode_meta.name END,
             updated_at = excluded.updated_at",
        params![pickcode.trim(), file_id, sha1.to_ascii_uppercase(), size as i64, name, now_ts()],
    );
}

pub fn get_sha1_target(conn: &Connection, sha1: &str, target_uid: &str) -> Option<String> {
    if sha1.trim().is_empty() || target_uid.trim().is_empty() {
        return None;
    }
    conn.query_row(
        "SELECT target_pickcode FROM sha1_pool WHERE sha1 = ?1 AND target_uid = ?2",
        params![sha1.trim().to_ascii_uppercase(), target_uid.trim()],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn invalidate_sha1_target(conn: &Connection, sha1: &str, target_uid: &str) {
    let _ = conn.execute(
        "DELETE FROM sha1_pool WHERE sha1 = ?1 AND target_uid = ?2",
        params![sha1.trim().to_ascii_uppercase(), target_uid.trim()],
    );
}

#[allow(clippy::too_many_arguments)]
pub fn record_rapid_transfer(
    conn: &Connection,
    sha1: &str,
    target_uid: &str,
    account_name: &str,
    username: &str,
    target_pickcode: &str,
    target_file_id: &str,
    file_name: &str,
    file_size: u64,
) {
    let now = now_ts();
    let sha_up = sha1.trim().to_ascii_uppercase();
    if !sha_up.is_empty() && !target_uid.trim().is_empty() && !target_pickcode.trim().is_empty() {
        let _ = conn.execute(
            "INSERT INTO sha1_pool (sha1, target_uid, target_pickcode, target_file_id, size, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(sha1, target_uid) DO UPDATE SET
                 target_pickcode = excluded.target_pickcode,
                 target_file_id = excluded.target_file_id,
                 size = excluded.size,
                 created_at = excluded.created_at",
            params![sha_up, target_uid.trim(), target_pickcode.trim(), target_file_id, file_size as i64, now],
        );
    }
    let _ = conn.execute(
        "INSERT INTO transfer_log
         (target_uid, account_name, username, sha1, target_pickcode, target_file_id, file_name, file_size, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            target_uid.trim(),
            account_name,
            username,
            sha_up,
            target_pickcode.trim(),
            target_file_id,
            file_name,
            file_size as i64,
            now
        ],
    );
}

pub fn count_user_daily_transfers(conn: &Connection, username: &str) -> u32 {
    let since = now_ts() - 86400;
    conn.query_row(
        "SELECT COUNT(*) FROM transfer_log WHERE username = ?1 AND created_at >= ?2",
        params![username, since],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
    .max(0) as u32
}

#[derive(Debug, Clone)]
pub struct TransferLogItem {
    pub id: i64,
    pub sha1: String,
    pub target_file_id: String,
    pub file_size: u64,
}

pub fn list_oldest_transfers_for_uid(
    conn: &Connection,
    target_uid: &str,
    limit: usize,
) -> Vec<TransferLogItem> {
    let mut stmt = match conn.prepare(
        "SELECT id, sha1, target_file_id, file_size
         FROM transfer_log WHERE target_uid = ?1
         ORDER BY created_at ASC, id ASC LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![target_uid.trim(), limit as i64], |r| {
        Ok(TransferLogItem {
            id: r.get(0)?,
            sha1: r.get(1)?,
            target_file_id: r.get(2)?,
            file_size: r.get::<_, i64>(3)?.max(0) as u64,
        })
    })
    .ok()
    .into_iter()
    .flatten()
    .flatten()
    .collect()
}

pub fn remove_transfer_logs(conn: &Connection, ids: &[i64], target_uid: &str, sha1s: &[String]) {
    for id in ids {
        let _ = conn.execute("DELETE FROM transfer_log WHERE id = ?1", params![id]);
    }
    for s in sha1s {
        if !s.is_empty() {
            let _ = conn.execute(
                "DELETE FROM sha1_pool WHERE sha1 = ?1 AND target_uid = ?2",
                params![s, target_uid.trim()],
            );
        }
    }
}

// === Load-Balance State & Affinity ===

#[derive(Debug, Clone, Default)]
pub struct LbRuntimeState {
    pub call_count: u64,
    pub cooldown_until: i64,
    pub last_error: String,
}

pub fn load_lb_states(conn: &Connection) -> HashMap<String, LbRuntimeState> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn.prepare("SELECT account_name, call_count, cooldown_until, last_error FROM lb_state") else {
        return out;
    };
    if let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            LbRuntimeState {
                call_count: r.get::<_, i64>(1)?.max(0) as u64,
                cooldown_until: r.get::<_, i64>(2)?,
                last_error: r.get::<_, String>(3)?,
            },
        ))
    }) {
        for item in rows.flatten() {
            out.insert(item.0, item.1);
        }
    }
    out
}

pub fn prune_orphaned_lb_states(conn: &Connection, valid_names: &[String]) {
    let _ = conn.execute("DELETE FROM lb_state WHERE account_name = ''", []);
    if valid_names.is_empty() {
        return;
    }
    let existing = load_lb_states(conn);
    for k in existing.keys() {
        if !valid_names.iter().any(|v| v == k) {
            let _ = conn.execute("DELETE FROM lb_state WHERE account_name = ?1", params![k]);
        }
    }
}

pub fn record_lb_selected(conn: &Connection, account_name: &str, username: &str, device_id: &str) {
    let now = now_ts();
    let _ = conn.execute(
        "INSERT INTO lb_state (account_name, call_count, cooldown_until, last_error, updated_at)
         VALUES (?1, 1, 0, '', ?2)
         ON CONFLICT(account_name) DO UPDATE SET
             call_count = lb_state.call_count + 1,
             updated_at = excluded.updated_at",
        params![account_name, now],
    );
    if !username.trim().is_empty() {
        let _ = conn.execute(
            "INSERT INTO lb_affinity (scope_key, account_name, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(scope_key) DO UPDATE SET account_name = excluded.account_name, updated_at = excluded.updated_at",
            params![format!("user:{}", username.trim()), account_name, now],
        );
    }
    if !device_id.trim().is_empty() {
        let _ = conn.execute(
            "INSERT INTO lb_affinity (scope_key, account_name, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(scope_key) DO UPDATE SET account_name = excluded.account_name, updated_at = excluded.updated_at",
            params![format!("dev:{}", device_id.trim()), account_name, now],
        );
    }
}

pub fn get_lb_affinity(conn: &Connection, scope_key: &str) -> Option<String> {
    conn.query_row(
        "SELECT account_name FROM lb_affinity WHERE scope_key = ?1",
        params![scope_key],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn mark_lb_cooldown(conn: &Connection, account_name: &str, cooldown_secs: u64, error: &str) {
    let now = now_ts();
    let until = if cooldown_secs > 0 {
        now + cooldown_secs as i64
    } else {
        0
    };
    let _ = conn.execute(
        "INSERT INTO lb_state (account_name, call_count, cooldown_until, last_error, updated_at)
         VALUES (?1, 0, ?2, ?3, ?4)
         ON CONFLICT(account_name) DO UPDATE SET
             cooldown_until = excluded.cooldown_until,
             last_error = excluded.last_error,
             updated_at = excluded.updated_at",
        params![account_name, until, error, now],
    );
}

// === Active Sessions, Devices, IP Lock & UA Stats ===

pub const SESSION_ACTIVE_WINDOW_SECS: i64 = 360;

pub fn prune_stale_data(conn: &Connection) {
    let now = now_ts();
    let _ = conn.execute(
        "DELETE FROM active_sessions WHERE last_seen_at < ?1",
        params![now - SESSION_ACTIVE_WINDOW_SECS],
    );
    let _ = conn.execute(
        "DELETE FROM link_cache WHERE expires_at <= ?1",
        params![now],
    );
    let _ = conn.execute(
        "DELETE FROM ip_events WHERE last_seen_at < ?1",
        params![now - 86400],
    );
    // Prune expired ephemeral cooldown keys from kv_store to prevent unbounded growth
    let _ = conn.execute(
        "DELETE FROM kv_store
         WHERE updated_at < ?1
           AND (key LIKE 'detail_pre:%' OR key LIKE 'prewarm_next:%' OR key LIKE 'notify_play:%' OR key LIKE 'notify_risk:%')",
        params![now - 86400],
    );
}

#[allow(clippy::too_many_arguments)]
pub fn touch_active_session(
    conn: &Connection,
    host_session_key: &str,
    username: &str,
    user_id: &str,
    device_id: &str,
    device_name: &str,
    client: &str,
    ua: &str,
    ip: &str,
    item_id: &str,
    item_name: &str,
    provider: &str,
) {
    let now = now_ts();
    let dev_part = if !device_id.trim().is_empty() {
        device_id.trim().to_string()
    } else if !host_session_key.trim().is_empty() {
        host_session_key.trim().to_string()
    } else {
        format!("ip:{}:{}", ip.trim(), crate::models::truncate_chars(ua.trim(), 24))
    };
    let session_key = format!("{}:{}", username.trim(), dev_part);
    let _ = conn.execute(
        "INSERT INTO active_sessions
         (session_key, host_session_key, username, user_id, device_id, device_name, client, ua, ip, item_id, item_name, provider, started_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
         ON CONFLICT(session_key) DO UPDATE SET
             host_session_key = CASE WHEN excluded.host_session_key != '' THEN excluded.host_session_key ELSE active_sessions.host_session_key END,
             user_id = CASE WHEN excluded.user_id != '' THEN excluded.user_id ELSE active_sessions.user_id END,
             device_name = CASE WHEN excluded.device_name != '' THEN excluded.device_name ELSE active_sessions.device_name END,
             client = CASE WHEN excluded.client != '' THEN excluded.client ELSE active_sessions.client END,
             item_id = CASE WHEN excluded.item_id != '' THEN excluded.item_id ELSE active_sessions.item_id END,
             item_name = CASE WHEN excluded.item_name != '' THEN excluded.item_name ELSE active_sessions.item_name END,
             provider = CASE WHEN excluded.provider != '' THEN excluded.provider ELSE active_sessions.provider END,
             ip = CASE WHEN excluded.ip != '' THEN excluded.ip ELSE active_sessions.ip END,
             ua = CASE WHEN excluded.ua != '' THEN excluded.ua ELSE active_sessions.ua END,
             last_seen_at = excluded.last_seen_at",
        params![
            session_key,
            host_session_key.trim(),
            username.trim(),
            user_id,
            device_id.trim(),
            device_name,
            client,
            ua,
            ip,
            item_id,
            item_name,
            provider,
            now
        ],
    );
}

pub fn get_active_session_info(conn: &Connection, session_key: &str) -> Option<(String, String, String)> {
    conn.query_row(
        "SELECT username, device_id, host_session_key FROM active_sessions WHERE session_key = ?1",
        params![session_key.trim()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn invalidate_user_device_links(conn: &Connection, username: &str, device_id: &str) {
    let uname = username.trim();
    if uname.is_empty() {
        return;
    }
    let prefix = format!("{uname}:");
    let prefix_len = prefix.chars().count() as i64;
    let dev = device_id.trim();
    if !dev.is_empty() {
        let dev_mid = format!(":{dev}:");
        let _ = conn.execute(
            "DELETE FROM link_cache
             WHERE substr(cache_key, 1, ?1) = ?2
               AND instr(cache_key, ?3) > ?1",
            params![prefix_len, prefix, dev_mid],
        );
        // Also clear device-less shared fallback keys (`{username}:{pickcode}:{ua_md5}:N`)
        // for this user so a kicked device cannot reuse prewarmed links by omitting `device_id`.
        let shared_colons = (uname.matches(':').count() + 3) as i64;
        let _ = conn.execute(
            "DELETE FROM link_cache
             WHERE substr(cache_key, 1, ?1) = ?2
               AND (LENGTH(cache_key) - LENGTH(REPLACE(cache_key, ':', ''))) = ?3",
            params![prefix_len, prefix, shared_colons],
        );
    } else {
        let _ = conn.execute(
            "DELETE FROM link_cache WHERE substr(cache_key, 1, ?1) = ?2",
            params![prefix_len, prefix],
        );
    }
}

pub fn reconcile_host_playbacks(
    conn: &Connection,
    playbacks: &[crate::host::HostActivePlayback],
) {
    let now = now_ts();
    let mut live_host_keys = std::collections::HashSet::new();
    for pb in playbacks {
        if !pb.session_key.is_empty() {
            live_host_keys.insert(pb.session_key.clone());
        }
        let uname = if !pb.username.is_empty() {
            pb.username.as_str()
        } else if !pb.user_id.is_empty() {
            &format!("user_{}", pb.user_id)
        } else {
            continue;
        };
        let dev_part = if !pb.device_id.trim().is_empty() {
            pb.device_id.trim().to_string()
        } else if !pb.session_key.is_empty() {
            pb.session_key.clone()
        } else {
            continue;
        };
        let local_key = format!("{uname}:{dev_part}");
        let seen_ts = pb.last_seen_at.max(now - 10).min(now);
        let start_ts = pb.started_at.min(seen_ts);
        let _ = conn.execute(
            "INSERT INTO active_sessions
             (session_key, host_session_key, username, user_id, device_id, device_name, client, ua, ip, item_id, item_name, provider, started_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, '', ?12, ?13)
             ON CONFLICT(session_key) DO UPDATE SET
                 host_session_key = CASE WHEN excluded.host_session_key != '' THEN excluded.host_session_key ELSE active_sessions.host_session_key END,
                 user_id = CASE WHEN excluded.user_id != '' THEN excluded.user_id ELSE active_sessions.user_id END,
                 device_name = CASE WHEN excluded.device_name != '' THEN excluded.device_name ELSE active_sessions.device_name END,
                 client = CASE WHEN excluded.client != '' THEN excluded.client ELSE active_sessions.client END,
                 ua = CASE WHEN excluded.ua != '' THEN excluded.ua ELSE active_sessions.ua END,
                 ip = CASE WHEN excluded.ip != '' THEN excluded.ip ELSE active_sessions.ip END,
                 item_id = CASE WHEN excluded.item_id != '' THEN excluded.item_id ELSE active_sessions.item_id END,
                 item_name = CASE WHEN excluded.item_name != '' THEN excluded.item_name ELSE active_sessions.item_name END,
                 last_seen_at = MAX(active_sessions.last_seen_at, excluded.last_seen_at)",
            params![
                local_key,
                pb.session_key,
                uname,
                pb.user_id,
                pb.device_id,
                pb.device_name,
                pb.client,
                pb.user_agent,
                pb.client_ip,
                pb.item_id,
                pb.item_name,
                start_ts,
                seen_ts,
            ],
        );
    }

    // Prune sessions tracked by Mediary that have already stopped (older than 30s grace window for initial stream startup)
    if let Ok(mut stmt) = conn.prepare(
        "SELECT session_key, host_session_key, last_seen_at FROM active_sessions WHERE host_session_key != ''",
    ) {
        let rows: Vec<(String, String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .collect();
        for (s_key, h_key, seen) in rows {
            if !live_host_keys.contains(&h_key) && seen < now - 30 {
                let _ = conn.execute("DELETE FROM active_sessions WHERE session_key = ?1", params![s_key]);
            }
        }
    }
}

pub fn count_other_active_streams_for_user(conn: &Connection, username: &str, current_device_id: &str) -> u32 {
    let cutoff = now_ts() - SESSION_ACTIVE_WINDOW_SECS;
    if !current_device_id.trim().is_empty() {
        conn.query_row(
            "SELECT COUNT(*) FROM active_sessions
             WHERE username = ?1 AND last_seen_at >= ?2 AND device_id != ?3",
            params![username.trim(), cutoff, current_device_id.trim()],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        .max(0) as u32
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM active_sessions WHERE username = ?1 AND last_seen_at >= ?2",
            params![username.trim(), cutoff],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        .max(0) as u32
    }
}

pub fn active_sessions_by_provider(conn: &Connection, exclude_device_id: &str) -> HashMap<String, usize> {
    let cutoff = now_ts() - SESSION_ACTIVE_WINDOW_SECS;
    let mut map = HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT provider, device_id FROM active_sessions WHERE last_seen_at >= ?1 AND provider != ''",
    ) else {
        return map;
    };
    if let Ok(rows) = stmt.query_map(params![cutoff], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    }) {
        for (prov, dev) in rows.flatten() {
            if !exclude_device_id.trim().is_empty() && dev == exclude_device_id.trim() {
                continue;
            }
            *map.entry(prov).or_insert(0) += 1;
        }
    }
    map
}

pub fn list_active_sessions(conn: &Connection) -> Vec<Value> {
    let cutoff = now_ts() - SESSION_ACTIVE_WINDOW_SECS;
    let Ok(mut stmt) = conn.prepare(
        "SELECT session_key, host_session_key, username, device_id, device_name, client, ua, ip, item_id, item_name, provider, started_at, last_seen_at
         FROM active_sessions WHERE last_seen_at >= ?1 ORDER BY last_seen_at DESC",
    ) else {
        return Vec::new();
    };
    stmt.query_map(params![cutoff], |r| {
        Ok(json!({
            "session_key": r.get::<_, String>(0)?,
            "host_session_key": r.get::<_, String>(1)?,
            "username": r.get::<_, String>(2)?,
            "device_id": r.get::<_, String>(3)?,
            "device_name": r.get::<_, String>(4)?,
            "client": r.get::<_, String>(5)?,
            "ua": r.get::<_, String>(6)?,
            "ip": r.get::<_, String>(7)?,
            "item_id": r.get::<_, String>(8)?,
            "item_name": r.get::<_, String>(9)?,
            "provider": r.get::<_, String>(10)?,
            "started_at": r.get::<_, i64>(11)?,
            "last_seen_at": r.get::<_, i64>(12)?,
        }))
    })
    .ok()
    .into_iter()
    .flatten()
    .flatten()
    .collect()
}

/// Checks if `device_id` is banned or exceeds `max_devices` (0 = unlimited).
/// Returns `Err(reason)` if blocked, otherwise registers/updates device.
pub fn check_and_touch_device(
    conn: &Connection,
    username: &str,
    device_id: &str,
    device_name: &str,
    client: &str,
    ip: &str,
    max_devices: u32,
) -> std::result::Result<(), String> {
    let dev = device_id.trim();
    if username.trim().is_empty() || dev.is_empty() {
        return Ok(());
    }
    let existing_ban: Option<i64> = conn
        .query_row(
            "SELECT is_banned FROM known_devices WHERE username = ?1 AND device_id = ?2",
            params![username.trim(), dev],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    if existing_ban == Some(1) {
        return Err("该播放设备已被管理员封禁".to_string());
    }
    if existing_ban.is_none() && max_devices > 0 {
        let active_cutoff = now_ts() - 30 * 86400;
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM known_devices WHERE username = ?1 AND is_banned = 0 AND last_seen_at >= ?2",
                params![username.trim(), active_cutoff],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if cnt as u32 >= max_devices {
            return Err(format!("设备数量超出上限 ({cnt}/{max_devices})"));
        }
    }
    let now = now_ts();
    let _ = conn.execute(
        "INSERT INTO known_devices (username, device_id, device_name, client, last_ip, first_seen_at, last_seen_at, is_banned)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 0)
         ON CONFLICT(username, device_id) DO UPDATE SET
             device_name = CASE WHEN excluded.device_name != '' THEN excluded.device_name ELSE known_devices.device_name END,
             client = CASE WHEN excluded.client != '' THEN excluded.client ELSE known_devices.client END,
             last_ip = CASE WHEN excluded.last_ip != '' THEN excluded.last_ip ELSE known_devices.last_ip END,
             last_seen_at = excluded.last_seen_at",
        params![username.trim(), dev, device_name, client, ip, now],
    );
    Ok(())
}

pub fn list_known_devices(conn: &Connection) -> Vec<Value> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT username, device_id, device_name, client, last_ip, first_seen_at, last_seen_at, is_banned
         FROM known_devices ORDER BY last_seen_at DESC LIMIT 200",
    ) else {
        return Vec::new();
    };
    stmt.query_map([], |r| {
        Ok(json!({
            "username": r.get::<_, String>(0)?,
            "device_id": r.get::<_, String>(1)?,
            "device_name": r.get::<_, String>(2)?,
            "client": r.get::<_, String>(3)?,
            "last_ip": r.get::<_, String>(4)?,
            "first_seen_at": r.get::<_, i64>(5)?,
            "last_seen_at": r.get::<_, i64>(6)?,
            "is_banned": r.get::<_, i64>(7)? == 1,
        }))
    })
    .ok()
    .into_iter()
    .flatten()
    .flatten()
    .collect()
}

pub fn check_and_touch_ip_window(
    conn: &Connection,
    username: &str,
    ip: &str,
    window_mins: u32,
    max_ips: u32,
) -> std::result::Result<(), String> {
    let clean_ip = ip.trim();
    if username.trim().is_empty()
        || clean_ip.is_empty()
        || clean_ip == "127.0.0.1"
        || clean_ip == "::1"
        || max_ips == 0
    {
        return Ok(());
    }
    let now = now_ts();
    let cutoff = now - (window_mins.max(5) as i64) * 60;
    let already_seen: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ip_events WHERE username = ?1 AND ip = ?2 AND last_seen_at >= ?3)",
            params![username.trim(), clean_ip, cutoff],
            |r| r.get(0),
        )
        .unwrap_or(false);
    if !already_seen {
        let distinct_cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ip_events WHERE username = ?1 AND last_seen_at >= ?2",
                params![username.trim(), cutoff],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if distinct_cnt as u32 >= max_ips {
            return Err(format!(
                "{window_mins} 分钟内跨 IP 播放超限 ({distinct_cnt}/{max_ips})"
            ));
        }
    }
    let _ = conn.execute(
        "INSERT INTO ip_events (username, ip, last_seen_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(username, ip) DO UPDATE SET last_seen_at = excluded.last_seen_at",
        params![username.trim(), clean_ip, now],
    );
    Ok(())
}

pub fn record_ua_and_check_db_ban(conn: &Connection, ua: &str, username: &str, ip: &str) -> bool {
    let ua_name = normalize_ua_label(ua);
    if ua_name.is_empty() {
        return false;
    }
    let now = now_ts();
    let _ = conn.execute(
        "INSERT INTO ua_stats (ua_name, hit_count, last_username, last_ip, last_seen_at, is_banned)
         VALUES (?1, 1, ?2, ?3, ?4, 0)
         ON CONFLICT(ua_name) DO UPDATE SET
             hit_count = ua_stats.hit_count + 1,
             last_username = CASE WHEN excluded.last_username != '' THEN excluded.last_username ELSE ua_stats.last_username END,
             last_ip = CASE WHEN excluded.last_ip != '' THEN excluded.last_ip ELSE ua_stats.last_ip END,
             last_seen_at = excluded.last_seen_at",
        params![ua_name, username, ip, now],
    );
    conn.query_row(
        "SELECT is_banned FROM ua_stats WHERE ua_name = ?1",
        params![ua_name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
        == 1
}

pub fn normalize_ua_label(ua: &str) -> String {
    let trimmed = ua.trim();
    if trimmed.is_empty() {
        return "Empty-UA".to_string();
    }
    let first = trimmed.split_whitespace().next().unwrap_or(trimmed);
    first.chars().take(64).collect()
}

pub fn list_ua_stats(conn: &Connection) -> Vec<Value> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT ua_name, hit_count, last_username, last_ip, last_seen_at, is_banned
         FROM ua_stats ORDER BY hit_count DESC, last_seen_at DESC LIMIT 120",
    ) else {
        return Vec::new();
    };
    stmt.query_map([], |r| {
        Ok(json!({
            "ua_name": r.get::<_, String>(0)?,
            "hit_count": r.get::<_, i64>(1)?,
            "last_username": r.get::<_, String>(2)?,
            "last_ip": r.get::<_, String>(3)?,
            "last_seen_at": r.get::<_, i64>(4)?,
            "is_banned": r.get::<_, i64>(5)? == 1,
        }))
    })
    .ok()
    .into_iter()
    .flatten()
    .flatten()
    .collect()
}

// === Playback History & Dashboard Summary ===

#[allow(clippy::too_many_arguments)]
pub fn record_playback_history(
    conn: &Connection,
    username: &str,
    item_id: &str,
    item_name: &str,
    provider: &str,
    mode_label: &str,
    ua: &str,
    ip: &str,
    file_size: u64,
    cache_hit: bool,
    status: &str,
    detail: &str,
) {
    let now = now_ts();
    let ua_norm = normalize_ua_label(ua);

    // Coalesce rapid HTTP Range / seek bursts hitting 1800s cache within 90s for the same user + media
    // so `plays_24h` and `traffic_bytes` are not inflated by multi-connection player probes.
    if cache_hit && status == "redirect" && (!item_id.trim().is_empty() || !item_name.trim().is_empty()) {
        let recent_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM playback_history
                 WHERE username = ?1
                   AND status = 'redirect'
                   AND created_at >= ?2
                   AND ((?3 != '' AND item_id = ?3) OR (?4 != '' AND item_name = ?4))
                 ORDER BY id DESC LIMIT 1",
                params![username, now - 90, item_id.trim(), item_name.trim()],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(existing_id) = recent_id {
            let _ = conn.execute(
                "UPDATE playback_history
                 SET created_at = ?2, ua = ?3, ip = CASE WHEN ?4 != '' THEN ?4 ELSE ip END
                 WHERE id = ?1",
                params![existing_id, now, ua_norm, ip],
            );
            return;
        }
    }

    let _ = conn.execute(
        "INSERT INTO playback_history
         (username, item_id, item_name, provider, mode_label, ua, ip, file_size, cache_hit, status, detail, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            username,
            item_id,
            item_name,
            provider,
            mode_label,
            ua_norm,
            ip,
            file_size as i64,
            if cache_hit { 1 } else { 0 },
            status,
            detail,
            now
        ],
    );
    // Keep latest 5000 rows to prevent unbounded growth
    let _ = conn.execute(
        "DELETE FROM playback_history WHERE id NOT IN (SELECT id FROM playback_history ORDER BY id DESC LIMIT 5000)",
        [],
    );
}

pub fn list_playback_history(conn: &Connection, limit: usize, keyword: &str) -> Vec<Value> {
    let lim = limit.clamp(1, 300) as i64;
    let kw = keyword.trim();
    let mut out = Vec::new();
    if kw.is_empty() {
        let Ok(mut stmt) = conn.prepare(
            "SELECT id, username, item_id, item_name, provider, mode_label, ua, ip, file_size, cache_hit, status, detail, created_at
             FROM playback_history ORDER BY id DESC LIMIT ?1",
        ) else {
            return out;
        };
        if let Ok(rows) = stmt.query_map(params![lim], map_history_row) {
            out.extend(rows.flatten());
        };
    } else {
        let pat = format!("%{kw}%");
        let Ok(mut stmt) = conn.prepare(
            "SELECT id, username, item_id, item_name, provider, mode_label, ua, ip, file_size, cache_hit, status, detail, created_at
             FROM playback_history
             WHERE username LIKE ?1 OR item_name LIKE ?1 OR provider LIKE ?1 OR detail LIKE ?1
             ORDER BY id DESC LIMIT ?2",
        ) else {
            return out;
        };
        if let Ok(rows) = stmt.query_map(params![pat, lim], map_history_row) {
            out.extend(rows.flatten());
        };
    }
    out
}

fn map_history_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let ts: i64 = r.get(12)?;
    let dt = chrono::DateTime::from_timestamp(ts, 0)
        .map(|u| u.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "username": r.get::<_, String>(1)?,
        "item_id": r.get::<_, String>(2)?,
        "item_name": r.get::<_, String>(3)?,
        "provider": r.get::<_, String>(4)?,
        "mode_label": r.get::<_, String>(5)?,
        "ua": r.get::<_, String>(6)?,
        "ip": r.get::<_, String>(7)?,
        "file_size": r.get::<_, i64>(8)?,
        "cache_hit": r.get::<_, i64>(9)? == 1,
        "status": r.get::<_, String>(10)?,
        "detail": r.get::<_, String>(11)?,
        "created_at": ts,
        "updated_at": dt,
    }))
}

pub fn compute_stats_summary(conn: &Connection) -> Value {
    let now = now_ts();
    let window_stats = |days: i64| -> Value {
        let since = now - days * 86400;
        let (total, redirects, cache_hits, denies, prewarms, bytes): (i64, i64, i64, i64, i64, i64) = conn
            .query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN status != 'prewarm' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'redirect' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'redirect' AND cache_hit = 1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'deny' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'prewarm' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'redirect' THEN file_size ELSE 0 END), 0)
                 FROM playback_history WHERE created_at >= ?1",
                params![since],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap_or((0, 0, 0, 0, 0, 0));
        json!({
            "total": total,
            "redirects": redirects,
            "cache_hits": cache_hits,
            "denies": denies,
            "prewarms": prewarms,
            "traffic_bytes": bytes,
        })
    };

    let cached_links: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM link_cache WHERE expires_at > ?1",
            params![now],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let sha1_pool_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sha1_pool", [], |r| r.get(0))
        .unwrap_or(0);
    let active_streams: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM active_sessions WHERE last_seen_at >= ?1",
            params![now - SESSION_ACTIVE_WINDOW_SECS],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let mut top_users = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT username, COUNT(*), COALESCE(SUM(file_size), 0)
         FROM playback_history WHERE created_at >= ?1 AND status = 'redirect' AND username != ''
         GROUP BY username ORDER BY COUNT(*) DESC LIMIT 10",
    ) {
        if let Ok(rows) = stmt.query_map(params![now - 7 * 86400], |r| {
            let plays: i64 = r.get(1)?;
            let bytes: i64 = r.get(2)?;
            // Activity score aligned with NextEmby idea
            let score = (plays as f64) * 2.5 + (bytes as f64) / (1024.0 * 1024.0 * 1024.0);
            Ok(json!({
                "username": r.get::<_, String>(0)?,
                "plays_7d": plays,
                "traffic_bytes_7d": bytes,
                "activity_score": (score * 10.0).round() / 10.0,
            }))
        }) {
            top_users.extend(rows.flatten());
        }
    }

    json!({
        "d1": window_stats(1),
        "d7": window_stats(7),
        "d30": window_stats(30),
        "cached_links": cached_links,
        "sha1_pool_count": sha1_pool_count,
        "active_streams": active_streams,
        "top_users_7d": top_users,
    })
}

pub fn write_dashboard_file(
    data_dir: &Path,
    conn: &Connection,
    cfg: &AppConfig,
    settings: &PluginSettings,
) {
    let now = now_ts();
    let lb_states = load_lb_states(conn);
    let lb_healthy = cfg
        .lb_accounts
        .iter()
        .filter(|a| {
            a.enabled
                && a.valid
                && lb_states
                    .get(&a.name)
                    .is_none_or(|st| st.cooldown_until <= now)
        })
        .count();

    let stats = compute_stats_summary(conn);
    let history = list_playback_history(conn, 60, "");
    let items: Vec<Value> = history
        .into_iter()
        .map(|h| {
            let title = h["item_name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| h["item_id"].as_str().unwrap_or("-"))
                .to_string();
            json!({
                "title": title,
                "username": h["username"],
                "route": h["mode_label"],
                "provider": h["provider"],
                "status": h["status"],
                "detail": h["detail"],
                "updated_at": h["updated_at"],
            })
        })
        .collect();

    let (plugin_enabled, hook_approved, _, _, _) = crate::host::inspect_local_plugin_state(data_dir);
    let mode_summary = if !plugin_enabled || !settings.enabled {
        "插件已停用".to_string()
    } else if hook_approved {
        "原生 302 钩子 · 已生效".to_string()
    } else {
        "待批准播放钩子".to_string()
    };

    let payload = json!({
        "updated_at": now_datetime_str(),
        "summary": {
            "mode_summary": mode_summary,
            "lb_healthy": format!("{lb_healthy}/{}", cfg.lb_accounts.len()),
            "managed_users": cfg.users.len(),
            "active_streams": stats["active_streams"],
            "cached_links": stats["cached_links"],
            "plays_24h": stats["d1"]["total"],
        },
        "items": items,
    });

    if let Ok(raw) = serde_json::to_vec_pretty(&payload) {
        if raw.len() < 1_900_000 {
            let tmp = data_dir.join("dashboard.json.tmp");
            let dst = data_dir.join("dashboard.json");
            if fs::write(&tmp, &raw).is_ok() {
                let _ = fs::rename(&tmp, &dst);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_cache_fallback_near_expiry_and_kick_invalidation() {
        let dir = std::env::temp_dir().join(format!("mn_db_cache_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let conn = open_db(&dir).unwrap();
        let headers = HashMap::new();

        // 1. Prewarm writes device-less shared key; playback with device_id falls back and hits!
        put_cached_link(
            &conn,
            "alice:pc001:deadbeef:N",
            "pc001",
            "lb1",
            "[详情预热]",
            "342589555",
            "https://cdn.example.com/v1.mkv",
            &headers,
            "Demo.mkv",
            "item1",
            1800,
        );
        let hit = get_and_refresh_cached_link_with_fallback(
            &conn,
            "alice:pc001:tv-1:deadbeef:N",
            "alice:pc001:deadbeef:N",
            1800,
        )
        .expect("should hit shared device-less key");
        assert_eq!(hit.url, "https://cdn.example.com/v1.mkv");
        assert!(hit.remaining_secs > 1700);

        // 2. Near-expiry link (< 300s) is detected by peek_cached_link_with_fallback & has_valid_cached_link_with_fallback
        put_cached_link(
            &conn,
            "alice:pc002:deadbeef:N",
            "pc002",
            "lb1",
            "[下集预热]",
            "342589555",
            "https://cdn.example.com/v2.mkv",
            &headers,
            "E02.mkv",
            "item2",
            120,
        );
        let peek = peek_cached_link_with_fallback(
            &conn,
            "alice:pc002:tv-1:deadbeef:N",
            "alice:pc002:deadbeef:N",
        )
        .expect("should still be present");
        assert!(peek.remaining_secs > 0 && peek.remaining_secs < 300);
        assert!(!has_valid_cached_link_with_fallback(
            &conn,
            "alice:pc002:tv-1:deadbeef:N",
            "alice:pc002:deadbeef:N",
            300
        ));

        // 3. Kick device `tv-1` for `alice`: removes tv-1 key AND alice's device-less keys,
        //    while keeping `alice`'s `tv-2` key and other users' (`alice_2`) keys intact!
        put_cached_link(
            &conn,
            "alice:pc001:tv-1:deadbeef:N",
            "pc001",
            "lb1",
            "[负载均衡]",
            "342589555",
            "https://cdn.example.com/tv1.mkv",
            &headers,
            "Demo.mkv",
            "item1",
            1800,
        );
        put_cached_link(
            &conn,
            "alice:pc001:tv-2:deadbeef:N",
            "pc001",
            "lb1",
            "[负载均衡]",
            "342589555",
            "https://cdn.example.com/tv2.mkv",
            &headers,
            "Demo.mkv",
            "item1",
            1800,
        );
        put_cached_link(
            &conn,
            "alice_2:pc001:deadbeef:N",
            "pc001",
            "lb1",
            "[详情预热]",
            "342589555",
            "https://cdn.example.com/other_user.mkv",
            &headers,
            "Demo.mkv",
            "item1",
            1800,
        );

        invalidate_user_device_links(&conn, "alice", "tv-1");
        assert!(peek_cached_link(&conn, "alice:pc001:tv-1:deadbeef:N").is_none());
        assert!(peek_cached_link(&conn, "alice:pc001:deadbeef:N").is_none());
        assert!(peek_cached_link(&conn, "alice:pc002:deadbeef:N").is_none());
        assert!(peek_cached_link(&conn, "alice:pc001:tv-2:deadbeef:N").is_some());
        assert!(peek_cached_link(&conn, "alice_2:pc001:deadbeef:N").is_some());

        // 4. UTF-8 safe truncation & ephemeral kv_store cooldown pruning
        assert_eq!(crate::models::truncate_chars("飞牛影视客户端/2.0", 4), "飞牛影视");
        touch_active_session(
            &conn,
            "",
            "alice",
            "10",
            "",
            "TV",
            "飞牛",
            "飞牛影视播放器客户端超长中文名称测试版本123456",
            "10.0.0.1",
            "item1",
            "Demo",
            "lb1",
        );
        conn.execute(
            "INSERT OR REPLACE INTO kv_store (key, value, updated_at) VALUES ('detail_pre:old', '1', ?1), ('folder_cid:old', '12345', ?1)",
            params![now_ts() - 100_000],
        )
        .unwrap();
        prune_stale_data(&conn);
        assert!(get_kv(&conn, "detail_pre:old").is_none());
        assert_eq!(get_kv(&conn, "folder_cid:old").as_deref(), Some("12345"));

        let _ = fs::remove_dir_all(&dir);
    }
}

use crate::models::{
    now_datetime_str, now_ts, AppConfig, CatalogDocument, CatalogPluginEntry, DaemonRuntimeState,
    ParsedPluginManifest, PluginSettings,
};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

pub fn open_db(data_dir: &Path) -> Result<Connection> {
    fs::create_dir_all(data_dir).context("创建插件数据目录失败")?;
    let db_path = data_dir.join("mediplug.db");
    let conn = Connection::open(&db_path).context("打开 mediplug.db 失败")?;
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

         CREATE TABLE IF NOT EXISTS release_asset_cache (
             cache_key TEXT PRIMARY KEY,
             repo TEXT NOT NULL DEFAULT '',
             tag TEXT NOT NULL DEFAULT '',
             asset_name TEXT NOT NULL DEFAULT '',
             asset_url TEXT NOT NULL DEFAULT '',
             asset_size INTEGER NOT NULL DEFAULT 0,
             sha256 TEXT NOT NULL DEFAULT '',
             manifest_json TEXT NOT NULL DEFAULT '{}',
             updated_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_release_cache_updated ON release_asset_cache(updated_at DESC);

         CREATE TABLE IF NOT EXISTS activity_logs (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             title TEXT NOT NULL,
             source TEXT NOT NULL DEFAULT '',
             version TEXT NOT NULL DEFAULT '',
             action TEXT NOT NULL,
             status TEXT NOT NULL,
             detail TEXT NOT NULL DEFAULT '',
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_activity_created ON activity_logs(created_at DESC);",
    )?;
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
        if let Ok(mut cfg) = serde_json::from_str::<AppConfig>(&raw) {
            cfg.engine = cfg.engine.normalized();
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
    cfg.engine = cfg.engine.normalized();
    let changed = f(&mut cfg)?;
    cfg.engine = cfg.engine.normalized();
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

pub fn get_cached_asset(
    conn: &Connection,
    cache_key: &str,
) -> Option<(String, u64, Option<crate::models::ParsedPluginManifest>)> {
    let (sha256, asset_size, manifest_raw): (String, i64, String) = conn
        .query_row(
            "SELECT sha256, asset_size, manifest_json FROM release_asset_cache WHERE cache_key = ?1",
            params![cache_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()??;
    if sha256.len() != 64 {
        return None;
    }
    let manifest = serde_json::from_str(&manifest_raw).ok();
    Some((sha256, asset_size.max(0) as u64, manifest))
}

pub fn load_cached_manifests_by_plugin_id(conn: &Connection) -> HashMap<String, ParsedPluginManifest> {
    let mut map = HashMap::new();
    let mut stmt = match conn.prepare(
        "SELECT manifest_json FROM release_asset_cache ORDER BY updated_at DESC LIMIT 500",
    ) {
        Ok(s) => s,
        Err(_) => return map,
    };
    let rows = match stmt.query_map([], |r| r.get::<_, String>(0)) {
        Ok(it) => it,
        Err(_) => return map,
    };
    for raw in rows.flatten() {
        if let Ok(m) = serde_json::from_str::<ParsedPluginManifest>(&raw) {
            let pid = m.id.trim().to_string();
            if !pid.is_empty() && !map.contains_key(&pid) {
                map.insert(pid, m);
            }
        }
    }
    map
}

pub fn put_cached_asset(
    conn: &Connection,
    cache_key: &str,
    repo: &str,
    tag: &str,
    asset_name: &str,
    asset_url: &str,
    asset_size: u64,
    sha256: &str,
    manifest: Option<&crate::models::ParsedPluginManifest>,
) {
    let manifest_json = manifest
        .and_then(|m| serde_json::to_string(m).ok())
        .unwrap_or_else(|| "{}".to_string());
    let _ = conn.execute(
        "INSERT INTO release_asset_cache (cache_key, repo, tag, asset_name, asset_url, asset_size, sha256, manifest_json, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(cache_key) DO UPDATE SET
           sha256 = excluded.sha256,
           manifest_json = excluded.manifest_json,
           updated_at = excluded.updated_at",
        params![
            cache_key,
            repo,
            tag,
            asset_name,
            asset_url,
            asset_size as i64,
            sha256,
            manifest_json,
            now_ts()
        ],
    );
    // Keep cache compact (latest 500 release assets)
    let _ = conn.execute(
        "DELETE FROM release_asset_cache WHERE cache_key NOT IN (
             SELECT cache_key FROM release_asset_cache ORDER BY updated_at DESC LIMIT 500
         )",
        [],
    );
}

pub fn append_log(
    conn: &Connection,
    title: &str,
    source: &str,
    version: &str,
    action: &str,
    status: &str,
    detail: &str,
) {
    let _ = conn.execute(
        "INSERT INTO activity_logs (title, source, version, action, status, detail, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![title, source, version, action, status, detail, now_ts()],
    );
    let _ = conn.execute(
        "DELETE FROM activity_logs WHERE id NOT IN (
             SELECT id FROM activity_logs ORDER BY id DESC LIMIT 400
         )",
        [],
    );
}

pub fn list_logs(conn: &Connection, limit: usize, keyword: &str) -> Vec<Value> {
    let kw = keyword.trim().to_ascii_lowercase();
    let mut stmt = match conn.prepare(
        "SELECT id, title, source, version, action, status, detail, created_at
         FROM activity_logs
         ORDER BY id DESC
         LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map(params![limit.max(1) as i64], |r| {
        let ts: i64 = r.get(7)?;
        let dt = chrono::DateTime::from_timestamp(ts, 0)
            .map(|u| u.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "-".to_string());
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "title": r.get::<_, String>(1)?,
            "source": r.get::<_, String>(2)?,
            "version": r.get::<_, String>(3)?,
            "action": r.get::<_, String>(4)?,
            "status": r.get::<_, String>(5)?,
            "detail": r.get::<_, String>(6)?,
            "created_at": ts,
            "updated_at": dt,
        }))
    }) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };

    rows.flatten()
        .filter(|row| {
            if kw.is_empty() {
                return true;
            }
            let hay = format!(
                "{} {} {} {} {}",
                row["title"].as_str().unwrap_or_default(),
                row["source"].as_str().unwrap_or_default(),
                row["version"].as_str().unwrap_or_default(),
                row["action"].as_str().unwrap_or_default(),
                row["detail"].as_str().unwrap_or_default()
            )
            .to_ascii_lowercase();
            hay.contains(&kw)
        })
        .collect()
}

/// Builds the unified `catalog.json` combining all enabled GitHub Release bridges
/// (`register_mode != "dedicated"` or all enabled repos when requested) + manually hosted entries.
pub fn build_unified_catalog(cfg: &AppConfig) -> CatalogDocument {
    let mut seen = HashSet::new();
    let mut plugins: Vec<CatalogPluginEntry> = Vec::new();

    for repo in &cfg.github_repos {
        if !repo.enabled {
            continue;
        }
        // Include both "unified" repos and fallback if user accesses `/catalog.json` directly
        if repo.register_mode == "dedicated" {
            continue;
        }
        for p in &repo.plugins {
            if !p.id.trim().is_empty() && seen.insert(p.id.clone()) {
                plugins.push(p.clone());
            }
        }
    }

    for p in &cfg.hosted_entries {
        if !p.id.trim().is_empty() && seen.insert(p.id.clone()) {
            plugins.push(p.clone());
        }
    }

    CatalogDocument {
        schema_version: 1,
        plugins,
    }
}

pub fn build_repo_catalog(cfg: &AppConfig, repo_id: &str) -> Option<CatalogDocument> {
    let target = repo_id.trim().trim_end_matches(".json");
    let repo = cfg.github_repos.iter().find(|r| r.id == target)?;
    let plugins = if repo.enabled {
        repo.plugins.clone()
    } else {
        Vec::new()
    };
    Some(CatalogDocument {
        schema_version: 1,
        plugins,
    })
}

pub fn write_catalog_snapshot_file(data_dir: &Path, cfg: &AppConfig) {
    let doc = build_unified_catalog(cfg);
    if let Ok(raw) = serde_json::to_vec_pretty(&doc) {
        let tmp = data_dir.join("catalog.json.tmp");
        let dst = data_dir.join("catalog.json");
        if fs::write(&tmp, &raw).is_ok() {
            let _ = fs::rename(&tmp, &dst);
        }
    }
}

pub fn read_daemon_state(data_dir: &Path) -> Option<DaemonRuntimeState> {
    let p = data_dir.join("daemon_state.json");
    let raw = fs::read_to_string(p).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn write_daemon_state(data_dir: &Path, st: &DaemonRuntimeState) {
    if let Ok(raw) = serde_json::to_vec_pretty(st) {
        let tmp = data_dir.join("daemon_state.json.tmp");
        let dst = data_dir.join("daemon_state.json");
        if fs::write(&tmp, &raw).is_ok() {
            let _ = fs::rename(&tmp, &dst);
        }
    }
}

pub fn remove_daemon_state_if_matches(data_dir: &Path, expected_pid: u32, expected_token: &str) {
    if let Some(st) = read_daemon_state(data_dir) {
        if st.pid == expected_pid
            && (expected_token.is_empty() || st.shutdown_token == expected_token)
        {
            let _ = fs::remove_file(data_dir.join("daemon_state.json"));
        }
    }
}

pub fn total_bridged_plugins(cfg: &AppConfig) -> usize {
    let mut ids = HashSet::new();
    for r in cfg.github_repos.iter().filter(|r| r.enabled) {
        for p in &r.plugins {
            if !p.id.is_empty() {
                ids.insert(p.id.clone());
            }
        }
    }
    for p in &cfg.hosted_entries {
        if !p.id.is_empty() {
            ids.insert(p.id.clone());
        }
    }
    ids.len()
}

pub fn write_dashboard_file(
    data_dir: &Path,
    conn: &Connection,
    cfg: &AppConfig,
    settings: &PluginSettings,
    daemon_ready: bool,
    active_port: u16,
    host_sources_count: Option<usize>,
    updates_available: Option<usize>,
) {
    let (plugin_enabled, _, _, _) = crate::host::inspect_local_plugin_state(data_dir);
    let bridge_status = if !plugin_enabled || !settings.enabled {
        "插件已停用".to_string()
    } else if daemon_ready {
        format!("运行中 (:{active_port})")
    } else {
        format!("待启动 (:{})", cfg.engine.listen_port)
    };

    let cached_sources = host_sources_count.unwrap_or_else(|| {
        get_kv(conn, "last_host_sources_count")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    });
    if let Some(c) = host_sources_count {
        let _ = set_kv(conn, "last_host_sources_count", &c.to_string());
    }

    let cached_updates = updates_available.unwrap_or_else(|| {
        get_kv(conn, "last_updates_available")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    });
    if let Some(u) = updates_available {
        let _ = set_kv(conn, "last_updates_available", &u.to_string());
    }

    let last_result = get_kv(conn, "last_sync_summary").unwrap_or_else(|| "就绪".to_string());
    let logs = list_logs(conn, 60, "");

    let payload = json!({
        "updated_at": now_datetime_str(),
        "summary": {
            "bridge_status": bridge_status,
            "host_sources": cached_sources,
            "github_repos": cfg.github_repos.len(),
            "bridged_plugins": total_bridged_plugins(cfg),
            "updates_available": cached_updates,
            "last_result": last_result,
        },
        "items": logs,
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

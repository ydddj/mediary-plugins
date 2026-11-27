use crate::models::{format_ts_cn, now_ts, AppConfig, RuleSubscription, StagingPipeline};
use crate::rules;
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

pub fn default_subscriptions() -> BTreeMap<String, RuleSubscription> {
    let base = "https://raw.githubusercontent.com/MuRongYun8/-/refs/heads/master";
    [
        (
            "mediatidy",
            "MediaTidy · 远程评分规则 (quality-rules.yaml)",
            "mediatidy",
            format!("{base}/quality-rules.yaml"),
        ),
        (
            "matching",
            "MediaTidy · 远程识别与季集映射 (full_rules.yaml)",
            "matching",
            format!("{base}/full_rules.yaml"),
        ),
        (
            "tmdb",
            "MediaTidy · 远程 TMDB 覆盖与黑名单 (tmdb_override)",
            "tmdb",
            format!("{base}/tmdb_override"),
        ),
    ]
    .into_iter()
    .map(|(id, name, kind, url)| {
        (
            id.to_string(),
            RuleSubscription {
                id: id.to_string(),
                name: name.to_string(),
                kind: kind.to_string(),
                url,
                enabled: true,
                cache: String::new(),
                last_sync: 0,
                error: String::new(),
            },
        )
    })
    .collect()
}

pub fn default_app_config() -> Result<AppConfig> {
    let builtin_rules = rules::default_rules()?;
    let symedia_id = builtin_rules
        .iter()
        .find(|r| r.kind == rules::SYMEDIA)
        .map(|r| r.id.clone())
        .unwrap_or_default();
    let mediatidy_id = builtin_rules
        .iter()
        .find(|r| r.kind == rules::MEDIATIDY)
        .map(|r| r.id.clone())
        .unwrap_or_else(|| symedia_id.clone());

    let mut cfg = AppConfig {
        revision: 1,
        rules: builtin_rules,
        subscriptions: default_subscriptions(),
        ..Default::default()
    };
    cfg.strategy.movie_rule_id = mediatidy_id.clone();
    cfg.strategy.tv_rule_id = mediatidy_id;
    cfg.pt_config.movie_rule_id = symedia_id.clone();
    cfg.pt_config.tv_rule_id = symedia_id;
    Ok(cfg)
}

pub fn open_db(data_dir: &Path) -> Result<Connection> {
    fs::create_dir_all(data_dir)?;
    let db_path = data_dir.join("mediwash.db");
    let is_new = !db_path.exists();
    let db = Connection::open(&db_path)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;

         CREATE TABLE IF NOT EXISTS app_config (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             revision INTEGER NOT NULL DEFAULT 1,
             body TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS staging_items (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             pipeline_id INTEGER NOT NULL,
             file_id TEXT NOT NULL UNIQUE,
             parent_cid TEXT NOT NULL DEFAULT '',
             rel_path TEXT NOT NULL DEFAULT '',
             name TEXT NOT NULL,
             size INTEGER NOT NULL DEFAULT 0,
             sha1 TEXT NOT NULL DEFAULT '',
             pick_code TEXT NOT NULL DEFAULT '',
             media_type TEXT NOT NULL DEFAULT '',
             tmdb_id TEXT NOT NULL DEFAULT '',
             title TEXT NOT NULL DEFAULT '',
             year INTEGER NOT NULL DEFAULT 0,
             season INTEGER NOT NULL DEFAULT 0,
             episode INTEGER NOT NULL DEFAULT 0,
             category TEXT NOT NULL DEFAULT '',
             slot TEXT NOT NULL DEFAULT '',
             cand_score REAL NOT NULL DEFAULT 0,
             lib_score REAL,
             verdict TEXT NOT NULL DEFAULT 'review',
             state TEXT NOT NULL DEFAULT 'pending',
             reason TEXT NOT NULL DEFAULT '',
             detail_json TEXT NOT NULL DEFAULT '{}',
             sidecar_ids_json TEXT NOT NULL DEFAULT '[]',
             updated_at INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_staging_pipeline_state ON staging_items(pipeline_id, state, verdict, updated_at DESC);

         CREATE TABLE IF NOT EXISTS wash_ledger (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             ledger_key TEXT NOT NULL UNIQUE,
             source_kind TEXT NOT NULL,
             media_type TEXT NOT NULL,
             tmdb_id TEXT NOT NULL,
             season INTEGER NOT NULL DEFAULT 0,
             episode INTEGER NOT NULL DEFAULT 0,
             item_id TEXT NOT NULL DEFAULT '',
             title TEXT NOT NULL,
             category TEXT NOT NULL DEFAULT '',
             slot TEXT NOT NULL DEFAULT '',
             rule_id TEXT NOT NULL DEFAULT '',
             expected_file_id TEXT NOT NULL DEFAULT '',
             expected_sha1 TEXT NOT NULL DEFAULT '',
             download_id TEXT NOT NULL DEFAULT '',
             old_source_ids_json TEXT NOT NULL DEFAULT '[]',
             cand_score REAL NOT NULL DEFAULT 0,
             old_score REAL NOT NULL DEFAULT 0,
             state TEXT NOT NULL DEFAULT 'awaiting_ingest',
             message TEXT NOT NULL DEFAULT '',
             created_at INTEGER NOT NULL DEFAULT 0,
             updated_at INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_ledger_state ON wash_ledger(state, updated_at DESC);

         CREATE TABLE IF NOT EXISTS lib_scan_jobs (
             scope TEXT PRIMARY KEY,
             revision INTEGER NOT NULL DEFAULT 1,
             cursor_start INTEGER NOT NULL DEFAULT 0,
             total INTEGER NOT NULL DEFAULT 0,
             scanned INTEGER NOT NULL DEFAULT 0,
             multi_source_items INTEGER NOT NULL DEFAULT 0,
             suggestion_groups INTEGER NOT NULL DEFAULT 0,
             suggestion_sources INTEGER NOT NULL DEFAULT 0,
             review_groups INTEGER NOT NULL DEFAULT 0,
             mode TEXT NOT NULL DEFAULT 'idle',
             warning TEXT NOT NULL DEFAULT '',
             updated_at INTEGER NOT NULL DEFAULT 0
         );

         CREATE TABLE IF NOT EXISTS lib_decisions (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             scope TEXT NOT NULL,
             decision_key TEXT NOT NULL UNIQUE,
             item_id TEXT NOT NULL,
             title TEXT NOT NULL,
             library_id TEXT NOT NULL DEFAULT '',
             category TEXT NOT NULL DEFAULT '',
             slot TEXT NOT NULL DEFAULT '',
             status TEXT NOT NULL,
             reason TEXT NOT NULL DEFAULT '',
             winner_score REAL NOT NULL DEFAULT 0,
             loser_score REAL NOT NULL DEFAULT 0,
             source_count INTEGER NOT NULL DEFAULT 0,
             body_json TEXT NOT NULL,
             state TEXT NOT NULL DEFAULT 'pending',
             updated_at INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_lib_decisions_filter ON lib_decisions(scope, status, state, id);
         CREATE INDEX IF NOT EXISTS idx_lib_decisions_item ON lib_decisions(item_id);

         CREATE TABLE IF NOT EXISTS pt_candidates (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             cand_key TEXT NOT NULL UNIQUE,
             item_id TEXT NOT NULL,
             media_type TEXT NOT NULL,
             tmdb_id TEXT NOT NULL,
             season INTEGER NOT NULL DEFAULT 0,
             episode INTEGER NOT NULL DEFAULT 0,
             title TEXT NOT NULL,
             category TEXT NOT NULL DEFAULT '',
             slot TEXT NOT NULL DEFAULT '',
             current_score REAL NOT NULL DEFAULT 0,
             graduate_score REAL NOT NULL DEFAULT 0,
             torrent_title TEXT NOT NULL,
             site_id INTEGER NOT NULL DEFAULT 0,
             site_name TEXT NOT NULL DEFAULT '',
             seeders INTEGER NOT NULL DEFAULT 0,
             size INTEGER NOT NULL DEFAULT 0,
             cand_score REAL NOT NULL DEFAULT 0,
             download_url TEXT NOT NULL,
             request_json TEXT NOT NULL,
             old_source_ids_json TEXT NOT NULL DEFAULT '[]',
             state TEXT NOT NULL DEFAULT 'pending',
             message TEXT NOT NULL DEFAULT '',
             updated_at INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_pt_state ON pt_candidates(state, updated_at DESC);

         CREATE TABLE IF NOT EXISTS cursors (
             key TEXT PRIMARY KEY,
             val TEXT NOT NULL,
             updated_at INTEGER NOT NULL DEFAULT 0
         );

         CREATE TABLE IF NOT EXISTS activity_logs (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             record_type TEXT NOT NULL DEFAULT 'media',
             kind TEXT NOT NULL,
             title TEXT NOT NULL,
             sub_title TEXT NOT NULL DEFAULT '',
             source_path TEXT NOT NULL DEFAULT '',
             target_path TEXT NOT NULL DEFAULT '',
             slot TEXT NOT NULL DEFAULT '',
             action TEXT NOT NULL,
             status TEXT NOT NULL,
             detail TEXT NOT NULL,
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_activity_created ON activity_logs(id DESC);",
    )?;
    ensure_activity_columns(&db)?;
    let _ = db.execute_batch(
        "UPDATE staging_items SET verdict = 'reject' WHERE verdict = 'isolate';
         UPDATE staging_items SET state = 'organized' WHERE state = 'moved_organize';
         DELETE FROM staging_items WHERE state IN ('moved_isolate', 'failed');
         UPDATE activity_logs SET action = 'organized' WHERE action = 'moved_organize';
         DELETE FROM activity_logs WHERE action = 'moved_isolate';",
    );

    if db
        .query_row("SELECT 1 FROM app_config WHERE id = 1", [], |_| Ok(()))
        .optional()?
        .is_none()
    {
        let mut cfg = default_app_config()?;
        if is_new {
            migrate_legacy_data(data_dir, &mut cfg);
        }
        let body = serde_json::to_string(&cfg)?;
        db.execute(
            "INSERT OR IGNORE INTO app_config (id, revision, body) VALUES (1, ?, ?)",
            params![cfg.revision, body],
        )?;
    }
    Ok(db)
}

fn migrate_legacy_data(data_dir: &Path, cfg: &mut AppConfig) {
    // 1. Migrate from v0.0.7 mediwash-v2.sqlite3 if present (skipping bloated keep rows)
    let v2_path = data_dir.join("mediwash-v2.sqlite3");
    if v2_path.is_file() {
        if let Ok(old_db) = Connection::open(&v2_path) {
            if let Ok(raw_cfg) =
                old_db.query_row("SELECT body FROM config WHERE id = 1", [], |r| r.get::<_, String>(0))
            {
                if let Ok(v) = serde_json::from_str::<Value>(&raw_cfg) {
                    // Custom rules
                    for r in v["rules"].as_array().into_iter().flatten() {
                        let id = r["id"].as_str().unwrap_or_default();
                        let kind = r["kind"].as_str().unwrap_or_default();
                        let raw = r["raw"].as_str().unwrap_or_default();
                        if id.starts_with("custom:") || id.starts_with("import:") || id.starts_with("legacy:") {
                            if let Ok(parsed) = rules::parse_rules(kind, raw, "custom") {
                                for mut rule in parsed {
                                    rule.id = id.to_string();
                                    if !cfg.rules.iter().any(|existing| existing.id == rule.id) {
                                        cfg.rules.push(rule);
                                    }
                                }
                            }
                        }
                    }
                    // Subscriptions & caches
                    if let Some(subs) = v["subscriptions"].as_object() {
                        for (k, sv) in subs {
                            if let Some(target) = cfg.subscriptions.get_mut(k) {
                                if let Some(u) = sv["url"].as_str().filter(|s| !s.trim().is_empty()) {
                                    target.url = u.trim().to_string();
                                }
                                if let Some(en) = sv["enabled"].as_bool() {
                                    target.enabled = en;
                                }
                                if let Some(cache) = sv["cache"].as_str().filter(|s| !s.is_empty()) {
                                    target.cache = cache.to_string();
                                    target.last_sync = sv["last_sync"].as_u64().unwrap_or(0);
                                    if target.kind == "mediatidy" {
                                        if let Ok(parsed) =
                                            rules::parse_rules("mediatidy", cache, "subscription")
                                        {
                                            for mut pr in parsed {
                                                pr.id = format!("sub:{}:{}", k, pr.id);
                                                if !cfg.rules.iter().any(|x| x.id == pr.id) {
                                                    cfg.rules.push(pr);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Local overrides
                    if let Some(m_raw) = v["references"]["matching"]["raw"].as_str() {
                        if !m_raw.trim().is_empty() {
                            cfg.local_matching_yaml = m_raw.to_string();
                        }
                    }
                    if let Some(t_raw) = v["references"]["tmdb"]["raw"].as_str() {
                        if !t_raw.trim().is_empty() {
                            cfg.local_tmdb_yaml = t_raw.to_string();
                        }
                    }
                    // Monitors -> StagingPipelines
                    if let Some(mons) = v["monitors"].as_object() {
                        let mut next_id = 1i64;
                        for (_mid, mv) in mons {
                            let src = mv["source_cid"].as_str().unwrap_or_default().trim();
                            if src.is_empty() {
                                continue;
                            }
                            cfg.pipelines.push(StagingPipeline {
                                id: next_id,
                                name: mv["name"]
                                    .as_str()
                                    .filter(|s| !s.trim().is_empty())
                                    .unwrap_or("115 待整理中转")
                                    .to_string(),
                                enabled: mv["scheduled"].as_bool().unwrap_or(true),
                                media_type: "auto".to_string(),
                                source_cid: src.to_string(),
                                source_name: String::new(),
                                recursive: mv["recursive"].as_bool().unwrap_or(true),
                                move_subtitles: true,
                                auto_admit: true,
                                wash_enabled: true,
                                reject_mode: "recycle_bin".to_string(),
                                probe_mode: if mv["metadata_mode"].as_str() == Some("always") {
                                    "always".to_string()
                                } else {
                                    "smart".to_string()
                                },
                                rule_id_override: String::new(),
                            });
                            next_id += 1;
                        }
                    }
                }
            }
        }
        return;
    }

    // 2. Fallback: migrate from v1.x state.json if present
    let v1_path = data_dir.join("state.json");
    if v1_path.is_file() {
        if let Ok(raw) = fs::read_to_string(&v1_path) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(m) = v["local_full_rules_yaml"].as_str().filter(|s| !s.trim().is_empty()) {
                    cfg.local_matching_yaml = m.to_string();
                }
                if let Some(t) = v["local_tmdb_override_yaml"].as_str().filter(|s| !s.trim().is_empty()) {
                    cfg.local_tmdb_yaml = t.to_string();
                }
            }
        }
    }
}

pub fn load_config(db: &Connection) -> Result<AppConfig> {
    let (rev, body): (i64, String) = db
        .query_row("SELECT revision, body FROM app_config WHERE id = 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .context("读取 MediWash 配置失败")?;
    let mut cfg: AppConfig = serde_json::from_str(&body).context("解析 MediWash 配置失败")?;
    cfg.revision = rev;
    if cfg.rules.is_empty() {
        cfg.rules = rules::default_rules()?;
    }
    if cfg.subscriptions.is_empty() {
        cfg.subscriptions = default_subscriptions();
    }
    Ok(cfg)
}

pub fn edit_config(
    db: &mut Connection,
    bump_revision: bool,
    f: impl FnOnce(&mut AppConfig) -> Result<bool>,
) -> Result<AppConfig> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut cfg = load_config(&tx)?;
    let changed = f(&mut cfg)?;
    if changed && bump_revision {
        cfg.revision += 1;
    }
    let body = serde_json::to_string(&cfg)?;
    tx.execute(
        "UPDATE app_config SET revision = ?, body = ? WHERE id = 1",
        params![cfg.revision, body],
    )?;
    tx.commit()?;
    Ok(cfg)
}

pub fn get_cursor(db: &Connection, key: &str) -> Option<String> {
    db.query_row("SELECT val FROM cursors WHERE key = ?", [key], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

pub fn set_cursor(db: &Connection, key: &str, val: &str) -> Result<()> {
    db.execute(
        "INSERT INTO cursors (key, val, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET val = excluded.val, updated_at = excluded.updated_at",
        params![key, val, now_ts()],
    )?;
    Ok(())
}

fn ensure_activity_columns(db: &Connection) -> Result<()> {
    let mut has_record_type = false;
    let mut has_sub_title = false;
    let mut has_source_path = false;
    let mut has_target_path = false;
    if let Ok(mut stmt) = db.prepare("PRAGMA table_info(activity_logs)") {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) {
            for col in rows.flatten() {
                match col.as_str() {
                    "record_type" => has_record_type = true,
                    "sub_title" => has_sub_title = true,
                    "source_path" => has_source_path = true,
                    "target_path" => has_target_path = true,
                    _ => {}
                }
            }
        }
    }
    if !has_record_type {
        db.execute(
            "ALTER TABLE activity_logs ADD COLUMN record_type TEXT NOT NULL DEFAULT 'media'",
            [],
        )?;
        db.execute(
            "UPDATE activity_logs SET record_type = 'task' WHERE action IN ('scan_staging', 'sync_rules', 'lib_scan', 'pt_sweep')",
            [],
        )?;
    }
    if !has_sub_title {
        db.execute(
            "ALTER TABLE activity_logs ADD COLUMN sub_title TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !has_source_path {
        db.execute(
            "ALTER TABLE activity_logs ADD COLUMN source_path TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !has_target_path {
        db.execute(
            "ALTER TABLE activity_logs ADD COLUMN target_path TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_activity_type ON activity_logs(record_type, id DESC);
         UPDATE activity_logs
            SET source_path = '115://待洗版/' || sub_title
          WHERE record_type = 'media' AND kind LIKE '115%' AND source_path = '' AND sub_title != '';
         UPDATE activity_logs
            SET target_path = '115 官方回收站'
          WHERE record_type = 'media' AND action = 'recycled_115' AND target_path = '';",
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn log_media_history(
    db: &Connection,
    kind: &str,
    title: &str,
    sub_title: &str,
    source_path: &str,
    target_path: &str,
    slot: &str,
    action: &str,
    status: &str,
    detail: &str,
) {
    let _ = db.execute(
        "INSERT INTO activity_logs (record_type, kind, title, sub_title, source_path, target_path, slot, action, status, detail, created_at)
         VALUES ('media', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![kind, title, sub_title, source_path, target_path, slot, action, status, detail, now_ts()],
    );
    let _ = db.execute(
        "DELETE FROM activity_logs WHERE record_type = 'media' AND id NOT IN (
            SELECT id FROM activity_logs WHERE record_type = 'media' ORDER BY id DESC LIMIT 2000
         )",
        [],
    );
}

pub fn log_task_history(
    db: &Connection,
    kind: &str,
    title: &str,
    slot: &str,
    action: &str,
    status: &str,
    detail: &str,
) {
    let _ = db.execute(
        "INSERT INTO activity_logs (record_type, kind, title, sub_title, slot, action, status, detail, created_at)
         VALUES ('task', ?, ?, '', ?, ?, ?, ?, ?)",
        params![kind, title, slot, action, status, detail, now_ts()],
    );
    let _ = db.execute(
        "DELETE FROM activity_logs WHERE record_type = 'task' AND id NOT IN (
            SELECT id FROM activity_logs WHERE record_type = 'task' ORDER BY id DESC LIMIT 300
         )",
        [],
    );
}

#[derive(Debug, Clone, Default)]
pub struct ActivityFilter {
    pub record_type: String,
    pub kind: String,
    pub action_group: String,
    pub status: String,
    pub keyword: String,
    pub limit: usize,
}

fn build_activity_where(f: &ActivityFilter) -> (String, Vec<String>) {
    let mut cond = String::from(" WHERE 1=1");
    let mut binds: Vec<String> = Vec::new();

    match f.record_type.as_str() {
        "media" => cond.push_str(" AND record_type = 'media'"),
        "task" => cond.push_str(" AND record_type = 'task'"),
        _ => {}
    }

    match f.kind.as_str() {
        "115" => cond.push_str(" AND kind LIKE '115%'"),
        "ledger" => cond.push_str(" AND kind = '删旧闭环'"),
        "library" => cond.push_str(" AND kind = '存量精洗'"),
        "pt" => cond.push_str(" AND kind = 'PT洗版'"),
        "subs" => cond.push_str(" AND kind = '远程订阅'"),
        _ => {}
    }

    match f.action_group.as_str() {
        "admit" => cond.push_str(" AND action = 'organized'"),
        "recycle" => cond.push_str(" AND action = 'recycled_115'"),
        "clean_old" => cond.push_str(" AND action IN ('auto_clean_old', 'clean_old', 'clean_library_dup')"),
        "pt" => cond.push_str(" AND action = 'submit_pt_download'"),
        _ => {}
    }

    match f.status.as_str() {
        "done" => cond.push_str(" AND status = 'done'"),
        "warning" => cond.push_str(" AND status = 'warning'"),
        _ => {}
    }

    let kw = f.keyword.trim();
    if !kw.is_empty() {
        let pat = format!("%{}%", kw.to_lowercase());
        cond.push_str(
            " AND (LOWER(title) LIKE ? OR LOWER(sub_title) LIKE ? OR LOWER(source_path) LIKE ? OR LOWER(target_path) LIKE ? OR LOWER(slot) LIKE ? OR LOWER(detail) LIKE ?)",
        );
        for _ in 0..6 {
            binds.push(pat.clone());
        }
    }
    (cond, binds)
}

pub fn action_display_label(action: &str) -> &'static str {
    match action {
        "organized" => "归档入库",
        "recycled_115" => "移入115回收站",
        "auto_clean_old" | "clean_old" => "洗版删旧源",
        "clean_library_dup" => "存量删重复源",
        "submit_pt_download" => "PT洗版下载",
        "scan_staging" => "监控目录扫描",
        "lib_scan" => "存量库扫描",
        "sync_rules" => "订阅规则同步",
        _ => "执行完成",
    }
}

pub fn query_activities(db: &Connection, filter: &ActivityFilter) -> Result<Value> {
    let media_total: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM activity_logs WHERE record_type = 'media'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let task_total: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM activity_logs WHERE record_type = 'task'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let (where_sql, binds) = build_activity_where(filter);
    let count_sql = format!("SELECT COUNT(*) FROM activity_logs{where_sql}");
    let filtered_total: i64 = db
        .query_row(&count_sql, rusqlite::params_from_iter(binds.iter()), |r| r.get(0))
        .unwrap_or(0);

    let lim = filter.limit.clamp(1, 500);
    let list_sql = format!(
        "SELECT id, record_type, kind, title, sub_title, source_path, target_path, slot, action, status, detail, created_at
         FROM activity_logs{where_sql} ORDER BY id DESC LIMIT {lim}"
    );
    let mut stmt = db.prepare(&list_sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), |r| {
        let act: String = r.get(8)?;
        let ts: u64 = r.get(11)?;
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "record_type": r.get::<_, String>(1)?,
            "kind": r.get::<_, String>(2)?,
            "title": r.get::<_, String>(3)?,
            "sub_title": r.get::<_, String>(4)?,
            "source_path": r.get::<_, String>(5)?,
            "target_path": r.get::<_, String>(6)?,
            "slot": r.get::<_, String>(7)?,
            "action": act.clone(),
            "action_label": action_display_label(&act),
            "status": r.get::<_, String>(9)?,
            "detail": r.get::<_, String>(10)?,
            "created_at": ts,
            "updated_at": format_ts_cn(ts),
        }))
    })?;
    let items: Vec<Value> = rows.filter_map(Result::ok).collect();

    Ok(json!({
        "items": items,
        "media_total": media_total,
        "task_total": task_total,
        "filtered_total": filtered_total,
    }))
}

pub fn delete_activities(
    db: &Connection,
    ids: &[i64],
    clear_filter: Option<&ActivityFilter>,
) -> Result<usize> {
    if !ids.is_empty() {
        let mut deleted = 0usize;
        for &id in ids {
            deleted += db.execute("DELETE FROM activity_logs WHERE id = ?", [id])?;
        }
        return Ok(deleted);
    }
    if let Some(f) = clear_filter {
        let (where_sql, binds) = build_activity_where(f);
        let sql = format!("DELETE FROM activity_logs{where_sql}");
        let deleted = db.execute(&sql, rusqlite::params_from_iter(binds.iter()))?;
        return Ok(deleted);
    }
    Ok(0)
}

pub fn list_activities(db: &Connection, limit: usize) -> Vec<Value> {
    query_activities(
        db,
        &ActivityFilter {
            record_type: "media".to_string(),
            limit,
            ..Default::default()
        },
    )
    .ok()
    .and_then(|v| v["items"].as_array().cloned())
    .unwrap_or_default()
}

pub fn summary_counts(db: &Connection) -> Value {
    let count = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let staging_pending = count("SELECT COUNT(*) FROM staging_items WHERE state = 'pending'");
    let staging_review =
        count("SELECT COUNT(*) FROM staging_items WHERE state = 'pending' AND verdict = 'review'");
    let awaiting_cleanup = count(
        "SELECT COUNT(*) FROM wash_ledger WHERE state IN ('downloading','downloaded','awaiting_ingest','ready_to_clean','review')",
    );
    let ready_cleanup =
        count("SELECT COUNT(*) FROM wash_ledger WHERE state = 'ready_to_clean'");
    let lib_suggestions = count(
        "SELECT COUNT(*) FROM lib_decisions WHERE status = 'suggestion' AND state IN ('pending','approved')",
    );
    let lib_reviews = count(
        "SELECT COUNT(*) FROM lib_decisions WHERE status = 'review' AND state = 'pending'",
    );
    let pt_pending = count("SELECT COUNT(*) FROM pt_candidates WHERE state = 'pending'");
    let media_history = count("SELECT COUNT(*) FROM activity_logs WHERE record_type = 'media'");
    let task_history = count("SELECT COUNT(*) FROM activity_logs WHERE record_type = 'task'");
    let last_result = get_cursor(db, "last_result").unwrap_or_else(|| "就绪".to_string());
    let last_tick_ts = get_cursor(db, "last_tick_ts")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    json!({
        "staging_pending": staging_pending,
        "staging_review": staging_review,
        "awaiting_cleanup": awaiting_cleanup,
        "ready_cleanup": ready_cleanup,
        "lib_suggestions": lib_suggestions,
        "lib_reviews": lib_reviews,
        "pt_pending": pt_pending,
        "media_history": media_history,
        "task_history": task_history,
        "last_result": last_result,
        "last_tick_at": format_ts_cn(last_tick_ts),
    })
}

pub fn write_dashboard_file(data_dir: &Path, db: &Connection, cfg: &AppConfig) {
    let counts = summary_counts(db);
    let items: Vec<Value> = list_activities(db, 80)
        .into_iter()
        .map(|mut it| {
            if let Some(lbl) = it["action_label"].as_str().map(str::to_string) {
                it["action"] = json!(lbl);
            }
            it
        })
        .collect();
    let payload = json!({
        "summary": {
            "media_history_count": format!("{} 条", counts["media_history"]),
            "staging_pending": format!("{} 条（待核 {}）", counts["staging_pending"], counts["staging_review"]),
            "awaiting_cleanup": format!("{} 条（可清 {}）", counts["awaiting_cleanup"], counts["ready_cleanup"]),
            "lib_suggestions": format!("{} 组", counts["lib_suggestions"]),
            "categories_synced": format!("{} 类", cfg.host_sync.categories.len()),
            "last_result": counts["last_result"],
        },
        "items": items,
    });
    if let Ok(pretty) = serde_json::to_string_pretty(&payload) {
        let tmp = data_dir.join("dashboard.json.tmp");
        let dst = data_dir.join("dashboard.json");
        if fs::write(&tmp, pretty).is_ok() {
            let _ = fs::rename(&tmp, &dst);
        }
    }
}

pub fn validate_cid(cid: &str) -> Result<()> {
    let trimmed = cid.trim();
    ensure!(
        !trimmed.is_empty() && trimmed.len() <= 30 && trimmed.bytes().all(|b| b.is_ascii_digit()),
        "115 目录 CID 必须为纯数字"
    );
    ensure!(
        trimmed == "0" || !trimmed.starts_with('0'),
        "115 目录 CID 不能有无效前导零"
    );
    Ok(())
}

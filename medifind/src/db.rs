use crate::models::{
    normalize_tg_level, ActivityRecord, AppConfig, ProbeCacheEntry, ResourceItem, ReviewingItem,
    ShieldIndexedFile, ShieldShareRecord,
};
use anyhow::{Context, Result};
use chrono::Local;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Db {
    pub data_dir: PathBuf,
    conn: Connection,
}

pub struct TickLockGuard {
    path: PathBuf,
}

impl Drop for TickLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Db {
    pub fn open(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir).context("创建插件数据目录失败")?;
        let db_path = data_dir.join("medifind.db");
        let conn = Connection::open(&db_path).context("打开 medifind.db 失败")?;
        // Define schema cleanly with explicit indexes
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA busy_timeout = 5000;

            CREATE TABLE IF NOT EXISTS kv (
                key TEXT PRIMARY KEY,
                val TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS probe_cache (
                clean_slug TEXT PRIMARY KEY,
                share_code TEXT NOT NULL,
                is_unlocked INTEGER NOT NULL DEFAULT 0,
                payload TEXT NOT NULL,
                updated_ts INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS provider_cache (
                cache_key TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                updated_ts INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tg_items (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                clean_slug TEXT NOT NULL UNIQUE,
                channel_norm TEXT NOT NULL,
                tmdb_id TEXT NOT NULL,
                search_blob TEXT NOT NULL,
                payload TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tg_tmdb ON tg_items(tmdb_id);
            CREATE INDEX IF NOT EXISTS idx_tg_channel ON tg_items(channel_norm);

            CREATE TABLE IF NOT EXISTS reviewing_queue (
                clean_slug TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                created_ts INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS transferred_slugs (
                clean_slug TEXT PRIMARY KEY,
                ts INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS in_flight_locks (
                lock_key TEXT PRIMARY KEY,
                ts INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS shield_shares (
                id TEXT PRIMARY KEY,
                share_code TEXT NOT NULL,
                payload TEXT NOT NULL,
                updated_ts INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS shield_files (
                share_code TEXT NOT NULL,
                file_id TEXT NOT NULL,
                parent_id TEXT NOT NULL,
                rel_path TEXT NOT NULL,
                file_name TEXT NOT NULL,
                file_size INTEGER NOT NULL,
                sha1 TEXT NOT NULL,
                pure_title TEXT NOT NULL,
                tmdb_id TEXT NOT NULL,
                media_type TEXT NOT NULL,
                parsed_episodes TEXT NOT NULL,
                receive_code TEXT NOT NULL,
                search_blob TEXT NOT NULL,
                PRIMARY KEY (share_code, file_id)
            );
            CREATE INDEX IF NOT EXISTS idx_shield_tmdb ON shield_files(tmdb_id);

            CREATE TABLE IF NOT EXISTS records (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                payload TEXT NOT NULL
            );

            DROP TABLE IF EXISTS poster_cache;
            "#,
        )
        .context("初始化 SQLite 表结构失败")?;

        let _ = fs::remove_dir_all(data_dir.join("posters"));

        let db = Self {
            data_dir: data_dir.to_path_buf(),
            conn,
        };
        db.repair_cached_metadata_v321();
        Ok(db)
    }

    fn repair_cached_metadata_v321(&self) {
        let done: Option<String> = self
            .conn
            .query_row(
                "SELECT val FROM kv WHERE key = 'schema_repair_v321'",
                [],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if done.as_deref() == Some("1") {
            return;
        }

        // 1. Re-clean cached `tg_items` titles, TMDB IDs (with space), and episode ranges (`-E60`/`-E80`)
        if let Ok(mut stmt) = self
            .conn
            .prepare("SELECT seq, payload FROM tg_items")
        {
            let rows: Vec<(i64, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|it| it.flatten().collect())
                .unwrap_or_default();
            for (seq, payload) in rows {
                let Ok(mut item) = serde_json::from_str::<ResourceItem>(&payload) else {
                    continue;
                };
                let mut changed = false;
                if !item.raw_title.trim().is_empty() {
                    let pure = crate::rules::extract_pure_title(&item.raw_title);
                    if !pure.is_empty()
                        && (crate::rules::is_suspect_banner_title(&item.title)
                            || item.title != crate::rules::clean_display_title(&item.title)
                            || item.title.starts_with('[')
                            || item.title.starts_with('【'))
                    {
                        item.title = pure;
                        changed = true;
                    }
                    if item.tmdb_id.trim().is_empty() {
                        let tid = crate::rules::extract_tmdb_id(&item.raw_title);
                        if !tid.is_empty() {
                            item.tmdb_id = tid;
                            changed = true;
                        }
                    }
                    let explicit_movie = item.raw_title.contains("类型：电影")
                        || item.raw_title.contains("类型: 电影")
                        || item.raw_title.contains("🎬 电影")
                        || item.raw_title.contains("[电影]")
                        || item.raw_title.contains("#电影");
                    let new_eps = if explicit_movie {
                        String::new()
                    } else {
                        crate::rules::parse_shallow_episodes(&item.raw_title, "tv", None)
                    };
                    if item.pan_type != "ed2k" && item.parsed_episodes != new_eps {
                        item.parsed_episodes = new_eps;
                        changed = true;
                    }
                    if explicit_movie && item.parsed_episodes.is_empty() && item.media_type != "movie" {
                        item.media_type = "movie".to_string();
                        changed = true;
                    }
                }
                if changed {
                    if let Ok(new_payload) = serde_json::to_string(&item) {
                        let blob =
                            format!("{} {} {}", item.title, item.raw_title, item.remark).to_lowercase();
                        let _ = self.conn.execute(
                            "UPDATE tg_items SET tmdb_id = ?1, search_blob = ?2, payload = ?3 WHERE seq = ?4",
                            params![item.tmdb_id, blob, new_payload, seq],
                        );
                    }
                }
            }
        }

        // 2. Repair `probe_cache` in-place (preserving unlocked HDHive `share_code` & `receive_code`!)
        if let Ok(mut stmt) = self
            .conn
            .prepare("SELECT clean_slug, payload FROM probe_cache")
        {
            let rows: Vec<(String, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|it| it.flatten().collect())
                .unwrap_or_default();
            for (ck, payload) in rows {
                let Ok(mut entry) = serde_json::from_str::<ProbeCacheEntry>(&payload) else {
                    continue;
                };
                let mut changed = false;
                let mut all_pairs = Vec::new();
                for f in &mut entry.files {
                    if f.is_video || f.is_subtitle {
                        let def_s = crate::rules::infer_season_num(&f.path, 1);
                        let mut pairs = crate::rules::extract_episode_pairs(&f.name, def_s);
                        if pairs.is_empty() {
                            pairs = crate::rules::extract_episode_pairs(&f.path, def_s);
                        }
                        let lbl = crate::rules::format_episode_pairs(&pairs);
                        if f.episode_label != lbl {
                            f.episode_label = lbl;
                            changed = true;
                        }
                        if f.is_video {
                            all_pairs.extend(pairs);
                        }
                    }
                }
                let new_eps = crate::rules::format_episode_pairs(&all_pairs);
                if entry.parsed_episodes != new_eps {
                    entry.parsed_episodes = new_eps;
                    changed = true;
                }
                if crate::rules::is_suspect_banner_title(&entry.title) {
                    if let Some(dir) = entry.files.iter().find(|n| n.is_dir && !crate::rules::is_suspect_banner_title(&n.name)) {
                        entry.title = crate::rules::extract_pure_title(&dir.name);
                        changed = true;
                    } else if let Some(vid) = entry.files.iter().find(|n| n.is_video) {
                        let vt = crate::rules::extract_pure_title(&vid.name);
                        if !vt.is_empty() {
                            entry.title = vt;
                            changed = true;
                        }
                    }
                }
                if changed {
                    if let Ok(new_payload) = serde_json::to_string(&entry) {
                        let _ = self.conn.execute(
                            "UPDATE probe_cache SET payload = ?1 WHERE clean_slug = ?2",
                            params![new_payload, ck],
                        );
                    }
                }
            }
        }

        let _ = self.conn.execute("DELETE FROM provider_cache", []);
        let _ = self.conn.execute(
            "INSERT OR REPLACE INTO kv (key, val) VALUES ('schema_repair_v321', '1')",
            [],
        );
    }

    pub fn load_config(&self) -> Result<AppConfig> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT val FROM kv WHERE key = 'app_config'", [], |r| r.get(0))
            .optional()?;
        let mut cfg = match raw {
            Some(s) => serde_json::from_str::<AppConfig>(&s).unwrap_or_default(),
            None => AppConfig::default(),
        };
        if cfg.hdhive.base_url.trim().is_empty() {
            cfg.hdhive.base_url = "https://hdhive.nextemby.com".to_string();
        }
        if cfg.ayclub.base_url.trim().is_empty() {
            cfg.ayclub.base_url = "http://api.ayclub.vip:5050/api/next_find".to_string();
        }
        if cfg.telegram.per_channel_limit == 0 {
            cfg.telegram.per_channel_limit = 40;
        }
        for ch in &mut cfg.telegram.channels {
            ch.level = normalize_tg_level(ch.level);
        }
        cfg.rules.ensure_weights_initialized();
        cfg.rules.sync_subscription_sources(&cfg.telegram.channels);
        Ok(cfg)
    }

    pub fn save_config(&self, cfg: &AppConfig) -> Result<()> {
        let s = serde_json::to_string(cfg)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO kv (key, val) VALUES ('app_config', ?1)",
            params![s],
        )?;
        self.write_dashboard(cfg)?;
        Ok(())
    }

    pub fn get_probe_cache(&self, clean_slug: &str) -> Option<ProbeCacheEntry> {
        let raw: String = self
            .conn
            .query_row(
                "SELECT payload FROM probe_cache WHERE clean_slug = ?1",
                params![clean_slug],
                |r| r.get(0),
            )
            .ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn put_probe_cache(&self, entry: &ProbeCacheEntry) -> Result<()> {
        let payload = serde_json::to_string(entry)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO probe_cache (clean_slug, share_code, is_unlocked, payload, updated_ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entry.clean_slug,
                entry.share_code,
                if entry.is_unlocked { 1 } else { 0 },
                payload,
                entry.updated_ts
            ],
        )?;
        // Prune non-unlocked entries older than 7 days if table exceeds 800 rows
        let cnt: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM probe_cache", [], |r| r.get(0))
            .unwrap_or(0);
        if cnt > 800 {
            let cutoff = chrono::Utc::now().timestamp() - 7 * 86400;
            let _ = self.conn.execute(
                "DELETE FROM probe_cache WHERE is_unlocked = 0 AND updated_ts < ?1",
                params![cutoff],
            );
        }
        Ok(())
    }

    pub fn probe_cache_count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM probe_cache", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn get_provider_cache(&self, key: &str, max_age_secs: i64) -> Option<Vec<ResourceItem>> {
        let (payload, ts): (String, i64) = self
            .conn
            .query_row(
                "SELECT payload, updated_ts FROM provider_cache WHERE cache_key = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        let now = chrono::Utc::now().timestamp();
        if max_age_secs > 0 && now - ts > max_age_secs {
            return None;
        }
        serde_json::from_str(&payload).ok()
    }

    pub fn put_provider_cache(&self, key: &str, items: &[ResourceItem]) -> Result<()> {
        let payload = serde_json::to_string(items)?;
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT OR REPLACE INTO provider_cache (cache_key, payload, updated_ts) VALUES (?1, ?2, ?3)",
            params![key, payload, now],
        )?;
        Ok(())
    }

    pub fn get_json_cache(&self, key: &str, max_age_secs: i64) -> Option<Value> {
        let (payload, ts): (String, i64) = self
            .conn
            .query_row(
                "SELECT payload, updated_ts FROM provider_cache WHERE cache_key = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        let now = chrono::Utc::now().timestamp();
        if max_age_secs > 0 && now - ts > max_age_secs {
            return None;
        }
        serde_json::from_str(&payload).ok()
    }

    pub fn put_json_cache(&self, key: &str, val: &Value) -> Result<()> {
        let payload = serde_json::to_string(val)?;
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT OR REPLACE INTO provider_cache (cache_key, payload, updated_ts) VALUES (?1, ?2, ?3)",
            params![key, payload, now],
        )?;
        Ok(())
    }

    pub fn insert_tg_items_with_channel(
        &self,
        items: &[ResourceItem],
        override_channel_norm: &str,
    ) -> Result<usize> {
        let mut added = 0usize;
        for it in items.iter().rev() {
            if it.clean_slug.is_empty() {
                continue;
            }
            let ch_norm = if !override_channel_norm.trim().is_empty() {
                override_channel_norm.trim().to_lowercase()
            } else {
                crate::providers::normalize_tg_channel(&it.channel_name).to_lowercase()
            };
            let blob = format!("{} {} {}", it.title, it.raw_title, it.remark).to_lowercase();
            let payload = serde_json::to_string(it)?;
            let changed = self.conn.execute(
                "INSERT OR IGNORE INTO tg_items (clean_slug, channel_norm, tmdb_id, search_blob, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![it.clean_slug, ch_norm, it.tmdb_id, blob, payload],
            )?;
            if changed > 0 {
                added += 1;
            }
        }
        // Keep latest 2000 cached TG items
        let _ = self.conn.execute(
            "DELETE FROM tg_items WHERE seq NOT IN (SELECT seq FROM tg_items ORDER BY seq DESC LIMIT 2000)",
            [],
        );
        Ok(added)
    }

    pub fn search_tg_cache(
        &self,
        keyword: &str,
        tmdb_id: &str,
        excluded_channels: &HashSet<String>,
    ) -> Vec<ResourceItem> {
        let kw = keyword.trim().to_lowercase();
        let tid = tmdb_id.trim();
        let mut out = Vec::new();

        let sql = if !tid.is_empty() && kw.is_empty() {
            "SELECT channel_norm, tmdb_id, search_blob, payload FROM tg_items WHERE tmdb_id = ?1 ORDER BY seq DESC LIMIT 400"
        } else if !tid.is_empty() {
            "SELECT channel_norm, tmdb_id, search_blob, payload FROM tg_items WHERE tmdb_id = ?1 OR instr(search_blob, ?2) > 0 ORDER BY seq DESC LIMIT 600"
        } else if !kw.is_empty() {
            "SELECT channel_norm, tmdb_id, search_blob, payload FROM tg_items WHERE instr(search_blob, ?1) > 0 ORDER BY seq DESC LIMIT 600"
        } else {
            "SELECT channel_norm, tmdb_id, search_blob, payload FROM tg_items ORDER BY seq DESC LIMIT 400"
        };
        let Ok(mut stmt) = self.conn.prepare(sql) else {
            return out;
        };
        let map_row = |r: &rusqlite::Row<'_>| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        };
        let rows: Vec<(String, String, String, String)> = if !tid.is_empty() && kw.is_empty() {
            stmt.query_map(params![tid], map_row)
                .map(|it| it.flatten().collect())
                .unwrap_or_default()
        } else if !tid.is_empty() {
            stmt.query_map(params![tid, kw], map_row)
                .map(|it| it.flatten().collect())
                .unwrap_or_default()
        } else if !kw.is_empty() {
            stmt.query_map(params![kw], map_row)
                .map(|it| it.flatten().collect())
                .unwrap_or_default()
        } else {
            stmt.query_map([], map_row)
                .map(|it| it.flatten().collect())
                .unwrap_or_default()
        };

        for (ch_norm, row_tid, blob, payload) in rows {
            if excluded_channels.contains(&ch_norm) {
                continue;
            }
            let matched = if !tid.is_empty() && row_tid == tid {
                true
            } else if !kw.is_empty() {
                blob.contains(&kw)
            } else {
                tid.is_empty()
            };
            if matched {
                if let Ok(mut item) = serde_json::from_str::<ResourceItem>(&payload) {
                    if item.channel_id.is_empty() {
                        item.channel_id = ch_norm;
                    }
                    out.push(item);
                    if out.len() >= 400 {
                        break;
                    }
                }
            }
        }
        out
    }

    pub fn list_tg_items_by_channel(&self, channel_norm: &str, limit: usize) -> Vec<ResourceItem> {
        let norm = channel_norm.trim().to_lowercase();
        if norm.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT channel_norm, payload FROM tg_items WHERE channel_norm = ?1 ORDER BY seq DESC LIMIT ?2",
        ) else {
            return out;
        };
        let Ok(rows) = stmt.query_map(params![norm, limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        }) else {
            return out;
        };
        for (ch_norm, payload) in rows.flatten() {
            if let Ok(mut item) = serde_json::from_str::<ResourceItem>(&payload) {
                if item.channel_id.is_empty() {
                    item.channel_id = ch_norm;
                }
                out.push(item);
            }
        }
        out
    }

    pub fn latest_tg_tmdb_seq_map(&self) -> HashMap<String, i64> {
        let mut map = HashMap::new();
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT tmdb_id, MAX(seq) FROM tg_items WHERE tmdb_id != '' GROUP BY tmdb_id",
        ) else {
            return map;
        };
        let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        else {
            return map;
        };
        for (tid, seq) in rows.flatten() {
            map.insert(tid, seq);
        }
        map
    }

    pub fn find_tg_channel_by_slug(&self, clean_slug: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT channel_norm FROM tg_items WHERE clean_slug = ?1",
                params![clean_slug],
                |r| r.get(0),
            )
            .ok()
    }

    pub fn tg_cached_total(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM tg_items", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn clear_tg_cache(&self) -> Result<usize> {
        let deleted = self.conn.execute("DELETE FROM tg_items", [])?;
        let _ = self
            .conn
            .execute("DELETE FROM provider_cache WHERE cache_key LIKE 'tg:%'", []);
        Ok(deleted)
    }

    pub fn list_reviewing(&self) -> Vec<ReviewingItem> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT payload FROM reviewing_queue ORDER BY created_ts ASC")
        else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
            return Vec::new();
        };
        rows.flatten()
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect()
    }

    pub fn upsert_reviewing(&self, item: &ReviewingItem) -> Result<()> {
        let payload = serde_json::to_string(item)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO reviewing_queue (clean_slug, payload, created_ts) VALUES (?1, ?2, ?3)",
            params![item.clean_slug, payload, item.created_ts],
        )?;
        Ok(())
    }

    pub fn remove_reviewing(&self, clean_slug: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM reviewing_queue WHERE clean_slug = ?1",
            params![clean_slug],
        )?;
        Ok(())
    }

    pub fn is_slug_transferred(&self, clean_slug: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM transferred_slugs WHERE clean_slug = ?1",
                params![clean_slug],
                |_| Ok(true),
            )
            .optional()
            .ok()
            .flatten()
            .unwrap_or(false)
    }

    pub fn mark_slug_transferred(&self, clean_slug: &str, ts: i64) -> Result<()> {
        if !clean_slug.trim().is_empty() {
            self.conn.execute(
                "INSERT OR REPLACE INTO transferred_slugs (clean_slug, ts) VALUES (?1, ?2)",
                params![clean_slug.trim(), ts],
            )?;
        }
        Ok(())
    }

    pub fn unmark_slug_transferred(&self, clean_slug: &str) -> usize {
        let ck = clean_slug.trim();
        if ck.is_empty() {
            return 0;
        }
        self.conn
            .execute(
                "DELETE FROM transferred_slugs WHERE lower(clean_slug) = lower(?1)",
                params![ck],
            )
            .unwrap_or(0)
    }

    pub fn transferred_slugs_count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM transferred_slugs", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn is_in_flight(&self, lock_key: &str, max_age_secs: i64) -> bool {
        let key = lock_key.trim();
        if key.is_empty() || max_age_secs <= 0 {
            return false;
        }
        let now = chrono::Utc::now().timestamp();
        let _ = self.conn.execute(
            "DELETE FROM in_flight_locks WHERE ts < ?1",
            params![now - 86400 * 2],
        );
        let ts: Option<i64> = self
            .conn
            .query_row(
                "SELECT ts FROM in_flight_locks WHERE lock_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        ts.map(|t| now - t <= max_age_secs).unwrap_or(false)
    }

    pub fn mark_in_flight(&self, lock_key: &str) {
        let key = lock_key.trim();
        if key.is_empty() {
            return;
        }
        let now = chrono::Utc::now().timestamp();
        let _ = self.conn.execute(
            "DELETE FROM in_flight_locks WHERE ts < ?1",
            params![now - 86400 * 2],
        );
        let _ = self.conn.execute(
            "INSERT OR REPLACE INTO in_flight_locks (lock_key, ts) VALUES (?1, ?2)",
            params![key, now],
        );
    }

    pub fn clear_in_flight_for_tmdb(&self, tmdb_id: &str) -> usize {
        let tid = tmdb_id.trim();
        if tid.is_empty() {
            return 0;
        }
        let pat_prefix = format!("%:{}:%", tid);
        let pat_suffix = format!("%:{}", tid);
        self.conn
            .execute(
                "DELETE FROM in_flight_locks WHERE lock_key LIKE ?1 OR lock_key LIKE ?2",
                params![pat_prefix, pat_suffix],
            )
            .unwrap_or(0)
    }

    pub fn clear_all_in_flight(&self) -> usize {
        self.conn
            .execute("DELETE FROM in_flight_locks", [])
            .unwrap_or(0)
    }

    pub fn try_acquire_tick_lock(&self) -> Option<TickLockGuard> {
        let lock_path = self.data_dir.join("tick.lock");
        let now = chrono::Utc::now().timestamp();
        let my_pid = std::process::id();

        if let Ok(raw) = fs::read_to_string(&lock_path) {
            let mut parts = raw.split_whitespace();
            let old_pid = parts.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let old_ts = parts.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let pid_alive = old_pid > 0 && Path::new(&format!("/proc/{}", old_pid)).exists();
            if pid_alive && now - old_ts < 1200 {
                return None;
            }
            let _ = fs::remove_file(&lock_path);
        }

        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut f) => {
                use std::io::Write;
                let _ = writeln!(f, "{} {}", my_pid, now);
                Some(TickLockGuard { path: lock_path })
            }
            Err(_) => None,
        }
    }

    pub fn list_shield_shares(&self) -> Vec<ShieldShareRecord> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT payload FROM shield_shares ORDER BY updated_ts DESC")
        else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
            return Vec::new();
        };
        rows.flatten()
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect()
    }

    pub fn upsert_shield_share(&self, rec: &ShieldShareRecord) -> Result<()> {
        let payload = serde_json::to_string(rec)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO shield_shares (id, share_code, payload, updated_ts) VALUES (?1, ?2, ?3, ?4)",
            params![rec.id, rec.share_code.to_lowercase(), payload, rec.updated_ts],
        )?;
        Ok(())
    }

    pub fn delete_shield_share(&self, id_or_code: &str) -> Result<()> {
        let target_code: Option<String> = self
            .conn
            .query_row(
                "SELECT share_code FROM shield_shares WHERE id = ?1 OR lower(share_code) = lower(?1)",
                params![id_or_code],
                |r| r.get(0),
            )
            .optional()?;
        let sc = target_code.unwrap_or_else(|| id_or_code.to_lowercase());
        self.conn.execute(
            "DELETE FROM shield_shares WHERE id = ?1 OR lower(share_code) = lower(?2)",
            params![id_or_code, sc],
        )?;
        self.conn.execute(
            "DELETE FROM shield_files WHERE lower(share_code) = lower(?1)",
            params![sc],
        )?;
        Ok(())
    }

    pub fn insert_shield_files(&self, files: &[ShieldIndexedFile]) -> Result<usize> {
        let mut added = 0usize;
        for f in files {
            let blob = format!("{} {} {}", f.file_name, f.rel_path, f.pure_title).to_lowercase();
            let changed = self.conn.execute(
                r#"INSERT OR IGNORE INTO shield_files
                   (share_code, file_id, parent_id, rel_path, file_name, file_size, sha1, pure_title, tmdb_id, media_type, parsed_episodes, receive_code, search_blob)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
                params![
                    f.share_code,
                    f.file_id,
                    f.parent_id,
                    f.rel_path,
                    f.file_name,
                    f.file_size,
                    f.sha1,
                    f.pure_title,
                    f.tmdb_id,
                    f.media_type,
                    f.parsed_episodes,
                    f.receive_code,
                    blob
                ],
            )?;
            if changed > 0 {
                added += 1;
            }
        }
        Ok(added)
    }

    pub fn query_shield_files(&self, keyword: &str, tmdb_id: &str) -> Vec<ShieldIndexedFile> {
        let kw = keyword.trim().to_lowercase();
        let tid = tmdb_id.trim();
        let mut out = Vec::new();
        let sql = "SELECT share_code, receive_code, file_id, parent_id, rel_path, file_name, file_size, sha1, pure_title, tmdb_id, media_type, parsed_episodes, search_blob FROM shield_files LIMIT 5000";
        let Ok(mut stmt) = self.conn.prepare(sql) else {
            return out;
        };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                ShieldIndexedFile {
                    share_code: r.get(0)?,
                    receive_code: r.get(1)?,
                    file_id: r.get(2)?,
                    parent_id: r.get(3)?,
                    rel_path: r.get(4)?,
                    file_name: r.get(5)?,
                    file_size: r.get(6)?,
                    sha1: r.get(7)?,
                    pure_title: r.get(8)?,
                    tmdb_id: r.get(9)?,
                    media_type: r.get(10)?,
                    parsed_episodes: r.get(11)?,
                },
                r.get::<_, String>(12)?,
            ))
        }) else {
            return out;
        };
        for (f, blob) in rows.flatten() {
            let matched = if !tid.is_empty() && f.tmdb_id == tid {
                true
            } else if !kw.is_empty() {
                blob.contains(&kw)
            } else {
                tid.is_empty()
            };
            if matched {
                out.push(f);
            }
        }
        out
    }

    pub fn query_shield_folder(&self, share_code: &str, parent_id: &str) -> Vec<ShieldIndexedFile> {
        let mut out = Vec::new();
        let sql = "SELECT share_code, receive_code, file_id, parent_id, rel_path, file_name, file_size, sha1, pure_title, tmdb_id, media_type, parsed_episodes FROM shield_files WHERE lower(share_code) = lower(?1)";
        let Ok(mut stmt) = self.conn.prepare(sql) else {
            return out;
        };
        let Ok(rows) = stmt.query_map(params![share_code], |r| {
            Ok(ShieldIndexedFile {
                share_code: r.get(0)?,
                receive_code: r.get(1)?,
                file_id: r.get(2)?,
                parent_id: r.get(3)?,
                rel_path: r.get(4)?,
                file_name: r.get(5)?,
                file_size: r.get(6)?,
                sha1: r.get(7)?,
                pure_title: r.get(8)?,
                tmdb_id: r.get(9)?,
                media_type: r.get(10)?,
                parsed_episodes: r.get(11)?,
            })
        }) else {
            return out;
        };
        for f in rows.flatten() {
            if parent_id == "0" || parent_id.is_empty() || f.parent_id == parent_id {
                out.push(f);
            }
        }
        out
    }

    pub fn shield_files_total(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM shield_files", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn append_activity(&self, mut rec: ActivityRecord) -> Result<()> {
        if rec.id.is_empty() {
            rec.id = format!("rec-{}", chrono::Utc::now().timestamp_millis());
        }
        if rec.updated_at.is_empty() {
            rec.updated_at = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        }
        if rec.record_type.is_empty() {
            rec.record_type = rec.effective_record_type().to_string();
        }
        rec.title = rec.title.trim().to_string();
        let payload = serde_json::to_string(&rec)?;
        self.conn
            .execute("INSERT INTO records (payload) VALUES (?1)", params![payload])?;
        let _ = self.conn.execute(
            "DELETE FROM records WHERE seq NOT IN (SELECT seq FROM records ORDER BY seq DESC LIMIT 1000)",
            [],
        );
        Ok(())
    }

    pub fn append_record(
        &self,
        title: &str,
        source: &str,
        episodes: &str,
        action: &str,
        status: &str,
        detail: &str,
    ) -> Result<()> {
        self.append_activity(ActivityRecord {
            title: title.trim().to_string(),
            source: source.to_string(),
            episodes: episodes.to_string(),
            action: action.to_string(),
            status: status.to_string(),
            detail: detail.to_string(),
            ..Default::default()
        })
    }

    fn load_all_records(&self) -> Vec<ActivityRecord> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT seq, payload FROM records ORDER BY seq DESC")
        else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (seq, raw) in rows.flatten() {
            if let Ok(mut rec) = serde_json::from_str::<ActivityRecord>(&raw) {
                rec.seq = seq;
                rec.record_type = rec.effective_record_type().to_string();
                out.push(rec);
            }
        }
        out
    }

    pub fn get_records_by_seqs(&self, seqs: &[i64]) -> Vec<ActivityRecord> {
        let want: HashSet<i64> = seqs.iter().copied().collect();
        if want.is_empty() {
            return Vec::new();
        }
        self.load_all_records()
            .into_iter()
            .filter(|r| want.contains(&r.seq))
            .collect()
    }

    pub fn record_counts(&self) -> (usize, usize) {
        let all = self.load_all_records();
        let transfer_cnt = all
            .iter()
            .filter(|r| r.effective_record_type() == "transfer")
            .count();
        let task_cnt = all.len().saturating_sub(transfer_cnt);
        (transfer_cnt, task_cnt)
    }

    pub fn list_records(&self, limit: usize) -> Vec<ActivityRecord> {
        self.load_all_records()
            .into_iter()
            .filter(|r| r.effective_record_type() == "transfer")
            .take(limit)
            .collect()
    }

    pub fn matches_history_filter(rec: &ActivityRecord, filter: &crate::models::HistoryFilter) -> bool {
        let want_type = if filter.record_type.trim().eq_ignore_ascii_case("task") {
            "task"
        } else {
            "transfer"
        };
        if rec.effective_record_type() != want_type {
            return false;
        }

        let src = filter.source.trim().to_ascii_lowercase();
        if !src.is_empty() && src != "all" && rec.source_group() != src {
            return false;
        }

        let act = filter.action_group.trim().to_ascii_lowercase();
        if want_type == "transfer" && !act.is_empty() && act != "all" && rec.action_group() != act {
            return false;
        }

        let st = filter.status.trim().to_ascii_lowercase();
        if !st.is_empty() && st != "all" {
            let rec_st = rec.status.trim().to_ascii_lowercase();
            let st_ok = match st.as_str() {
                "error" => matches!(rec_st.as_str(), "error" | "warn" | "failed"),
                other => rec_st == other,
            };
            if !st_ok {
                return false;
            }
        }

        let kw = filter.keyword.trim().to_lowercase();
        if !kw.is_empty() {
            let hay = format!(
                "{} {} {} {} {} {} {} {} {}",
                rec.title,
                rec.source,
                rec.channel_name,
                rec.slug,
                rec.clean_slug,
                rec.tmdb_id,
                rec.episodes,
                rec.action,
                rec.detail
            )
            .to_lowercase();
            if !hay.contains(&kw) {
                return false;
            }
        }
        true
    }

    pub fn query_records(&self, filter: &crate::models::HistoryFilter) -> Value {
        let all = self.load_all_records();
        let mut transfer_total = 0usize;
        let mut task_total = 0usize;
        let mut matched = Vec::new();

        for rec in all {
            if rec.effective_record_type() == "task" {
                task_total += 1;
            } else {
                transfer_total += 1;
            }
            if Self::matches_history_filter(&rec, filter) {
                matched.push(rec);
            }
        }

        let filtered_total = matched.len();
        let lim = if filter.limit == 0 {
            250
        } else {
            filter.limit.clamp(1, 1000)
        };
        matched.truncate(lim);

        json!({
            "items": matched,
            "transfer_total": transfer_total,
            "task_total": task_total,
            "filtered_total": filtered_total,
            "dedupe_locks_total": self.transferred_slugs_count(),
        })
    }

    pub fn delete_records(
        &self,
        seqs: &[i64],
        clear_filtered: Option<&crate::models::HistoryFilter>,
        unlock_dedupe: bool,
    ) -> Result<(usize, usize)> {
        let all = self.load_all_records();
        let seq_set: HashSet<i64> = seqs.iter().copied().collect();
        let mut to_delete = Vec::new();

        for rec in all {
            let hit = if let Some(flt) = clear_filtered {
                Self::matches_history_filter(&rec, flt)
            } else {
                seq_set.contains(&rec.seq)
            };
            if hit {
                to_delete.push(rec);
            }
        }

        let mut deleted = 0usize;
        let mut unlocked = 0usize;

        for rec in &to_delete {
            deleted += self
                .conn
                .execute("DELETE FROM records WHERE seq = ?1", params![rec.seq])
                .unwrap_or(0);
            if unlock_dedupe && rec.effective_record_type() == "transfer" {
                let mut keys_to_unlock = HashSet::new();
                if !rec.clean_slug.trim().is_empty() {
                    keys_to_unlock.insert(rec.clean_slug.trim().to_string());
                }
                if !rec.slug.trim().is_empty() {
                    let ck = crate::rules::clean_slug(&rec.slug);
                    if !ck.is_empty() {
                        keys_to_unlock.insert(ck);
                    }
                    let (sc, _) = crate::rules::parse_115_share(&rec.slug, "");
                    if !sc.is_empty() {
                        keys_to_unlock.insert(format!("115:{}", sc.to_lowercase()));
                    }
                }
                let (sc_det, _) = crate::rules::parse_115_share(&rec.detail, "");
                if !sc_det.is_empty() {
                    keys_to_unlock.insert(format!("115:{}", sc_det.to_lowercase()));
                }
                for k in keys_to_unlock {
                    unlocked += self.unmark_slug_transferred(&k);
                }
                if !rec.tmdb_id.trim().is_empty() {
                    unlocked += self.clear_in_flight_for_tmdb(&rec.tmdb_id);
                }
            }
        }

        // When clearing ALL unfiltered transfer history with unlock_dedupe=true, also purge any orphan dedupe locks
        if unlock_dedupe {
            if let Some(flt) = clear_filtered {
                let is_unfiltered_transfer = flt.record_type.trim() != "task"
                    && (flt.source.trim().is_empty() || flt.source.trim() == "all")
                    && (flt.action_group.trim().is_empty() || flt.action_group.trim() == "all")
                    && (flt.status.trim().is_empty() || flt.status.trim() == "all")
                    && flt.keyword.trim().is_empty();
                if is_unfiltered_transfer {
                    unlocked += self
                        .conn
                        .execute("DELETE FROM transferred_slugs", [])
                        .unwrap_or(0);
                    unlocked += self.clear_all_in_flight();
                }
            }
        }

        Ok((deleted, unlocked))
    }

    pub fn write_dashboard(&self, cfg: &AppConfig) -> Result<()> {
        let mut enabled_channels = 0usize;
        if cfg.hdhive.enabled && !cfg.hdhive.api_key.trim().is_empty() {
            enabled_channels += 1;
        }
        if cfg.telegram.enabled && cfg.telegram.channels.iter().any(|c| c.enabled) {
            enabled_channels += 1;
        }
        if cfg.ayclub.enabled && !cfg.ayclub.nf_token.trim().is_empty() {
            enabled_channels += 1;
        }
        if cfg.rss_feeds.iter().any(|f| f.enabled) {
            enabled_channels += 1;
        }

        let cached_total = self.tg_cached_total() + self.probe_cache_count();
        let reviewing_cnt = self.list_reviewing().len();
        let shield_cnt = self.shield_files_total();
        let recs = self.list_records(100);

        let payload = json!({
            "summary": {
                "enabled_channels": enabled_channels,
                "cached_resources": cached_total,
                "reviewing_count": reviewing_cnt,
                "shield_files": shield_cnt,
                "transferred_count": cfg.transferred_count,
                "last_result": cfg.last_result,
            },
            "items": recs,
        });
        let dash_path = self.data_dir.join("dashboard.json");
        let tmp = self.data_dir.join("dashboard.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&payload)?)?;
        fs::rename(&tmp, &dash_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlite_wal_config_and_probe_cache() {
        let dir = std::env::temp_dir().join(format!(
            "medifind-test-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(1)
        ));
        fs::create_dir_all(&dir).unwrap();

        let db = Db::open(&dir).unwrap();
        let mut cfg = AppConfig::default();
        cfg.hdhive.enabled = true;
        cfg.hdhive.api_key = "test_hd_key".into();
        cfg.hdhive.points = 88;
        cfg.telegram.channels.push(crate::models::TGChannelEntry {
            id: "ch-1".into(),
            identifier: "Lsp115".into(),
            name: "Lsp115影视".into(),
            level: 2,
            enabled: true,
            ..Default::default()
        });
        db.save_config(&cfg).unwrap();
        db.put_probe_cache(&ProbeCacheEntry {
            clean_slug: "abcslug".into(),
            share_code: "sw999".into(),
            receive_code: "1234".into(),
            is_unlocked: true,
            updated_ts: 1000,
            ..Default::default()
        })
        .unwrap();

        let loaded = db.load_config().unwrap();
        assert_eq!(loaded.hdhive.api_key, "test_hd_key");
        assert_eq!(loaded.hdhive.points, 88);
        assert_eq!(loaded.telegram.channels[0].level, 48);
        let cached = db.get_probe_cache("abcslug").unwrap();
        assert_eq!(cached.share_code, "sw999");

        // Verify separated transfer vs task history, multi-condition filtering, and dedupe + in-flight lock release
        {
            let _guard = db.try_acquire_tick_lock().expect("first tick lock");
            assert!(db.try_acquire_tick_lock().is_none(), "second tick lock must block");
        }
        assert!(db.try_acquire_tick_lock().is_some(), "tick lock released on drop");

        db.mark_slug_transferred("115:sw888888", 2000).unwrap();
        db.mark_in_flight("tv:12345:S02E01");
        assert!(db.is_slug_transferred("115:sw888888"));
        assert!(db.is_in_flight("tv:12345:S02E01", 3600));
        db.append_activity(ActivityRecord {
            record_type: "transfer".into(),
            title: "庆余年 第二季".into(),
            source: "telegram".into(),
            channel_name: "115搬搬群组".into(),
            slug: "https://115.com/s/sw888888?password=abcd".into(),
            clean_slug: "115:sw888888".into(),
            tmdb_id: "12345".into(),
            media_type: "tv".into(),
            episodes: "S02E01-E36".into(),
            action: "订阅自动追更".into(),
            status: "success".into(),
            detail: "已接收至中转目录".into(),
            ..Default::default()
        })
        .unwrap();
        db.append_record("HDHive 每日签到", "hdhive", "", "每日签到", "success", "+10 积分")
            .unwrap();

        let q_trans = db.query_records(&crate::models::HistoryFilter {
            record_type: "transfer".into(),
            source: "telegram".into(),
            action_group: "chase".into(),
            keyword: "115搬搬".into(),
            ..Default::default()
        });
        assert_eq!(q_trans["transfer_total"], 1);
        assert_eq!(q_trans["task_total"], 1);
        assert_eq!(q_trans["filtered_total"], 1);
        let seq = q_trans["items"][0]["seq"].as_i64().unwrap();

        let (del_cnt, unlocked_cnt) = db.delete_records(&[seq], None, true).unwrap();
        assert_eq!(del_cnt, 1);
        assert!(unlocked_cnt >= 1);
        assert!(!db.is_slug_transferred("115:sw888888"));
        assert!(!db.is_in_flight("tv:12345:S02E01", 3600));
        assert_eq!(db.record_counts(), (0, 1));

        // Verify search_tg_cache searches full cache beyond 800 rows by tmdb_id and keyword
        let old_target = vec![ResourceItem {
            slug: "https://115.com/s/swoldtarget01?password=abcd".into(),
            clean_slug: "115:swoldtarget01".into(),
            title: "远古经典剧集".into(),
            tmdb_id: "987654".into(),
            media_type: "tv".into(),
            source_type: "telegram".into(),
            ..Default::default()
        }];
        db.insert_tg_items_with_channel(&old_target, "lsp115").unwrap();
        let mut bulk_tg = Vec::with_capacity(850);
        for i in 0..850 {
            bulk_tg.push(ResourceItem {
                slug: format!("https://115.com/s/swfill{:06}?password=1234", i),
                clean_slug: format!("115:swfill{:06}", i),
                title: format!("填充片名 {}", i),
                tmdb_id: format!("{}", 100000 + i),
                media_type: "movie".into(),
                source_type: "telegram".into(),
                ..Default::default()
            });
        }
        db.insert_tg_items_with_channel(&bulk_tg, "lsp115").unwrap();
        let empty_excl = HashSet::new();
        let by_tid = db.search_tg_cache("", "987654", &empty_excl);
        assert_eq!(by_tid.len(), 1);
        assert_eq!(by_tid[0].clean_slug, "115:swoldtarget01");
        let by_kw = db.search_tg_cache("远古经典", "", &empty_excl);
        assert_eq!(by_kw.len(), 1);
        assert_eq!(by_kw[0].tmdb_id, "987654");

        let _ = fs::remove_dir_all(&dir);
    }
}

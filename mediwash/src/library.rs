use crate::category;
use crate::db;
use crate::host::{HostClient, LibraryItem, MediaStream};
use crate::models::{format_ts_cn, now_ts, AppConfig, PluginSettings};
use crate::rules::{self, MediaProfile, ReferenceBaseline, Scorer};
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanJobStatus {
    pub scope: String,
    pub revision: i64,
    pub cursor_start: usize,
    pub total: usize,
    pub scanned: usize,
    pub multi_source_items: usize,
    pub suggestion_groups: usize,
    pub suggestion_sources: usize,
    pub review_groups: usize,
    pub mode: String,
    pub warning: String,
    pub updated_at: String,
}

pub fn get_scan_jobs(db: &Connection) -> BTreeMap<String, ScanJobStatus> {
    let mut out = BTreeMap::new();
    for scope in ["movie", "tv"] {
        let job = read_job(db, scope).unwrap_or_else(|| ScanJobStatus {
            scope: scope.to_string(),
            mode: "idle".to_string(),
            ..Default::default()
        });
        out.insert(scope.to_string(), job);
    }
    out
}

pub fn read_job(db: &Connection, scope: &str) -> Option<ScanJobStatus> {
    db.query_row(
        "SELECT scope, revision, cursor_start, total, scanned, multi_source_items,
                suggestion_groups, suggestion_sources, review_groups, mode, warning, updated_at
         FROM lib_scan_jobs WHERE scope = ?",
        [scope],
        |r| {
            let ts: u64 = r.get(11)?;
            Ok(ScanJobStatus {
                scope: r.get(0)?,
                revision: r.get(1)?,
                cursor_start: r.get::<_, i64>(2)?.max(0) as usize,
                total: r.get::<_, i64>(3)?.max(0) as usize,
                scanned: r.get::<_, i64>(4)?.max(0) as usize,
                multi_source_items: r.get::<_, i64>(5)?.max(0) as usize,
                suggestion_groups: r.get::<_, i64>(6)?.max(0) as usize,
                suggestion_sources: r.get::<_, i64>(7)?.max(0) as usize,
                review_groups: r.get::<_, i64>(8)?.max(0) as usize,
                mode: r.get(9)?,
                warning: r.get(10)?,
                updated_at: format_ts_cn(ts),
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

pub fn control_scan(db: &mut Connection, cfg: &AppConfig, scope: &str, op: &str) -> Result<ScanJobStatus> {
    ensure!(matches!(scope, "movie" | "tv"), "扫描范围仅支持 movie 或 tv");
    let now = now_ts();
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match op {
        "start" | "restart" => {
            tx.execute("DELETE FROM lib_decisions WHERE scope = ?", [scope])?;
            tx.execute(
                "INSERT INTO lib_scan_jobs (
                    scope, revision, cursor_start, total, scanned, multi_source_items,
                    suggestion_groups, suggestion_sources, review_groups, mode, warning, updated_at
                 ) VALUES (?, ?, 0, 0, 0, 0, 0, 0, 0, 'active', '', ?)
                 ON CONFLICT(scope) DO UPDATE SET
                    revision=excluded.revision, cursor_start=0, total=0, scanned=0,
                    multi_source_items=0, suggestion_groups=0, suggestion_sources=0,
                    review_groups=0, mode='active', warning='', updated_at=excluded.updated_at",
                params![scope, cfg.revision, now],
            )?;
        }
        "pause" => {
            tx.execute(
                "UPDATE lib_scan_jobs SET mode = 'paused', updated_at = ? WHERE scope = ?",
                params![now, scope],
            )?;
        }
        "resume" => {
            let exists = read_job(&tx, scope).is_some();
            if exists {
                tx.execute(
                    "UPDATE lib_scan_jobs SET mode = 'active', warning = '', updated_at = ? WHERE scope = ?",
                    params![now, scope],
                )?;
            } else {
                tx.execute(
                    "INSERT INTO lib_scan_jobs (scope, revision, mode, updated_at) VALUES (?, ?, 'active', ?)",
                    params![scope, cfg.revision, now],
                )?;
            }
        }
        _ => bail!("不支持的扫描控制操作: {op}"),
    }
    tx.commit()?;
    read_job(db, scope).context("读取扫描任务失败")
}

/// Runs a bounded step of the resumable library multi-version scan WITHOUT storing bloated `keep` rows.
pub fn scan_step(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    scope: &str,
    budget_secs: u64,
) -> Result<ScanJobStatus> {
    let Some(mut job) = read_job(db, scope) else {
        bail!("请先启动 {scope} 存量库扫描");
    };
    if job.mode != "active" {
        return Ok(job);
    }

    let clock = Instant::now();
    job.warning.clear();

    while clock.elapsed() < Duration::from_secs(budget_secs.max(5)) {
        // Mediary 3.4.6+: Query only multi-version items via indexed SQL filter (`has_multiple_versions=true`)!
        let page = match host
            .library_page_filtered(job.cursor_start, 50, scope, "", true, "", "")
            .or_else(|_| host.library_page(job.cursor_start, 50, scope, ""))
        {
            Ok(p) => p,
            Err(err) => {
                job.warning = format!("读取媒体库分页超时或异常，下轮自动从 {} 续扫（{err:#}）", job.cursor_start);
                break;
            }
        };
        job.total = page.total;
        if page.items.is_empty() || job.cursor_start >= page.total {
            job.mode = "done".to_string();
            break;
        }

        let mut processed_in_page = 0usize;
        for summary_item in &page.items {
            if clock.elapsed() >= Duration::from_secs(budget_secs.max(5)) {
                break;
            }
            // Fast path: if list endpoint already included sources and there is only 1 source, skip detail fetch!
            let list_src_count = summary_item.effective_sources().len();
            if list_src_count == 1 {
                job.scanned += 1;
                job.cursor_start += 1;
                processed_in_page += 1;
                continue;
            }

            let detail = match host.library_item_detail(&summary_item.id) {
                Ok(d) => d,
                Err(err) => {
                    job.warning = format!("读取条目 {} 详情未完成，下轮续扫（{err:#}）", summary_item.id);
                    break;
                }
            };
            if detail.effective_sources().len() > 1 {
                job.multi_source_items += 1;
                let groups = evaluate_item_groups(&detail, scope, cfg)?;
                for grp in groups {
                    // LEAN STORAGE: Only persist actionable `suggestion` and `review` groups!
                    if !matches!(grp.status, "suggestion" | "review") {
                        continue;
                    }
                    save_decision_row(db, scope, &grp)?;
                }
            }
            job.scanned += 1;
            job.cursor_start += 1;
            processed_in_page += 1;
        }

        // Refresh live counts from `lib_decisions`
        refresh_job_counts(db, &mut job)?;
        if !job.warning.is_empty() {
            break;
        }
        if job.cursor_start >= job.total || processed_in_page == 0 {
            if job.cursor_start >= job.total {
                job.mode = "done".to_string();
                db::log_task_history(
                    db,
                    "存量精洗",
                    if scope == "tv" { "剧集媒体库全扫" } else { "电影媒体库全扫" },
                    scope,
                    "lib_scan",
                    "done",
                    &format!(
                        "共扫描 {} 条（多版本 {} 条，拟清理 {} 组 / {} 个重复源）",
                        job.scanned, job.multi_source_items, job.suggestion_groups, job.suggestion_sources
                    ),
                );
            }
            break;
        }
        // Check if user paused concurrently from UI
        if let Some(live) = read_job(db, scope) {
            if live.mode == "paused" {
                job.mode = "paused".to_string();
                break;
            }
        }
    }

    refresh_job_counts(db, &mut job)?;
    db.execute(
        "UPDATE lib_scan_jobs SET
            cursor_start=?, total=?, scanned=?, multi_source_items=?,
            suggestion_groups=?, suggestion_sources=?, review_groups=?,
            mode=?, warning=?, updated_at=?
         WHERE scope=?",
        params![
            job.cursor_start as i64,
            job.total as i64,
            job.scanned as i64,
            job.multi_source_items as i64,
            job.suggestion_groups as i64,
            job.suggestion_sources as i64,
            job.review_groups as i64,
            job.mode,
            job.warning,
            now_ts(),
            scope,
        ],
    )?;
    read_job(db, scope).context("更新扫描状态失败")
}

fn refresh_job_counts(db: &Connection, job: &mut ScanJobStatus) -> Result<()> {
    let (sug_g, sug_s, rev_g): (i64, i64, i64) = db.query_row(
        "SELECT
            COUNT(*) FILTER (WHERE status = 'suggestion' AND state IN ('pending','approved')),
            COALESCE(SUM(source_count) FILTER (WHERE status = 'suggestion' AND state IN ('pending','approved')), 0),
            COUNT(*) FILTER (WHERE status = 'review' AND state = 'pending')
         FROM lib_decisions WHERE scope = ?",
        [&job.scope],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    job.suggestion_groups = sug_g.max(0) as usize;
    job.suggestion_sources = sug_s.max(0) as usize;
    job.review_groups = rev_g.max(0) as usize;
    Ok(())
}

pub struct DecisionGroup {
    pub key: String,
    pub item_id: String,
    pub title: String,
    pub library_id: String,
    pub category: String,
    pub slot: String,
    pub status: &'static str,
    pub reason: String,
    pub winner_score: f64,
    pub loser_score: f64,
    pub retained: Vec<Value>,
    pub candidates: Vec<Value>,
}

pub fn evaluate_item_groups(
    item: &LibraryItem,
    media_type: &str,
    cfg: &AppConfig,
) -> Result<Vec<DecisionGroup>> {
    let sources = item.effective_sources();
    if sources.len() <= 1 {
        return Ok(Vec::new());
    }

    let mut by_category: BTreeMap<String, Vec<(&crate::host::LibrarySource, MediaProfile, String)>> =
        BTreeMap::new();
    let mut unclassified_sources = Vec::new();

    for src in sources {
        let paths = [
            src.strm_path.as_str(),
            src.cloud_path.as_str(),
            src.file_path.as_str(),
        ];
        let cat_opt = if cfg.host_sync.categories.is_empty() {
            Some("默认分类".to_string())
        } else if paths.iter().any(|p| !p.trim().is_empty()) {
            category::locate(&cfg.host_sync.categories, media_type, &paths)
        } else {
            category::locate(&cfg.host_sync.categories, media_type, &[&item.path])
        };

        let disp_path = paths
            .into_iter()
            .find(|p| !p.trim().is_empty())
            .unwrap_or(&item.path)
            .to_string();
        let refs: Vec<&MediaStream> = item
            .streams
            .iter()
            .filter(|st| !src.id.is_empty() && st.media_source_id == src.id)
            .collect();
        let prof = MediaProfile::extract(src, &refs);

        if let Some(cat) = cat_opt {
            by_category
                .entry(cat)
                .or_default()
                .push((src, prof, disp_path));
        } else {
            unclassified_sources.push((src, prof, disp_path));
        }
    }

    let mut out = Vec::new();

    for (cat, cat_entries) in by_category {
        if cat_entries.len() <= 1 {
            continue;
        }
        let eff = cfg.strategy.effective_for(
            media_type,
            &cat,
            &cfg.host_sync.categories,
            &cfg.rules,
        );
        if eff.skip_library_wash {
            continue;
        }
        let rule = rules::find_rule(cfg, &eff.rule_id)?;
        let scorer = Scorer::new(rule)?;

        // Group by slot within this category
        let mut by_slot: BTreeMap<String, Vec<(&crate::host::LibrarySource, MediaProfile, String)>> =
            BTreeMap::new();
        for (src, prof, path) in cat_entries {
            let init = scorer.score(&prof, &ReferenceBaseline::default(), &cat, eff.iso_protected);
            by_slot.entry(init.slot).or_default().push((src, prof, path));
        }

        for (slot, slot_entries) in by_slot {
            if slot_entries.len() <= eff.keep_per_slot {
                continue;
            }

            let mut shared_ref = ReferenceBaseline::default();
            for (_, p, _) in &slot_entries {
                shared_ref.observe(p);
            }
            let max_sz = slot_entries
                .iter()
                .map(|(_, p, _)| p.size)
                .fold(0.0f64, f64::max);

            let mut scored = Vec::new();
            let mut any_missing_id = false;
            let mut any_weak_evidence = false;

            let first_has_br = slot_entries[0].1.bitrate > 0.0;
            for (src, prof, path) in slot_entries {
                if src.id.is_empty() {
                    any_missing_id = true;
                }
                if prof.resolution.is_empty()
                    || (rule.kind == rules::MEDIATIDY && (prof.bitrate > 0.0) != first_has_br && prof.size == 0.0)
                {
                    any_weak_evidence = true;
                }
                let r = if rule.kind == rules::SYMEDIA {
                    ReferenceBaseline {
                        size: if (prof.size - max_sz).abs() < 1.0 {
                            0.0
                        } else {
                            max_sz
                        },
                        ..shared_ref.clone()
                    }
                } else {
                    shared_ref.clone()
                };
                let sc = scorer.score(&prof, &r, &cat, eff.iso_protected);
                scored.push((src, prof, path, sc));
            }

            scored.sort_by(|a, b| {
                b.3.total
                    .total_cmp(&a.3.total)
                    .then_with(|| a.0.id.cmp(&b.0.id))
            });

            let cutoff_idx = eff.keep_per_slot.min(scored.len()) - 1;
            let cutoff_score = scored[cutoff_idx].3.total;
            let mut retained = Vec::new();
            let mut retained_srcs: Vec<&crate::host::LibrarySource> = Vec::new();
            let mut candidates = Vec::new();
            let mut loser_score = 0.0f64;
            let mut skipped_same_cloud = false;

            for (idx, (src, prof, path, sc)) in scored.into_iter().enumerate() {
                let entry_json = json!({
                    "id": src.id,
                    "name": prof.display_name,
                    "path": path,
                    "score": sc.total,
                    "slot": sc.slot,
                    "breakdown": sc.breakdown,
                    "profile": prof.summary_json(),
                });
                // SAFETY: Never mark a `.strm` / source for deletion if it points to the EXACT SAME
                // 115 `pick_code` / `file_id` as a retained winner (e.g. coexisting old/new `.strm`
                // after in-place 115 rename before FlowLink's regular sync cleans stale `.strm`).
                let same_cloud_as_winner = retained_srcs.iter().any(|w| w.shares_cloud_identity(src));
                if same_cloud_as_winner {
                    skipped_same_cloud = true;
                }
                let can_clean = idx >= eff.keep_per_slot
                    && !same_cloud_as_winner
                    && !any_missing_id
                    && !any_weak_evidence
                    && sc.total < cutoff_score
                    && rules::round2(cutoff_score - sc.total) >= eff.min_diff;
                if can_clean {
                    if loser_score == 0.0 || sc.total > loser_score {
                        loser_score = sc.total;
                    }
                    candidates.push(entry_json);
                } else {
                    retained_srcs.push(src);
                    retained.push(entry_json);
                }
            }

            let (status, reason) = if any_missing_id || any_weak_evidence {
                (
                    "review",
                    "多源条目缺少源 ID、分辨率或码率证据不一致，保留整组供人工核对".to_string(),
                )
            } else if !candidates.is_empty() {
                (
                    "suggestion",
                    format!(
                        "同分类 [{}] 同槽位 [{}] 保留 {:.2} 分版本，拟清理 {} 个低分重复源（最低分差 {:.2}）",
                        cat,
                        slot,
                        cutoff_score,
                        candidates.len(),
                        eff.min_diff
                    ),
                )
            } else if skipped_same_cloud {
                (
                    "keep",
                    "指向同一 115 云端文件的重命名残影 STRM，已保护禁删并交由 STRM 常规同步清理".to_string(),
                )
            } else {
                ("keep", "分差未达洗版门槛".to_string())
            };

            if status != "keep" {
                out.push(DecisionGroup {
                    key: format!("{}:{}:{}", item.id, cat, slot),
                    item_id: item.id.clone(),
                    title: if item.item_type.eq_ignore_ascii_case("episode") {
                        format!(
                            "{} S{:02}E{:02}",
                            item.name,
                            item.effective_season(),
                            item.effective_episode()
                        )
                    } else {
                        item.name.clone()
                    },
                    library_id: item.library_id.clone(),
                    category: cat.clone(),
                    slot,
                    status,
                    reason,
                    winner_score: cutoff_score,
                    loser_score,
                    retained,
                    candidates,
                });
            }
        }
    }

    if unclassified_sources.len() > 1 {
        let retained: Vec<Value> = unclassified_sources
            .into_iter()
            .map(|(src, prof, path)| {
                json!({
                    "id": src.id,
                    "name": prof.display_name,
                    "path": path,
                    "score": 0.0,
                    "profile": prof.summary_json(),
                })
            })
            .collect();
        out.push(DecisionGroup {
            key: format!("{}::unclassified", item.id),
            item_id: item.id.clone(),
            title: item.name.clone(),
            library_id: item.library_id.clone(),
            category: "未匹配分类".to_string(),
            slot: "-".to_string(),
            status: "review",
            reason: "多源路径无法唯一对应 Mediary 二级分类或跨技术区冲突，请核对".to_string(),
            winner_score: 0.0,
            loser_score: 0.0,
            retained,
            candidates: Vec::new(),
        });
    }

    Ok(out)
}

fn save_decision_row(db: &Connection, scope: &str, grp: &DecisionGroup) -> Result<()> {
    let body = json!({
        "retained": grp.retained,
        "candidates": grp.candidates,
    });
    db.execute(
        "INSERT INTO lib_decisions (
            scope, decision_key, item_id, title, library_id, category, slot,
            status, reason, winner_score, loser_score, source_count, body_json, state, updated_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)
         ON CONFLICT(decision_key) DO UPDATE SET
            title=excluded.title, library_id=excluded.library_id, category=excluded.category,
            slot=excluded.slot, status=excluded.status, reason=excluded.reason,
            winner_score=excluded.winner_score, loser_score=excluded.loser_score,
            source_count=excluded.source_count, body_json=excluded.body_json,
            state=CASE WHEN lib_decisions.state = 'dismissed' THEN 'dismissed' ELSE 'pending' END,
            updated_at=excluded.updated_at",
        params![
            scope,
            grp.key,
            grp.item_id,
            grp.title,
            grp.library_id,
            grp.category,
            grp.slot,
            grp.status,
            grp.reason,
            grp.winner_score,
            grp.loser_score,
            grp.candidates.len() as i64,
            body.to_string(),
            now_ts(),
        ],
    )?;
    Ok(())
}

pub fn list_decisions(
    db: &Connection,
    scope: &str,
    status: &str,
    state: &str,
    category_prefix: &str,
    after_id: i64,
    limit: usize,
) -> Result<Value> {
    let mut where_sql = String::from(" WHERE 1=1");
    let mut count_params: Vec<String> = Vec::new();
    if !scope.is_empty() && scope != "all" {
        where_sql.push_str(&format!(" AND scope = ?{}", count_params.len() + 1));
        count_params.push(scope.to_string());
    }
    if !status.is_empty() && status != "all" {
        where_sql.push_str(&format!(" AND status = ?{}", count_params.len() + 1));
        count_params.push(status.to_string());
    }
    if !state.is_empty() && state != "all" {
        where_sql.push_str(&format!(" AND state = ?{}", count_params.len() + 1));
        count_params.push(state.to_string());
    }
    if !category_prefix.trim().is_empty() {
        where_sql.push_str(&format!(" AND category LIKE ?{}", count_params.len() + 1));
        count_params.push(format!("{}%", category_prefix.trim().trim_matches('/')));
    }

    let total: i64 = db.query_row(
        &format!("SELECT COUNT(*) FROM lib_decisions{where_sql}"),
        rusqlite::params_from_iter(count_params.iter()),
        |r| r.get(0),
    )?;

    let mut page_params = count_params;
    let mut page_where = where_sql;
    if after_id > 0 {
        page_where.push_str(&format!(" AND id > ?{}", page_params.len() + 1));
        page_params.push(after_id.to_string());
    }
    let lim = limit.clamp(1, 100);
    let sql = format!(
        "SELECT id, scope, item_id, title, category, slot, status, reason,
                winner_score, loser_score, source_count, body_json, state, updated_at
         FROM lib_decisions{} ORDER BY id ASC LIMIT {}",
        page_where,
        lim + 1
    );
    let mut stmt = db.prepare(&sql)?;
    let mut rows_iter = stmt.query(rusqlite::params_from_iter(page_params.iter()))?;
    let mut items = Vec::new();
    let mut has_more = false;
    let mut next_cursor = after_id;

    while let Some(r) = rows_iter.next()? {
        if items.len() == lim {
            has_more = true;
            break;
        }
        let id: i64 = r.get(0)?;
        next_cursor = id;
        let body: Value =
            serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_else(|_| json!({}));
        let ts: u64 = r.get(13)?;
        items.push(json!({
            "id": id,
            "scope": r.get::<_, String>(1)?,
            "item_id": r.get::<_, String>(2)?,
            "title": r.get::<_, String>(3)?,
            "category": r.get::<_, String>(4)?,
            "slot": r.get::<_, String>(5)?,
            "status": r.get::<_, String>(6)?,
            "reason": r.get::<_, String>(7)?,
            "winner_score": r.get::<_, f64>(8)?,
            "loser_score": r.get::<_, f64>(9)?,
            "source_count": r.get::<_, i64>(10)?,
            "retained": body["retained"],
            "candidates": body["candidates"],
            "state": r.get::<_, String>(12)?,
            "updated_at": format_ts_cn(ts),
        }));
    }

    Ok(json!({
        "total": total,
        "items": items,
        "has_more": has_more,
        "next_cursor": next_cursor,
    }))
}

/// Approves or executes library source cleanup (single, explicit IDs, or entire filtered category scope).
/// Re-verifies every item live against Mediary right before deletion so the last source is NEVER deleted!
pub fn act_decisions(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    ids: &[i64],
    all_scope: Option<&str>,
    category_prefix: &str,
    operation: &str,
) -> Result<Value> {
    let target_ids: Vec<i64> = if !ids.is_empty() {
        ids.to_vec()
    } else if let Some(sc) = all_scope {
        let mut sql = String::from(
            "SELECT id FROM lib_decisions WHERE status = 'suggestion' AND state = 'pending'",
        );
        let mut ps: Vec<String> = Vec::new();
        if !sc.is_empty() && sc != "all" {
            sql.push_str(&format!(" AND scope = ?{}", ps.len() + 1));
            ps.push(sc.to_string());
        }
        if !category_prefix.trim().is_empty() {
            sql.push_str(&format!(" AND category LIKE ?{}", ps.len() + 1));
            ps.push(format!("{}%", category_prefix.trim().trim_matches('/')));
        }
        let mut stmt = db.prepare(&sql)?;
        let collected: Vec<i64> = stmt
            .query_map(rusqlite::params_from_iter(ps.iter()), |r| r.get(0))?
            .filter_map(Result::ok)
            .collect();
        collected
    } else {
        Vec::new()
    };
    ensure!(!target_ids.is_empty(), "没有匹配的待处理存量精洗记录");

    let now = now_ts();
    if operation == "dismiss" {
        let mut n = 0usize;
        for id in &target_ids {
            n += db.execute(
                "UPDATE lib_decisions SET state = 'dismissed', updated_at = ? WHERE id = ?",
                params![now, id],
            )?;
        }
        for sc in ["movie", "tv"] {
            if let Some(mut j) = read_job(db, sc) {
                let _ = refresh_job_counts(db, &mut j);
                let _ = db.execute(
                    "UPDATE lib_scan_jobs SET suggestion_groups=?, suggestion_sources=?, review_groups=? WHERE scope=?",
                    params![j.suggestion_groups as i64, j.suggestion_sources as i64, j.review_groups as i64, sc],
                );
            }
        }
        return Ok(json!({ "dismissed": n }));
    }

    ensure!(
        !settings.dry_run,
        "当前处于「仅预览模式（干跑）」，请先在插件配置关闭「仅预览模式」后再执行删旧"
    );

    if target_ids.len() > 15 && operation == "execute" {
        // Mark all as `approved` and immediately run the first batch synchronously; remaining run in `scan_library`!
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for id in &target_ids {
            tx.execute(
                "UPDATE lib_decisions SET state = 'approved', updated_at = ? WHERE id = ? AND status = 'suggestion' AND state = 'pending'",
                params![now, id],
            )?;
        }
        tx.commit()?;
        let res = process_approved_cleanups(db, host, cfg, settings, 20)?;
        return Ok(json!({
            "queued_batch": target_ids.len(),
            "cleaned_now": res["cleaned_groups"],
            "deleted_sources": res["deleted_sources"],
            "remaining_approved": res["remaining_approved"],
        }));
    }

    for id in &target_ids {
        db.execute(
            "UPDATE lib_decisions SET state = 'approved', updated_at = ? WHERE id = ? AND status = 'suggestion' AND state IN ('pending','approved')",
            params![now, id],
        )?;
    }
    process_approved_cleanups(db, host, cfg, settings, 25)
}

pub fn process_approved_cleanups(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    max_groups: usize,
) -> Result<Value> {
    if settings.dry_run {
        return Ok(json!({ "cleaned_groups": 0, "deleted_sources": 0, "remaining_approved": 0 }));
    }
    let clock = Instant::now();
    let mut stmt = db.prepare(
        "SELECT id, scope, item_id, title, category, slot, body_json
         FROM lib_decisions
         WHERE status = 'suggestion' AND state = 'approved'
         ORDER BY id ASC LIMIT ?",
    )?;
    let rows: Vec<(i64, String, String, String, String, String, String)> = stmt
        .query_map([max_groups.clamp(1, 50)], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .filter_map(Result::ok)
        .collect();
    drop(stmt);

    let mut cleaned_groups = 0usize;
    let mut deleted_sources = 0usize;

    for (did, scope, item_id, title, category, slot, body_raw) in rows {
        if clock.elapsed() >= Duration::from_secs(32) {
            break;
        }
        let one_res = (|| -> Result<(usize, String, String)> {
            let live_item = host.library_item_detail(&item_id)?;
            let fresh_groups = evaluate_item_groups(&live_item, &scope, cfg)?;
            let matched_grp = fresh_groups
                .iter()
                .find(|g| g.category == category && g.slot == slot && g.status == "suggestion")
                .context("实时复核发现该分类/槽位版本已变化或无需清理")?;

            let prior_body: Value = serde_json::from_str(&body_raw)?;
            let approved_ids: BTreeSet<String> = prior_body["candidates"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect();
            let live_cand_ids: BTreeSet<String> = matched_grp
                .candidates
                .iter()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect();

            let mut to_delete: Vec<String> = approved_ids
                .intersection(&live_cand_ids)
                .cloned()
                .collect();
            let live_srcs = live_item.effective_sources();
            let retained_live: Vec<&crate::host::LibrarySource> = live_srcs
                .iter()
                .filter(|s| !to_delete.contains(&s.id))
                .collect();
            to_delete.retain(|del_id| {
                let Some(cand_src) = live_srcs.iter().find(|s| &s.id == del_id) else {
                    return false;
                };
                !retained_live.iter().any(|w| w.shares_cloud_identity(cand_src))
            });
            ensure!(!to_delete.is_empty(), "拟清理源 ID 与实时复核结果不一致（或与保留源共享同一 115 云端文件已自动保护）");
            ensure!(
                !matched_grp.retained.is_empty()
                    && to_delete.len() < live_srcs.len(),
                "安全保护：必须保留至少 1 个胜出片源"
            );

            let del_paths: Vec<String> = matched_grp
                .candidates
                .iter()
                .filter(|v| v["id"].as_str().is_some_and(|id| to_delete.iter().any(|d| d == id)))
                .filter_map(|v| v["path"].as_str().or_else(|| v["name"].as_str()).map(str::to_string))
                .collect();
            let win_path = matched_grp
                .retained
                .first()
                .and_then(|v| v["path"].as_str().or_else(|| v["name"].as_str()))
                .map(|p| format!("保留胜出源：{p}"))
                .unwrap_or_default();

            if let Err(err) = host.delete_library_sources(&item_id, &to_delete) {
                if crate::host::is_same_file_delete_refused(&err) {
                    return Ok((0, String::new(), win_path));
                }
                return Err(err);
            }
            let _ = host.scan_library_item(&item_id);
            Ok((to_delete.len(), del_paths.join(" ; "), win_path))
        })();

        match one_res {
            Ok((0, _, _)) => {
                db.execute(
                    "UPDATE lib_decisions SET state = 'dismissed', reason = ?, updated_at = ? WHERE id = ?",
                    params!["宿主复核所选版本与保留版本指向同一底层文件，已安全跳过无需删旧", now_ts(), did],
                )?;
            }
            Ok((cnt, del_paths_s, win_path)) => {
                cleaned_groups += 1;
                deleted_sources += cnt;
                db.execute(
                    "UPDATE lib_decisions SET state = 'cleaned', updated_at = ? WHERE id = ?",
                    params![now_ts(), did],
                )?;
                db::log_media_history(
                    db,
                    "存量精洗",
                    &title,
                    &format!("ID: {item_id}"),
                    &del_paths_s,
                    &win_path,
                    &format!("{category} · {slot}"),
                    "clean_library_dup",
                    "done",
                    &format!("已二次复核并删除 {cnt} 个同槽重复低分源"),
                );
            }
            Err(err) => {
                db.execute(
                    "UPDATE lib_decisions SET status = 'review', state = 'pending', reason = ?, updated_at = ? WHERE id = ?",
                    params![format!("执行前复核拦截：{err:#}"), now_ts(), did],
                )?;
            }
        }
    }

    for sc in ["movie", "tv"] {
        if let Some(mut j) = read_job(db, sc) {
            let _ = refresh_job_counts(db, &mut j);
            let _ = db.execute(
                "UPDATE lib_scan_jobs SET suggestion_groups=?, suggestion_sources=?, review_groups=? WHERE scope=?",
                params![j.suggestion_groups as i64, j.suggestion_sources as i64, j.review_groups as i64, sc],
            );
        }
    }

    let remaining: i64 = db.query_row(
        "SELECT COUNT(*) FROM lib_decisions WHERE status = 'suggestion' AND state = 'approved'",
        [],
        |r| r.get(0),
    )?;
    Ok(json!({
        "cleaned_groups": cleaned_groups,
        "deleted_sources": deleted_sources,
        "remaining_approved": remaining,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::LibrarySource;

    #[test]
    fn same_pickcode_or_file_id_sources_are_never_marked_for_deletion() {
        let cfg = crate::db::default_app_config().unwrap();
        let same_pc_item = LibraryItem {
            id: "ep_44".to_string(),
            name: "妈妈发疯了".to_string(),
            item_type: "episode".to_string(),
            season: 1,
            episode: 44,
            sources: vec![
                LibrarySource {
                    id: "src_new".to_string(),
                    name: "妈妈发疯了.2026.S01E44.2160p.WEB-DL.DV.H.265.DDP5.1.strm".to_string(),
                    strm_path: "/strm/cloud/媒体库/电视剧/日韩剧/妈妈发疯了/妈妈发疯了.2026.S01E44.2160p.WEB-DL.DV.H.265.DDP5.1.strm".to_string(),
                    stream_url: "http://127.0.0.1:8080/api/302?pickcode=d0i09yy32jlhrxicl".to_string(),
                    size: 4_000_000_000.0,
                    ..Default::default()
                },
                LibrarySource {
                    id: "src_old_stale_strm".to_string(),
                    name: "妈妈发疯了.2026.S01E44.2160p.DV.H.265.AAC.strm".to_string(),
                    strm_path: "/strm/cloud/媒体库/电视剧/日韩剧/妈妈发疯了/妈妈发疯了.2026.S01E44.2160p.DV.H.265.AAC.strm".to_string(),
                    stream_url: "http://127.0.0.1:8080/api/302?pickcode=d0i09yy32jlhrxicl".to_string(),
                    size: 4_000_000_000.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let groups = evaluate_item_groups(&same_pc_item, "tv", &cfg).unwrap();
        assert!(groups.is_empty(), "coexisting STRMs sharing pickcode must NEVER generate a delete suggestion");

        // Distinct pickcodes (two genuinely separate 115 files) DO generate a suggestion!
        let distinct_pc_item = LibraryItem {
            sources: vec![
                LibrarySource {
                    pick_code: "pickwinner111".to_string(),
                    ..same_pc_item.sources[0].clone()
                },
                LibrarySource {
                    pick_code: "pickloser2222".to_string(),
                    size: 1_200_000_000.0,
                    ..same_pc_item.sources[1].clone()
                },
            ],
            ..same_pc_item
        };
        let groups2 = evaluate_item_groups(&distinct_pc_item, "tv", &cfg).unwrap();
        assert_eq!(groups2.len(), 1);
        assert_eq!(groups2[0].status, "suggestion");
        assert_eq!(groups2[0].candidates.len(), 1);
    }
}

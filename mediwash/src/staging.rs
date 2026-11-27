use crate::category;
use crate::db;
use crate::host::{Cloud115File, HostClient, LibrarySource, MediaStream, OrganizeFileItem};
use crate::identity::{norm_title_key, RecognitionEngine, ResolveOutcome};
use crate::models::{
    format_ts_cn, now_ts, AppConfig, ManualBinding, PluginSettings, StagingPipeline,
};
use crate::rules::{self, MediaProfile, ReferenceBaseline, Scorer};
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "avi", "m2ts", "ts", "mov", "iso", "wmv", "webm", "flv", "rmvb", "mpg", "mpeg",
];
const SUBTITLE_EXTS: &[&str] = &["ass", "ssa", "srt", "sup", "vtt", "sub", "idx"];
const CLUTTER_EXTS: &[&str] = &[
    "nfo", "xml", "json", "txt", "md", "url", "htm", "html", "sfv", "md5", "sha1", "log",
    "jpg", "jpeg", "png", "webp", "bmp", "gif", "tbn", "ico",
    "ass", "ssa", "srt", "sup", "vtt", "sub", "idx", "cue", "ini", "db", "torrent",
];

pub fn is_video_name(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| VIDEO_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

pub fn is_subtitle_name(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| SUBTITLE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

fn is_leftover_clutter(file: &Cloud115File) -> bool {
    if file.is_dir || is_video_name(&file.name) {
        return false;
    }
    let trimmed = file.name.trim();
    if trimmed.starts_with('.') && file.size <= 10 * 1024 * 1024 {
        return true;
    }
    if file.size > 50 * 1024 * 1024 {
        return false;
    }
    trimmed.rsplit_once('.').is_some_and(|(_, ext)| {
        let low = ext.to_ascii_lowercase();
        if low == "json" {
            // Clean Mediary PT upload `*-mediainfo.json` / metadata sidecars <= 10 MB
            return file.size <= 10 * 1024 * 1024;
        }
        CLUTTER_EXTS.contains(&low.as_str())
    })
}

/// Bottom-up Mode B cleanup of subdirectories inside `root_cid`:
/// Recycles any non-protected subdirectory whose entire subtree contains ZERO videos
/// and ZERO unknown/large files (cleaning both pure empty directories and directories
/// left with only `.nfo / .jpg / .png / .txt / .url / *-mediainfo.json / orphan subtitles` after videos moved out).
#[allow(dead_code)]
pub fn prune_staging_subdirs(host: &HostClient, cfg: &AppConfig, root_cid: &str) -> usize {
    prune_staging_subdirs_with_budget(host, cfg, root_cid, 12)
}

pub fn prune_staging_subdirs_with_budget(
    host: &HostClient,
    cfg: &AppConfig,
    root_cid: &str,
    budget_secs: u64,
) -> usize {
    let root = root_cid.trim();
    if root.is_empty() || root == "0" {
        return 0;
    }
    let mut protected: BTreeSet<String> = cfg.host_sync.protected_cids.iter().cloned().collect();
    for p in &cfg.pipelines {
        if !p.source_cid.trim().is_empty() {
            protected.insert(p.source_cid.trim().to_string());
        }
        if !p.target_cid.trim().is_empty() {
            protected.insert(p.target_cid.trim().to_string());
        }
    }
    protected.insert(root.to_string());

    let clock = Instant::now();
    let max_dur = Duration::from_secs(budget_secs.clamp(3, 25));
    let mut bfs: VecDeque<(String, usize)> = VecDeque::from([(root.to_string(), 0usize)]);
    let mut seen: BTreeSet<String> = BTreeSet::from([root.to_string()]);
    let mut parent_of: BTreeMap<String, String> = BTreeMap::new();
    let mut children_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut depth_of: BTreeMap<String, usize> = BTreeMap::new();
    let mut own_clean: BTreeMap<String, bool> = BTreeMap::new();

    while let Some((cid, depth)) = bfs.pop_front() {
        if seen.len() > 150 || clock.elapsed() >= max_dur {
            // Incomplete subtree discovery: mark unexpanded directory as non-clean so we never delete an uninspected parent!
            if cid != root {
                own_clean.insert(cid, false);
            }
            continue;
        }
        let mut offset = 0usize;
        let mut all_pages_ok = true;
        let mut dir_files_clean = true;
        let mut pages_read = 0usize;
        loop {
            pages_read += 1;
            if pages_read > 10 {
                all_pages_ok = false;
                break;
            }
            let Ok(page) = host.open115_list_page(&cid, offset, 200, true) else {
                all_pages_ok = false;
                break;
            };
            let n = page.items.len();
            for item in page.items {
                if item.is_dir {
                    // Protect depth-1 upstream mount folders (`待洗版/link`, `待洗版/upload`, etc.) so FlowLink upload targets stay intact!
                    if depth == 0
                        && matches!(
                            item.name.trim().to_lowercase().as_str(),
                            "link" | "upload" | "offline" | "云下载" | "转存" | "转存监控" | "最近接收"
                        )
                    {
                        protected.insert(item.file_id.clone());
                    }
                    if !item.file_id.is_empty() && seen.insert(item.file_id.clone()) {
                        parent_of.insert(item.file_id.clone(), cid.clone());
                        children_of.entry(cid.clone()).or_default().push(item.file_id.clone());
                        depth_of.insert(item.file_id.clone(), depth + 1);
                        bfs.push_back((item.file_id, depth + 1));
                    }
                } else if cid != root && !is_leftover_clutter(&item) {
                    dir_files_clean = false;
                }
            }
            offset += n;
            if n == 0 || offset >= page.count {
                break;
            }
        }
        if cid != root {
            own_clean.insert(cid, all_pages_ok && dir_files_clean);
        }
    }

    let mut subdirs: Vec<(usize, String)> = depth_of
        .iter()
        .map(|(cid, &d)| (d, cid.clone()))
        .collect();
    subdirs.sort_by(|a, b| b.0.cmp(&a.0));

    let mut removable: BTreeMap<String, bool> = BTreeMap::new();
    for (_depth, cid) in &subdirs {
        let is_rem = !protected.contains(cid)
            && own_clean.get(cid) == Some(&true)
            && children_of
                .get(cid)
                .is_none_or(|kids| kids.iter().all(|k| removable.get(k) == Some(&true)));
        removable.insert(cid.clone(), is_rem);
    }

    // Delete highest removable subtree roots per parent (falling back to children if a parent dir is host-protected, e.g., `待洗版/link`)
    let mut cleaned_dirs = 0usize;
    let mut cursor_parents: VecDeque<String> = VecDeque::from([root.to_string()]);
    while let Some(p_cid) = cursor_parents.pop_front() {
        let Some(kids) = children_of.get(&p_cid) else {
            continue;
        };
        let mut batch_rem: Vec<String> = Vec::new();
        for k in kids {
            if removable.get(k) == Some(&true) {
                batch_rem.push(k.clone());
            } else {
                cursor_parents.push_back(k.clone());
            }
        }
        if !batch_rem.is_empty() {
            if host.open115_delete_files(&p_cid, &batch_rem).is_ok() {
                cleaned_dirs += batch_rem.len();
            } else {
                for single in batch_rem {
                    if host
                        .open115_delete_files(&p_cid, std::slice::from_ref(&single))
                        .is_ok()
                    {
                        cleaned_dirs += 1;
                    } else {
                        // Likely a host-protected mount directory (e.g. `待洗版/link`): preserve it and prune its removable children!
                        cursor_parents.push_back(single);
                    }
                }
            }
        }
    }
    cleaned_dirs
}

fn stem_prefix(name: &str) -> String {
    name.rsplit_once('.')
        .map_or(name, |(s, _)| s)
        .trim()
        .to_lowercase()
}

pub fn validate_pipeline(p: &StagingPipeline) -> Result<()> {
    ensure!(
        !p.name.trim().is_empty() && p.name.len() <= 200,
        "请填写 115 监控目录名称"
    );
    db::validate_cid(&p.source_cid).context("待洗版监控目录 CID 无效")?;
    ensure!(p.source_cid != "0", "待洗版监控目录不能设为根目录 0");
    if !p.target_cid.trim().is_empty() {
        db::validate_cid(&p.target_cid).context("归档目标媒体库根目录 CID 无效")?;
        ensure!(p.target_cid.trim() != "0", "归档目标媒体库目录不能设为根目录 0");
        ensure!(
            p.target_cid.trim() != p.source_cid.trim(),
            "归档目标媒体库目录不能与待洗版监控目录相同"
        );
    }
    Ok(())
}

/// Resolves `(target_cid, task_name)` for `POST /api/plugin/open115/files/organize`.
/// Explicitly passing `task_name` (e.g. `"115-895"`) lets Mediary inherit `[tmdbid-{{tmdbid}}]` rename templates
/// from the user's default FlowLink transfer task even when that task is disabled (`enabled = false`)!
fn resolve_archive_context(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    pipeline_id: i64,
) -> (Option<String>, Option<String>) {
    let valid_cid = |s: &str| -> Option<String> {
        let t = s.trim();
        (!t.is_empty() && t != "0" && t.bytes().all(|b| b.is_ascii_digit())).then(|| t.to_string())
    };

    let mut synced_task_name = cfg.host_sync.transfer_task_name.trim().to_string();
    let mut synced_target_cid = cfg.host_sync.transfer_target_cid.clone();
    if synced_task_name.is_empty() && cfg.host_sync.has_flowlink_transfer_template {
        let _ = host.sync_unified_rules(db);
        if let Ok(fresh) = db::load_config(db) {
            synced_task_name = fresh.host_sync.transfer_task_name.trim().to_string();
            if valid_cid(&synced_target_cid).is_none() {
                synced_target_cid = fresh.host_sync.transfer_target_cid;
            }
        }
    }
    let task_opt = (!synced_task_name.is_empty()).then_some(synced_task_name);

    if let Some(p) = cfg.pipelines.iter().find(|p| p.id == pipeline_id) {
        if let Some(tcid) = valid_cid(&p.target_cid) {
            return (Some(tcid), task_opt);
        }
    }
    if let Some(tcid) = valid_cid(&synced_target_cid) {
        return (Some(tcid), task_opt);
    }
    // Live auto-discovery of `媒体库` / `影视库` under 115 root "0"
    let skip_cids: BTreeSet<&str> = [
        cfg.host_sync.open115_target_cid.trim(),
        cfg.host_sync.open115_offline_cid.trim(),
        cfg.host_sync.transfer_source_cid.trim(),
    ]
    .into_iter()
    .chain(cfg.pipelines.iter().map(|p| p.source_cid.trim()))
    .filter(|s| !s.is_empty())
    .collect();

    for cand in ["媒体库", "影视库", "影视"] {
        if let Ok(Some(cid)) = host.open115_resolve_subpath_cid("0", &[cand]) {
            if let Some(clean_cid) = valid_cid(&cid) {
                if !skip_cids.contains(clean_cid.as_str()) {
                    let _ = db::edit_config(db, false, |c| {
                        c.host_sync.transfer_target_folder = cand.to_string();
                        c.host_sync.transfer_target_cid = clean_cid.clone();
                        if !c.host_sync.protected_cids.contains(&clean_cid) {
                            c.host_sync.protected_cids.push(clean_cid.clone());
                        }
                        Ok(false)
                    });
                    return (Some(clean_cid), task_opt);
                }
            }
        }
    }
    (None, task_opt)
}

/// Scans a 115 staging pipeline directory (with recursive folder context & sidecar subtitle pairing),
/// evaluates every candidate against Mediary's library & unified categories, and optionally auto-executes moves.
pub fn scan_pipeline(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    pipeline_id: i64,
    max_videos: usize,
) -> Result<Value> {
    scan_pipeline_with_budget(db, host, cfg, settings, pipeline_id, max_videos, 55, 210, false)
}

/// UI-safe bounded scan + auto-execute slice that stays well under Mediary's 45s UI & 60s Axios timeouts
/// even when 115's 0.8 QPS rate limiter is shared with MediFind / FlowLink!
pub fn scan_pipeline_ui(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    pipeline_id: i64,
    max_videos: usize,
) -> Result<Value> {
    scan_pipeline_with_budget(
        db,
        host,
        cfg,
        settings,
        pipeline_id,
        max_videos.clamp(1, 15),
        7,
        16,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn scan_pipeline_with_budget(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    pipeline_id: i64,
    max_videos: usize,
    scan_budget_secs: u64,
    total_budget_secs: u64,
    is_ui_slice: bool,
) -> Result<Value> {
    let pipeline = cfg
        .pipelines
        .iter()
        .find(|p| p.id == pipeline_id)
        .cloned()
        .context("找不到指定的 115 中转管道")?;
    validate_pipeline(&pipeline)?;

    let recognition = RecognitionEngine::new(cfg);
    let clock = Instant::now();
    let cursor_key = format!("staging_walk:{pipeline_id}");

    type WalkQueue = (VecDeque<(String, String, usize)>, BTreeSet<String>);
    let (mut queue, mut visited): WalkQueue = db::get_cursor(db, &cursor_key)
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|(q, _): &WalkQueue| !q.is_empty())
        .unwrap_or_else(|| {
            (
                VecDeque::from([(pipeline.source_cid.clone(), String::new(), 0usize)]),
                BTreeSet::from([pipeline.source_cid.clone()]),
            )
        });

    let mut inspected = 0usize;
    let mut admit_count = 0usize;
    let mut upgrade_count = 0usize;
    let mut reject_count = 0usize;
    let mut review_count = 0usize;
    let mut probing_count = 0usize;
    let mut probing_resolved = 0usize;
    let mut auto_ready_ids = Vec::new();

    // Optimization 1: Priority re-check of previously queued `probing` items (round-robin by oldest `updated_at`)
    // BEFORE advancing the BFS cursor, so async 115 MediaInfo results archive immediately without waiting for a full walk cycle!
    let probe_recheck_limit: usize = if is_ui_slice { 10 } else { 45 };
    let probe_recheck_budget_secs: u64 = if is_ui_slice { 4 } else { 15 };
    if let Ok(mut pstmt) = db.prepare(
        "SELECT id, file_id, parent_cid, rel_path, sidecar_ids_json
           FROM staging_items
          WHERE pipeline_id = ?1
            AND state = 'pending'
            AND verdict = 'probing'
          ORDER BY updated_at ASC, id ASC
          LIMIT ?2",
    ) {
        let probing_rows: Vec<(i64, String, String, String, String)> = pstmt
            .query_map(params![pipeline.id, probe_recheck_limit as i64], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                ))
            })
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .collect();
        drop(pstmt);

        let probe_clock = Instant::now();
        let mut probe_dir_cache: BTreeMap<String, (BTreeMap<String, Cloud115File>, bool)> =
            BTreeMap::new();
        for (p_idx, (row_id, file_id, parent_cid, rel_path, sidecars_raw)) in
            probing_rows.into_iter().enumerate()
        {
            if p_idx > 0 && probe_clock.elapsed() >= Duration::from_secs(probe_recheck_budget_secs)
            {
                break;
            }
            let lookup_cid = if parent_cid.trim().is_empty() {
                pipeline.source_cid.trim().to_string()
            } else {
                parent_cid.trim().to_string()
            };
            if lookup_cid.is_empty() {
                continue;
            }
            if !probe_dir_cache.contains_key(&lookup_cid) {
                match host.open115_list_dir_files_map_ext(&lookup_cid, 4) {
                    Ok(v) => {
                        probe_dir_cache.insert(lookup_cid.clone(), v);
                    }
                    Err(_) => continue,
                }
            }
            let (cached_map, is_complete) = probe_dir_cache.get(&lookup_cid).unwrap();
            let live_opt = cached_map.get(&file_id).cloned().or_else(|| {
                if *is_complete {
                    None
                } else {
                    host.open115_find_file_in_cid(&lookup_cid, &file_id).ok().flatten()
                }
            });
            let Some(mut live_file) = live_opt else {
                let _ = db.execute("DELETE FROM staging_items WHERE id = ?", [row_id]);
                continue;
            };
            if live_file.parent_id.trim().is_empty() {
                live_file.parent_id = lookup_cid;
            }
            let eval = evaluate_staging_file(
                host,
                cfg,
                &pipeline,
                &recognition,
                &live_file,
                &rel_path,
                false,
            );
            if eval.verdict == "probing" {
                let _ = db.execute(
                    "UPDATE staging_items SET reason = ?, updated_at = ? WHERE id = ?",
                    params![eval.reason, now_ts(), row_id],
                );
            } else {
                let sidecar_ids: Vec<String> =
                    serde_json::from_str(&sidecars_raw).unwrap_or_default();
                let saved_id = upsert_staging_item(
                    db,
                    pipeline.id,
                    &live_file,
                    &rel_path,
                    &sidecar_ids,
                    &eval,
                )?;
                probing_resolved += 1;
                match eval.verdict.as_str() {
                    "admit_new" => {
                        admit_count += 1;
                        auto_ready_ids.push(saved_id);
                    }
                    "admit_upgrade" => {
                        upgrade_count += 1;
                        auto_ready_ids.push(saved_id);
                    }
                    "reject" | "already_in_lib" => {
                        reject_count += 1;
                        auto_ready_ids.push(saved_id);
                    }
                    _ => review_count += 1,
                }
            }
        }
    }

    while inspected < max_videos && clock.elapsed() < Duration::from_secs(scan_budget_secs) {
        let Some((cid, rel_dir, offset)) = queue.pop_front() else {
            break;
        };
        let page = host.open115_list_page(&cid, offset, 1000, true)?;

        // Self-heal disappeared ghost `pending` / `failed` rows whenever we get a complete 1-page snapshot of `cid`!
        if offset == 0 && page.items.len() >= page.count {
            let live_video_ids: BTreeSet<&str> = page
                .items
                .iter()
                .filter(|f| !f.is_dir && is_video_name(&f.name))
                .map(|f| f.file_id.as_str())
                .collect();
            reconcile_disappeared_in_cid(
                db,
                pipeline.id,
                &cid,
                cid == pipeline.source_cid,
                &live_video_ids,
            );
        }

        if page.items.is_empty() {
            continue;
        }

        // Enqueue all child directories on this page upfront so mid-page quota breaks never miss trailing subfolders!
        if pipeline.recursive {
            for item in &page.items {
                if item.is_dir && visited.len() < 2500 && visited.insert(item.file_id.clone()) {
                    let child_rel = if rel_dir.is_empty() {
                        item.name.clone()
                    } else {
                        format!("{rel_dir}/{}", item.name)
                    };
                    queue.push_back((item.file_id.clone(), child_rel, 0));
                }
            }
        }

        // Collect sidecar subtitles in this directory page so winning videos bring their subtitles along
        let dir_subtitles: Vec<&Cloud115File> = page
            .items
            .iter()
            .filter(|f| !f.is_dir && is_subtitle_name(&f.name))
            .collect();

        let mut processed_in_page = 0usize;
        for item in &page.items {
            processed_in_page += 1;
            if item.is_dir || !is_video_name(&item.name) {
                continue;
            }

            // Only skip if explicitly `dismissed` IN THIS EXACT SAME FOLDER (`parent_cid == cid`) with unchanged sha1!
            let existing_state: Option<(String, String, String)> = db
                .query_row(
                    "SELECT state, sha1, parent_cid FROM staging_items WHERE file_id = ?",
                    [&item.file_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            if let Some((ref st, ref old_sha1, ref old_parent)) = existing_state {
                if st == "dismissed"
                    && old_parent == &cid
                    && (old_sha1.is_empty() || old_sha1 == &item.sha1)
                {
                    continue;
                }
            }

            let v_stem = stem_prefix(&item.name);
            let sidecar_ids: Vec<String> = if pipeline.move_subtitles && !v_stem.is_empty() {
                dir_subtitles
                    .iter()
                    .filter(|sub| {
                        let s_low = sub.name.to_lowercase();
                        s_low.starts_with(&v_stem)
                    })
                    .map(|sub| sub.file_id.clone())
                    .collect()
            } else {
                Vec::new()
            };

            let eval = evaluate_staging_file(
                host,
                cfg,
                &pipeline,
                &recognition,
                item,
                &rel_dir,
                false,
            );
            let row_id = upsert_staging_item(db, pipeline.id, item, &rel_dir, &sidecar_ids, &eval)?;
            match eval.verdict.as_str() {
                "admit_new" => {
                    admit_count += 1;
                    auto_ready_ids.push(row_id);
                }
                "admit_upgrade" => {
                    upgrade_count += 1;
                    auto_ready_ids.push(row_id);
                }
                "reject" | "already_in_lib" => {
                    reject_count += 1;
                    auto_ready_ids.push(row_id);
                }
                "probing" => probing_count += 1,
                _ => review_count += 1,
            }
            inspected += 1;
            if inspected >= max_videos || clock.elapsed() >= Duration::from_secs(scan_budget_secs) {
                if offset + processed_in_page < page.count {
                    queue.push_front((cid.clone(), rel_dir.clone(), offset + processed_in_page));
                }
                break;
            }
        }

        if inspected < max_videos
            && clock.elapsed() < Duration::from_secs(scan_budget_secs)
            && offset + page.items.len() < page.count
        {
            queue.push_front((cid, rel_dir, offset + page.items.len()));
        }
    }

    let walk_finished = queue.is_empty();
    if walk_finished {
        db.execute("DELETE FROM cursors WHERE key = ?", [&cursor_key])?;
    } else {
        db::set_cursor(db, &cursor_key, &serde_json::to_string(&(&queue, &visited))?)?;
    }

    // When `auto_admit` is on, also drain any previously scanned `pending` actionable rows in this pipeline!
    if pipeline.auto_admit && !settings.dry_run {
        if let Ok(mut stmt) = db.prepare(
            "SELECT id FROM staging_items
              WHERE pipeline_id = ?
                AND state = 'pending'
                AND verdict IN ('already_in_lib', 'reject', 'admit_new', 'admit_upgrade')
              ORDER BY CASE WHEN verdict IN ('already_in_lib', 'reject') THEN 0 ELSE 1 END, id ASC
              LIMIT 200",
        ) {
            if let Ok(rows) = stmt.query_map([pipeline.id], |r| r.get::<_, i64>(0)) {
                let mut merged = Vec::new();
                let mut seen_ids = BTreeSet::new();
                for id in rows.flatten().chain(auto_ready_ids.into_iter()) {
                    if seen_ids.insert(id) {
                        merged.push(id);
                    }
                }
                auto_ready_ids = merged;
            }
        }
    }

    let mut auto_moved = 0usize;
    let mut cleaned_dirs = 0usize;
    let mut remaining_auto = 0usize;
    if pipeline.auto_admit && !settings.dry_run && !auto_ready_ids.is_empty() {
        let rem_secs = total_budget_secs.saturating_sub(clock.elapsed().as_secs()).max(3);
        let opts = if is_ui_slice {
            ExecOpts {
                soft_budget_secs: rem_secs.min(11),
                max_organize_files: 8,
                organize_http_timeout_secs: 34,
                prune_budget_secs: 6,
            }
        } else {
            ExecOpts {
                soft_budget_secs: rem_secs,
                max_organize_files: 80,
                organize_http_timeout_secs: 80,
                prune_budget_secs: 12,
            }
        };
        let exec_res =
            execute_staging_items_opts(db, host, cfg, settings, &auto_ready_ids, None, opts)?;
        auto_moved = exec_res["moved"].as_u64().unwrap_or(0) as usize;
        cleaned_dirs = exec_res["cleaned_dirs"].as_u64().unwrap_or(0) as usize;
        remaining_auto = exec_res["remaining"].as_u64().unwrap_or(0) as usize;

        // Optimization 2: Compensate mid-directory pagination `offset` left-shift when files were moved/recycled out of `q_cid`!
        if !walk_finished {
            if let Some(moved_map) = exec_res["moved_by_cid"].as_object() {
                let mut adjusted = false;
                for (q_cid, _, q_off) in &mut queue {
                    if *q_off > 0 {
                        if let Some(m) = moved_map.get(q_cid.as_str()).and_then(Value::as_u64) {
                            if m > 0 {
                                *q_off = q_off.saturating_sub(m as usize);
                                adjusted = true;
                            }
                        }
                    }
                }
                if adjusted {
                    db::set_cursor(db, &cursor_key, &serde_json::to_string(&(&queue, &visited))?)?;
                }
            }
        }
    } else if walk_finished
        && pipeline.auto_admit
        && !settings.dry_run
        && pipeline.recursive
        && pipeline.reject_mode != "skip"
        && clock.elapsed() < Duration::from_secs(total_budget_secs.saturating_sub(5))
    {
        cleaned_dirs = prune_staging_subdirs_with_budget(
            host,
            cfg,
            &pipeline.source_cid,
            if is_ui_slice { 6 } else { 12 },
        );
    }

    let has_more = !walk_finished || remaining_auto > 0;
    let mut tail_notes = Vec::new();
    if probing_resolved > 0 {
        tail_notes.push(format!("复检就绪 {probing_resolved} 项"));
    }
    if auto_moved > 0 {
        tail_notes.push(format!("已自动执行 {auto_moved} 项"));
    }
    if cleaned_dirs > 0 {
        tail_notes.push(format!("清理空壳目录 {cleaned_dirs} 个"));
    }
    if remaining_auto > 0 {
        tail_notes.push(format!("剩余 {remaining_auto} 项待续执行"));
    } else if !walk_finished {
        tail_notes.push("下轮续扫剩余子目录".to_string());
    }
    let tail_suffix = if tail_notes.is_empty() {
        String::new()
    } else {
        format!("，{}", tail_notes.join("，"))
    };

    let summary_text = format!(
        "[{}] 扫描 {} 个视频（新片归档 {} · 洗版升级 {} · 淘汰回收 {} · 探测中 {} · 待核 {}）{}",
        pipeline.name,
        inspected,
        admit_count,
        upgrade_count,
        reject_count,
        probing_count,
        review_count,
        tail_suffix
    );
    if inspected > 0 || probing_resolved > 0 || auto_moved > 0 || cleaned_dirs > 0 || !walk_finished {
        db::log_task_history(
            db,
            "115归档",
            &pipeline.name,
            &pipeline.source_cid,
            "scan_staging",
            "done",
            &summary_text,
        );
    }

    Ok(json!({
        "pipeline_id": pipeline.id,
        "inspected": inspected,
        "probing_resolved": probing_resolved,
        "admit_new": admit_count,
        "admit_upgrade": upgrade_count,
        "reject": reject_count,
        "probing": probing_count,
        "review": review_count,
        "auto_moved": auto_moved,
        "cleaned_dirs": cleaned_dirs,
        "remaining_auto": remaining_auto,
        "finished": walk_finished && remaining_auto == 0,
        "has_more": has_more,
        "notice": summary_text,
    }))
}

struct StagingEval {
    media_type: String,
    tmdb_id: String,
    title: String,
    year: u32,
    season: u32,
    episode: u32,
    category: String,
    slot: String,
    cand_score: f64,
    lib_score: Option<f64>,
    verdict: String,
    reason: String,
    detail: Value,
}

fn evaluate_staging_file(
    host: &HostClient,
    cfg: &AppConfig,
    pipeline: &StagingPipeline,
    recognition: &RecognitionEngine,
    file: &Cloud115File,
    rel_dir: &str,
    wait_probe: bool,
) -> StagingEval {
    let mut out = StagingEval {
        media_type: String::new(),
        tmdb_id: String::new(),
        title: file.name.clone(),
        year: 0,
        season: 0,
        episode: 0,
        category: String::new(),
        slot: String::new(),
        cand_score: 0.0,
        lib_score: None,
        verdict: "review".to_string(),
        reason: String::new(),
        detail: json!({}),
    };

    // 1. Check Mediary global filter exclude keywords
    let name_low = file.name.to_lowercase();
    if let Some(hit_kw) = cfg
        .host_sync
        .filter_keywords
        .iter()
        .find(|kw| !kw.is_empty() && name_low.contains(&kw.to_lowercase()))
    {
        out.verdict = "reject".to_string();
        out.reason = format!("命中 Mediary 全局过滤屏蔽词「{hit_kw}」，直接移入 115 回收站");
        return out;
    }

    // 2. Identity resolution (`auto` / `movie` / `tv`)
    let rec = match recognition.resolve(host, &file.name, rel_dir, &pipeline.media_type) {
        Ok(ResolveOutcome::Matched(m)) => m,
        Ok(ResolveOutcome::Blacklisted { tmdb_id, reason }) => {
            out.tmdb_id = tmdb_id;
            out.verdict = "review".to_string();
            out.reason = reason;
            return out;
        }
        Ok(ResolveOutcome::Unresolved(msg)) => {
            out.verdict = "review".to_string();
            out.reason = msg;
            return out;
        }
        Err(err) => {
            out.verdict = "review".to_string();
            out.reason = format!("识别接口异常：{err:#}");
            return out;
        }
    };

    out.media_type = rec.media_type.clone();
    out.tmdb_id = rec.tmdb_id.clone();
    out.title = rec.title.clone();
    out.year = rec.year.unwrap_or(0);
    if let Some((s, e)) = rec.episode {
        out.season = s;
        out.episode = e;
    }

    // 3. Query existing Mediary library items for this TMDB (+ episode)
    let lib_page = match host.library_items_by_tmdb(&rec.tmdb_id, &rec.media_type, rec.episode) {
        Ok(p) => p,
        Err(err) => {
            out.verdict = "review".to_string();
            out.reason = format!("查询宿主媒体库失败：{err:#}");
            return out;
        }
    };

    let mut existing_peers: Vec<(String, String, LibrarySource, Vec<MediaStream>)> = Vec::new();
    let mut detected_category = String::new();
    let mut item_id_for_ledger = String::new();
    let mut episode_title = String::new();

    for entry in &lib_page.items {
        if entry.effective_tmdb() != rec.tmdb_id {
            continue;
        }
        if let Some((s, e)) = rec.episode {
            if (entry.effective_season(), entry.effective_episode()) != (s, e) {
                continue;
            }
        }
        let detail = match host.library_item_detail(&entry.id) {
            Ok(d) => d,
            Err(err) => {
                out.verdict = "review".to_string();
                out.reason = format!("读取媒体库条目详情失败：{err:#}");
                return out;
            }
        };
        if item_id_for_ledger.is_empty() {
            item_id_for_ledger = detail.id.clone();
            let clean_detail_name = crate::identity::strip_invisible_chars(&detail.name);
            if detail.item_type.eq_ignore_ascii_case("episode") {
                if !clean_detail_name.trim().is_empty()
                    && clean_detail_name.trim() != out.title.trim()
                {
                    episode_title = clean_detail_name.trim().to_string();
                }
            } else if !clean_detail_name.trim().is_empty() {
                out.title = clean_detail_name;
            }
        }
        for src in detail.effective_sources() {
            let src_pc = src.effective_pick_code();
            let same_id_or_sha = (!file.file_id.is_empty() && src.file_id == file.file_id)
                || (!file.sha1.is_empty() && src.sha1.eq_ignore_ascii_case(&file.sha1))
                || (!file.pick_code.is_empty() && src_pc.eq_ignore_ascii_case(&file.pick_code));
            let same_exact_bytes = file.size >= 10_000_000
                && src.file_id.trim().is_empty()
                && src.sha1.trim().is_empty()
                && src.size > 0.0
                && (src.size.round() as u64) == file.size;
            if same_id_or_sha || same_exact_bytes {
                let existing_path = [
                    src.strm_path.as_str(),
                    src.cloud_path.as_str(),
                    src.file_path.as_str(),
                    detail.path.as_str(),
                ]
                .into_iter()
                .find(|p| !p.trim().is_empty())
                .unwrap_or_default()
                .to_string();
                let dup_paths = [
                    src.strm_path.as_str(),
                    src.cloud_path.as_str(),
                    src.file_path.as_str(),
                    detail.path.as_str(),
                    rel_dir,
                ];
                if let Some(cat) =
                    category::locate(&cfg.host_sync.categories, &rec.media_type, &dup_paths)
                {
                    out.category = cat;
                }
                let mut dup_policy = cfg.strategy.effective_for(
                    &rec.media_type,
                    &out.category,
                    &cfg.host_sync.categories,
                    &cfg.rules,
                );
                if !pipeline.rule_id_override.trim().is_empty()
                    && cfg.rules.iter().any(|r| r.id == pipeline.rule_id_override)
                {
                    dup_policy.rule_id = pipeline.rule_id_override.clone();
                }
                if let Ok(rule) = rules::find_rule(cfg, &dup_policy.rule_id) {
                    if let Ok(sc) = Scorer::new(rule) {
                        let prof = MediaProfile::extract(
                            &LibrarySource {
                                name: file.name.clone(),
                                size: file.size as f64,
                                ..Default::default()
                            },
                            &[],
                        );
                        out.slot = sc
                            .score(
                                &prof,
                                &ReferenceBaseline::default(),
                                &out.category,
                                dup_policy.iso_protected,
                            )
                            .slot;
                    }
                }
                out.verdict = "already_in_lib".to_string();
                out.reason = if same_id_or_sha {
                    "该 115 文件（相同 file_id / SHA1）已在 Mediary 媒体库内，直接移入 115 回收站".to_string()
                } else {
                    "媒体库同集已有字节数完全相等的源（同一文件），无需重复入库，直接移入 115 回收站".to_string()
                };
                out.detail = json!({
                    "matched_by": rec.matched_by,
                    "episode_title": episode_title,
                    "item_id": item_id_for_ledger,
                    "existing_path": existing_path,
                });
                return out;
            }
            let src_paths = [
                src.strm_path.as_str(),
                src.cloud_path.as_str(),
                src.file_path.as_str(),
                detail.path.as_str(),
            ];
            let cat = category::locate(&cfg.host_sync.categories, &rec.media_type, &src_paths)
                .unwrap_or_default();
            if detected_category.is_empty() && !cat.is_empty() {
                detected_category = cat.clone();
            }
            let src_streams: Vec<MediaStream> = detail
                .streams
                .iter()
                .filter(|st| !src.id.is_empty() && st.media_source_id == src.id)
                .cloned()
                .collect();
            existing_peers.push((detail.id.clone(), cat, src.clone(), src_streams));
        }
    }
    if detected_category.is_empty() && !rel_dir.trim().is_empty() {
        if let Some(cat) = category::locate(&cfg.host_sync.categories, &rec.media_type, &[rel_dir]) {
            detected_category = cat;
        }
    }
    out.category = detected_category.clone();

    if !existing_peers.is_empty() && !pipeline.wash_enabled {
        let first_peer_path = existing_peers
            .first()
            .map(|(_, _, s, _)| {
                [s.strm_path.as_str(), s.cloud_path.as_str(), s.file_path.as_str()]
                    .into_iter()
                    .find(|p| !p.trim().is_empty())
                    .unwrap_or_default()
                    .to_string()
            })
            .unwrap_or_default();
        out.verdict = "reject".to_string();
        out.reason = "媒体库已有该影片/剧集，当前已关闭「归档权重洗版」，不覆盖现有版本并移入 115 回收站".to_string();
        out.detail = json!({
            "matched_by": rec.matched_by,
            "episode_title": episode_title,
            "item_id": item_id_for_ledger,
            "existing_path": first_peer_path,
        });
        return out;
    }

    // 4. Resolve effective scoring policy & rule
    let mut eff_policy = cfg.strategy.effective_for(
        &rec.media_type,
        &detected_category,
        &cfg.host_sync.categories,
        &cfg.rules,
    );
    if !pipeline.rule_id_override.trim().is_empty()
        && cfg.rules.iter().any(|r| r.id == pipeline.rule_id_override)
    {
        eff_policy.rule_id = pipeline.rule_id_override.clone();
    }
    let Ok(rule) = rules::find_rule(cfg, &eff_policy.rule_id) else {
        out.verdict = "review".to_string();
        out.reason = "未找到可用评分规则".to_string();
        return out;
    };
    let Ok(scorer) = Scorer::new(rule) else {
        out.verdict = "review".to_string();
        out.reason = "评分规则编译失败".to_string();
        return out;
    };

    // 5. Probe 115 MediaInfo when needed
    let mut probed_streams: Vec<MediaStream> = Vec::new();
    let mut probed_bitrate = 0.0f64;
    let mut probe_cached = None;
    let should_probe = pipeline.probe_mode != "filename_only"
        && (pipeline.probe_mode == "always" || scorer.wants_source_probe())
        && !file.pick_code.is_empty();

    if should_probe {
        match host.open115_probe_media_info(
            file,
            true,
            wait_probe,
            false,
            if wait_probe { 15_000 } else { 2_500 },
        ) {
            Ok(info) if info.status == "ready" => {
                probed_bitrate = info.bitrate;
                probed_streams = info.streams;
                probe_cached = info.cached;
            }
            Ok(info) if matches!(info.status.as_str(), "pending" | "probing" | "queued") => {
                out.verdict = "probing".to_string();
                out.reason = "115 远程 MediaInfo 正在后台排队探测（1.25s 节流队列），下轮自动复检".to_string();
                return out;
            }
            Ok(_) | Err(_) if pipeline.probe_mode == "always" => {
                out.verdict = "review".to_string();
                out.reason = "115 MediaInfo 探测未返回有效流数据，留待复核".to_string();
                return out;
            }
            _ => {}
        }
    }

    let stream_refs: Vec<&MediaStream> = probed_streams.iter().collect();
    let cand_profile = MediaProfile::extract(
        &LibrarySource {
            name: file.name.clone(),
            size: file.size as f64,
            bitrate: probed_bitrate,
            ..Default::default()
        },
        &stream_refs,
    );

    // 6. Score candidate & compare with same-category same-slot library versions
    let mut shared_ref = ReferenceBaseline::default();
    shared_ref.observe(&cand_profile);

    let mut same_slot_peers: Vec<(String, MediaProfile)> = Vec::new();
    let init_cand_score = scorer.score(
        &cand_profile,
        &ReferenceBaseline::default(),
        &detected_category,
        eff_policy.iso_protected,
    );
    out.slot = init_cand_score.slot.clone();

    if init_cand_score.excluded {
        out.cand_score = init_cand_score.total;
        out.verdict = "reject".to_string();
        out.reason = init_cand_score
            .withheld_reason
            .unwrap_or_else(|| "命中评分排除条件，直接移入 115 回收站".to_string());
        out.detail = json!({
            "matched_by": rec.matched_by,
            "rule_name": rule.name,
            "profile": cand_profile.summary_json(),
            "breakdown": init_cand_score.breakdown,
            "probe_cached": probe_cached,
        });
        return out;
    }
    if let Some(ref wh) = init_cand_score.withheld_reason {
        out.cand_score = init_cand_score.total;
        out.verdict = "review".to_string();
        out.reason = format!("{wh}，请人工确认");
        out.detail = json!({
            "matched_by": rec.matched_by,
            "rule_name": rule.name,
            "profile": cand_profile.summary_json(),
            "breakdown": init_cand_score.breakdown,
        });
        return out;
    }

    for (_iid, pcat, psrc, pstreams) in &existing_peers {
        if !detected_category.is_empty() && !pcat.is_empty() && pcat != &detected_category {
            continue;
        }
        let refs: Vec<&MediaStream> = pstreams.iter().collect();
        let p_prof = MediaProfile::extract(psrc, &refs);
        let p_init = scorer.score(
            &p_prof,
            &ReferenceBaseline::default(),
            &detected_category,
            eff_policy.iso_protected,
        );
        if p_init.slot == init_cand_score.slot && p_init.withheld_reason.is_none() {
            shared_ref.observe(&p_prof);
            same_slot_peers.push((psrc.id.clone(), p_prof));
        }
    }

    if existing_peers.is_empty() {
        let final_cand = scorer.score(
            &cand_profile,
            &shared_ref,
            &detected_category,
            eff_policy.iso_protected,
        );
        out.cand_score = final_cand.total;
        out.verdict = "admit_new".to_string();
        out.reason = format!(
            "新入库媒体（{}）· 槽位 [{}] 得分 {:.2}，放行交由 Mediary 统一分类与命名入库",
            rec.matched_by, out.slot, final_cand.total
        );
        out.detail = json!({
            "matched_by": rec.matched_by,
            "rule_id": rule.id,
            "rule_name": rule.name,
            "profile": cand_profile.summary_json(),
            "breakdown": final_cand.breakdown,
            "probe_cached": probe_cached,
        });
        return out;
    }

    if same_slot_peers.is_empty() {
        let final_cand = scorer.score(
            &cand_profile,
            &shared_ref,
            &detected_category,
            eff_policy.iso_protected,
        );
        out.cand_score = final_cand.total;
        out.verdict = "admit_new".to_string();
        out.reason = format!(
            "库内尚无 [{}] 槽位版本（得分 {:.2}），放行入库与现有槽位多版本共存",
            out.slot, final_cand.total
        );
        out.detail = json!({
            "matched_by": rec.matched_by,
            "rule_id": rule.id,
            "rule_name": rule.name,
            "item_id": item_id_for_ledger,
            "profile": cand_profile.summary_json(),
            "breakdown": final_cand.breakdown,
            "probe_cached": probe_cached,
        });
        return out;
    }

    // Compare fairly in the same slot
    let max_peer_size = same_slot_peers
        .iter()
        .map(|(_, p)| p.size)
        .fold(0.0f64, f64::max);
    let cand_ref = if rule.kind == rules::SYMEDIA {
        ReferenceBaseline {
            size: max_peer_size,
            ..shared_ref.clone()
        }
    } else {
        shared_ref.clone()
    };
    let final_cand = scorer.score(
        &cand_profile,
        &cand_ref,
        &detected_category,
        eff_policy.iso_protected,
    );
    out.cand_score = final_cand.total;

    let mut scored_peers: Vec<(String, f64, MediaProfile)> = same_slot_peers
        .into_iter()
        .map(|(sid, p)| {
            let p_ref = if rule.kind == rules::SYMEDIA {
                ReferenceBaseline {
                    size: cand_profile.size.max(
                        if (p.size - max_peer_size).abs() < 1.0 {
                            0.0
                        } else {
                            max_peer_size
                        },
                    ),
                    ..shared_ref.clone()
                }
            } else {
                shared_ref.clone()
            };
            let sc = scorer.score(&p, &p_ref, &detected_category, eff_policy.iso_protected);
            (sid, sc.total, p)
        })
        .collect();
    scored_peers.sort_by(|a, b| b.1.total_cmp(&a.1));

    let keep_idx = eff_policy.keep_per_slot.min(scored_peers.len()).saturating_sub(1);
    let cutoff_score = scored_peers[keep_idx].1;
    out.lib_score = Some(cutoff_score);
    let delta = rules::round2(final_cand.total - cutoff_score);

    if final_cand.total > cutoff_score && delta >= eff_policy.min_diff {
        let superseded_ids: Vec<String> = scored_peers
            .iter()
            .map(|(sid, _, _)| sid.clone())
            .filter(|sid| !sid.is_empty())
            .collect();
        out.verdict = "admit_upgrade".to_string();
        out.reason = format!(
            "同槽位 [{}] 洗版胜出：{:.2} > 库内 {:.2}（+{:.2} ≥ 门槛 {:.2}），放行整理入库并在确认新源后清理旧源",
            out.slot, final_cand.total, cutoff_score, delta, eff_policy.min_diff
        );
        out.detail = json!({
            "matched_by": rec.matched_by,
            "episode_title": episode_title,
            "rule_id": rule.id,
            "rule_name": rule.name,
            "item_id": item_id_for_ledger,
            "old_source_ids": superseded_ids,
            "old_sources_preview": scored_peers.iter().map(|(id, sc, p)| json!({"id": id, "score": sc, "name": p.display_name})).collect::<Vec<_>>(),
            "profile": cand_profile.summary_json(),
            "breakdown": final_cand.breakdown,
            "probe_cached": probe_cached,
        });
    } else {
        let best_peer_id = scored_peers.first().map(|(id, _, _)| id.as_str()).unwrap_or_default();
        let best_peer_path = existing_peers
            .iter()
            .find(|(_, _, s, _)| !best_peer_id.is_empty() && s.id == best_peer_id)
            .or_else(|| existing_peers.first())
            .map(|(_, _, s, _)| {
                [s.strm_path.as_str(), s.cloud_path.as_str(), s.file_path.as_str()]
                    .into_iter()
                    .find(|p| !p.trim().is_empty())
                    .unwrap_or_default()
                    .to_string()
            })
            .unwrap_or_default();
        out.verdict = "reject".to_string();
        out.reason = format!(
            "同槽位 [{}] 洗版落败：候选 {:.2} 未超过库内 {:.2} 达最低分差 {:.2}（差值 {:+.2}），直接移入 115 回收站",
            out.slot, final_cand.total, cutoff_score, eff_policy.min_diff, delta
        );
        out.detail = json!({
            "matched_by": rec.matched_by,
            "episode_title": episode_title,
            "rule_id": rule.id,
            "rule_name": rule.name,
            "item_id": item_id_for_ledger,
            "existing_path": best_peer_path,
            "profile": cand_profile.summary_json(),
            "breakdown": final_cand.breakdown,
            "probe_cached": probe_cached,
        });
    }
    out
}

fn upsert_staging_item(
    db: &Connection,
    pipeline_id: i64,
    file: &Cloud115File,
    rel_path: &str,
    sidecar_ids: &[String],
    eval: &StagingEval,
) -> Result<i64> {
    let now = now_ts();
    db.execute(
        "INSERT INTO staging_items (
            pipeline_id, file_id, parent_cid, rel_path, name, size, sha1, pick_code,
            media_type, tmdb_id, title, year, season, episode, category, slot,
            cand_score, lib_score, verdict, state, reason, detail_json, sidecar_ids_json, updated_at
         ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'pending',?,?,?,?)
         ON CONFLICT(file_id) DO UPDATE SET
            pipeline_id=excluded.pipeline_id, parent_cid=excluded.parent_cid, rel_path=excluded.rel_path,
            name=excluded.name, size=excluded.size, sha1=excluded.sha1, pick_code=excluded.pick_code,
            media_type=excluded.media_type, tmdb_id=excluded.tmdb_id, title=excluded.title,
            year=excluded.year, season=excluded.season, episode=excluded.episode,
            category=excluded.category, slot=excluded.slot, cand_score=excluded.cand_score,
            lib_score=excluded.lib_score, verdict=excluded.verdict, state='pending',
            reason=excluded.reason, detail_json=excluded.detail_json,
            sidecar_ids_json=excluded.sidecar_ids_json, updated_at=excluded.updated_at",
        params![
            pipeline_id,
            file.file_id,
            file.parent_id,
            rel_path,
            file.name,
            file.size as i64,
            file.sha1,
            file.pick_code,
            eval.media_type,
            eval.tmdb_id,
            eval.title,
            eval.year,
            eval.season,
            eval.episode,
            eval.category,
            eval.slot,
            eval.cand_score,
            eval.lib_score,
            eval.verdict,
            eval.reason,
            eval.detail.to_string(),
            serde_json::to_string(sidecar_ids)?,
            now,
        ],
    )?;
    let id: i64 = db.query_row(
        "SELECT id FROM staging_items WHERE file_id = ?",
        [&file.file_id],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Prunes stale `staging_items` rows in `cid` whose `file_id` is no longer present in 115.
fn reconcile_disappeared_in_cid(
    db: &Connection,
    pipeline_id: i64,
    cid: &str,
    is_root_cid: bool,
    live_video_ids: &BTreeSet<&str>,
) {
    let sql = if is_root_cid {
        "SELECT id, file_id FROM staging_items
          WHERE pipeline_id = ?1
            AND state IN ('pending', 'failed')
            AND (parent_cid = ?2 OR parent_cid = '')"
    } else {
        "SELECT id, file_id FROM staging_items
          WHERE pipeline_id = ?1
            AND state IN ('pending', 'failed')
            AND parent_cid = ?2"
    };
    let Ok(mut stmt) = db.prepare(sql) else {
        return;
    };
    let stale_ids: Vec<i64> = stmt
        .query_map(params![pipeline_id, cid], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|(_, fid)| !live_video_ids.contains(fid.as_str()))
        .map(|(id, _)| id)
        .collect();
    drop(stmt);
    for sid in stale_ids {
        let _ = db.execute("DELETE FROM staging_items WHERE id = ?", [sid]);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExecOpts {
    pub soft_budget_secs: u64,
    pub max_organize_files: usize,
    pub organize_http_timeout_secs: u64,
    pub prune_budget_secs: u64,
}

/// Executes native Mediary 3.4.6 single-step archive (`POST /api/plugin/open115/files/organize`)
/// for winning items and direct 115 Recycle Bin deletion (`POST /api/plugin/open115/files/delete`)
/// for rejected/duplicate items.
pub fn execute_staging_items(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    ids: &[i64],
    override_route: Option<&str>,
) -> Result<Value> {
    execute_staging_items_with_budget(db, host, cfg, settings, ids, override_route, 35)
}

pub fn execute_staging_items_ui(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    ids: &[i64],
    override_route: Option<&str>,
) -> Result<Value> {
    execute_staging_items_opts(
        db,
        host,
        cfg,
        settings,
        ids,
        override_route,
        ExecOpts {
            soft_budget_secs: 12,
            max_organize_files: 8,
            organize_http_timeout_secs: 34,
            prune_budget_secs: 6,
        },
    )
}

pub fn execute_staging_items_with_budget(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    ids: &[i64],
    override_route: Option<&str>,
    soft_budget_secs: u64,
) -> Result<Value> {
    execute_staging_items_opts(
        db,
        host,
        cfg,
        settings,
        ids,
        override_route,
        ExecOpts {
            soft_budget_secs,
            max_organize_files: 40,
            organize_http_timeout_secs: 80,
            prune_budget_secs: 12,
        },
    )
}

pub fn execute_staging_items_opts(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    ids: &[i64],
    override_route: Option<&str>,
    opts: ExecOpts,
) -> Result<Value> {
    ensure!(!ids.is_empty(), "请先选择要处理的条目");
    if override_route == Some("dismiss") {
        let now = now_ts();
        let mut count = 0usize;
        for id in ids {
            count += db.execute(
                "UPDATE staging_items SET state = 'dismissed', updated_at = ? WHERE id = ?",
                params![now, id],
            )?;
        }
        return Ok(json!({ "dismissed": count, "moved": 0, "remaining_ids": [] }));
    }

    ensure!(
        !settings.dry_run,
        "当前处于「仅预览模式（干跑）」，请先在插件配置中关闭「仅预览模式」后再执行实际归档/清理"
    );

    let exec_clock = Instant::now();
    let soft_budget_secs = opts.soft_budget_secs.max(3);

    struct PendingAction {
        row_id: i64,
        pipeline_id: i64,
        file_id: String,
        parent_cid: String,
        name: String,
        live_name: String,
        live_size: u64,
        live_sha1: String,
        live_pick_code: String,
        sidecar_ids: Vec<String>,
        move_subtitles: bool,
        reject_skip: bool,
        display_title: String,
        source_path: String,
        existing_lib_path: String,
        verdict: String,
        media_type: String,
        tmdb_id: String,
        season: u32,
        episode: u32,
        category: String,
        slot: String,
        cand_score: f64,
        slot_label: String,
        reason: String,
        route: &'static str, // "organize" | "recycle"
        org_key: (String, String, String),
        ledger_info: Option<Value>,
    }

    let mut recycle_by_parent: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dir_cache: BTreeMap<String, (BTreeMap<String, Cloud115File>, bool)> = BTreeMap::new();
    let mut touched_pipelines: BTreeSet<i64> = BTreeSet::new();
    let mut pending_actions: Vec<PendingAction> = Vec::new();
    let mut remaining_ids: Vec<i64> = Vec::new();

    for (idx, &row_id) in ids.iter().enumerate() {
        let row = db
            .query_row(
                "SELECT pipeline_id, file_id, parent_cid, rel_path, name, sha1, media_type, tmdb_id,
                        title, season, episode, category, slot, cand_score, lib_score,
                        verdict, state, reason, detail_json, sidecar_ids_json
                 FROM staging_items WHERE id = ?",
                [row_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, String>(8)?,
                        r.get::<_, u32>(9)?,
                        r.get::<_, u32>(10)?,
                        r.get::<_, String>(11)?,
                        r.get::<_, String>(12)?,
                        r.get::<_, f64>(13)?,
                        r.get::<_, Option<f64>>(14)?,
                        r.get::<_, String>(15)?,
                        r.get::<_, String>(16)?,
                        r.get::<_, String>(17)?,
                        r.get::<_, String>(18)?,
                        r.get::<_, String>(19)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            pipeline_id,
            file_id,
            parent_cid,
            rel_path,
            name,
            sha1,
            media_type,
            tmdb_id,
            title,
            season,
            episode,
            category,
            slot,
            cand_score,
            lib_score,
            verdict,
            state,
            reason,
            detail_raw,
            sidecars_raw,
        )) = row
        else {
            continue;
        };
        if state != "pending" {
            continue;
        }
        let Some(pipeline) = cfg.pipelines.iter().find(|p| p.id == pipeline_id) else {
            continue;
        };
        touched_pipelines.insert(pipeline_id);

        let route = match override_route {
            Some("organize") => "organize",
            Some("recycle") => "recycle",
            None => match verdict.as_str() {
                "admit_new" | "admit_upgrade" => "organize",
                "reject" | "already_in_lib" => {
                    if pipeline.reject_mode == "skip" {
                        db.execute(
                            "UPDATE staging_items SET state = 'dismissed', updated_at = ? WHERE id = ?",
                            params![now_ts(), row_id],
                        )?;
                        continue;
                    }
                    "recycle"
                }
                _ => continue,
            },
            _ => bail!("不支持的操作类型"),
        };

        let lookup_cid = if parent_cid.trim().is_empty() {
            pipeline.source_cid.trim().to_string()
        } else {
            parent_cid.trim().to_string()
        };
        if !dir_cache.contains_key(&lookup_cid) {
            if !dir_cache.is_empty() && exec_clock.elapsed() >= Duration::from_secs(soft_budget_secs) {
                remaining_ids.extend_from_slice(&ids[idx..]);
                break;
            }
            dir_cache.insert(
                lookup_cid.clone(),
                host.open115_list_dir_files_map_ext(&lookup_cid, 3)?,
            );
        }
        let (cached_map, is_complete) = dir_cache.get(&lookup_cid).unwrap();
        let live_opt = cached_map.get(&file_id).cloned().or_else(|| {
            if *is_complete {
                None
            } else {
                host.open115_find_file_in_cid(&lookup_cid, &file_id).ok().flatten()
            }
        });
        let Some(live_file) = live_opt else {
            // File was already moved/cleaned out of `lookup_cid`: self-heal by removing the stale cache row!
            db.execute("DELETE FROM staging_items WHERE id = ?", [row_id])?;
            continue;
        };
        if !sha1.is_empty() && !live_file.sha1.is_empty() && !live_file.sha1.eq_ignore_ascii_case(&sha1) {
            db.execute(
                "UPDATE staging_items SET state = 'review', verdict = 'review', reason = '文件 SHA1 已变化，请重新扫描核对', updated_at = ? WHERE id = ?",
                params![now_ts(), row_id],
            )?;
            continue;
        }

        let sidecar_ids: Vec<String> = serde_json::from_str(&sidecars_raw).unwrap_or_default();
        let org_key = if route == "organize" {
            (
                lookup_cid.clone(),
                media_type.trim().to_string(),
                tmdb_id.trim().to_string(),
            )
        } else {
            (String::new(), String::new(), String::new())
        };

        let detail: Value = serde_json::from_str(&detail_raw).unwrap_or_default();
        let src_root = if !pipeline.source_name.trim().is_empty() {
            pipeline.source_name.trim().to_string()
        } else {
            format!("CID:{}", pipeline.source_cid.trim())
        };
        let source_path = if rel_path.trim().is_empty() {
            format!("115://{src_root}/{name}")
        } else {
            format!(
                "115://{src_root}/{}/{name}",
                rel_path.trim().trim_matches('/')
            )
        };
        let existing_lib_path = detail["existing_path"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        let display_title = if !tmdb_id.is_empty() && !title.trim().is_empty() {
            if media_type == "tv" && episode > 0 {
                format!("{} S{:02}E{:02}", title.trim(), season.max(1), episode)
            } else {
                title.trim().to_string()
            }
        } else {
            name.clone()
        };
        let eff_category = if !category.trim().is_empty() {
            category.trim().to_string()
        } else {
            category::locate(
                &cfg.host_sync.categories,
                &media_type,
                &[existing_lib_path.as_str(), source_path.as_str()],
            )
            .unwrap_or_default()
        };
        let slot_label = match (!eff_category.is_empty(), !slot.trim().is_empty()) {
            (true, true) => format!("{eff_category} · {}", slot.trim()),
            (true, false) => eff_category.clone(),
            (false, true) => format!("主程序自动分类 · {}", slot.trim()),
            (false, false) => "主程序自动分类".to_string(),
        };

        let ledger_info = if route == "organize" && verdict == "admit_upgrade" && !tmdb_id.is_empty() {
            Some(json!({
                "media_type": media_type,
                "tmdb_id": tmdb_id,
                "season": season,
                "episode": episode,
                "item_id": detail["item_id"].as_str().unwrap_or_default(),
                "title": display_title.clone(),
                "category": eff_category,
                "slot": slot,
                "rule_id": detail["rule_id"].as_str().unwrap_or_default(),
                "file_id": file_id,
                "sha1": live_file.sha1,
                "old_source_ids": detail["old_source_ids"],
                "cand_score": cand_score,
                "old_score": lib_score.unwrap_or(0.0),
            }))
        } else {
            None
        };

        pending_actions.push(PendingAction {
            row_id,
            pipeline_id,
            file_id,
            parent_cid: lookup_cid,
            name,
            live_name: live_file.name,
            live_size: live_file.size,
            live_sha1: live_file.sha1,
            live_pick_code: live_file.pick_code,
            sidecar_ids,
            move_subtitles: pipeline.move_subtitles,
            reject_skip: pipeline.reject_mode == "skip",
            display_title,
            source_path,
            existing_lib_path,
            verdict,
            media_type,
            tmdb_id,
            season,
            episode,
            category: eff_category,
            slot,
            cand_score,
            slot_label,
            reason,
            route,
            org_key,
            ledger_info,
        });
    }

    // Intra-batch tournament dedup: when multiple files in `待洗版` map to the EXACT SAME
    // `(media_type, tmdb_id, season, episode, slot)` (e.g. duplicate shares from MediFind),
    // keep only the top `keep_per_slot` highest-scoring candidate(s) for `organize` and recycle duplicates!
    if override_route.is_none() {
        type EpSlotKey = (String, String, u32, u32, String);
        let mut by_ep_slot: BTreeMap<EpSlotKey, Vec<usize>> = BTreeMap::new();
        for (i, pa) in pending_actions.iter().enumerate() {
            if pa.route == "organize"
                && !pa.tmdb_id.trim().is_empty()
                && (pa.media_type != "tv" || pa.episode > 0)
            {
                let key = (
                    pa.media_type.clone(),
                    pa.tmdb_id.clone(),
                    if pa.media_type == "tv" { pa.season.max(1) } else { 0 },
                    if pa.media_type == "tv" { pa.episode } else { 0 },
                    pa.slot.clone(),
                );
                by_ep_slot.entry(key).or_default().push(i);
            }
        }
        let mut dismissed_indices = BTreeSet::new();
        for ((mt, _, _, _, _), mut indices) in by_ep_slot {
            let sample_cat = &pending_actions[indices[0]].category;
            let keep_n = cfg
                .strategy
                .effective_for(&mt, sample_cat, &cfg.host_sync.categories, &cfg.rules)
                .keep_per_slot
                .max(1);
            if indices.len() <= keep_n {
                continue;
            }
            indices.sort_by(|&a, &b| {
                pending_actions[b]
                    .cand_score
                    .total_cmp(&pending_actions[a].cand_score)
                    .then_with(|| pending_actions[b].live_size.cmp(&pending_actions[a].live_size))
                    .then_with(|| pending_actions[a].row_id.cmp(&pending_actions[b].row_id))
            });
            let win_name = pending_actions[indices[0]].name.clone();
            let win_score = pending_actions[indices[0]].cand_score;
            for &loser_idx in indices.iter().skip(keep_n) {
                let pa = &mut pending_actions[loser_idx];
                if pa.reject_skip {
                    let _ = db.execute(
                        "UPDATE staging_items SET state = 'dismissed', updated_at = ? WHERE id = ?",
                        params![now_ts(), pa.row_id],
                    );
                    dismissed_indices.insert(loser_idx);
                } else {
                    pa.route = "recycle";
                    pa.ledger_info = None;
                    pa.reason = format!(
                        "同批次待洗版已有同槽更优/同分胜出文件《{win_name}》({win_score:.2} ≥ {:.2})，自动淘汰重复候选移入 115 回收站",
                        pa.cand_score
                    );
                }
            }
        }
        if !dismissed_indices.is_empty() {
            let mut idx_counter = 0usize;
            pending_actions.retain(|_| {
                let keep = !dismissed_indices.contains(&idx_counter);
                idx_counter += 1;
                keep
            });
        }
    }

    // Build `recycle_by_parent` and `organize_groups` after intra-batch dedup!
    let mut organize_groups: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
    for (idx, pa) in pending_actions.iter().enumerate() {
        if pa.route == "organize" {
            let grp = organize_groups.entry(pa.org_key.clone()).or_default();
            if !grp.iter().any(|&prev| pending_actions[prev].file_id == pa.file_id) {
                grp.push(idx);
            }
        } else {
            let entry = recycle_by_parent.entry(pa.parent_cid.clone()).or_default();
            if !entry.contains(&pa.file_id) {
                entry.push(pa.file_id.clone());
            }
            if pa.move_subtitles {
                for sc_id in &pa.sidecar_ids {
                    if !sc_id.is_empty() && !entry.contains(sc_id) {
                        entry.push(sc_id.clone());
                    }
                }
            }
        }
    }

    let mut organized_count = 0usize;
    let mut recycled_115 = 0usize;
    let mut has_upgrade_ledger = false;
    let mut organized_indices: Vec<usize> = Vec::new();
    let mut recycled_indices: Vec<usize> = Vec::new();
    let mut failed_notices: Vec<(usize, String)> = Vec::new();
    let mut moved_by_cid: BTreeMap<String, usize> = BTreeMap::new();

    // 1. Move rejected/duplicate files directly into 115 Official Recycle Bin grouped by `parent_cid` and commit immediately!
    for (p_cid, batch_ids) in &recycle_by_parent {
        if (recycled_115 > 0 || organized_count > 0)
            && exec_clock.elapsed() >= Duration::from_secs(soft_budget_secs)
        {
            for pa in pending_actions
                .iter()
                .filter(|pa| pa.route == "recycle" && &pa.parent_cid == p_cid)
            {
                remaining_ids.push(pa.row_id);
            }
            continue;
        }
        host.open115_delete_files(p_cid, batch_ids)?;
        *moved_by_cid.entry(p_cid.clone()).or_insert(0) += batch_ids.len();
        let now = now_ts();
        for (p_idx, pa) in pending_actions
            .iter()
            .enumerate()
            .filter(|(_, pa)| pa.route == "recycle" && &pa.parent_cid == p_cid)
        {
            recycled_115 += 1;
            recycled_indices.push(p_idx);
            let recycle_target = if !pa.existing_lib_path.is_empty() {
                if pa.verdict == "already_in_lib" {
                    format!("115 官方回收站（库内已有：{}）", pa.existing_lib_path)
                } else {
                    format!("115 官方回收站（保留库内：{}）", pa.existing_lib_path)
                }
            } else {
                "115 官方回收站".to_string()
            };
            let log_detail = format!("已直接移入 115 官方回收站 · {}", pa.reason);
            db.execute(
                "UPDATE staging_items SET state = 'recycled_115', updated_at = ? WHERE id = ?",
                params![now, pa.row_id],
            )?;
            db::log_media_history(
                db,
                "115归档",
                &pa.display_title,
                &pa.name,
                &pa.source_path,
                &recycle_target,
                &pa.slot_label,
                "recycled_115",
                "done",
                &log_detail,
            );
        }
    }

    // 2. Directly archive winning files per TMDB group, capping files per slice (`opts.max_organize_files`) and stopping cleanly before `soft_budget_secs`!
    let mut org_submitted_total = 0usize;
    for (key, group_indices) in &organize_groups {
        let did_any_work = organized_count > 0 || recycled_115 > 0;
        let quota_left = opts.max_organize_files.saturating_sub(org_submitted_total);
        if (did_any_work && exec_clock.elapsed() >= Duration::from_secs(soft_budget_secs))
            || quota_left == 0
        {
            for &pi in group_indices {
                remaining_ids.push(pending_actions[pi].row_id);
            }
            continue;
        }

        let take_n = group_indices.len().min(quota_left.max(1));
        let (run_indices, defer_indices) = group_indices.split_at(take_n);
        for &di in defer_indices {
            remaining_ids.push(pending_actions[di].row_id);
        }

        let group_files: Vec<OrganizeFileItem> = run_indices
            .iter()
            .map(|&pi| {
                let pa = &pending_actions[pi];
                OrganizeFileItem {
                    file_id: pa.file_id.clone(),
                    parent_id: pa.parent_cid.clone(),
                    name: pa.live_name.clone(),
                    size: pa.live_size,
                    sha1: pa.live_sha1.clone(),
                    pick_code: pa.live_pick_code.clone(),
                    season: pa.season,
                    episode: pa.episode,
                }
            })
            .collect();
        org_submitted_total += group_files.len();

        let grp_pid = pending_actions[run_indices[0]].pipeline_id;
        let (archive_target_cid, archive_task_name) =
            resolve_archive_context(db, host, cfg, grp_pid);

        let mut active_indices: Vec<usize> = run_indices.to_vec();
        let mut res = host
            .open115_organize_files_with_timeout(
                &key.0,
                archive_target_cid.as_deref(),
                archive_task_name.as_deref(),
                &key.1,
                &key.2,
                &group_files,
                opts.organize_http_timeout_secs,
            )
            .map_err(|e| format!("{e:#}"));

        // If a multi-episode TV batch failed because later episodes exceed TMDB's season episode count
        // (e.g. S01E31 when TMDB splits eps 31-60 into Season 2), isolate out-of-range episodes into `review`
        // and immediately archive the valid episodes `< bad_ep`!
        if let Err(ref err_msg) = res {
            if active_indices.len() > 1 && key.1 == "tv" && err_msg.contains("季集范围") {
                let mut bad_ep_opt: Option<u32> = None;
                for tok in err_msg.split(|c: char| !c.is_ascii_alphanumeric()) {
                    let up = tok.to_ascii_uppercase();
                    if up.starts_with('S') {
                        if let Some((_, ep_str)) = up.split_once('E') {
                            if let Ok(ep_num) = ep_str.parse::<u32>() {
                                if ep_num > 0 {
                                    bad_ep_opt = Some(ep_num);
                                    break;
                                }
                            }
                        }
                    }
                }
                if let Some(bad_ep) = bad_ep_opt {
                    let (valid_idx, oob_idx): (Vec<usize>, Vec<usize>) = active_indices
                        .iter()
                        .copied()
                        .partition(|&pi| {
                            pending_actions[pi].episode > 0 && pending_actions[pi].episode < bad_ep
                        });
                    let now_oob = now_ts();
                    for oob_pi in oob_idx {
                        let oob_pa = &pending_actions[oob_pi];
                        let oob_reason = format!(
                            "TMDB 当前第 {} 季不含第 {} 集（可能属于下一季或 SP），已转入待人工核对季集",
                            oob_pa.season.max(1),
                            oob_pa.episode
                        );
                        let _ = db.execute(
                            "UPDATE staging_items SET verdict = 'review', state = 'pending', reason = ?, updated_at = ? WHERE id = ?",
                            params![oob_reason, now_oob, oob_pa.row_id],
                        );
                        let _ = db.execute(
                            "DELETE FROM activity_logs WHERE record_type = 'media' AND status = 'warning' AND sub_title = ?",
                            params![oob_pa.name],
                        );
                    }
                    if !valid_idx.is_empty() {
                        let valid_files: Vec<OrganizeFileItem> = valid_idx
                            .iter()
                            .map(|&pi| {
                                let pa = &pending_actions[pi];
                                OrganizeFileItem {
                                    file_id: pa.file_id.clone(),
                                    parent_id: pa.parent_cid.clone(),
                                    name: pa.live_name.clone(),
                                    size: pa.live_size,
                                    sha1: pa.live_sha1.clone(),
                                    pick_code: pa.live_pick_code.clone(),
                                    season: pa.season,
                                    episode: pa.episode,
                                }
                            })
                            .collect();
                        active_indices = valid_idx;
                        res = host
                            .open115_organize_files_with_timeout(
                                &key.0,
                                archive_target_cid.as_deref(),
                                archive_task_name.as_deref(),
                                &key.1,
                                &key.2,
                                &valid_files,
                                opts.organize_http_timeout_secs,
                            )
                            .map_err(|e| format!("{e:#}"));
                    } else {
                        continue;
                    }
                }
            }
        }

        let now = now_ts();
        if res.is_ok() {
            let moved_items: usize = active_indices
                .iter()
                .map(|&pi| {
                    1 + if pending_actions[pi].move_subtitles {
                        pending_actions[pi].sidecar_ids.len()
                    } else {
                        0
                    }
                })
                .sum();
            *moved_by_cid.entry(key.0.clone()).or_insert(0) += moved_items;
        }
        for &pi in &active_indices {
            let pa = &pending_actions[pi];
            let _ = pa.pipeline_id;
            match &res {
                Ok(resp) => {
                    organized_count += 1;
                    organized_indices.push(pi);
                    // Clean up any prior transient `warning` rows for this exact file now that archiving succeeded!
                    let _ = db.execute(
                        "DELETE FROM activity_logs
                          WHERE record_type = 'media'
                            AND action = 'organized'
                            AND status = 'warning'
                            AND sub_title = ?",
                        params![pa.name],
                    );
                    let mut target_path = resolve_organized_target_path(
                        host,
                        resp,
                        &pa.file_id,
                        &pa.live_pick_code,
                        &pa.name,
                        &pa.media_type,
                        &pa.tmdb_id,
                        pa.season,
                        pa.episode,
                        "",
                    );
                    if target_path.is_empty() {
                        target_path = if !cfg.host_sync.transfer_target_folder.is_empty() {
                            format!("115://{}（主程序二级分类归档）", cfg.host_sync.transfer_target_folder)
                        } else {
                            "Mediary 媒体库（主程序二级分类归档）".to_string()
                        };
                    }
                    let mut eff_slot_label = pa.slot_label.clone();
                    if pa.category.trim().is_empty() {
                        if let Some(cat) = category::locate(
                            &cfg.host_sync.categories,
                            &pa.media_type,
                            &[&target_path],
                        ) {
                            eff_slot_label = if !pa.slot.trim().is_empty() {
                                format!("{cat} · {}", pa.slot.trim())
                            } else {
                                cat
                            };
                        }
                    }
                    let log_detail = format!(
                        "已通过主程序原生归档一步移入媒体库二级分类并生成 STRM · {}",
                        pa.reason
                    );
                    db.execute(
                        "UPDATE staging_items SET state = 'organized', updated_at = ? WHERE id = ?",
                        params![now, pa.row_id],
                    )?;
                    if let Some(ref lg) = pa.ledger_info {
                        has_upgrade_ledger = true;
                        let lkey = format!(
                            "115:{}:{}:{}:{}:{}",
                            lg["media_type"].as_str().unwrap_or_default(),
                            lg["tmdb_id"].as_str().unwrap_or_default(),
                            lg["season"].as_u64().unwrap_or(0),
                            lg["episode"].as_u64().unwrap_or(0),
                            lg["file_id"].as_str().unwrap_or_default()
                        );
                        db.execute(
                            "INSERT INTO wash_ledger (
                                ledger_key, source_kind, media_type, tmdb_id, season, episode, item_id,
                                title, category, slot, rule_id, expected_file_id, expected_sha1,
                                old_source_ids_json, cand_score, old_score, state, message, created_at, updated_at
                             ) VALUES (?,'cloud115',?,?,?,?,?,?,?,?,?,?,?, ?, ?,?,'awaiting_ingest',?,?,?)
                             ON CONFLICT(ledger_key) DO UPDATE SET
                                state='awaiting_ingest', updated_at=excluded.updated_at",
                            params![
                                lkey,
                                lg["media_type"].as_str().unwrap_or_default(),
                                lg["tmdb_id"].as_str().unwrap_or_default(),
                                lg["season"].as_u64().unwrap_or(0),
                                lg["episode"].as_u64().unwrap_or(0),
                                lg["item_id"].as_str().unwrap_or_default(),
                                lg["title"].as_str().unwrap_or(&pa.name),
                                lg["category"].as_str().unwrap_or_default(),
                                lg["slot"].as_str().unwrap_or_default(),
                                lg["rule_id"].as_str().unwrap_or_default(),
                                lg["file_id"].as_str().unwrap_or_default(),
                                lg["sha1"].as_str().unwrap_or_default(),
                                serde_json::to_string(&lg["old_source_ids"]).unwrap_or_else(|_| "[]".into()),
                                lg["cand_score"].as_f64().unwrap_or(0.0),
                                lg["old_score"].as_f64().unwrap_or(0.0),
                                "主程序已一步归档生成 STRM，等待媒体库增量入库确认后自动清理旧源",
                                now,
                                now,
                            ],
                        )?;
                    }
                    db::log_media_history(
                        db,
                        "115归档",
                        &pa.display_title,
                        &pa.name,
                        &pa.source_path,
                        &target_path,
                        &eff_slot_label,
                        "organized",
                        "done",
                        &log_detail,
                    );
                }
                Err(err_msg) => {
                    if failed_notices.len() < 4 {
                        failed_notices.push((pi, err_msg.clone()));
                    }
                    let fail_reason = format!("主程序原生归档失败：{err_msg}");
                    db.execute(
                        "UPDATE staging_items SET state = 'pending', reason = ?, updated_at = ? WHERE id = ?",
                        params![fail_reason, now, pa.row_id],
                    )?;
                    let updated_warn = db
                        .execute(
                            "UPDATE activity_logs
                                SET title = ?1, source_path = ?2, slot = ?3, detail = ?4, created_at = ?5
                              WHERE id = (
                                  SELECT id FROM activity_logs
                                   WHERE record_type = 'media'
                                     AND action = 'organized'
                                     AND status = 'warning'
                                     AND sub_title = ?6
                                   ORDER BY id DESC LIMIT 1
                              )",
                            params![
                                pa.display_title,
                                pa.source_path,
                                pa.slot_label,
                                fail_reason,
                                now,
                                pa.name
                            ],
                        )
                        .unwrap_or(0);
                    if updated_warn == 0 {
                        db::log_media_history(
                            db,
                            "115归档",
                            &pa.display_title,
                            &pa.name,
                            &pa.source_path,
                            "归档未完成（保留原目录）",
                            &pa.slot_label,
                            "organized",
                            "warning",
                            &fail_reason,
                        );
                    }
                }
            }
        }
    }

    if has_upgrade_ledger && exec_clock.elapsed() < Duration::from_secs(soft_budget_secs) {
        let _ = reconcile_ledger(db, host, cfg, settings);
    }

    let total_handled = organized_count + recycled_115;
    let mut cleaned_dirs = 0usize;
    if total_handled > 0
        && remaining_ids.is_empty()
        && exec_clock.elapsed() < Duration::from_secs(soft_budget_secs.saturating_sub(4))
    {
        for pid in &touched_pipelines {
            if let Some(p) = cfg.pipelines.iter().find(|p| p.id == *pid) {
                if p.recursive && p.reject_mode != "skip" {
                    cleaned_dirs +=
                        prune_staging_subdirs_with_budget(host, cfg, &p.source_cid, opts.prune_budget_secs);
                }
            }
        }
    }

    if settings.notify && (total_handled > 0 || !failed_notices.is_empty()) {
        let strip_ep = |s: &str| -> String {
            if let Some(pos) = s.rfind(" S") {
                if s[pos + 2..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    return s[..pos].trim().to_string();
                }
            }
            s.trim().to_string()
        };
        let build_group_labels = |idxs: &[usize]| -> Vec<(String, String, String)> {
            // Returns Vec<(media_type, tmdb_id, formatted_label)>
            let mut groups: Vec<(String, String, String, Vec<(u32, u32)>)> = Vec::new();
            for &i in idxs {
                let pa = &pending_actions[i];
                let base = strip_ep(&pa.display_title);
                if let Some(g) = groups.iter_mut().find(|g| {
                    (!pa.tmdb_id.is_empty() && g.0 == pa.media_type && g.1 == pa.tmdb_id)
                        || (pa.tmdb_id.is_empty() && g.2 == base)
                }) {
                    if pa.media_type == "tv" && pa.episode > 0 {
                        let se = (pa.season.max(1), pa.episode);
                        if !g.3.contains(&se) {
                            g.3.push(se);
                        }
                    }
                } else {
                    let mut eps = Vec::new();
                    if pa.media_type == "tv" && pa.episode > 0 {
                        eps.push((pa.season.max(1), pa.episode));
                    }
                    groups.push((pa.media_type.clone(), pa.tmdb_id.clone(), base, eps));
                }
            }
            groups
                .into_iter()
                .map(|(mt, tid, base, mut eps)| {
                    eps.sort_unstable();
                    let label = if eps.is_empty() {
                        base
                    } else if eps.len() == 1 {
                        format!("{base} · S{:02}E{:02}", eps[0].0, eps[0].1)
                    } else {
                        let s0 = eps[0].0;
                        let same_s = eps.iter().all(|(s, _)| *s == s0);
                        let contiguous = same_s
                            && eps.windows(2).all(|w| w[1].1 == w[0].1 + 1);
                        if contiguous {
                            format!(
                                "{base} · S{:02}E{:02}-E{:02}",
                                s0,
                                eps.first().unwrap().1,
                                eps.last().unwrap().1
                            )
                        } else if same_s && eps.len() <= 4 {
                            let ep_list: Vec<String> =
                                eps.iter().map(|(_, e)| format!("E{e:02}")).collect();
                            format!("{base} · S{s0:02}{}", ep_list.join("/"))
                        } else {
                            format!("{base}（{}集）", eps.len())
                        }
                    };
                    (mt, tid, label)
                })
                .collect()
        };

        let now_str = format_ts_cn(now_ts());
        let tpls = host.get_notification_templates();

        if !organized_indices.is_empty() {
            if tpls.transfer_completed.enabled {
                let org_groups = build_group_labels(&organized_indices);
                let first_pa = &pending_actions[organized_indices[0]];
                let media_icon = if first_pa.media_type == "tv" { "📺" } else { "🎬" };
                let display_name = if org_groups.len() == 1 {
                    org_groups[0].2.clone()
                } else {
                    format!("{} 等 {} 部影视", strip_ep(&first_pa.display_title), org_groups.len())
                };
                let mut vars = BTreeMap::new();
                vars.insert("is_success", "true".to_string());
                vars.insert("media_icon", media_icon.to_string());
                vars.insert("name", strip_ep(&first_pa.display_title));
                vars.insert("display_name", display_name);
                if org_groups.len() == 1 && !first_pa.category.is_empty() {
                    vars.insert("category", first_pa.category.clone());
                }
                let nt_title =
                    crate::host::render_mediary_template(&tpls.transfer_completed.title, &vars);
                let mut lines: Vec<String> = Vec::new();
                let rendered_body =
                    crate::host::render_mediary_template(&tpls.transfer_completed.content, &vars);
                if !rendered_body.is_empty() {
                    lines.push(rendered_body);
                }
                if organized_indices.len() == 1 {
                    if first_pa.verdict == "admit_upgrade" {
                        let old_sc = first_pa
                            .ledger_info
                            .as_ref()
                            .and_then(|lg| lg["old_score"].as_f64())
                            .unwrap_or(0.0);
                        lines.push(format!(
                            "🎞️ 洗版: {} ({:.2} > {:.2})",
                            first_pa.slot, first_pa.cand_score, old_sc
                        ));
                    } else if !first_pa.slot.is_empty() {
                        lines.push(format!(
                            "🎞️ 规格: {} ({:.2}分 · 新片入库)",
                            first_pa.slot, first_pa.cand_score
                        ));
                    }
                } else {
                    let preview: Vec<&str> =
                        org_groups.iter().take(4).map(|g| g.2.as_str()).collect();
                    let more = if org_groups.len() > 4 { " 等" } else { "" };
                    lines.push(format!("📥 入库: {organized_count} 项（生成 STRM）"));
                    if org_groups.len() > 1 {
                        lines.push(format!("📌 明细: {}{more}", preview.join(" / ")));
                    }
                }
                if recycled_115 > 0 {
                    lines.push(format!("🗑️ 拦截: 同批去重/低分回收 {recycled_115} 项"));
                }
                if cleaned_dirs > 0 {
                    lines.push(format!("🧹 目录: 清理空目录 {cleaned_dirs} 个"));
                }
                if !remaining_ids.is_empty() {
                    lines.push(format!("⏳ 队列: 其余 {} 项分批继续处理", remaining_ids.len()));
                }
                lines.push(format!("🗓️ 时间: {now_str}"));
                let img = host.resolve_tmdb_card_image(&first_pa.tmdb_id, &first_pa.media_type);
                host.notify(&nt_title, &lines.join("\n"), img.as_deref());
            }
        } else if !recycled_indices.is_empty() {
            let rec_groups = build_group_labels(&recycled_indices);
            let first_rec = recycled_indices
                .iter()
                .map(|&i| &pending_actions[i])
                .find(|pa| !pa.tmdb_id.is_empty())
                .unwrap_or(&pending_actions[recycled_indices[0]]);
            let media_icon = if first_rec.media_type == "tv" { "📺" } else { "🎬" };
            let nt_title = if recycled_indices.len() == 1 {
                format!("{media_icon} {} 🗑️去重回收", rec_groups[0].2)
            } else if rec_groups.len() == 1 {
                format!("{media_icon} {} 🗑️回收清理 ({recycled_115} 项)", rec_groups[0].2)
            } else {
                format!(
                    "🧹 {} 等 {} 部影视 🗑️回收清理 ({recycled_115} 项)",
                    strip_ep(&first_rec.display_title),
                    rec_groups.len()
                )
            };
            let dup_cnt = recycled_indices
                .iter()
                .filter(|&&i| {
                    pending_actions[i].verdict == "already_in_lib"
                        || pending_actions[i].reason.contains("同批次")
                })
                .count();
            let low_cnt = recycled_indices.len().saturating_sub(dup_cnt);
            let reason_summary = match (dup_cnt > 0, low_cnt > 0) {
                (true, true) => format!("已在库/重复 {dup_cnt} 项 · 低分淘汰 {low_cnt} 项"),
                (true, false) => format!("同源已在库/同批重复 {dup_cnt} 项"),
                (false, true) => format!("权重评分未达标淘汰 {low_cnt} 项"),
                (false, false) => format!("移入 115 回收站 {recycled_115} 项"),
            };
            let preview: Vec<&str> = rec_groups.iter().take(4).map(|g| g.2.as_str()).collect();
            let more = if rec_groups.len() > 4 { " 等" } else { "" };
            let mut lines = Vec::new();
            if rec_groups.len() > 1 || recycled_indices.len() > 1 {
                lines.push(format!("📌 涉及: {}{more}", preview.join(" / ")));
            }
            lines.push(format!("🛡️ 原因: {reason_summary}"));
            if cleaned_dirs > 0 {
                lines.push(format!("🧹 目录: 清理空目录 {cleaned_dirs} 个"));
            }
            lines.push(format!("🗓️ 时间: {now_str}"));
            let img = host.resolve_tmdb_card_image(&first_rec.tmdb_id, &first_rec.media_type);
            host.notify(&nt_title, &lines.join("\n"), img.as_deref());
        } else if !failed_notices.is_empty() && tpls.transfer_completed.enabled {
            let (f_idx, ref f_err) = failed_notices[0];
            let f_pa = &pending_actions[f_idx];
            let mut vars = BTreeMap::new();
            vars.insert("is_failed", "true".to_string());
            vars.insert("name", f_pa.display_title.clone());
            vars.insert("path", f_pa.source_path.clone());
            vars.insert("error", f_err.clone());
            let nt_title =
                crate::host::render_mediary_template(&tpls.transfer_completed.title, &vars);
            let nt_body =
                crate::host::render_mediary_template(&tpls.transfer_completed.content, &vars);
            let img = host.resolve_tmdb_card_image(&f_pa.tmdb_id, &f_pa.media_type);
            host.notify(&nt_title, &nt_body, img.as_deref());
        }
    }

    Ok(json!({
        "moved": total_handled,
        "organized": organized_count,
        "recycled_115": recycled_115,
        "cleaned_dirs": cleaned_dirs,
        "moved_by_cid": moved_by_cid,
        "remaining": remaining_ids.len(),
        "remaining_ids": remaining_ids,
    }))
}

#[allow(clippy::too_many_arguments)]
fn resolve_organized_target_path(
    host: &HostClient,
    resp: &Value,
    file_id: &str,
    pick_code: &str,
    source_name: &str,
    media_type: &str,
    tmdb_id: &str,
    season: u32,
    episode: u32,
    prev_target_path: &str,
) -> String {
    let mut resp_candidate = String::new();
    let scopes = [resp, &resp["data"]];
    for scope in scopes {
        for arr_key in ["items", "files", "results", "moved", "organized"] {
            if let Some(items) = scope[arr_key].as_array() {
                let matched = items
                    .iter()
                    .find(|it| {
                        (!file_id.is_empty() && it["file_id"].as_str() == Some(file_id))
                            || (!pick_code.is_empty()
                                && (it["pick_code"].as_str() == Some(pick_code)
                                    || it["pickcode"].as_str() == Some(pick_code)))
                            || it["source_name"].as_str() == Some(source_name)
                            || it["name"].as_str() == Some(source_name)
                    })
                    .or_else(|| (items.len() == 1).then(|| &items[0]));
                if let Some(it) = matched {
                    for k in [
                        "strm_path",
                        "dest_path",
                        "target_path",
                        "cloud_path",
                        "destination",
                        "new_path",
                        "path",
                    ] {
                        if let Some(p) = it[k].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                            resp_candidate = p.to_string();
                            break;
                        }
                    }
                }
            }
            if !resp_candidate.is_empty() {
                break;
            }
        }
        if !resp_candidate.is_empty() {
            break;
        }
    }

    if resp_candidate.ends_with(".strm") {
        return resp_candidate;
    }

    for scope in scopes {
        if let Some(paths) = scope["strm_paths"].as_array() {
            let ep_tag = if media_type == "tv" && episode > 0 {
                format!("s{:02}e{:02}", season.max(1), episode)
            } else {
                String::new()
            };
            if !ep_tag.is_empty() {
                if let Some(found) = paths
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|p| p.to_ascii_lowercase().contains(&ep_tag))
                {
                    return found.trim().to_string();
                }
            }
            if paths.len() == 1 {
                if let Some(p) = paths[0].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    return p.to_string();
                }
            }
        }
    }

    if resp_candidate.is_empty() {
        for scope in scopes {
            for k in ["strm_path", "dest_path", "cloud_path", "target_path", "new_path"] {
                if let Some(p) = scope[k].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    resp_candidate = p.to_string();
                    break;
                }
            }
            if !resp_candidate.is_empty() {
                break;
            }
        }
    }
    if resp_candidate.ends_with(".strm") {
        return resp_candidate;
    }

    // Normalize `__cid_<digits>__/` prefix from FlowLink organizer responses into `媒体库/`
    if let Some(rest) = resp_candidate.strip_prefix("__cid_") {
        if let Some((_, after_cid)) = rest.split_once("__/") {
            resp_candidate = format!("媒体库/{after_cid}");
        }
    }

    let cand_stem = resp_candidate
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or_default()
        .to_lowercase();
    let cand_has_tmdbid = resp_candidate.contains("[tmdbid-");

    // Query the newly ingested `.strm` / `cloud_path` from Mediary's library item using exact `file_id` / `pick_code` first!
    if !tmdb_id.trim().is_empty() {
        let ep_pair = (media_type == "tv" && episode > 0).then_some((season.max(1), episode));
        if let Ok(page) = host.library_items_by_tmdb(tmdb_id, media_type, ep_pair) {
            for entry in &page.items {
                if let Ok(detail) = host.library_item_detail(&entry.id) {
                    let srcs = detail.effective_sources();
                    let is_stale_prev = |p: &str| -> bool {
                        let pt = p.trim();
                        !prev_target_path.trim().is_empty()
                            && pt == prev_target_path.trim()
                            && ((!cand_stem.is_empty() && !pt.to_lowercase().contains(&cand_stem))
                                || (cand_has_tmdbid && !pt.contains("[tmdbid-")))
                    };
                    let best = srcs
                        .iter()
                        .find(|s| {
                            !cand_stem.is_empty()
                                && s.strm_path.to_lowercase().contains(&cand_stem)
                                && (!cand_has_tmdbid || s.strm_path.contains("[tmdbid-"))
                        })
                        .or_else(|| {
                            srcs.iter().find(|s| {
                                !file_id.is_empty()
                                    && s.file_id == file_id
                                    && !is_stale_prev(&s.strm_path)
                            })
                        })
                        .or_else(|| {
                            srcs.iter().find(|s| {
                                !pick_code.is_empty()
                                    && s.effective_pick_code().eq_ignore_ascii_case(pick_code)
                                    && !is_stale_prev(&s.strm_path)
                            })
                        })
                        .or_else(|| {
                            (!prev_target_path.trim().is_empty())
                                .then(|| {
                                    srcs.iter().find(|s| {
                                        !s.strm_path.trim().is_empty()
                                            && s.strm_path.trim() != prev_target_path.trim()
                                            && (pick_code.is_empty()
                                                || s.effective_pick_code().is_empty())
                                    })
                                })
                                .flatten()
                        })
                        .or_else(|| {
                            (srcs.len() == 1
                                && !is_stale_prev(&srcs[0].strm_path)
                                && (pick_code.is_empty()
                                    || srcs[0].effective_pick_code().is_empty()))
                            .then(|| &srcs[0])
                        });
                    if let Some(hit) = best {
                        for p in [&hit.strm_path, &hit.cloud_path, &hit.file_path, &detail.path] {
                            if !p.trim().is_empty() && !is_stale_prev(p) {
                                return p.trim().to_string();
                            }
                        }
                    }
                }
            }
        }
    }

    // Map `媒体库/.../xxx.mkv` directly to `/strm/cloud/媒体库/.../xxx.strm`!
    if !resp_candidate.is_empty() {
        if let Some(rel_after_lib) = resp_candidate.strip_prefix("媒体库/") {
            let strm_rel = match rel_after_lib.rsplit_once('.') {
                Some((stem, _)) => format!("{stem}.strm"),
                None => format!("{rel_after_lib}.strm"),
            };
            if let Some((strm_prefix, _)) = prev_target_path.split_once("/媒体库/") {
                return format!("{strm_prefix}/媒体库/{strm_rel}");
            }
            return format!("/strm/cloud/媒体库/{strm_rel}");
        }
        return resp_candidate;
    }
    String::new()
}

/// Automatically backfills placeholder `target_path` (`115://媒体库（主程序二级分类归档）`) and `slot`
/// (`主程序自动分类 · ...`) in `activity_logs` once Mediary's asynchronous library scan indexes the new `.strm`.
pub fn backfill_recent_media_history(
    db: &Connection,
    host: Option<&HostClient>,
    cfg: &AppConfig,
    max_http_rows: usize,
) -> usize {
    let mut updated = 0usize;

    // Pass 1: Zero-HTTP local category & slot upgrade from `source_path` / `target_path` / `staging_items`
    if let Ok(mut stmt) = db.prepare(
        "SELECT al.id, al.title, al.sub_title, al.source_path, al.target_path, al.slot,
                COALESCE(si.media_type, ''), COALESCE(si.slot, '')
           FROM activity_logs al
           LEFT JOIN staging_items si
             ON si.id = (
                  SELECT s2.id FROM staging_items s2
                   WHERE (al.sub_title != '' AND s2.name = al.sub_title)
                   ORDER BY s2.id DESC LIMIT 1
                )
          WHERE al.record_type = 'media'
            AND (al.slot = '' OR al.slot LIKE '主程序自动分类%')
          ORDER BY al.id DESC LIMIT 120",
    ) {
        let rows: Vec<(i64, String, String, String, String, String, String, String)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .collect();
        drop(stmt);

        for (aid, title, sub_title, src_path, dst_path, cur_slot, s_mt, s_slot) in rows {
            let mt = if !s_mt.trim().is_empty() {
                s_mt.trim().to_string()
            } else if title.contains("S0")
                || title.contains("S1")
                || src_path.contains("/Season")
                || dst_path.contains("/Season")
            {
                "tv".to_string()
            } else {
                "movie".to_string()
            };
            let cat_opt = category::locate(
                &cfg.host_sync.categories,
                &mt,
                &[dst_path.as_str(), src_path.as_str()],
            );
            let mut slot_part = cur_slot
                .split_once(" · ")
                .map(|(_, r)| r.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| s_slot.trim().to_string());
            if slot_part.is_empty() && !sub_title.trim().is_empty() {
                let eff = cfg.strategy.effective_for(
                    &mt,
                    cat_opt.as_deref().unwrap_or_default(),
                    &cfg.host_sync.categories,
                    &cfg.rules,
                );
                if let Ok(rule) = rules::find_rule(cfg, &eff.rule_id) {
                    if let Ok(sc) = Scorer::new(rule) {
                        let prof = MediaProfile::extract(
                            &LibrarySource {
                                name: sub_title.clone(),
                                ..Default::default()
                            },
                            &[],
                        );
                        slot_part = sc
                            .score(
                                &prof,
                                &ReferenceBaseline::default(),
                                cat_opt.as_deref().unwrap_or_default(),
                                eff.iso_protected,
                            )
                            .slot;
                    }
                }
            }
            if let Some(cat) = cat_opt {
                let new_slot = if !slot_part.is_empty() {
                    format!("{cat} · {slot_part}")
                } else {
                    cat
                };
                if new_slot != cur_slot {
                    if db
                        .execute(
                            "UPDATE activity_logs SET slot = ? WHERE id = ?",
                            params![new_slot, aid],
                        )
                        .is_ok()
                    {
                        updated += 1;
                    }
                }
            }
        }
    }

    // Pass 2: Query Mediary library (bounded to `max_http_rows` & 2.5s) for organized rows whose `.strm` finished indexing asynchronously
    let Some(host) = host else {
        return updated;
    };
    if max_http_rows == 0 {
        return updated;
    }

    let Ok(mut stmt) = db.prepare(
        "SELECT al.id, al.source_path, al.target_path, al.slot,
                COALESCE(si.file_id, ''), COALESCE(si.pick_code, ''),
                COALESCE(si.media_type, ''), COALESCE(si.tmdb_id, ''),
                COALESCE(si.season, 0), COALESCE(si.episode, 0), COALESCE(si.slot, '')
           FROM activity_logs al
           JOIN staging_items si
             ON si.id = (
                  SELECT s2.id FROM staging_items s2
                   WHERE (al.sub_title != '' AND s2.name = al.sub_title)
                   ORDER BY s2.id DESC LIMIT 1
                )
          WHERE al.record_type = 'media'
            AND al.action = 'organized'
            AND al.status = 'done'
          ORDER BY al.id DESC LIMIT 80",
    ) else {
        return updated;
    };

    #[allow(clippy::type_complexity)]
    let org_rows: Vec<(
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        u32,
        u32,
        String,
    )> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect();
    drop(stmt);

    let clock = Instant::now();
    let mut detail_cache: BTreeMap<(String, String, u32, u32), Vec<_>> = BTreeMap::new();
    let mut http_checked = 0usize;

    for (
        aid,
        src_path,
        cur_target,
        cur_slot,
        s_fid,
        s_pc,
        s_mt,
        s_tmdb,
        s_season,
        s_ep,
        s_slot,
    ) in org_rows
    {
        if s_tmdb.trim().is_empty() || !matches!(s_mt.as_str(), "movie" | "tv") {
            continue;
        }
        let is_placeholder = cur_target.trim().is_empty()
            || cur_target.contains("主程序二级分类归档")
            || cur_target.starts_with("Mediary 媒体库");
        let dst_low = cur_target.to_ascii_lowercase();
        let is_slot_mismatch = !s_pc.trim().is_empty()
            && ((s_slot.starts_with("1080p") && dst_low.contains("2160p"))
                || (s_slot.starts_with("4K") && dst_low.contains("1080p")));
        let expects_tmdbid = if s_mt == "tv" {
            cfg.host_sync.tv_rename_format.contains("[tmdbid-")
        } else {
            cfg.host_sync.movie_rename_format.contains("[tmdbid-")
        };
        let is_missing_tmdbid = expects_tmdbid && !cur_target.contains("[tmdbid-");
        if !is_placeholder && !is_slot_mismatch && !is_missing_tmdbid {
            continue;
        }
        if http_checked >= max_http_rows || clock.elapsed() >= Duration::from_millis(2500) {
            break;
        }

        let cache_key = (
            s_mt.clone(),
            s_tmdb.clone(),
            if s_mt == "tv" { s_season.max(1) } else { 0 },
            if s_mt == "tv" { s_ep } else { 0 },
        );
        if !detail_cache.contains_key(&cache_key) {
            http_checked += 1;
            let ep_pair = (s_mt == "tv" && s_ep > 0).then_some((s_season.max(1), s_ep));
            let mut details = Vec::new();
            if let Ok(page) = host.library_items_by_tmdb(&s_tmdb, &s_mt, ep_pair) {
                for entry in &page.items {
                    if let Ok(det) = host.library_item_detail(&entry.id) {
                        details.push(det);
                    }
                }
            }
            detail_cache.insert(cache_key.clone(), details);
        }

        let Some(details) = detail_cache.get(&cache_key) else {
            continue;
        };
        let mut resolved = String::new();
        for det in details {
            let srcs = det.effective_sources();
            let hit = srcs
                .iter()
                .find(|s| !s_fid.is_empty() && s.file_id == s_fid)
                .or_else(|| {
                    srcs.iter().find(|s| {
                        !s_pc.is_empty() && s.effective_pick_code().eq_ignore_ascii_case(&s_pc)
                    })
                })
                .or_else(|| {
                    (srcs.len() == 1 && (is_placeholder || (s_fid.is_empty() && s_pc.is_empty())))
                        .then(|| &srcs[0])
                });
            if let Some(src) = hit {
                for p in [&src.strm_path, &src.cloud_path, &src.file_path, &det.path] {
                    if !p.trim().is_empty() {
                        resolved = p.trim().to_string();
                        break;
                    }
                }
                if !resolved.is_empty() {
                    break;
                }
            }
        }

        if !resolved.is_empty() && resolved != cur_target {
            let slot_tail = cur_slot
                .split_once(" · ")
                .map(|(_, r)| r.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| s_slot.trim().to_string());
            let new_slot = if let Some(cat) = category::locate(
                &cfg.host_sync.categories,
                &s_mt,
                &[resolved.as_str(), src_path.as_str()],
            ) {
                if !slot_tail.is_empty() {
                    format!("{cat} · {slot_tail}")
                } else {
                    cat
                }
            } else {
                cur_slot.clone()
            };
            if db
                .execute(
                    "UPDATE activity_logs SET target_path = ?, slot = ? WHERE id = ?",
                    params![resolved, new_slot, aid],
                )
                .is_ok()
            {
                updated += 1;
            }
        }
    }

    updated
}

/// Binds a manual TMDB identity to a `review` staging item (and optionally saves folder/title rule), then re-evaluates!
pub fn bind_staging_tmdb(
    db: &mut Connection,
    host: &HostClient,
    id: i64,
    media_type: &str,
    tmdb_id: &str,
    title: &str,
    season: Option<u32>,
    episode: Option<u32>,
    remember: bool,
) -> Result<Value> {
    ensure!(
        matches!(media_type, "movie" | "tv"),
        "媒体类型必须为 movie 或 tv"
    );
    ensure!(
        !tmdb_id.trim().is_empty() && tmdb_id.trim().bytes().all(|b| b.is_ascii_digit()),
        "TMDB ID 必须为数字"
    );
    let (pipeline_id, file_id, parent_cid, rel_path, name, size, sha1, pick_code, sidecars_raw): (
        i64,
        String,
        String,
        String,
        String,
        i64,
        String,
        String,
        String,
    ) = db
        .query_row(
            "SELECT pipeline_id, file_id, parent_cid, rel_path, name, size, sha1, pick_code, sidecar_ids_json
             FROM staging_items WHERE id = ?",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .context("找不到该中转条目")?;

    let parsed = crate::identity::parse_media_name(&name, &rel_path);
    let bind_key = if remember && !rel_path.trim().is_empty() {
        norm_title_key(&rel_path)
    } else {
        norm_title_key(if !parsed.title.is_empty() {
            &parsed.title
        } else {
            &name
        })
    };

    let cfg = db::edit_config(db, false, |c| {
        if !bind_key.is_empty() {
            c.manual_bindings.insert(
                bind_key.clone(),
                ManualBinding {
                    pattern_key: bind_key,
                    title: title.trim().to_string(),
                    media_type: media_type.to_string(),
                    tmdb_id: tmdb_id.trim().to_string(),
                    season: if remember { season } else { season },
                    episode: if remember && !rel_path.trim().is_empty() {
                        None
                    } else {
                        episode
                    },
                    updated_at: now_ts(),
                },
            );
        }
        Ok(false)
    })?;

    let pipeline = cfg
        .pipelines
        .iter()
        .find(|p| p.id == pipeline_id)
        .cloned()
        .context("所属 115 管道不存在")?;
    let mut rec_cfg = cfg.clone();
    // Ensure single-file episode override applies to this exact re-evaluation
    let file_key = norm_title_key(if !parsed.title.is_empty() {
        &parsed.title
    } else {
        &name
    });
    if !file_key.is_empty() {
        rec_cfg.manual_bindings.insert(
            file_key.clone(),
            ManualBinding {
                pattern_key: file_key,
                title: title.trim().to_string(),
                media_type: media_type.to_string(),
                tmdb_id: tmdb_id.trim().to_string(),
                season,
                episode,
                updated_at: now_ts(),
            },
        );
    }

    let recognition = RecognitionEngine::new(&rec_cfg);
    let cloud_file = Cloud115File {
        file_id,
        parent_id: parent_cid,
        name,
        size: size.max(0) as u64,
        sha1,
        pick_code,
        is_dir: false,
        updated_at: String::new(),
    };
    let eval = evaluate_staging_file(
        host,
        &rec_cfg,
        &pipeline,
        &recognition,
        &cloud_file,
        &rel_path,
        true,
    );
    let sidecars: Vec<String> = serde_json::from_str(&sidecars_raw).unwrap_or_default();
    upsert_staging_item(db, pipeline_id, &cloud_file, &rel_path, &sidecars, &eval)?;
    Ok(json!({
        "id": id,
        "verdict": eval.verdict,
        "reason": eval.reason,
        "slot": eval.slot,
        "cand_score": eval.cand_score,
        "lib_score": eval.lib_score,
    }))
}

pub fn list_staging_items(
    db: &Connection,
    pipeline_id: Option<i64>,
    state_filter: &str,
    verdict_filter: &str,
    limit: usize,
) -> Result<Vec<Value>> {
    let mut sql = String::from(
        "SELECT id, pipeline_id, file_id, parent_cid, rel_path, name, size, sha1,
                media_type, tmdb_id, title, year, season, episode, category, slot,
                cand_score, lib_score, verdict, state, reason, detail_json, sidecar_ids_json, updated_at
         FROM staging_items WHERE 1=1",
    );
    if let Some(pid) = pipeline_id.filter(|&p| p > 0) {
        sql.push_str(&format!(" AND pipeline_id = {pid}"));
    }
    if !state_filter.is_empty() && state_filter != "all" {
        let safe = match state_filter {
            "pending" => "pending",
            "organized" => "organized",
            "recycled_115" => "recycled_115",
            "dismissed" => "dismissed",
            "failed" => "failed",
            _ => "pending",
        };
        sql.push_str(&format!(" AND state = '{safe}'"));
    }
    if !verdict_filter.is_empty() && verdict_filter != "all" {
        let safe = match verdict_filter {
            "admit_new" => "admit_new",
            "admit_upgrade" => "admit_upgrade",
            "reject" => "reject",
            "review" => "review",
            "probing" => "probing",
            "already_in_lib" => "already_in_lib",
            _ => "",
        };
        if !safe.is_empty() {
            sql.push_str(&format!(" AND verdict = '{safe}'"));
        }
    }
    sql.push_str(&format!(
        " ORDER BY updated_at DESC, id DESC LIMIT {}",
        limit.clamp(1, 300)
    ));

    let mut stmt = db.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        let ts: u64 = r.get(23)?;
        let detail: Value =
            serde_json::from_str(&r.get::<_, String>(21)?).unwrap_or_else(|_| json!({}));
        let sidecars: Vec<String> =
            serde_json::from_str(&r.get::<_, String>(22)?).unwrap_or_default();
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "pipeline_id": r.get::<_, i64>(1)?,
            "file_id": r.get::<_, String>(2)?,
            "parent_cid": r.get::<_, String>(3)?,
            "rel_path": r.get::<_, String>(4)?,
            "name": r.get::<_, String>(5)?,
            "size": r.get::<_, i64>(6)?,
            "sha1": r.get::<_, String>(7)?,
            "media_type": r.get::<_, String>(8)?,
            "tmdb_id": r.get::<_, String>(9)?,
            "title": r.get::<_, String>(10)?,
            "year": r.get::<_, u32>(11)?,
            "season": r.get::<_, u32>(12)?,
            "episode": r.get::<_, u32>(13)?,
            "category": r.get::<_, String>(14)?,
            "slot": r.get::<_, String>(15)?,
            "cand_score": r.get::<_, f64>(16)?,
            "lib_score": r.get::<_, Option<f64>>(17)?,
            "verdict": r.get::<_, String>(18)?,
            "state": r.get::<_, String>(19)?,
            "reason": r.get::<_, String>(20)?,
            "detail": detail,
            "sidecar_count": sidecars.len(),
            "updated_at": format_ts_cn(ts),
        }))
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

/// Reconciles `wash_ledger` entries (`downloading`, `downloaded`, `awaiting_ingest`):
/// - Advances PT downloads -> triggers `/organize` -> verifies new source in Mediary library;
/// - Verifies 115 `admit_upgrade` arrivals in Mediary library;
/// - Automatically cleans superseded old sources when `settings.auto_delete_old && !settings.dry_run`,
///   or marks `ready_to_clean` for 1-click confirmation!
pub fn reconcile_ledger(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
) -> Result<Value> {
    let mut stmt = db.prepare(
        "SELECT id, source_kind, media_type, tmdb_id, season, episode, item_id, title,
                category, slot, rule_id, expected_file_id, expected_sha1, download_id,
                old_source_ids_json, state
         FROM wash_ledger
         WHERE state IN ('downloading', 'downloaded', 'awaiting_ingest')
         ORDER BY updated_at ASC LIMIT 40",
    )?;
    let entries: Vec<(
        i64,
        String,
        String,
        String,
        u32,
        u32,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
                r.get(13)?,
                r.get(14)?,
                r.get(15)?,
            ))
        })?
        .filter_map(Result::ok)
        .collect();
    drop(stmt);

    let mut organized_pt = 0usize;
    let mut verified_ready = 0usize;
    let mut auto_cleaned = 0usize;

    for (
        lid,
        source_kind,
        media_type,
        tmdb_id,
        season,
        episode,
        item_id,
        title,
        category,
        slot,
        rule_id,
        expected_file_id,
        expected_sha1,
        download_id,
        old_ids_raw,
        mut state,
    ) in entries
    {
        // Step A: Poll PT download status if applicable
        if source_kind == "pt" && !download_id.is_empty() && matches!(state.as_str(), "downloading" | "downloaded") {
            if let Ok(dl) = host.get_media_download(&download_id) {
                let dl_status = dl["status"].as_str().unwrap_or_default();
                match dl_status {
                    "downloaded" => {
                        if !settings.dry_run {
                            if host
                                .organize_media_download(&download_id, &cfg.pt_config.conflict_strategy)
                                .is_ok()
                            {
                                organized_pt += 1;
                                state = "awaiting_ingest".to_string();
                                db.execute(
                                    "UPDATE wash_ledger SET state = 'awaiting_ingest', message = 'PT 下载完成，已触发宿主按统一模板归档入库', updated_at = ? WHERE id = ?",
                                    params![now_ts(), lid],
                                )?;
                            } else {
                                db.execute(
                                    "UPDATE wash_ledger SET state = 'downloaded', message = 'PT 下载完成，可点击「确认整理」入库', updated_at = ? WHERE id = ?",
                                    params![now_ts(), lid],
                                )?;
                            }
                        } else {
                            db.execute(
                                "UPDATE wash_ledger SET state = 'downloaded', message = 'PT 下载已完成（当前为预览模式，等待手动整理）', updated_at = ? WHERE id = ?",
                                params![now_ts(), lid],
                            )?;
                        }
                    }
                    "completed" | "organized" => {
                        state = "awaiting_ingest".to_string();
                        db.execute(
                            "UPDATE wash_ledger SET state = 'awaiting_ingest', message = '宿主已执行整理，正在核验媒体库新源', updated_at = ? WHERE id = ?",
                            params![now_ts(), lid],
                        )?;
                    }
                    "failed" | "error" => {
                        db.execute(
                            "UPDATE wash_ledger SET state = 'review', message = '宿主下载或整理报告失败，请核对下载记录', updated_at = ? WHERE id = ?",
                            params![now_ts(), lid],
                        )?;
                        continue;
                    }
                    _ => continue,
                }
            }
        }

        if state != "awaiting_ingest" {
            continue;
        }

        // Step B: Verify new source has arrived in Mediary library!
        let old_ids: BTreeSet<String> = serde_json::from_str::<Vec<String>>(&old_ids_raw)
            .unwrap_or_default()
            .into_iter()
            .collect();

        let live_item_opt = if !item_id.is_empty() {
            host.library_item_detail(&item_id).ok()
        } else {
            let ep_pair = (media_type == "tv" && episode > 0).then_some((season, episode));
            host.library_items_by_tmdb(&tmdb_id, &media_type, ep_pair)
                .ok()
                .and_then(|p| p.items.first().cloned())
                .and_then(|first| host.library_item_detail(&first.id).ok())
        };
        let Some(live_item) = live_item_opt else {
            continue;
        };

        let arrived = live_item.effective_sources().iter().any(|src| {
            let paths = [
                src.strm_path.as_str(),
                src.cloud_path.as_str(),
                src.file_path.as_str(),
                live_item.path.as_str(),
            ];
            let src_cat =
                category::locate(&cfg.host_sync.categories, &media_type, &paths).unwrap_or_default();
            if !category.is_empty() && !src_cat.is_empty() && src_cat != category {
                return false;
            }
            (!expected_file_id.is_empty() && src.file_id == expected_file_id)
                || (!expected_sha1.is_empty()
                    && !src.sha1.is_empty()
                    && src.sha1.eq_ignore_ascii_case(&expected_sha1))
                || (!src.id.is_empty() && !old_ids.contains(&src.id))
        });

        if !arrived {
            continue;
        }

        verified_ready += 1;
        if settings.auto_delete_old && !settings.dry_run {
            match verify_and_clean_old_sources(
                host,
                cfg,
                &live_item.id,
                &media_type,
                &category,
                &slot,
                &rule_id,
                &old_ids,
            ) {
                Ok((deleted_cnt, old_paths_s, new_winner_path)) if deleted_cnt > 0 => {
                    auto_cleaned += deleted_cnt;
                    let msg = format!("已确认新源入库并自动清理 {deleted_cnt} 个同槽低分旧源");
                    db.execute(
                        "UPDATE wash_ledger SET item_id = ?, state = 'cleaned', message = ?, updated_at = ? WHERE id = ?",
                        params![live_item.id, msg, now_ts(), lid],
                    )?;
                    db::log_media_history(
                        db,
                        "删旧闭环",
                        &title,
                        "",
                        &old_paths_s,
                        &new_winner_path,
                        &format!("{} · {}", if category.is_empty() { "媒体库" } else { &category }, slot),
                        "auto_clean_old",
                        "done",
                        &msg,
                    );
                    if settings.notify {
                        let icon = if media_type == "tv" { "📺" } else { "🎬" };
                        let cat_lbl = if category.is_empty() { "媒体库" } else { &category };
                        let img = host.resolve_tmdb_card_image(&tmdb_id, &media_type);
                        host.notify(
                            &format!("{icon} {title} ✨洗版删旧完成"),
                            &format!(
                                "🗂 分类: {cat_lbl} · {slot}\n🧹 清理: 自动移除 {deleted_cnt} 个低分旧源\n🗓️ 时间: {}",
                                format_ts_cn(now_ts())
                            ),
                            img.as_deref(),
                        );
                    }
                }
                Ok(_) => {
                    db.execute(
                        "UPDATE wash_ledger SET item_id = ?, state = 'cleaned', message = '新源已确认入库，原旧源已不存在或已被替换', updated_at = ? WHERE id = ?",
                        params![live_item.id, now_ts(), lid],
                    )?;
                }
                Err(err) => {
                    db.execute(
                        "UPDATE wash_ledger SET item_id = ?, state = 'review', message = ?, updated_at = ? WHERE id = ?",
                        params![live_item.id, format!("新源已入库，但删旧复核未通过：{err:#}"), now_ts(), lid],
                    )?;
                }
            }
        } else {
            db.execute(
                "UPDATE wash_ledger SET item_id = ?, state = 'ready_to_clean', message = '新源已确认入库生成 STRM/片源，可一键安全清理旧低分源', updated_at = ? WHERE id = ?",
                params![live_item.id, now_ts(), lid],
            )?;
        }
    }

    let _ = backfill_recent_media_history(db, Some(host), cfg, 20);

    Ok(json!({
        "organized_pt": organized_pt,
        "verified_ready": verified_ready,
        "auto_cleaned": auto_cleaned,
    }))
}

pub fn execute_ledger_action(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    ledger_id: i64,
    operation: &str,
) -> Result<Value> {
    if operation == "dismiss" {
        db.execute(
            "UPDATE wash_ledger SET state = 'dismissed', updated_at = ? WHERE id = ?",
            params![now_ts(), ledger_id],
        )?;
        return Ok(json!({ "ok": true, "state": "dismissed" }));
    }

    ensure!(
        !settings.dry_run,
        "当前处于「仅预览模式（干跑）」，请先关闭仅预览模式后再执行"
    );

    let (media_type, title, item_id, category, slot, rule_id, download_id, old_ids_raw, state): (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = db
        .query_row(
            "SELECT media_type, title, item_id, category, slot, rule_id, download_id, old_source_ids_json, state
             FROM wash_ledger WHERE id = ?",
            [ledger_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .context("找不到该洗版账本记录")?;

    match operation {
        "organize" => {
            ensure!(!download_id.is_empty(), "缺少下载任务 ID");
            host.organize_media_download(&download_id, &cfg.pt_config.conflict_strategy)?;
            db.execute(
                "UPDATE wash_ledger SET state = 'awaiting_ingest', message = '已提交 Mediary 整理，等待新源入库后核验', updated_at = ? WHERE id = ?",
                params![now_ts(), ledger_id],
            )?;
            Ok(json!({ "ok": true, "state": "awaiting_ingest" }))
        }
        "clean_old" => {
            ensure!(
                matches!(state.as_str(), "ready_to_clean" | "awaiting_ingest" | "review"),
                "当前状态不支持清理旧源"
            );
            ensure!(!item_id.is_empty(), "尚未关联到媒体库条目，请先点「刷新核验」");
            let old_ids: BTreeSet<String> = serde_json::from_str::<Vec<String>>(&old_ids_raw)
                .unwrap_or_default()
                .into_iter()
                .collect();
            let (deleted, old_paths_s, new_winner_path) = verify_and_clean_old_sources(
                host, cfg, &item_id, &media_type, &category, &slot, &rule_id, &old_ids,
            )?;
            let msg = format!("已安全清理 {deleted} 个旧低分源并局部刷新");
            db.execute(
                "UPDATE wash_ledger SET state = 'cleaned', message = ?, updated_at = ? WHERE id = ?",
                params![msg, now_ts(), ledger_id],
            )?;
            db::log_media_history(
                db,
                "删旧闭环",
                &title,
                "",
                &old_paths_s,
                &new_winner_path,
                &format!("{} · {}", if category.is_empty() { "媒体库" } else { &category }, slot),
                "clean_old",
                "done",
                &msg,
            );
            Ok(json!({ "ok": true, "deleted": deleted, "message": msg }))
        }
        _ => bail!("不支持的账本动作: {operation}"),
    }
}

fn verify_and_clean_old_sources(
    host: &HostClient,
    cfg: &AppConfig,
    item_id: &str,
    media_type: &str,
    expected_category: &str,
    expected_slot: &str,
    rule_id: &str,
    old_ids: &BTreeSet<String>,
) -> Result<(usize, String, String)> {
    let detail = host.library_item_detail(item_id)?;
    let sources = detail.effective_sources();
    if sources.len() <= 1 {
        return Ok((0, String::new(), String::new()));
    }
    let eff = cfg.strategy.effective_for(
        media_type,
        expected_category,
        &cfg.host_sync.categories,
        &cfg.rules,
    );
    let active_rule_id = if !rule_id.is_empty() && cfg.rules.iter().any(|r| r.id == rule_id) {
        rule_id
    } else {
        &eff.rule_id
    };
    let rule = rules::find_rule(cfg, active_rule_id)?;
    let scorer = Scorer::new(rule)?;

    // Gather all live sources in the target category & slot
    let mut slot_profiles: Vec<(String, MediaProfile)> = Vec::new();
    let mut shared_ref = ReferenceBaseline::default();
    for src in sources {
        if src.id.is_empty() {
            continue;
        }
        let paths = [
            src.strm_path.as_str(),
            src.cloud_path.as_str(),
            src.file_path.as_str(),
            detail.path.as_str(),
        ];
        let src_cat =
            category::locate(&cfg.host_sync.categories, media_type, &paths).unwrap_or_default();
        if !expected_category.is_empty() && !src_cat.is_empty() && src_cat != expected_category {
            continue;
        }
        let refs: Vec<&MediaStream> = detail
            .streams
            .iter()
            .filter(|st| st.media_source_id == src.id)
            .collect();
        let prof = MediaProfile::extract(src, &refs);
        let init = scorer.score(
            &prof,
            &ReferenceBaseline::default(),
            expected_category,
            eff.iso_protected,
        );
        if expected_slot.is_empty() || init.slot == expected_slot {
            shared_ref.observe(&prof);
            slot_profiles.push((src.id.clone(), prof));
        }
    }
    ensure!(
        slot_profiles.len() > eff.keep_per_slot,
        "同槽位当前仅有 {} 个源，必须至少保留 {} 个胜出版本，取消删旧",
        slot_profiles.len(),
        eff.keep_per_slot
    );

    let max_sz = slot_profiles
        .iter()
        .map(|(_, p)| p.size)
        .fold(0.0f64, f64::max);
    let mut scored: Vec<(String, f64)> = slot_profiles
        .into_iter()
        .map(|(sid, p)| {
            let r = if rule.kind == rules::SYMEDIA {
                ReferenceBaseline {
                    size: if (p.size - max_sz).abs() < 1.0 { 0.0 } else { max_sz },
                    ..shared_ref.clone()
                }
            } else {
                shared_ref.clone()
            };
            (sid, scorer.score(&p, &r, expected_category, eff.iso_protected).total)
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));

    let retained_ids: BTreeSet<&str> = scored
        .iter()
        .take(eff.keep_per_slot)
        .map(|(sid, _)| sid.as_str())
        .collect();
    // Must confirm at least one NEW source (not in `old_ids`) is among retained winners!
    ensure!(
        retained_ids.iter().any(|&sid| !old_ids.contains(sid)),
        "尚未检测到高于旧源的新增胜出片源，已保护保留原片源"
    );

    let retained_srcs: Vec<&LibrarySource> = sources
        .iter()
        .filter(|s| retained_ids.contains(s.id.as_str()))
        .collect();
    let to_delete: Vec<String> = scored
        .iter()
        .skip(eff.keep_per_slot)
        .filter(|(sid, _)| old_ids.contains(sid.as_str()))
        .filter(|(sid, _)| {
            let Some(cand_src) = sources.iter().find(|s| &s.id == sid) else {
                return false;
            };
            !retained_srcs.iter().any(|w| w.shares_cloud_identity(cand_src))
        })
        .map(|(sid, _)| sid.clone())
        .collect();
    if to_delete.is_empty() {
        return Ok((0, String::new(), String::new()));
    }
    ensure!(
        to_delete.len() < sources.len(),
        "安全保护：禁止删除条目的全部媒体源"
    );
    let src_disp = |s: &LibrarySource| -> String {
        [s.strm_path.as_str(), s.cloud_path.as_str(), s.file_path.as_str(), s.name.as_str()]
            .into_iter()
            .find(|p| !p.trim().is_empty())
            .unwrap_or_default()
            .to_string()
    };
    let deleted_paths: Vec<String> = sources
        .iter()
        .filter(|s| to_delete.contains(&s.id))
        .map(src_disp)
        .filter(|s| !s.is_empty())
        .collect();
    let winner_path = scored
        .first()
        .and_then(|(wid, _)| sources.iter().find(|s| &s.id == wid))
        .map(src_disp)
        .unwrap_or_default();
    host.delete_library_sources(item_id, &to_delete)?;
    let _ = host.scan_library_item(item_id);
    Ok((
        to_delete.len(),
        deleted_paths.join(" ; "),
        if winner_path.is_empty() {
            String::new()
        } else {
            format!("保留胜出源：{winner_path}")
        },
    ))
}

pub fn list_ledger(db: &Connection, state_filter: &str, limit: usize) -> Result<Vec<Value>> {
    let mut sql = String::from(
        "SELECT id, source_kind, media_type, tmdb_id, season, episode, item_id, title,
                category, slot, cand_score, old_score, state, message, download_id, updated_at
         FROM wash_ledger WHERE 1=1",
    );
    if !state_filter.is_empty() && state_filter != "all" {
        if state_filter == "active" {
            sql.push_str(" AND state IN ('downloading','downloaded','awaiting_ingest','ready_to_clean','review')");
        } else {
            let safe = match state_filter {
                "ready_to_clean" => "ready_to_clean",
                "awaiting_ingest" => "awaiting_ingest",
                "cleaned" => "cleaned",
                "dismissed" => "dismissed",
                _ => "",
            };
            if !safe.is_empty() {
                sql.push_str(&format!(" AND state = '{safe}'"));
            }
        }
    }
    sql.push_str(&format!(
        " ORDER BY updated_at DESC, id DESC LIMIT {}",
        limit.clamp(1, 200)
    ));
    let mut stmt = db.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        let ts: u64 = r.get(15)?;
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "source_kind": r.get::<_, String>(1)?,
            "media_type": r.get::<_, String>(2)?,
            "tmdb_id": r.get::<_, String>(3)?,
            "season": r.get::<_, u32>(4)?,
            "episode": r.get::<_, u32>(5)?,
            "item_id": r.get::<_, String>(6)?,
            "title": r.get::<_, String>(7)?,
            "category": r.get::<_, String>(8)?,
            "slot": r.get::<_, String>(9)?,
            "cand_score": r.get::<_, f64>(10)?,
            "old_score": r.get::<_, f64>(11)?,
            "state": r.get::<_, String>(12)?,
            "message": r.get::<_, String>(13)?,
            "download_id": r.get::<_, String>(14)?,
            "updated_at": format_ts_cn(ts),
        }))
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn extract_target_rel_segments(path: &str, target_folder_name: &str) -> Vec<String> {
    let trimmed = path.trim().trim_start_matches("115://");
    let Some((dir_part, _file_name)) = trimmed.rsplit_once('/') else {
        return Vec::new();
    };
    let parts: Vec<&str> = dir_part
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return Vec::new();
    }
    let anchor = target_folder_name.trim().trim_matches('/');
    if !anchor.is_empty() {
        if let Some(pos) = parts.iter().position(|&seg| seg == anchor) {
            return parts[pos + 1..].iter().map(|s| (*s).to_string()).collect();
        }
    }
    if let Some(pos) = parts.iter().position(|&seg| seg == "媒体库") {
        return parts[pos + 1..].iter().map(|s| (*s).to_string()).collect();
    }
    Vec::new()
}

#[allow(clippy::too_many_arguments)]
fn locate_archived_in_target_library(
    host: &HostClient,
    cfg: &AppConfig,
    resolved_target_cid: Option<&str>,
    file_id: &str,
    pick_code: &str,
    media_type: &str,
    tmdb_id: &str,
    season: u32,
    episode: u32,
    recorded_target_path: &str,
) -> Option<(String, Cloud115File, Vec<Cloud115File>)> {
    let mut root_cids: Vec<&str> = Vec::new();
    if let Some(rc) = resolved_target_cid.map(str::trim) {
        if !rc.is_empty() && rc != "0" {
            root_cids.push(rc);
        }
    }
    for p in &cfg.pipelines {
        let tc = p.target_cid.trim();
        if !tc.is_empty() && tc != "0" && !root_cids.contains(&tc) {
            root_cids.push(tc);
        }
    }
    for c in [
        cfg.host_sync.transfer_target_cid.trim(),
        cfg.host_sync.open115_target_cid.trim(),
    ] {
        if !c.is_empty() && c != "0" && !root_cids.contains(&c) {
            root_cids.push(c);
        }
    }
    if root_cids.is_empty() {
        return None;
    }

    let mut tried_cids: BTreeSet<String> = BTreeSet::new();
    let mut discovered_pick_code = pick_code.trim().to_string();

    // Fast path 1: resolve `recorded_target_path` directly BEFORE querying library API!
    if !recorded_target_path.trim().is_empty() {
        let segs = extract_target_rel_segments(
            recorded_target_path.trim(),
            &cfg.host_sync.transfer_target_folder,
        );
        if !segs.is_empty() {
            let seg_refs: Vec<&str> = segs.iter().map(String::as_str).collect();
            for &root_cid in &root_cids {
                if let Ok(Some(leaf_cid)) = host.open115_resolve_subpath_cid(root_cid, &seg_refs) {
                    if tried_cids.insert(leaf_cid.clone()) {
                        if let Ok(Some((hit, files))) = host.open115_find_file_in_cid_ext(
                            &leaf_cid,
                            file_id,
                            &discovered_pick_code,
                        ) {
                            return Some((leaf_cid, hit, files));
                        }
                    }
                }
            }
        }
    }

    // Fallback path 2: query Mediary library item sources for moved/updated paths or pickcode
    let mut candidate_paths: Vec<String> = Vec::new();
    if !tmdb_id.trim().is_empty() && matches!(media_type, "movie" | "tv") {
        let ep_pair = (media_type == "tv" && episode > 0).then_some((season.max(1), episode));
        if let Ok(page) = host.library_items_by_tmdb(tmdb_id, media_type, ep_pair) {
            for entry in &page.items {
                if let Ok(detail) = host.library_item_detail(&entry.id) {
                    for src in detail.effective_sources() {
                        let spc = src.effective_pick_code();
                        let matches_src = (!file_id.is_empty() && src.file_id == file_id)
                            || (!discovered_pick_code.is_empty()
                                && spc.eq_ignore_ascii_case(&discovered_pick_code))
                            || detail.effective_sources().len() == 1;
                        if matches_src {
                            if discovered_pick_code.is_empty() && !spc.is_empty() {
                                discovered_pick_code = spc;
                            }
                            for p in [&src.strm_path, &src.cloud_path, &src.file_path, &detail.path] {
                                let pt = p.trim();
                                if !pt.is_empty() && !candidate_paths.iter().any(|x| x == pt) {
                                    candidate_paths.push(pt.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    for cpath in &candidate_paths {
        let segs = extract_target_rel_segments(cpath, &cfg.host_sync.transfer_target_folder);
        if segs.is_empty() {
            continue;
        }
        let seg_refs: Vec<&str> = segs.iter().map(String::as_str).collect();
        for &root_cid in &root_cids {
            if let Ok(Some(leaf_cid)) = host.open115_resolve_subpath_cid(root_cid, &seg_refs) {
                if tried_cids.insert(leaf_cid.clone()) {
                    if let Ok(Some((hit, files))) = host.open115_find_file_in_cid_ext(
                        &leaf_cid,
                        file_id,
                        &discovered_pick_code,
                    ) {
                        return Some((leaf_cid, hit, files));
                    }
                }
            }
        }
    }

    // Fallback: direct target root bucket (e.g. flat library bucket without nested category subfolders)
    for &root_cid in &root_cids {
        if tried_cids.insert(root_cid.to_string()) {
            if let Ok(Some((hit, files))) =
                host.open115_find_file_in_cid_ext(root_cid, file_id, &discovered_pick_code)
            {
                return Some((root_cid.to_string(), hit, files));
            }
        }
    }
    None
}

/// Retries selected Media History or Task History rows intelligently (with 9s UI slice soft budget,
/// shared 115 directory cache, duplicate warning dedup, and instant stale-warning healing/cleanup):
pub fn retry_history_items(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    activity_ids: &[i64],
) -> Result<Value> {
    ensure!(!activity_ids.is_empty(), "请选择要重试的历史记录");
    ensure!(
        !settings.dry_run,
        "当前处于「仅预览模式（干跑）」，请先关闭仅预览模式后再执行重试"
    );

    let retry_clock = Instant::now();
    let recognition = RecognitionEngine::new(cfg);
    let mut retried = 0usize;
    let mut cleared_stale = 0usize;
    let mut healed_done = 0usize;
    let mut notices: Vec<String> = Vec::new();
    let mut remaining_ids: Vec<i64> = Vec::new();
    let mut handled_warning_subs: BTreeSet<String> = BTreeSet::new();
    let mut staging_dir_cache: BTreeMap<String, (BTreeMap<String, Cloud115File>, bool)> =
        BTreeMap::new();

    let lookup_in_staging_cid =
        |cache: &mut BTreeMap<String, (BTreeMap<String, Cloud115File>, bool)>,
         cid: &str,
         file_id: &str,
         file_name: &str|
         -> Option<Cloud115File> {
            let clean_cid = cid.trim();
            if clean_cid.is_empty() || clean_cid == "0" {
                return None;
            }
            if !cache.contains_key(clean_cid) {
                if let Ok(v) = host.open115_list_dir_files_map_ext(clean_cid, 3) {
                    cache.insert(clean_cid.to_string(), v);
                } else {
                    return None;
                }
            }
            let (fmap, is_complete) = cache.get(clean_cid)?;
            if !file_id.trim().is_empty() {
                if let Some(mut hit) = fmap.get(file_id.trim()).cloned() {
                    hit.parent_id = clean_cid.to_string();
                    return Some(hit);
                }
                if !*is_complete {
                    if let Ok(Some(mut hit)) = host.open115_find_file_in_cid(clean_cid, file_id) {
                        hit.parent_id = clean_cid.to_string();
                        return Some(hit);
                    }
                }
            }
            if !file_name.trim().is_empty() {
                if let Some(mut hit) = fmap.values().find(|f| f.name == file_name.trim()).cloned() {
                    hit.parent_id = clean_cid.to_string();
                    return Some(hit);
                }
            }
            None
        };

    for (idx, &aid) in activity_ids.iter().enumerate() {
        if idx > 0 && retry_clock.elapsed() >= Duration::from_secs(9) {
            remaining_ids.extend_from_slice(&activity_ids[idx..]);
            break;
        }
        let row = db
            .query_row(
                "SELECT record_type, kind, title, sub_title, target_path, slot, action, status, detail
                 FROM activity_logs WHERE id = ?",
                [aid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            record_type,
            kind,
            title,
            sub_title,
            cur_target_path,
            slot,
            action,
            row_status,
            cur_detail,
        )) = row
        else {
            continue;
        };

        if record_type == "task" {
            match action.as_str() {
                "scan_staging" => {
                    if let Some(p) = cfg
                        .pipelines
                        .iter()
                        .find(|p| p.source_cid == slot || p.name == title)
                        .or_else(|| cfg.pipelines.first())
                    {
                        let _ = db.execute(
                            "DELETE FROM cursors WHERE key = ?",
                            [format!("staging_walk:{}", p.id)],
                        );
                        let sr = scan_pipeline_ui(db, host, cfg, settings, p.id, 12)?;
                        retried += 1;
                        if let Some(n) = sr["notice"].as_str() {
                            notices.push(n.to_string());
                        }
                    }
                }
                "sync_rules" => {
                    let r = crate::rules::sync_subscriptions(db)?;
                    retried += 1;
                    notices.push(format!(
                        "远程订阅已重新同步（成功 {}，失败 {}）",
                        r["ok_count"].as_u64().unwrap_or(0),
                        r["fail_count"].as_u64().unwrap_or(0)
                    ));
                }
                "lib_scan" => {
                    let sc = if slot == "tv" { "tv" } else { "movie" };
                    let j = crate::library::scan_step(db, host, cfg, sc, 20)?;
                    retried += 1;
                    notices.push(format!(
                        "{}续扫完成（已扫 {}/{}）",
                        if sc == "tv" { "剧集库" } else { "电影库" },
                        j.scanned,
                        j.total
                    ));
                }
                _ => {}
            }
            continue;
        }

        // Media History Retry
        if kind.starts_with("115") {
            let is_warning = row_status == "warning";
            if is_warning
                && !sub_title.trim().is_empty()
                && !handled_warning_subs.insert(sub_title.trim().to_string())
            {
                let _ = db.execute(
                    "DELETE FROM activity_logs WHERE id = ? AND status = 'warning'",
                    [aid],
                );
                retried += 1;
                continue;
            }

            let has_exact_sub = !sub_title.trim().is_empty();
            let lookup_name = if has_exact_sub {
                sub_title.trim()
            } else {
                title.trim()
            };
            #[allow(clippy::type_complexity)]
            let s_row: Option<(
                i64,
                i64,
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                u32,
                u32,
                String,
                String,
                String,
            )> = if has_exact_sub {
                db.query_row(
                    "SELECT id, pipeline_id, file_id, parent_cid, rel_path, sidecar_ids_json,
                            pick_code, media_type, tmdb_id, season, episode, slot, reason, state
                       FROM staging_items
                      WHERE name = ?1
                      ORDER BY id DESC LIMIT 1",
                    params![lookup_name],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                            r.get(9)?,
                            r.get(10)?,
                            r.get(11)?,
                            r.get(12)?,
                            r.get(13)?,
                        ))
                    },
                )
                .optional()?
            } else {
                db.query_row(
                    "SELECT id, pipeline_id, file_id, parent_cid, rel_path, sidecar_ids_json,
                            pick_code, media_type, tmdb_id, season, episode, slot, reason, state
                       FROM staging_items
                      WHERE name = ?1
                         OR (title || CASE WHEN media_type='tv' AND episode>0 THEN printf(' S%02dE%02d', MAX(season,1), episode) ELSE '' END) = ?2
                      ORDER BY id DESC LIMIT 1",
                    params![lookup_name, title.trim()],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                            r.get(9)?,
                            r.get(10)?,
                            r.get(11)?,
                            r.get(12)?,
                            r.get(13)?,
                        ))
                    },
                )
                .optional()?
            };

            let Some((
                s_id,
                pid,
                file_id,
                parent_cid,
                rel_path,
                sidecars_raw,
                pick_code,
                s_media_type,
                s_tmdb_id,
                s_season,
                s_episode,
                s_slot,
                s_reason,
                s_state,
            )) = s_row
            else {
                // If `staging_items` row was already cleaned out, check if a file with `lookup_name` is still in primary `source_cid`
                let fallback_pipe = cfg.pipelines.first().cloned();
                let live_by_name = fallback_pipe.as_ref().and_then(|p| {
                    lookup_in_staging_cid(&mut staging_dir_cache, &p.source_cid, "", lookup_name)
                });
                if let (Some(live_file), Some(pipeline)) = (live_by_name, fallback_pipe) {
                    let eval = evaluate_staging_file(
                        host,
                        cfg,
                        &pipeline,
                        &recognition,
                        &live_file,
                        "",
                        false,
                    );
                    let new_sid =
                        upsert_staging_item(db, pipeline.id, &live_file, "", &[], &eval)?;
                    let _ = db.execute("DELETE FROM activity_logs WHERE id = ?", [aid]);
                    let exec = execute_staging_items_opts(
                        db,
                        host,
                        cfg,
                        settings,
                        &[new_sid],
                        None,
                        ExecOpts {
                            soft_budget_secs: 8,
                            max_organize_files: 4,
                            organize_http_timeout_secs: 22,
                            prune_budget_secs: 0,
                        },
                    )?;
                    if let Some((fmap, _)) = staging_dir_cache.get_mut(&live_file.parent_id) {
                        fmap.remove(&live_file.file_id);
                    }
                    retried += 1;
                    if notices.len() < 4 {
                        notices.push(format!(
                            "《{title}》已在待洗版重评判并执行（归档 {} · 回收站 {}）",
                            exec["organized"].as_u64().unwrap_or(0),
                            exec["recycled_115"].as_u64().unwrap_or(0)
                        ));
                    }
                } else if is_warning {
                    let _ = db.execute(
                        "DELETE FROM activity_logs WHERE id = ? AND status = 'warning'",
                        [aid],
                    );
                    cleared_stale += 1;
                    retried += 1;
                } else if notices.len() < 4 {
                    notices.push(format!("《{title}》未找到关联的 115 文件记录"));
                }
                continue;
            };

            let pipeline_opt = cfg
                .pipelines
                .iter()
                .find(|p| p.id == pid)
                .or_else(|| cfg.pipelines.first())
                .cloned();

            let is_archived_done = !is_warning && action == "organized" && s_state == "organized";
            let mut located_in_staging: Option<Cloud115File> = None;
            if !is_archived_done {
                let src_cid_fallback = pipeline_opt
                    .as_ref()
                    .map(|p| p.source_cid.trim())
                    .unwrap_or_default();
                for cand_cid in [parent_cid.trim(), src_cid_fallback] {
                    if cand_cid.is_empty() {
                        continue;
                    }
                    if let Some(found) = lookup_in_staging_cid(
                        &mut staging_dir_cache,
                        cand_cid,
                        &file_id,
                        lookup_name,
                    ) {
                        located_in_staging = Some(found);
                        break;
                    }
                }
            }

            if let Some(live_file) = located_in_staging {
                let Some(pipeline) = pipeline_opt else {
                    bail!("尚未配置 115 待洗版监控目录");
                };
                if !live_file.pick_code.is_empty() {
                    let _ = host.open115_probe_media_info(&live_file, true, false, false, 2_500);
                }
                let eval = evaluate_staging_file(
                    host,
                    cfg,
                    &pipeline,
                    &recognition,
                    &live_file,
                    &rel_path,
                    false,
                );
                let sidecars: Vec<String> =
                    serde_json::from_str(&sidecars_raw).unwrap_or_default();
                let saved_sid =
                    upsert_staging_item(db, pipeline.id, &live_file, &rel_path, &sidecars, &eval)?;

                if eval.verdict == "probing" {
                    let _ = db.execute(
                        "UPDATE activity_logs SET detail = ?, created_at = ? WHERE id = ?",
                        params![eval.reason, now_ts(), aid],
                    );
                    retried += 1;
                    if notices.len() < 4 {
                        notices.push(format!("《{title}》正在后台探测 115 MediaInfo，下轮自动归档"));
                    }
                    continue;
                }

                let _ = db.execute("DELETE FROM activity_logs WHERE id = ?", [aid]);
                if has_exact_sub {
                    let _ = db.execute(
                        "DELETE FROM activity_logs
                          WHERE record_type = 'media'
                            AND status = 'warning'
                            AND sub_title = ?",
                        params![lookup_name],
                    );
                }
                let exec = execute_staging_items_opts(
                    db,
                    host,
                    cfg,
                    settings,
                    &[saved_sid],
                    None,
                    ExecOpts {
                        soft_budget_secs: 8,
                        max_organize_files: 4,
                        organize_http_timeout_secs: 22,
                        prune_budget_secs: 0,
                    },
                )?;
                if exec["moved"].as_u64().unwrap_or(0) > 0 {
                    if let Some((fmap, _)) = staging_dir_cache.get_mut(&live_file.parent_id) {
                        fmap.remove(&live_file.file_id);
                    }
                }
                retried += 1;
                if notices.len() < 4 {
                    notices.push(format!(
                        "《{title}》已在待洗版重评判并执行（归档 {} · 回收站 {}）",
                        exec["organized"].as_u64().unwrap_or(0),
                        exec["recycled_115"].as_u64().unwrap_or(0)
                    ));
                }
            } else if is_warning {
                // Fast zero-115-API resolution for `warning` rows whose files already left `待洗版`!
                let mut resolved_strm = String::new();
                if !s_tmdb_id.trim().is_empty() && matches!(s_media_type.as_str(), "movie" | "tv") {
                    let ep_pair =
                        (s_media_type == "tv" && s_episode > 0).then_some((s_season.max(1), s_episode));
                    if let Ok(page) = host.library_items_by_tmdb(&s_tmdb_id, &s_media_type, ep_pair)
                    {
                        for entry in &page.items {
                            if let Ok(detail) = host.library_item_detail(&entry.id) {
                                for src in detail.effective_sources() {
                                    let exact_hit = (!file_id.is_empty() && src.file_id == file_id)
                                        || (!pick_code.is_empty()
                                            && src
                                                .effective_pick_code()
                                                .eq_ignore_ascii_case(&pick_code))
                                        || (s_state == "organized"
                                            && detail.effective_sources().len() == 1);
                                    if exact_hit {
                                        for p in [
                                            &src.strm_path,
                                            &src.cloud_path,
                                            &src.file_path,
                                            &detail.path,
                                        ] {
                                            if !p.trim().is_empty() {
                                                resolved_strm = p.trim().to_string();
                                                break;
                                            }
                                        }
                                    }
                                    if !resolved_strm.is_empty() {
                                        break;
                                    }
                                }
                            }
                            if !resolved_strm.is_empty() {
                                break;
                            }
                        }
                    }
                }

                if !resolved_strm.is_empty() || s_state == "organized" {
                    let final_target = if !resolved_strm.is_empty() {
                        resolved_strm
                    } else if !cfg.host_sync.transfer_target_folder.is_empty() {
                        format!(
                            "115://{}（主程序二级分类归档）",
                            cfg.host_sync.transfer_target_folder
                        )
                    } else {
                        "Mediary 媒体库（主程序二级分类归档）".to_string()
                    };
                    let mut eff_slot_label = slot.clone();
                    if let Some(cat) = category::locate(
                        &cfg.host_sync.categories,
                        &s_media_type,
                        &[&final_target],
                    ) {
                        eff_slot_label = if !s_slot.trim().is_empty() {
                            format!("{cat} · {}", s_slot.trim())
                        } else {
                            cat
                        };
                    }
                    let healed_detail = if !s_reason.trim().is_empty()
                        && !s_reason.starts_with("主程序原生归档失败")
                    {
                        format!(
                            "已核验主程序归档入库并生成 STRM · {}",
                            s_reason.trim()
                        )
                    } else {
                        "已核验主程序归档入库并生成 STRM".to_string()
                    };
                    db.execute(
                        "UPDATE activity_logs
                            SET target_path = ?, slot = ?, detail = ?, status = 'done', created_at = ?
                          WHERE id = ?",
                        params![final_target, eff_slot_label, healed_detail, now_ts(), aid],
                    )?;
                    if has_exact_sub {
                        let _ = db.execute(
                            "DELETE FROM activity_logs
                              WHERE record_type = 'media'
                                AND status = 'warning'
                                AND sub_title = ?
                                AND id != ?",
                            params![lookup_name, aid],
                        );
                    }
                    healed_done += 1;
                    retried += 1;
                } else {
                    if s_state == "pending" {
                        let _ = db.execute("DELETE FROM staging_items WHERE id = ?", [s_id]);
                    }
                    let _ = db.execute(
                        "DELETE FROM activity_logs WHERE id = ? AND status = 'warning'",
                        [aid],
                    );
                    if has_exact_sub {
                        let _ = db.execute(
                            "DELETE FROM activity_logs
                              WHERE record_type = 'media'
                                AND status = 'warning'
                                AND sub_title = ?",
                            params![lookup_name],
                        );
                    }
                    cleared_stale += 1;
                    retried += 1;
                }
            } else if action == "organized" || s_state == "organized" {
                let (archive_target_cid, archive_task_name) =
                    resolve_archive_context(db, host, cfg, pid);
                // Target-directory in-place re-organize using latest Mediary secondary category & rename templates!
                if let Some((target_dir_cid, live_file, dir_files)) =
                    locate_archived_in_target_library(
                        host,
                        cfg,
                        archive_target_cid.as_deref(),
                        &file_id,
                        &pick_code,
                        &s_media_type,
                        &s_tmdb_id,
                        s_season,
                        s_episode,
                        &cur_target_path,
                    )
                {
                    if !live_file.pick_code.is_empty() {
                        // Fast non-blocking cache check (2.5s max) so we don't burn 15s of the 45s UI budget!
                        let _ = host.open115_probe_media_info(&live_file, true, false, false, 2_500);
                    }
                    let v_stem = stem_prefix(&live_file.name);
                    let mut org_batch = vec![OrganizeFileItem {
                        file_id: live_file.file_id.clone(),
                        parent_id: target_dir_cid.clone(),
                        name: live_file.name.clone(),
                        size: live_file.size,
                        sha1: live_file.sha1.clone(),
                        pick_code: live_file.pick_code.clone(),
                        season: s_season,
                        episode: s_episode,
                    }];
                    if !v_stem.is_empty() {
                        for sub in dir_files {
                            if sub.file_id != live_file.file_id
                                && is_subtitle_name(&sub.name)
                                && sub.name.to_lowercase().starts_with(&v_stem)
                            {
                                org_batch.push(OrganizeFileItem {
                                    file_id: sub.file_id,
                                    parent_id: target_dir_cid.clone(),
                                    name: sub.name,
                                    size: sub.size,
                                    sha1: sub.sha1,
                                    pick_code: sub.pick_code,
                                    season: s_season,
                                    episode: s_episode,
                                });
                            }
                        }
                    }

                    let resp = host.open115_organize_files_with_timeout(
                        &target_dir_cid,
                        archive_target_cid.as_deref(),
                        archive_task_name.as_deref(),
                        &s_media_type,
                        &s_tmdb_id,
                        &org_batch,
                        22,
                    )?;
                    let new_target = resolve_organized_target_path(
                        host,
                        &resp,
                        &live_file.file_id,
                        &live_file.pick_code,
                        &live_file.name,
                        &s_media_type,
                        &s_tmdb_id,
                        s_season,
                        s_episode,
                        &cur_target_path,
                    );
                    let final_target = if !new_target.is_empty() {
                        new_target
                    } else {
                        cur_target_path.clone()
                    };
                    // Clean up now-empty old show/season folders on 115 if template rename moved the file to a new `[tmdbid-...]` folder!
                    if let Some(root_cid) = archive_target_cid.as_deref() {
                        let old_segs = extract_target_rel_segments(
                            &cur_target_path,
                            &cfg.host_sync.transfer_target_folder,
                        );
                        let new_segs = extract_target_rel_segments(
                            &final_target,
                            &cfg.host_sync.transfer_target_folder,
                        );
                        if !old_segs.is_empty() && old_segs != new_segs {
                            let cat_depth = category::locate(
                                &cfg.host_sync.categories,
                                &s_media_type,
                                &[&cur_target_path],
                            )
                            .map(|c| c.split('/').filter(|s| !s.is_empty()).count())
                            .unwrap_or(0);
                            if cat_depth > 0 && old_segs.len() > cat_depth {
                                // Prune leaf-up (e.g. `Season 1`, then `天声一对 (2025)`) only when 100% empty!
                                for depth in ((cat_depth + 1)..=old_segs.len()).rev() {
                                    let parent_refs: Vec<&str> =
                                        old_segs[..depth - 1].iter().map(String::as_str).collect();
                                    let leaf_refs: Vec<&str> =
                                        old_segs[..depth].iter().map(String::as_str).collect();
                                    if let (Ok(Some(p_cid)), Ok(Some(l_cid))) = (
                                        host.open115_resolve_subpath_cid(root_cid, &parent_refs),
                                        host.open115_resolve_subpath_cid(root_cid, &leaf_refs),
                                    ) {
                                        if !cfg.host_sync.protected_cids.contains(&l_cid) {
                                            if let Ok(pg) =
                                                host.open115_list_page(&l_cid, 0, 5, true)
                                            {
                                                if pg.count == 0 && pg.items.is_empty() {
                                                    let _ =
                                                        host.open115_delete_files(&p_cid, &[l_cid]);
                                                } else {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let mut eff_slot_label = slot.clone();
                    if let Some(cat) = category::locate(
                        &cfg.host_sync.categories,
                        &s_media_type,
                        &[&final_target],
                    ) {
                        eff_slot_label = if !s_slot.trim().is_empty() {
                            format!("{cat} · {}", s_slot.trim())
                        } else {
                            cat
                        };
                    }
                    let preserved_reason = if !s_reason.trim().is_empty() {
                        s_reason.trim().to_string()
                    } else if let Some((_, tail)) = cur_detail.split_once(" · ") {
                        tail.trim().to_string()
                    } else {
                        String::new()
                    };
                    let updated_detail = if preserved_reason.is_empty() {
                        "已从媒体库目标目录按最新命名模板重新整理并刷新 STRM".to_string()
                    } else {
                        format!("已从媒体库目标目录按最新命名模板重新整理并刷新 STRM · {preserved_reason}")
                    };
                    db.execute(
                        "UPDATE activity_logs
                            SET target_path = ?, slot = ?, detail = ?, status = 'done', created_at = ?
                          WHERE id = ?",
                        params![final_target, eff_slot_label, updated_detail, now_ts(), aid],
                    )?;
                    retried += 1;
                    if notices.len() < 4 {
                        notices.push(format!("《{title}》已从媒体库目标目录按最新模板重新整理"));
                    }
                } else {
                    // Fallback: check if user manually moved it back into `待洗版`!
                    let src_cid_fallback = pipeline_opt
                        .as_ref()
                        .map(|p| p.source_cid.trim())
                        .unwrap_or_default();
                    let mut back_in_staging = None;
                    for cand_cid in [parent_cid.trim(), src_cid_fallback] {
                        if let Some(found) = lookup_in_staging_cid(
                            &mut staging_dir_cache,
                            cand_cid,
                            &file_id,
                            lookup_name,
                        ) {
                            back_in_staging = Some(found);
                            break;
                        }
                    }
                    if let (Some(live_file), Some(pipeline)) = (back_in_staging, pipeline_opt) {
                        let eval = evaluate_staging_file(
                            host,
                            cfg,
                            &pipeline,
                            &recognition,
                            &live_file,
                            &rel_path,
                            false,
                        );
                        let sidecars: Vec<String> =
                            serde_json::from_str(&sidecars_raw).unwrap_or_default();
                        let saved_sid = upsert_staging_item(
                            db,
                            pipeline.id,
                            &live_file,
                            &rel_path,
                            &sidecars,
                            &eval,
                        )?;
                        let _ = db.execute("DELETE FROM activity_logs WHERE id = ?", [aid]);
                        let exec = execute_staging_items_opts(
                            db,
                            host,
                            cfg,
                            settings,
                            &[saved_sid],
                            None,
                            ExecOpts {
                                soft_budget_secs: 8,
                                max_organize_files: 4,
                                organize_http_timeout_secs: 22,
                                prune_budget_secs: 0,
                            },
                        )?;
                        retried += 1;
                        if notices.len() < 4 {
                            notices.push(format!(
                                "《{title}》已在待洗版重评判并执行（归档 {}）",
                                exec["organized"].as_u64().unwrap_or(0)
                            ));
                        }
                    } else if notices.len() < 4 {
                        notices.push(format!(
                            "《{title}》在待洗版与目标媒体库目录中均未找到（可能已被移动或删除），已保留原历史详情"
                        ));
                    }
                }
            } else {
                // e.g. `recycled_115` item already in 115 Recycle Bin — NEVER clobber original `detail`!
                if notices.len() < 4 {
                    notices.push(format!(
                        "《{title}》已移入 115 官方回收站，如需重洗请先从 115 回收站还原至待洗版目录"
                    ));
                }
            }
        } else {
            let r = reconcile_ledger(db, host, cfg, settings)?;
            retried += 1;
            notices.push(format!(
                "已重新核验洗版删旧账本（就绪 {} · 自动清理 {}）",
                r["verified_ready"].as_u64().unwrap_or(0),
                r["auto_cleaned"].as_u64().unwrap_or(0)
            ));
        }
    }

    if healed_done > 0 {
        notices.insert(
            0,
            format!("核验确认已完成归档并自动消除告警 {healed_done} 条"),
        );
    }
    if cleared_stale > 0 {
        notices.push(format!(
            "已自动清理原文件已移出待洗版的过期告警 {cleared_stale} 条"
        ));
    }
    let notice = if notices.is_empty() {
        format!("已重试 {retried} 条记录")
    } else {
        notices.join("；")
    };
    Ok(json!({
        "retried": retried,
        "healed_done": healed_done,
        "cleared_stale": cleared_stale,
        "remaining_ids": remaining_ids,
        "notice": notice,
    }))
}

/// Batch-repairs all organized Media History rows whose `target_path` lacks `[tmdbid-...]`
/// by grouping files in the same 115 folder (`(pid, media_type, tmdb_id, dir_segments)`) into
/// single folder-level `open115/files/organize` requests (with `task_name` template inheritance)
/// and pruning emptied legacy folders afterward.
pub fn repair_missing_tmdbid_history(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    soft_budget_secs: u64,
) -> Result<Value> {
    ensure!(
        !settings.dry_run,
        "当前处于「仅预览模式（干跑）」，请先关闭仅预览模式后再执行批量重整"
    );

    let repair_clock = Instant::now();
    let healed_first = backfill_recent_media_history(db, Some(host), cfg, 25);

    #[derive(Clone)]
    struct RepairRow {
        aid: i64,
        title: String,
        sub_title: String,
        cur_target_path: String,
        slot: String,
        cur_detail: String,
        pid: i64,
        file_id: String,
        pick_code: String,
        media_type: String,
        tmdb_id: String,
        season: u32,
        episode: u32,
        s_slot: String,
        s_reason: String,
    }

    let raw_rows: Vec<(i64, String, String, String, String, String)> = {
        let mut stmt = db.prepare(
            "SELECT id, title, sub_title, target_path, slot, detail
               FROM activity_logs
              WHERE record_type = 'media'
                AND action = 'organized'
                AND status = 'done'
                AND target_path NOT LIKE '%[tmdbid-%'
              ORDER BY id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        rows.filter_map(Result::ok).collect()
    };

    if raw_rows.is_empty() {
        return Ok(json!({
            "repaired": healed_first,
            "groups_processed": 0,
            "remaining": 0,
            "has_more": false,
            "notice": if healed_first > 0 {
                format!("已自动回填核验 {healed_first} 条带 [tmdbid-...] 的归档记录，全部历史记录均已齐全")
            } else {
                "历史记录中所有归档条目均已包含 [tmdbid-...] 标签".to_string()
            },
        }));
    }

    type GroupKey = (i64, String, String, Vec<String>);
    let mut folder_groups: BTreeMap<GroupKey, Vec<RepairRow>> = BTreeMap::new();
    let mut fallback_ids: Vec<i64> = Vec::new();

    for (aid, title, sub_title, cur_target_path, slot, cur_detail) in raw_rows {
        let lookup_name = if !sub_title.trim().is_empty() {
            sub_title.trim()
        } else {
            title.trim()
        };
        #[allow(clippy::type_complexity)]
        let s_row: Option<(
            i64,
            String,
            String,
            String,
            String,
            u32,
            u32,
            String,
            String,
        )> = db
            .query_row(
                "SELECT pipeline_id, file_id, pick_code, media_type, tmdb_id, season, episode, slot, reason
                   FROM staging_items
                  WHERE name = ?1
                  ORDER BY id DESC LIMIT 1",
                params![lookup_name],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .optional()?;

        if let Some((pid, file_id, pick_code, media_type, tmdb_id, season, episode, s_slot, s_reason)) =
            s_row
        {
            let segs = extract_target_rel_segments(
                &cur_target_path,
                &cfg.host_sync.transfer_target_folder,
            );
            if !segs.is_empty()
                && !tmdb_id.trim().is_empty()
                && matches!(media_type.as_str(), "movie" | "tv")
            {
                let rr = RepairRow {
                    aid,
                    title,
                    sub_title,
                    cur_target_path,
                    slot,
                    cur_detail,
                    pid,
                    file_id,
                    pick_code,
                    media_type: media_type.clone(),
                    tmdb_id: tmdb_id.clone(),
                    season,
                    episode,
                    s_slot,
                    s_reason,
                };
                folder_groups
                    .entry((pid, media_type, tmdb_id, segs))
                    .or_default()
                    .push(rr);
            } else {
                fallback_ids.push(aid);
            }
        }
    }

    let mut subpath_cache: BTreeMap<(String, Vec<String>), Option<String>> = BTreeMap::new();
    let resolve_cached =
        |cache: &mut BTreeMap<(String, Vec<String>), Option<String>>,
         root_cid: &str,
         segs: &[String]|
         -> Option<String> {
            if segs.is_empty() || root_cid.trim().is_empty() || root_cid.trim() == "0" {
                return None;
            }
            let full_key = (root_cid.trim().to_string(), segs.to_vec());
            if let Some(v) = cache.get(&full_key) {
                return v.clone();
            }
            let mut cur_cid = root_cid.trim().to_string();
            for depth in 1..=segs.len() {
                let prefix: Vec<String> = segs[..depth].to_vec();
                let key = (root_cid.trim().to_string(), prefix);
                if let Some(cached) = cache.get(&key) {
                    match cached {
                        Some(c) => cur_cid = c.clone(),
                        None => return None,
                    }
                } else {
                    let one = [segs[depth - 1].as_str()];
                    let resolved = host
                        .open115_resolve_subpath_cid(&cur_cid, &one)
                        .ok()
                        .flatten()
                        .filter(|s| !s.is_empty());
                    cache.insert(key, resolved.clone());
                    match resolved {
                        Some(c) => cur_cid = c,
                        None => return None,
                    }
                }
            }
            Some(cur_cid)
        };

    let mut repaired = healed_first;
    let mut groups_done = 0usize;
    let mut group_titles: Vec<String> = Vec::new();

    for ((pid, media_type, tmdb_id, old_segs), grp_rows) in folder_groups {
        if groups_done > 0 && repair_clock.elapsed() >= Duration::from_secs(soft_budget_secs) {
            break;
        }
        let (archive_target_cid, archive_task_name) =
            resolve_archive_context(db, host, cfg, pid);
        let Some(root_cid) = archive_target_cid else {
            continue;
        };

        let Some(target_dir_cid) = resolve_cached(&mut subpath_cache, &root_cid, &old_segs) else {
            for r in grp_rows {
                fallback_ids.push(r.aid);
            }
            continue;
        };

        let Ok((dir_map, _)) = host.open115_list_dir_files_map_ext(&target_dir_cid, 6) else {
            continue;
        };
        let all_dir_files: Vec<Cloud115File> = dir_map.values().cloned().collect();
        if all_dir_files.is_empty() {
            for r in grp_rows {
                fallback_ids.push(r.aid);
            }
            continue;
        }

        let mut org_batch: Vec<OrganizeFileItem> = Vec::new();
        let mut matched_rows: Vec<(RepairRow, Cloud115File)> = Vec::new();
        let mut seen_fids: BTreeSet<String> = BTreeSet::new();

        for r in grp_rows {
            let target_fname = r
                .cur_target_path
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .trim();
            let target_stem = stem_prefix(target_fname);
            let found = all_dir_files
                .iter()
                .find(|f| !r.file_id.is_empty() && f.file_id == r.file_id)
                .or_else(|| {
                    all_dir_files.iter().find(|f| {
                        !r.pick_code.is_empty() && f.pick_code.eq_ignore_ascii_case(&r.pick_code)
                    })
                })
                .or_else(|| {
                    all_dir_files
                        .iter()
                        .find(|f| !r.sub_title.is_empty() && f.name == r.sub_title)
                })
                .or_else(|| {
                    all_dir_files.iter().find(|f| {
                        !target_stem.is_empty()
                            && is_video_name(&f.name)
                            && stem_prefix(&f.name) == target_stem
                    })
                })
                .cloned();

            let Some(live_file) = found else {
                fallback_ids.push(r.aid);
                continue;
            };

            if seen_fids.insert(live_file.file_id.clone()) {
                if !live_file.pick_code.is_empty() {
                    let _ = host.open115_probe_media_info(&live_file, true, false, false, 2_000);
                }
                let v_stem = stem_prefix(&live_file.name);
                org_batch.push(OrganizeFileItem {
                    file_id: live_file.file_id.clone(),
                    parent_id: target_dir_cid.clone(),
                    name: live_file.name.clone(),
                    size: live_file.size,
                    sha1: live_file.sha1.clone(),
                    pick_code: live_file.pick_code.clone(),
                    season: r.season,
                    episode: r.episode,
                });
                if !v_stem.is_empty() {
                    for sub in &all_dir_files {
                        if sub.file_id != live_file.file_id
                            && is_subtitle_name(&sub.name)
                            && sub.name.to_lowercase().starts_with(&v_stem)
                            && seen_fids.insert(sub.file_id.clone())
                        {
                            org_batch.push(OrganizeFileItem {
                                file_id: sub.file_id.clone(),
                                parent_id: target_dir_cid.clone(),
                                name: sub.name.clone(),
                                size: sub.size,
                                sha1: sub.sha1.clone(),
                                pick_code: sub.pick_code.clone(),
                                season: r.season,
                                episode: r.episode,
                            });
                        }
                    }
                }
            }
            matched_rows.push((r, live_file));
        }

        if org_batch.is_empty() {
            continue;
        }

        let resp = match host.open115_organize_files_with_timeout(
            &target_dir_cid,
            Some(&root_cid),
            archive_task_name.as_deref(),
            &media_type,
            &tmdb_id,
            &org_batch,
            90,
        ) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Discover the newly created `[tmdbid-<tmdb_id>]` directory on 115 so every file gets its exact new `.strm` path immediately!
        let cat_depth = category::locate(
            &cfg.host_sync.categories,
            &media_type,
            &[&matched_rows[0].0.cur_target_path],
        )
        .map(|c| c.split('/').filter(|s| !s.is_empty()).count())
        .unwrap_or(0);

        let mut new_rel_by_fid: BTreeMap<String, String> = BTreeMap::new();
        if cat_depth > 0 && old_segs.len() >= cat_depth {
            let cat_segs = &old_segs[..cat_depth];
            if let Some(cat_cid) = resolve_cached(&mut subpath_cache, &root_cid, cat_segs) {
                let tag_needle = format!("[tmdbid-{tmdb_id}]");
                if let Ok(cat_page) = host.open115_list_page(&cat_cid, 0, 1000, true) {
                    if let Some(show_dir) = cat_page
                        .items
                        .iter()
                        .find(|it| it.is_dir && it.name.contains(&tag_needle))
                    {
                        let show_name = show_dir.name.clone();
                        let show_cid = show_dir.file_id.clone();
                        if media_type == "movie" {
                            if let Ok((fmap, _)) = host.open115_list_dir_files_map_ext(&show_cid, 2)
                            {
                                for (fid, nf) in fmap {
                                    new_rel_by_fid.insert(fid, format!("{show_name}/{}", nf.name));
                                }
                            }
                        } else {
                            let seasons: BTreeSet<u32> = matched_rows
                                .iter()
                                .map(|(r, _)| r.season.max(1))
                                .collect();
                            for sn in seasons {
                                let sn_dir = format!("Season {sn:02}");
                                if let Ok(Some(sn_cid)) =
                                    host.open115_resolve_subpath_cid(&show_cid, &[sn_dir.as_str()])
                                {
                                    if let Ok((fmap, _)) =
                                        host.open115_list_dir_files_map_ext(&sn_cid, 3)
                                    {
                                        for (fid, nf) in fmap {
                                            new_rel_by_fid.insert(
                                                fid,
                                                format!("{show_name}/{sn_dir}/{}", nf.name),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let now = now_ts();
        let mut any_moved_out = false;
        for (r, live_file) in &matched_rows {
            let mut final_target = String::new();
            if let Some(rel_after_cat) = new_rel_by_fid.get(&live_file.file_id) {
                let cat_str = old_segs[..cat_depth].join("/");
                let strm_rel = match rel_after_cat.rsplit_once('.') {
                    Some((stem, _)) => format!("{stem}.strm"),
                    None => format!("{rel_after_cat}.strm"),
                };
                let strm_prefix = r
                    .cur_target_path
                    .split_once("/媒体库/")
                    .map(|(p, _)| p)
                    .unwrap_or("/strm/cloud");
                final_target = format!("{strm_prefix}/媒体库/{cat_str}/{strm_rel}");
            }
            if final_target.is_empty() {
                final_target = resolve_organized_target_path(
                    host,
                    &resp,
                    &live_file.file_id,
                    &live_file.pick_code,
                    &live_file.name,
                    &media_type,
                    &tmdb_id,
                    r.season,
                    r.episode,
                    &r.cur_target_path,
                );
            }
            if final_target.is_empty() {
                final_target = r.cur_target_path.clone();
            }
            if final_target != r.cur_target_path {
                any_moved_out = true;
            }
            let mut eff_slot_label = r.slot.clone();
            if let Some(cat) =
                category::locate(&cfg.host_sync.categories, &media_type, &[&final_target])
            {
                eff_slot_label = if !r.s_slot.trim().is_empty() {
                    format!("{cat} · {}", r.s_slot.trim())
                } else {
                    cat
                };
            }
            let preserved_reason = if !r.s_reason.trim().is_empty() {
                r.s_reason.trim().to_string()
            } else if let Some((_, tail)) = r.cur_detail.split_once(" · ") {
                tail.trim().to_string()
            } else {
                String::new()
            };
            let updated_detail = if preserved_reason.is_empty() {
                "已从媒体库目标目录按最新 [tmdbid] 命名模板重新整理并刷新 STRM".to_string()
            } else {
                format!(
                    "已从媒体库目标目录按最新 [tmdbid] 命名模板重新整理并刷新 STRM · {preserved_reason}"
                )
            };
            db.execute(
                "UPDATE activity_logs
                    SET target_path = ?, slot = ?, detail = ?, status = 'done', created_at = ?
                  WHERE id = ?",
                params![final_target, eff_slot_label, updated_detail, now, r.aid],
            )?;
            repaired += 1;
        }

        // Prune emptied legacy show/season folders (`Season 1`, `天声一对 (2025)`) once moved into `[tmdbid-...]`
        if any_moved_out && cat_depth > 0 && old_segs.len() > cat_depth {
            for depth in ((cat_depth + 1)..=old_segs.len()).rev() {
                let p_opt = resolve_cached(&mut subpath_cache, &root_cid, &old_segs[..depth - 1]);
                let l_opt = resolve_cached(&mut subpath_cache, &root_cid, &old_segs[..depth]);
                if let (Some(p_cid), Some(l_cid)) = (p_opt, l_opt) {
                    if !cfg.host_sync.protected_cids.contains(&l_cid) {
                        if let Ok(pg) = host.open115_list_page(&l_cid, 0, 5, true) {
                            if pg.count == 0 && pg.items.is_empty() {
                                let _ = host.open115_delete_files(&p_cid, &[l_cid]);
                                subpath_cache
                                    .remove(&(root_cid.clone(), old_segs[..depth].to_vec()));
                            } else {
                                break;
                            }
                        }
                    }
                }
            }
        }

        groups_done += 1;
        if group_titles.len() < 6 {
            let base_title = matched_rows[0]
                .0
                .title
                .rsplit_once(" S")
                .map(|(b, _)| b)
                .unwrap_or(&matched_rows[0].0.title);
            group_titles.push(format!("{base_title}({}项)", matched_rows.len()));
        }
    }

    for faid in fallback_ids {
        if repair_clock.elapsed() >= Duration::from_secs(soft_budget_secs) {
            break;
        }
        if let Ok(r) = retry_history_items(db, host, cfg, settings, &[faid]) {
            repaired += r["retried"].as_u64().unwrap_or(0) as usize;
        }
    }

    let remaining_after: usize = db
        .query_row(
            "SELECT COUNT(*) FROM activity_logs
              WHERE record_type = 'media'
                AND action = 'organized'
                AND status = 'done'
                AND target_path NOT LIKE '%[tmdbid-%'
                AND target_path LIKE '/%'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0) as usize;

    let preview = if group_titles.is_empty() {
        String::new()
    } else {
        format!("（{}）", group_titles.join("、"))
    };
    let notice = if remaining_after > 0 {
        format!(
            "已按 [tmdbid] 模板批量重整 {repaired} 条记录{preview}，剩余 {remaining_after} 条继续处理"
        )
    } else {
        format!("已按 [tmdbid] 模板全部重整完成，共更新 {repaired} 条记录{preview}")
    };

    Ok(json!({
        "repaired": repaired,
        "groups_processed": groups_done,
        "remaining": remaining_after,
        "has_more": remaining_after > 0,
        "notice": notice,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mediainfo_json_sidecar_clutter_and_ghost_reconcile_work() {
        let sidecar = Cloud115File {
            file_id: "1001".to_string(),
            name: "骨鉴卿心 (2026) - S01E01 - 2160p-mediainfo.json".to_string(),
            size: 18_432,
            is_dir: false,
            ..Default::default()
        };
        assert!(is_leftover_clutter(&sidecar));

        let huge_json = Cloud115File {
            size: 25 * 1024 * 1024,
            ..sidecar.clone()
        };
        assert!(!is_leftover_clutter(&huge_json));

        let video = Cloud115File {
            file_id: "1002".to_string(),
            name: "骨鉴卿心.S01E01.mp4".to_string(),
            size: 1_500_000_000,
            is_dir: false,
            ..Default::default()
        };
        assert!(!is_leftover_clutter(&video));

        let tmp = std::env::temp_dir().join(format!("mediwash_stg_test_{}", now_ts()));
        let db = db::open_db(&tmp).unwrap();
        db.execute(
            "INSERT INTO staging_items (pipeline_id, file_id, parent_cid, name, state, verdict)
             VALUES (1, 'live_1', '999', 'a.mkv', 'pending', 'review'),
                    (1, 'ghost_2', '999', 'b.mkv', 'pending', 'review'),
                    (1, 'archived_3', '999', 'c.mkv', 'organized', 'admit_new')",
            [],
        )
        .unwrap();
        let live_set = BTreeSet::from(["live_1"]);
        reconcile_disappeared_in_cid(&db, 1, "999", true, &live_set);
        let remaining: Vec<(String, String)> = db
            .prepare("SELECT file_id, state FROM staging_items ORDER BY file_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            remaining,
            vec![
                ("archived_3".to_string(), "organized".to_string()),
                ("live_1".to_string(), "pending".to_string()),
            ]
        );

        // Verify Pass 1 local category + slot auto-backfill on placeholder history rows
        let mut cfg = db::load_config(&db).unwrap();
        cfg.host_sync.categories = vec![
            crate::models::CategoryRule {
                path: "电视剧/国产剧/2020-2030".to_string(),
                media_type: "tv".to_string(),
                iso_zone: false,
            },
            crate::models::CategoryRule {
                path: "动漫/国漫/2010-2019".to_string(),
                media_type: "tv".to_string(),
                iso_zone: false,
            },
        ];
        db.execute(
            "INSERT INTO staging_items (pipeline_id, file_id, parent_cid, name, media_type, tmdb_id, season, episode, slot, state, verdict)
             VALUES (1, 'f_bz', '999', '宝珠.2026.S01E18.1080P.SDR.H.264.AAC.mkv', 'tv', '318349', 1, 18, '1080p-SDR', 'organized', 'admit_new')",
            [],
        )
        .unwrap();
        db::log_media_history(
            &db,
            "115归档",
            "宝珠 S01E18",
            "宝珠.2026.S01E18.1080P.SDR.H.264.AAC.mkv",
            "115://待洗版/link/电视剧/国产剧/2020-2030/宝珠 (2026) [tmdbid-318349]/Season 01/宝珠.2026.S01E18.1080P.SDR.H.264.AAC.mkv",
            "115://媒体库（主程序二级分类归档）",
            "主程序自动分类 · 1080p-SDR",
            "organized",
            "done",
            "测试入库",
        );
        db::log_media_history(
            &db,
            "115归档",
            "斗破苍穹 S05E91",
            "斗破苍穹 - S05E91 - 2160p.WEB-DL.H265.mp4",
            "115://待洗版/斗破苍穹/斗破苍穹 - S05E91 - 2160p.WEB-DL.H265.mp4",
            "115 官方回收站（库内已有：/strm/cloud/媒体库/动漫/国漫/2010-2019/斗破苍穹/S05E91.strm）",
            "主程序自动分类",
            "recycled_115",
            "done",
            "同源已在库",
        );
        let healed = backfill_recent_media_history(&db, None, &cfg, 0);
        assert_eq!(healed, 2);
        let slots: Vec<(String, String)> = db
            .prepare("SELECT title, slot FROM activity_logs WHERE record_type='media' ORDER BY id ASC")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(slots[0].1, "电视剧/国产剧/2020-2030 · 1080p-SDR");
        assert!(slots[1].1.starts_with("动漫/国漫/2010-2019 · 4K"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

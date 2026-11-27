mod category;
mod db;
mod host;
mod identity;
mod library;
mod models;
mod pt;
mod rules;
mod staging;

use anyhow::{bail, ensure, Context, Result};
use host::{Cloud115File, HostClient, LibrarySource, MediaStream};
use identity::RecognitionEngine;
use models::{
    now_ts, AppConfig, PluginSettings, PtWashConfig, ScoringRule, StagingPipeline, UnifiedStrategy,
};
use rules::{MediaProfile, ReferenceBaseline, Scorer};
use serde_json::{json, Value};
use std::env;
use std::io::{self, Read};
use std::path::PathBuf;

const MAX_STDIN_BYTES: u64 = 6 * 1024 * 1024;

fn main() -> std::process::ExitCode {
    match run() {
        Ok(val) => {
            println!("{val}");
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Value> {
    let data_dir = env::var_os("MEDIARY_PLUGIN_DATA_DIR")
        .map_or_else(|| PathBuf::from("data"), PathBuf::from);
    let action = env::var("MEDIARY_PLUGIN_ACTION").unwrap_or_else(|_| "ui_bootstrap".to_string());

    let mut stdin_buf = Vec::new();
    io::stdin()
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut stdin_buf)?;
    ensure!(stdin_buf.len() as u64 <= MAX_STDIN_BYTES, "请求载荷超过 6 MB 上限");
    let input: Value = if stdin_buf.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        serde_json::from_slice(&stdin_buf).context("输入参数不是合法的 JSON")?
    };

    let settings: PluginSettings = env::var("MEDIARY_PLUGIN_SETTINGS_JSON")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut conn = db::open_db(&data_dir)?;

    let res = dispatch(&mut conn, &data_dir, &action, &input, &settings)?;
    if let Ok(cfg) = db::load_config(&conn) {
        db::write_dashboard_file(&data_dir, &conn, &cfg);
    }
    Ok(res)
}

fn dispatch(
    conn: &mut rusqlite::Connection,
    _data_dir: &PathBuf,
    action: &str,
    input: &Value,
    settings: &PluginSettings,
) -> Result<Value> {
    match action {
        "ui_bootstrap" | "dashboard" => {
            let mut host_status = host::HostStatus::default();
            let mut pt_sites = Vec::new();
            if let Ok(host) = HostClient::from_env() {
                let _ = host.sync_unified_rules_if_stale(conn, 180);
                host_status = host.host_status();
                pt_sites = host.list_pt_sites();
            } else if let Some(local_yaml) = category::read_local_host_category_yaml() {
                if let Ok(cats) = category::parse_category_yaml(&local_yaml) {
                    let _ = db::edit_config(conn, false, |c| {
                        c.host_sync.categories = cats;
                        c.host_sync.category_source = "Mediary 宿主 · category.yaml".to_string();
                        c.host_sync.last_synced = now_ts();
                        Ok(false)
                    });
                }
            }
            let cfg = db::load_config(conn)?;
            Ok(build_bootstrap_payload(
                conn,
                &cfg,
                settings,
                Some(host_status),
                Some(pt_sites),
            ))
        }

        "ui_sync_host" => {
            if let Some(raw_yaml) = input["category_yaml"].as_str().filter(|s| !s.trim().is_empty())
            {
                let cats = category::parse_category_yaml(raw_yaml)?;
                let filename = input["filename"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("手动导入 · category.yaml")
                    .to_string();
                let cfg = db::edit_config(conn, true, |c| {
                    c.host_sync.categories = cats;
                    c.host_sync.category_source = filename;
                    c.host_sync.last_synced = now_ts();
                    Ok(true)
                })?;
                return Ok(build_bootstrap_payload(conn, &cfg, settings, None, None));
            }
            let host = HostClient::from_env()?;
            host.sync_unified_rules(conn)?;
            let status = host.host_status();
            let sites = host.list_pt_sites();
            let cfg = db::load_config(conn)?;
            Ok(build_bootstrap_payload(
                conn,
                &cfg,
                settings,
                Some(status),
                Some(sites),
            ))
        }

        "tick" => {
            let is_manual_ui = input["manual"].as_bool() == Some(true)
                || env::var("MEDIARY_PLUGIN_TRIGGER").ok().as_deref() == Some("manual");
            let host = HostClient::from_env()?;
            let _ = host.sync_unified_rules_if_stale(conn, if is_manual_ui { 300 } else { 120 });
            let cfg = db::load_config(conn)?;

            let mut notices = Vec::new();
            let mut any_has_more = false;
            let tick_clock = std::time::Instant::now();
            for p in cfg
                .pipelines
                .iter()
                .filter(|p| p.enabled || (is_manual_ui && !p.source_cid.trim().is_empty()))
            {
                if is_manual_ui && tick_clock.elapsed().as_secs() >= 15 {
                    any_has_more = true;
                    break;
                }
                if input["manual"].as_bool() == Some(true) {
                    let _ = conn.execute(
                        "DELETE FROM cursors WHERE key = ?",
                        [format!("staging_walk:{}", p.id)],
                    );
                }
                let scan_res = if is_manual_ui {
                    staging::scan_pipeline_ui(conn, &host, &cfg, settings, p.id, 10)
                } else {
                    staging::scan_pipeline(conn, &host, &cfg, settings, p.id, 30)
                };
                match scan_res {
                    Ok(r) => {
                        if r["has_more"].as_bool() == Some(true) {
                            any_has_more = true;
                        }
                        if let Some(n) = r["notice"].as_str().filter(|s| !s.is_empty()) {
                            notices.push(n.to_string());
                        }
                    }
                    Err(e) => notices.push(format!("[{}] 异常: {e:#}", p.name)),
                }
            }

            let ledger_res = if !is_manual_ui || (!any_has_more && tick_clock.elapsed().as_secs() < 16) {
                staging::reconcile_ledger(conn, &host, &cfg, settings)?
            } else {
                json!({})
            };
            let ready_cnt = ledger_res["verified_ready"].as_u64().unwrap_or(0);
            let cleaned_cnt = ledger_res["auto_cleaned"].as_u64().unwrap_or(0);
            if ready_cnt > 0 || cleaned_cnt > 0 {
                notices.push(format!(
                    "删旧账本：核验入库 {ready_cnt} 项，自动清理 {cleaned_cnt} 个旧源"
                ));
            }

            let pt_res = if !is_manual_ui || (!any_has_more && tick_clock.elapsed().as_secs() < 12) {
                pt::scheduled_pt_sweep(conn, &host, &cfg, settings)?
            } else {
                json!({ "checked": 0 })
            };
            let pt_checked = pt_res["checked"].as_u64().unwrap_or(0);
            if pt_checked > 0 {
                notices.push(format!(
                    "PT 巡检 {pt_checked} 项（命中 {} 个候选）",
                    pt_res["candidates_found"].as_u64().unwrap_or(0)
                ));
            }

            let final_notice = if notices.is_empty() {
                "巡检完成（暂无待处理新增项）".to_string()
            } else {
                notices.join("；")
            };
            let _ = db::set_cursor(conn, "last_result", &final_notice);
            let _ = db::set_cursor(conn, "last_tick_ts", &now_ts().to_string());
            Ok(json!({
                "ok": true,
                "has_more": any_has_more,
                "notice": final_notice,
                "ledger": ledger_res,
                "pt": pt_res,
                "counts": db::summary_counts(conn),
            }))
        }

        "scan_library" => {
            let jobs = library::get_scan_jobs(conn);
            let has_active_scan = jobs.values().any(|j| j.mode == "active");
            let has_approved: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM lib_decisions WHERE status = 'suggestion' AND state = 'approved')",
                [],
                |r| r.get(0),
            )?;
            if !has_active_scan && (!has_approved || settings.dry_run) {
                return Ok(json!({ "idle": true, "scanned": 0, "cleaned": 0 }));
            }

            let host = HostClient::from_env()?;
            let _ = host.sync_unified_rules(conn);
            let cfg = db::load_config(conn)?;

            let mut cleanup_summary = json!({});
            if has_approved && !settings.dry_run {
                cleanup_summary = library::process_approved_cleanups(conn, &host, &cfg, settings, 20)?;
            }

            let mut scan_results = serde_json::Map::new();
            for scope in ["movie", "tv"] {
                if jobs.get(scope).is_some_and(|j| j.mode == "active") {
                    let updated = library::scan_step(conn, &host, &cfg, scope, 24)?;
                    scan_results.insert(scope.to_string(), serde_json::to_value(updated)?);
                }
            }
            Ok(json!({
                "idle": false,
                "cleanups": cleanup_summary,
                "scans": scan_results,
            }))
        }

        "sync_rules" | "ui_sync_rules" => {
            let res = rules::sync_subscriptions(conn)?;
            let cfg = db::load_config(conn)?;
            Ok(json!({
                "sync": res,
                "bootstrap": build_bootstrap_payload(conn, &cfg, settings, None, None),
            }))
        }

        // === Strategy & Category Overrides ===
        "ui_save_strategy" => {
            let strategy: UnifiedStrategy =
                serde_json::from_value(input["strategy"].clone()).context("策略格式无效")?;
            ensure!(
                (1..=27).contains(&strategy.keep_per_slot),
                "每槽保留版本数须在 1~27 之间"
            );
            let cfg = db::edit_config(conn, true, |c| {
                ensure!(
                    c.rules.iter().any(|r| r.id == strategy.movie_rule_id),
                    "所选电影默认评分规则不存在"
                );
                ensure!(
                    c.rules.iter().any(|r| r.id == strategy.tv_rule_id),
                    "所选剧集默认评分规则不存在"
                );
                c.strategy = strategy;
                Ok(true)
            })?;
            Ok(build_bootstrap_payload(conn, &cfg, settings, None, None))
        }

        // === 115 Staging Pipelines & Directory Browser ===
        "ui_browse_115" => {
            let cid = input["cid"].as_str().unwrap_or("0").trim();
            db::validate_cid(cid)?;
            let offset = input["offset"].as_u64().unwrap_or(0) as usize;
            let show_files = input["show_files"].as_bool().unwrap_or(true);
            let host = HostClient::from_env()?;
            let page = host.open115_list_page(cid, offset, 80, true)?;
            let mut dirs = Vec::new();
            let mut files = Vec::new();
            for item in page.items {
                if item.is_dir {
                    dirs.push(json!({
                        "file_id": item.file_id,
                        "name": item.name,
                        "updated_at": item.updated_at,
                    }));
                } else if show_files && staging::is_video_name(&item.name) {
                    files.push(json!({
                        "file_id": item.file_id,
                        "parent_id": item.parent_id,
                        "name": item.name,
                        "size": item.size,
                        "sha1": item.sha1,
                        "pick_code": item.pick_code,
                    }));
                }
            }
            Ok(json!({
                "cid": page.cid,
                "offset": page.offset,
                "count": page.count,
                "auth_source": page.auth_source,
                "directories": dirs,
                "files": files,
            }))
        }

        "ui_probe_115_file" => {
            let file: Cloud115File =
                serde_json::from_value(input["file"].clone()).context("缺少 115 文件信息")?;
            let force_refresh = input["force_refresh"].as_bool().unwrap_or(false);
            let host = HostClient::from_env()?;
            let cfg = db::load_config(conn)?;
            let info = host.open115_probe_media_info(&file, true, true, force_refresh, 20_000)?;
            let refs: Vec<&MediaStream> = info.streams.iter().collect();
            let prof = MediaProfile::extract(
                &LibrarySource {
                    name: file.name.clone(),
                    size: file.size as f64,
                    bitrate: info.bitrate,
                    ..Default::default()
                },
                &refs,
            );
            let mut ref_base = ReferenceBaseline::default();
            ref_base.observe(&prof);
            let rule_id = input["rule_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(&cfg.strategy.movie_rule_id);
            let rule = rules::find_rule(&cfg, rule_id)?;
            let scorer = Scorer::new(rule)?;
            let score = scorer.score(&prof, &ref_base, "", false);
            Ok(json!({
                "status": info.status,
                "cached": info.cached,
                "profile": prof.summary_json(),
                "rule_name": rule.name,
                "score": score,
            }))
        }

        "ui_save_pipeline" => {
            let mut pipe: StagingPipeline =
                serde_json::from_value(input["pipeline"].clone()).context("管道配置格式无效")?;
            staging::validate_pipeline(&pipe)?;
            let mut cfg = db::edit_config(conn, true, |c| {
                if pipe.id > 0 {
                    if let Some(existing) = c.pipelines.iter_mut().find(|p| p.id == pipe.id) {
                        *existing = pipe.clone();
                        return Ok(true);
                    }
                }
                let next_id = c.pipelines.iter().map(|p| p.id).max().unwrap_or(0) + 1;
                pipe.id = next_id;
                c.pipelines.push(pipe.clone());
                Ok(true)
            })?;
            let mut scan_result = None;
            if input["run_scan"].as_bool() == Some(true) {
                if let Ok(host) = HostClient::from_env() {
                    let _ = host.sync_unified_rules_if_stale(conn, 300);
                    cfg = db::load_config(conn)?;
                    let _ = conn.execute(
                        "DELETE FROM cursors WHERE key = ?",
                        [format!("staging_walk:{}", pipe.id)],
                    );
                    scan_result = Some(staging::scan_pipeline_ui(conn, &host, &cfg, settings, pipe.id, 10)?);
                }
            }
            let mut boot = build_bootstrap_payload(conn, &cfg, settings, None, None);
            if let Some(sr) = scan_result {
                boot["scan"] = sr;
            }
            Ok(boot)
        }

        "ui_delete_pipeline" => {
            let pid = input["id"].as_i64().context("缺少管道 ID")?;
            let cfg = db::edit_config(conn, true, |c| {
                let before = c.pipelines.len();
                c.pipelines.retain(|p| p.id != pid);
                ensure!(c.pipelines.len() < before, "找不到要删除的管道");
                Ok(true)
            })?;
            conn.execute("DELETE FROM staging_items WHERE pipeline_id = ?", [pid])?;
            conn.execute(
                "DELETE FROM cursors WHERE key = ?",
                [format!("staging_walk:{pid}")],
            )?;
            Ok(build_bootstrap_payload(conn, &cfg, settings, None, None))
        }

        "ui_scan_pipeline" => {
            let pid = input["id"].as_i64().context("请选择要扫描的 115 管道")?;
            if input["reset_cursor"].as_bool() == Some(true) {
                conn.execute(
                    "DELETE FROM cursors WHERE key = ?",
                    [format!("staging_walk:{pid}")],
                )?;
            }
            let host = HostClient::from_env()?;
            let _ = host.sync_unified_rules_if_stale(conn, 300);
            let cfg = db::load_config(conn)?;
            let res = staging::scan_pipeline_ui(conn, &host, &cfg, settings, pid, 10)?;
            Ok(json!({
                "scan": res,
                "items": staging::list_staging_items(conn, Some(pid), "pending", "all", 150)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_staging_list" => {
            let pid = input["pipeline_id"].as_i64();
            let state = input["state"].as_str().unwrap_or("pending");
            let verdict = input["verdict"].as_str().unwrap_or("all");
            Ok(json!({
                "items": staging::list_staging_items(conn, pid, state, verdict, 200)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_staging_act" => {
            let ids: Vec<i64> = input["ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .collect();
            let override_route = input["route"].as_str().filter(|s| !s.is_empty());
            let host = HostClient::from_env()?;
            let cfg = db::load_config(conn)?;
            let res = staging::execute_staging_items_ui(conn, &host, &cfg, settings, &ids, override_route)?;
            Ok(json!({
                "result": res,
                "items": staging::list_staging_items(conn, input["pipeline_id"].as_i64(), "pending", "all", 200)?,
                "ledger": staging::list_ledger(conn, "active", 100)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_search_tmdb" => {
            let query = input["query"].as_str().unwrap_or_default().trim();
            ensure!(!query.is_empty(), "请输入要搜索的片名或 TMDB ID");
            let mt = input["media_type"].as_str().unwrap_or("all");
            let host = HostClient::from_env()?;
            if query.bytes().all(|b| b.is_ascii_digit()) {
                let mut hits = Vec::new();
                for check_mt in if mt == "movie" {
                    vec!["movie"]
                } else if mt == "tv" {
                    vec!["tv"]
                } else {
                    vec!["movie", "tv"]
                } {
                    if let Ok(det) = host.get_tmdb_details(query, check_mt) {
                        let title = det["title"]
                            .as_str()
                            .or_else(|| det["name"].as_str())
                            .unwrap_or_default();
                        if !title.is_empty() {
                            hits.push(json!({
                                "tmdb_id": query,
                                "media_type": check_mt,
                                "title": title,
                                "year": det["release_date"].as_str().or_else(|| det["first_air_date"].as_str()).unwrap_or_default().chars().take(4).collect::<String>(),
                                "overview": det["overview"].as_str().unwrap_or_default(),
                            }));
                        }
                    }
                }
                return Ok(json!({ "items": hits }));
            }
            let raw_hits = host.search_tmdb(query, mt)?;
            let formatted: Vec<Value> = raw_hits
                .into_iter()
                .take(15)
                .map(|h| {
                    let eff_mt = if !h.media_type.is_empty() {
                        h.media_type.clone()
                    } else if !h.first_air_date.is_empty() && h.release_date.is_empty() {
                        "tv".to_string()
                    } else {
                        "movie".to_string()
                    };
                    json!({
                        "tmdb_id": h.effective_id(),
                        "media_type": eff_mt,
                        "title": h.effective_title(),
                        "year": h.effective_year(),
                        "overview": h.overview,
                    })
                })
                .collect();
            Ok(json!({ "items": formatted }))
        }

        "ui_staging_bind_tmdb" => {
            let id = input["id"].as_i64().context("缺少中转条目 ID")?;
            let mt = input["media_type"].as_str().unwrap_or("movie");
            let tmdb_id = input["tmdb_id"].as_str().unwrap_or_default();
            let title = input["title"].as_str().unwrap_or_default();
            let season = input["season"].as_u64().filter(|&n| n > 0).map(|n| n as u32);
            let episode = input["episode"].as_u64().filter(|&n| n > 0).map(|n| n as u32);
            let remember = input["remember"].as_bool().unwrap_or(true);
            let host = HostClient::from_env()?;
            let res = staging::bind_staging_tmdb(
                conn, host.borrow_ref(), id, mt, tmdb_id, title, season, episode, remember,
            )?;
            Ok(json!({
                "bound": res,
                "items": staging::list_staging_items(conn, input["pipeline_id"].as_i64(), "pending", "all", 200)?,
                "counts": db::summary_counts(conn),
            }))
        }

        // === Post-Arrival Wash Ledger ===
        "ui_ledger_list" => {
            let state = input["state"].as_str().unwrap_or("active");
            Ok(json!({
                "items": staging::list_ledger(conn, state, 150)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_ledger_reconcile" => {
            let host = HostClient::from_env()?;
            let cfg = db::load_config(conn)?;
            let res = staging::reconcile_ledger(conn, &host, &cfg, settings)?;
            Ok(json!({
                "reconcile": res,
                "items": staging::list_ledger(conn, input["state"].as_str().unwrap_or("active"), 150)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_ledger_act" => {
            let id = input["id"].as_i64().context("缺少账本记录 ID")?;
            let op = input["operation"].as_str().unwrap_or_default();
            let host = HostClient::from_env()?;
            let cfg = db::load_config(conn)?;
            let res = staging::execute_ledger_action(conn, &host, &cfg, settings, id, op)?;
            Ok(json!({
                "result": res,
                "items": staging::list_ledger(conn, input["state"].as_str().unwrap_or("active"), 150)?,
                "counts": db::summary_counts(conn),
            }))
        }

        // === Library Multi-Version Dedup ===
        "ui_lib_control" => {
            let scope = input["scope"].as_str().unwrap_or("movie");
            let op = input["operation"].as_str().unwrap_or("start");
            let host = HostClient::from_env()?;
            let _ = host.sync_unified_rules_if_stale(conn, 300);
            let cfg = db::load_config(conn)?;
            let mut job = library::control_scan(conn, &cfg, scope, op)?;
            if matches!(op, "start" | "restart" | "resume") {
                job = library::scan_step(conn, &host, &cfg, scope, 12)?;
            }
            Ok(json!({
                "job": job,
                "scan_jobs": library::get_scan_jobs(conn),
                "page": library::list_decisions(conn, scope, "suggestion", "pending", "", 0, 30)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_lib_step" => {
            let scope = input["scope"].as_str().unwrap_or("movie");
            let host = HostClient::from_env()?;
            let cfg = db::load_config(conn)?;
            let job = library::scan_step(conn, &host, &cfg, scope, 15)?;
            Ok(json!({
                "job": job,
                "scan_jobs": library::get_scan_jobs(conn),
                "page": library::list_decisions(conn, scope, "suggestion", "pending", "", 0, 30)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_lib_decisions" => {
            let scope = input["scope"].as_str().unwrap_or("all");
            let status = input["status"].as_str().unwrap_or("suggestion");
            let state = input["state"].as_str().unwrap_or("pending");
            let cat = input["category"].as_str().unwrap_or_default();
            let after_id = input["after_id"].as_i64().unwrap_or(0);
            Ok(json!({
                "scan_jobs": library::get_scan_jobs(conn),
                "page": library::list_decisions(conn, scope, status, state, cat, after_id, 30)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_lib_act" => {
            let ids: Vec<i64> = input["ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .collect();
            let all_scope = input["all_scope"].as_str().filter(|s| !s.is_empty());
            let cat = input["category"].as_str().unwrap_or_default();
            let op = input["operation"].as_str().unwrap_or("execute");
            let host = HostClient::from_env()?;
            let cfg = db::load_config(conn)?;
            let res = library::act_decisions(conn, &host, &cfg, settings, &ids, all_scope, cat, op)?;
            let list_scope = input["scope"].as_str().unwrap_or("all");
            Ok(json!({
                "result": res,
                "scan_jobs": library::get_scan_jobs(conn),
                "page": library::list_decisions(conn, list_scope, "suggestion", "pending", cat, 0, 30)?,
                "counts": db::summary_counts(conn),
            }))
        }

        // === PT Active Wash ===
        "ui_save_pt_config" => {
            let pt_cfg: PtWashConfig =
                serde_json::from_value(input["pt_config"].clone()).context("PT 配置格式无效")?;
            let cfg = db::edit_config(conn, true, |c| {
                c.pt_config = pt_cfg;
                Ok(true)
            })?;
            Ok(build_bootstrap_payload(conn, &cfg, settings, None, None))
        }

        "ui_pt_lookup" => {
            let kw = input["keyword"].as_str().unwrap_or_default();
            let mt = input["media_type"].as_str().unwrap_or("all");
            let hdr = input["hdr_format"].as_str().unwrap_or_default();
            let sk = input["source_kind"].as_str().unwrap_or_default();
            let host = HostClient::from_env()?;
            let cfg = db::load_config(conn)?;
            pt::lookup_library_items(&host, &cfg, kw, mt, hdr, sk)
        }

        "ui_pt_search" => {
            let item_id = input["item_id"].as_str().unwrap_or_default().trim();
            ensure!(!item_id.is_empty(), "请选择要搜种洗版的媒体库条目");
            let custom_kw = input["keyword"].as_str().unwrap_or_default();
            let host = HostClient::from_env()?;
            let cfg = db::load_config(conn)?;
            let res = pt::search_pt_for_item(conn, &host, &cfg, settings, item_id, custom_kw, false)?;
            Ok(json!({
                "search": res,
                "candidates": pt::list_pt_candidates(conn, "pending", 100)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_pt_candidates" => {
            let state = input["state"].as_str().unwrap_or("pending");
            Ok(json!({
                "candidates": pt::list_pt_candidates(conn, state, 120)?,
                "counts": db::summary_counts(conn),
            }))
        }

        "ui_pt_act" => {
            let id = input["id"].as_i64().context("缺少 PT 候选 ID")?;
            let op = input["operation"].as_str().unwrap_or("download");
            let host = HostClient::from_env()?;
            let cfg = db::load_config(conn)?;
            let res = pt::act_pt_candidate(conn, &host, &cfg, settings, id, op)?;
            Ok(json!({
                "result": res,
                "candidates": pt::list_pt_candidates(conn, "pending", 120)?,
                "ledger": staging::list_ledger(conn, "active", 100)?,
                "counts": db::summary_counts(conn),
            }))
        }

        // === Dual-Engine Rules, Subscriptions, Overrides & Sample Tester ===
        "ui_rule_detail" => {
            let cfg = db::load_config(conn)?;
            let id = input["id"].as_str().unwrap_or_default();
            let rule = rules::find_rule(&cfg, id)?;
            Ok(serde_json::to_value(rule)?)
        }

        "ui_save_rule" => {
            let id_in = input["id"].as_str().unwrap_or_default().trim().to_string();
            let save_as_new = input["save_as_new"].as_bool().unwrap_or(false);
            let kind = input["kind"].as_str().unwrap_or(rules::SYMEDIA).to_string();
            let name = input["name"].as_str().unwrap_or_default().trim().to_string();
            ensure!(!name.is_empty() && name.len() <= 200, "请填写评分规则名称");

            let mut edited_rule = if let Some(raw_str) = input["raw"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
            {
                let mut list = rules::parse_rules(&kind, raw_str, "custom")?;
                let mut r = list.remove(0);
                r.name = name.clone();
                rules::rebuild_rule_raw(&mut r)?;
                r
            } else {
                let body = input["body"].clone();
                ensure!(body.is_object(), "缺少规则结构化 body 或 raw 原文");
                let mut r = ScoringRule {
                    id: String::new(),
                    name: name.clone(),
                    kind: kind.clone(),
                    builtin: false,
                    source: "custom".to_string(),
                    body,
                    raw: String::new(),
                };
                rules::rebuild_rule_raw(&mut r)?;
                r
            };

            let mut final_id = String::new();
            let cfg = db::edit_config(conn, true, |c| {
                if !save_as_new && !id_in.is_empty() {
                    if let Some(existing) = c.rules.iter_mut().find(|r| r.id == id_in) {
                        edited_rule.id = existing.id.clone();
                        edited_rule.builtin = existing.builtin;
                        edited_rule.source = existing.source.clone();
                        *existing = edited_rule.clone();
                        final_id = existing.id.clone();
                        return Ok(true);
                    }
                }
                let new_id = format!("custom:{}:{kind}", c.revision + 1);
                edited_rule.id = new_id.clone();
                edited_rule.builtin = false;
                edited_rule.source = "custom".to_string();
                c.rules.push(edited_rule.clone());
                final_id = new_id;
                Ok(true)
            })?;
            let saved = rules::find_rule(&cfg, &final_id)?.clone();
            Ok(json!({
                "saved_rule": saved,
                "bootstrap": build_bootstrap_payload(conn, &cfg, settings, None, None),
            }))
        }

        "ui_import_rule" => {
            let kind = input["kind"].as_str().unwrap_or_default().trim();
            let content = input["content"].as_str().unwrap_or_default();
            let parsed = rules::parse_rules(kind, content, "import")?;
            let mut last_id = String::new();
            let cfg = db::edit_config(conn, true, |c| {
                for (i, mut r) in parsed.into_iter().enumerate() {
                    r.id = format!("import:{}:{}:{}", c.revision + 1, i + 1, r.id);
                    last_id = r.id.clone();
                    c.rules.push(r);
                }
                Ok(true)
            })?;
            Ok(json!({
                "imported_id": last_id,
                "bootstrap": build_bootstrap_payload(conn, &cfg, settings, None, None),
            }))
        }

        "ui_delete_rule" => {
            let id = input["id"].as_str().unwrap_or_default().trim();
            let cfg = db::edit_config(conn, true, |c| {
                let target = c
                    .rules
                    .iter()
                    .find(|r| r.id == id)
                    .context("找不到该评分规则")?;
                ensure!(!target.builtin, "内置预设规则不可删除（可直接编辑或另存副本）");
                c.rules.retain(|r| r.id != id);
                let fallback_tidy = c
                    .rules
                    .iter()
                    .find(|r| r.kind == rules::MEDIATIDY)
                    .or_else(|| c.rules.first())
                    .map(|r| r.id.clone())
                    .unwrap_or_default();
                let fallback_sym = c
                    .rules
                    .iter()
                    .find(|r| r.kind == rules::SYMEDIA)
                    .or_else(|| c.rules.first())
                    .map(|r| r.id.clone())
                    .unwrap_or_default();
                if c.strategy.movie_rule_id == id {
                    c.strategy.movie_rule_id = fallback_tidy.clone();
                }
                if c.strategy.tv_rule_id == id {
                    c.strategy.tv_rule_id = fallback_tidy;
                }
                if c.pt_config.movie_rule_id == id {
                    c.pt_config.movie_rule_id = fallback_sym.clone();
                }
                if c.pt_config.tv_rule_id == id {
                    c.pt_config.tv_rule_id = fallback_sym;
                }
                Ok(true)
            })?;
            Ok(build_bootstrap_payload(conn, &cfg, settings, None, None))
        }

        "ui_reset_builtin_rules" => {
            let presets = rules::default_rules()?;
            let cfg = db::edit_config(conn, true, |c| {
                for p in presets {
                    if let Some(ex) = c.rules.iter_mut().find(|r| r.id == p.id) {
                        *ex = p;
                    } else {
                        c.rules.insert(0, p);
                    }
                }
                Ok(true)
            })?;
            Ok(build_bootstrap_payload(conn, &cfg, settings, None, None))
        }

        "ui_export_rule" => {
            let cfg = db::load_config(conn)?;
            let id = input["id"].as_str().unwrap_or_default();
            let rule = rules::find_rule(&cfg, id)?;
            let ext = if rule.kind == rules::SYMEDIA { "json" } else { "yaml" };
            Ok(json!({
                "filename": format!("{}.{}", rule.name, ext),
                "content": rule.raw,
            }))
        }

        "ui_save_subscription" => {
            let sub_id = input["id"].as_str().unwrap_or_default().trim();
            let url = input["url"].as_str().unwrap_or_default().trim().to_string();
            let enabled = input["enabled"].as_bool().unwrap_or(true);
            let parsed_url = url::Url::parse(&url).context("请输入合法的 HTTPS 订阅地址")?;
            ensure!(parsed_url.scheme() == "https", "远程订阅地址必须使用 HTTPS");
            let cfg = db::edit_config(conn, false, |c| {
                let sub = c.subscriptions.get_mut(sub_id).context("订阅项不存在")?;
                sub.url = url;
                sub.enabled = enabled;
                sub.error.clear();
                Ok(false)
            })?;
            Ok(build_bootstrap_payload(conn, &cfg, settings, None, None))
        }

        "ui_get_aux_yaml" => {
            let cfg = db::load_config(conn)?;
            let sub_cache = |k: &str| {
                cfg.subscriptions
                    .get(k)
                    .map(|s| s.cache.as_str())
                    .unwrap_or_default()
            };
            Ok(json!({
                "local_matching_yaml": cfg.local_matching_yaml,
                "local_tmdb_yaml": cfg.local_tmdb_yaml,
                "preset_matching_stats": rules::inspect_aux_yaml("matching", identity::PRESET_MATCHING_YAML).unwrap_or_default(),
                "preset_tmdb_stats": rules::inspect_aux_yaml("tmdb", identity::PRESET_TMDB_YAML).unwrap_or_default(),
                "local_matching_stats": rules::inspect_aux_yaml("matching", &cfg.local_matching_yaml).unwrap_or_default(),
                "local_tmdb_stats": rules::inspect_aux_yaml("tmdb", &cfg.local_tmdb_yaml).unwrap_or_default(),
                "remote_matching_stats": rules::inspect_aux_yaml("matching", sub_cache("matching")).unwrap_or_default(),
                "remote_tmdb_stats": rules::inspect_aux_yaml("tmdb", sub_cache("tmdb")).unwrap_or_default(),
                "manual_bindings": cfg.manual_bindings.values().collect::<Vec<_>>(),
            }))
        }

        "ui_save_aux_yaml" => {
            let kind = input["kind"].as_str().unwrap_or_default();
            let content = input["content"].as_str().unwrap_or_default().to_string();
            let diag = if content.trim().is_empty() {
                json!({ "counts": {}, "warnings": [] })
            } else {
                rules::inspect_aux_yaml(kind, &content)?
            };
            let cfg = db::edit_config(conn, true, |c| {
                match kind {
                    "matching" => c.local_matching_yaml = content,
                    "tmdb" => c.local_tmdb_yaml = content,
                    _ => bail!("不支持的覆盖规则类型: {kind}"),
                }
                Ok(true)
            })?;
            Ok(json!({
                "diagnostics": diag,
                "bootstrap": build_bootstrap_payload(conn, &cfg, settings, None, None),
            }))
        }

        "ui_delete_manual_binding" => {
            let key = input["pattern_key"].as_str().unwrap_or_default().to_string();
            db::edit_config(conn, false, |c| {
                c.manual_bindings.remove(&key);
                Ok(false)
            })?;
            let cfg = db::load_config(conn)?;
            Ok(json!({
                "manual_bindings": cfg.manual_bindings.values().collect::<Vec<_>>(),
            }))
        }

        "ui_compare_samples" => {
            let cfg = db::load_config(conn)?;
            let rule_id = input["rule_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(&cfg.strategy.movie_rule_id);
            let category = input["category"].as_str().unwrap_or_default();
            let rule = rules::find_rule(&cfg, rule_id)?;
            let scorer = Scorer::new(rule)?;
            let recognition = RecognitionEngine::new(&cfg);
            let iso_prot = cfg
                .strategy
                .effective_for("movie", category, &cfg.host_sync.categories, &cfg.rules)
                .iso_protected;

            let build_sample = |v: &Value| -> Result<(MediaProfile, Value)> {
                let name = v["name"].as_str().unwrap_or_default().trim();
                ensure!(!name.is_empty() && name.len() <= 600, "请输入对比样本文件名");
                let folder = v["folder"].as_str().unwrap_or_default().trim();
                let size_gb = v["size_gb"].as_f64().unwrap_or(0.0).max(0.0);
                let br_mbps = v["bitrate_mbps"].as_f64().unwrap_or(0.0).max(0.0);
                let vbr_mbps = v["video_bitrate_mbps"].as_f64().unwrap_or(0.0).max(0.0);
                let mut prof = MediaProfile::extract(
                    &LibrarySource {
                        name: name.to_string(),
                        size: size_gb * 1_000_000_000.0,
                        bitrate: br_mbps * 1_000_000.0,
                        ..Default::default()
                    },
                    &[],
                );
                if vbr_mbps > 0.0 {
                    prof.video_bitrate = vbr_mbps * 1_000_000.0;
                }
                let (parsed, bound) = recognition.preprocess(name, folder, "auto");
                let mapped = match (&bound, parsed.season, parsed.episode) {
                    (Some((tid, _, _)), Some(s), Some(e)) => {
                        let (mtid, ms, me, did) = recognition.apply_mp_mapping(*tid, s, e);
                        did.then(|| json!({"tmdb_id": mtid, "season": ms, "episode": me}))
                    }
                    _ => None,
                };
                Ok((
                    prof,
                    json!({
                        "parsed": parsed,
                        "bound": bound.map(|(tid, mt, src)| json!({"tmdb_id": tid, "media_type": mt, "source": src})),
                        "mp_mapped": mapped,
                    }),
                ))
            };

            let (left_p, left_rec) = build_sample(&input["left"])?;
            let (right_p, right_rec) = build_sample(&input["right"])?;

            let mut shared_ref = ReferenceBaseline::default();
            shared_ref.observe(&left_p);
            shared_ref.observe(&right_p);

            let left_score = scorer.score(
                &left_p,
                &ReferenceBaseline {
                    size: if rule.kind == rules::SYMEDIA {
                        right_p.size
                    } else {
                        shared_ref.size
                    },
                    ..shared_ref.clone()
                },
                category,
                iso_prot,
            );
            let right_score = scorer.score(
                &right_p,
                &ReferenceBaseline {
                    size: if rule.kind == rules::SYMEDIA {
                        left_p.size
                    } else {
                        shared_ref.size
                    },
                    ..shared_ref
                },
                category,
                iso_prot,
            );

            let min_diff = if rule.kind == rules::SYMEDIA {
                cfg.strategy.min_diff_100
            } else {
                cfg.strategy.min_diff_1000
            };
            let delta = rules::round2(right_score.total - left_score.total);
            let verdict = if right_score.excluded {
                "候选 B 命中排除条件，不予采用".to_string()
            } else if left_score.slot != right_score.slot {
                format!(
                    "跨版本槽位共存：现有 [{}] ({:.2}) 与 候选 [{}] ({:.2}) 分属不同槽位，同时保留",
                    left_score.slot, left_score.total, right_score.slot, right_score.total
                )
            } else if delta >= min_diff {
                format!(
                    "同槽位 [{}] 升级胜出：候选 B 比现有 A 高 +{:.2} 分（≥ 最低分差 {:.2}）",
                    right_score.slot, delta, min_diff
                )
            } else {
                format!(
                    "保留现有版本 A：候选 B 差值 {:+.2} 分（未达同槽位最低分差 {:.2}）",
                    delta, min_diff
                )
            };

            Ok(json!({
                "rule_name": rule.name,
                "rule_kind": rule.kind,
                "min_diff": min_diff,
                "delta": delta,
                "verdict": verdict,
                "left": {
                    "profile": left_p.summary_json(),
                    "recognition": left_rec,
                    "score": left_score,
                },
                "right": {
                    "profile": right_p.summary_json(),
                    "recognition": right_rec,
                    "score": right_score,
                },
            }))
        }

        // === History & Task Logs (Separated Media vs Task + Filter + Delete) ===
        "ui_history_list" => {
            let filter = db::ActivityFilter {
                record_type: input["record_type"].as_str().unwrap_or("media").to_string(),
                kind: input["kind"].as_str().unwrap_or("all").to_string(),
                action_group: input["action_group"].as_str().unwrap_or("all").to_string(),
                status: input["status"].as_str().unwrap_or("all").to_string(),
                keyword: input["keyword"].as_str().unwrap_or_default().to_string(),
                limit: input["limit"].as_u64().unwrap_or(200) as usize,
            };
            if filter.record_type != "task" {
                if let Ok(cfg) = db::load_config(conn) {
                    let host_opt = HostClient::from_env().ok();
                    let _ = staging::backfill_recent_media_history(
                        conn,
                        host_opt.as_ref(),
                        &cfg,
                        25,
                    );
                }
            }
            let mut res = db::query_activities(conn, &filter)?;
            res["counts"] = db::summary_counts(conn);
            Ok(res)
        }

        "ui_history_delete" => {
            let ids: Vec<i64> = input["ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .collect();
            let clear_filtered = input["clear_filtered"].as_bool() == Some(true);
            let filter = db::ActivityFilter {
                record_type: input["record_type"].as_str().unwrap_or("media").to_string(),
                kind: input["kind"].as_str().unwrap_or("all").to_string(),
                action_group: input["action_group"].as_str().unwrap_or("all").to_string(),
                status: input["status"].as_str().unwrap_or("all").to_string(),
                keyword: input["keyword"].as_str().unwrap_or_default().to_string(),
                limit: input["limit"].as_u64().unwrap_or(200) as usize,
            };
            ensure!(
                !ids.is_empty() || clear_filtered,
                "请先勾选要删除的历史记录，或确认清空当前筛选"
            );
            let deleted = db::delete_activities(
                conn,
                &ids,
                if clear_filtered { Some(&filter) } else { None },
            )?;
            let mut res = db::query_activities(conn, &filter)?;
            res["deleted"] = json!(deleted);
            res["counts"] = db::summary_counts(conn);
            Ok(res)
        }

        "ui_history_retry" => {
            let ids: Vec<i64> = input["ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .collect();
            let filter = db::ActivityFilter {
                record_type: input["record_type"].as_str().unwrap_or("media").to_string(),
                kind: input["kind"].as_str().unwrap_or("all").to_string(),
                action_group: input["action_group"].as_str().unwrap_or("all").to_string(),
                status: input["status"].as_str().unwrap_or("all").to_string(),
                keyword: input["keyword"].as_str().unwrap_or_default().to_string(),
                limit: input["limit"].as_u64().unwrap_or(200) as usize,
            };
            let host = HostClient::from_env()?;
            let _ = host.sync_unified_rules_if_stale(conn, 180);
            let cfg = db::load_config(conn)?;
            let retry_res = staging::retry_history_items(conn, &host, &cfg, settings, &ids)?;
            let mut res = db::query_activities(conn, &filter)?;
            res["retry"] = retry_res;
            res["counts"] = db::summary_counts(conn);
            Ok(res)
        }

        _ => bail!("MediWash 未知插件动作: {action}"),
    }
}

trait BorrowHost {
    fn borrow_ref(&self) -> &HostClient;
}
impl BorrowHost for HostClient {
    fn borrow_ref(&self) -> &HostClient {
        self
    }
}

fn build_bootstrap_payload(
    conn: &rusqlite::Connection,
    cfg: &AppConfig,
    settings: &PluginSettings,
    host_status: Option<host::HostStatus>,
    pt_sites: Option<Vec<host::PtSite>>,
) -> Value {
    let rules_summary: Vec<Value> = cfg
        .rules
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "name": r.name,
                "kind": r.kind,
                "builtin": r.builtin,
                "source": r.source,
            })
        })
        .collect();

    let subs_summary: Vec<Value> = cfg
        .subscriptions
        .values()
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.name,
                "kind": s.kind,
                "url": s.url,
                "enabled": s.enabled,
                "cached": !s.cache.is_empty(),
                "last_sync": models::format_ts_cn(s.last_sync),
                "error": s.error,
            })
        })
        .collect();

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "revision": cfg.revision,
        "settings": settings,
        "host_status": host_status,
        "host_sync": {
            "category_count": cfg.host_sync.categories.len(),
            "categories": cfg.host_sync.categories,
            "category_source": cfg.host_sync.category_source,
            "movie_rename_format": cfg.host_sync.movie_rename_format,
            "tv_rename_format": cfg.host_sync.tv_rename_format,
            "filter_keyword_count": cfg.host_sync.filter_keywords.len(),
            "open115_target_folder": cfg.host_sync.open115_target_folder,
            "open115_target_cid": cfg.host_sync.open115_target_cid,
            "open115_offline_folder": cfg.host_sync.open115_offline_folder,
            "open115_offline_cid": cfg.host_sync.open115_offline_cid,
            "transfer_source_folder": cfg.host_sync.transfer_source_folder,
            "transfer_source_cid": cfg.host_sync.transfer_source_cid,
            "transfer_target_folder": cfg.host_sync.transfer_target_folder,
            "transfer_target_cid": cfg.host_sync.transfer_target_cid,
            "last_synced": models::format_ts_cn(cfg.host_sync.last_synced),
        },
        "counts": db::summary_counts(conn),
        "strategy": cfg.strategy,
        "pipelines": cfg.pipelines,
        "pt_config": cfg.pt_config,
        "pt_sites": pt_sites.unwrap_or_default(),
        "rules": rules_summary,
        "subscriptions": subs_summary,
        "scan_jobs": library::get_scan_jobs(conn),
        "activities": db::list_activities(conn, 25),
    })
}

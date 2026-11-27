use crate::category;
use crate::db;
use crate::host::{HostClient, LibraryItem, LibrarySource, MediaStream};
use crate::identity::{RecognitionEngine, ResolveOutcome};
use crate::models::{format_ts_cn, now_ts, AppConfig, PluginSettings};
use crate::rules::{self, MediaProfile, ReferenceBaseline, Scorer};
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Searches library items by keyword, TMDB ID, or Mediary 3.4.6+ diagnostic filters (`hdr_format` / `source_kind`)
/// and returns their live Symedia/MediaTidy score vs graduation threshold.
pub fn lookup_library_items(
    host: &HostClient,
    cfg: &AppConfig,
    keyword: &str,
    media_type: &str,
    hdr_format: &str,
    source_kind: &str,
) -> Result<Value> {
    let kw = keyword.trim();
    let hdr = hdr_format.trim();
    let sk = source_kind.trim();
    let has_diag = (!hdr.is_empty() && hdr != "all") || (!sk.is_empty() && sk != "all");
    ensure!(
        (!kw.is_empty() && kw.len() <= 200) || has_diag,
        "请输入片名/TMDB ID，或选择一项专项片源筛选"
    );

    let items = if kw.is_empty() && has_diag {
        host.library_page_filtered(0, 25, media_type, "", false, hdr, sk)?
            .items
    } else if kw.bytes().all(|b| b.is_ascii_digit()) {
        let mt = if media_type == "tv" { "tv" } else { "movie" };
        host.library_items_by_tmdb(kw, mt, None)?.items
    } else {
        let mut hits = host.library_items_search(kw, media_type)?;
        if hits.is_empty() {
            // Fallback via TMDB resolve -> library_items_by_tmdb
            let mt = if media_type == "tv" { "tv" } else { "movie" };
            if let Ok(Some(tid)) = host.resolve_tmdb(kw, None, mt) {
                hits = host.library_items_by_tmdb(&tid, mt, None)?.items;
            }
        }
        hits
    };

    let mut rows = Vec::new();
    for entry in items.into_iter().take(20) {
        let detail = host.library_item_detail(&entry.id).unwrap_or(entry);
        let is_tv = detail.item_type.eq_ignore_ascii_case("episode") || media_type == "tv";
        let mt = if is_tv { "tv" } else { "movie" };
        let rule_id = active_pt_rule_id(cfg, mt);
        let Ok(rule) = rules::find_rule(cfg, &rule_id) else {
            continue;
        };
        let Ok(scorer) = Scorer::new(rule) else {
            continue;
        };
        let (best_score, best_slot, cat) = inspect_item_best_score(&detail, mt, cfg, &scorer);
        let has_dv_p5 = detail.streams.iter().any(|s| {
            s.stream_type.eq_ignore_ascii_case("video") && {
                let h = s.hdr_format.to_ascii_lowercase();
                h.contains("profile 5") || h == "dv_p5" || h == "dovi_p5"
            }
        });
        let (grad, _) = effective_pt_thresholds(&rule.kind, cfg, is_tv);
        rows.push(json!({
            "id": detail.id,
            "name": detail.name,
            "item_type": detail.item_type,
            "tmdb_id": detail.effective_tmdb(),
            "season": detail.effective_season(),
            "episode": detail.effective_episode(),
            "category": cat,
            "slot": best_slot,
            "dv_p5": has_dv_p5,
            "current_score": best_score,
            "graduate_score": grad,
            "graduated": best_score >= grad,
            "source_count": detail.effective_sources().len(),
        }));
    }
    Ok(json!({ "items": rows }))
}

fn active_pt_rule_id(cfg: &AppConfig, media_type: &str) -> String {
    let configured = if media_type == "tv" {
        &cfg.pt_config.tv_rule_id
    } else {
        &cfg.pt_config.movie_rule_id
    };
    if !configured.is_empty() && cfg.rules.iter().any(|r| r.id == *configured) {
        return configured.clone();
    }
    cfg.rules
        .iter()
        .find(|r| r.kind == rules::SYMEDIA)
        .or_else(|| cfg.rules.first())
        .map(|r| r.id.clone())
        .unwrap_or_default()
}

/// Auto-adapts PT `graduate_score` & `min_score_diff` when the user selects a 1000-point MediaTidy rule
/// while leaving 100-point Symedia default thresholds (e.g. 75 / 65 / 3), or vice versa.
fn effective_pt_thresholds(rule_kind: &str, cfg: &AppConfig, is_tv: bool) -> (f64, f64) {
    let raw_grad = if is_tv {
        cfg.pt_config.tv_graduate
    } else {
        cfg.pt_config.movie_graduate
    };
    let raw_diff = cfg.pt_config.min_score_diff;
    if rule_kind == rules::MEDIATIDY {
        let grad = if raw_grad > 0.0 && raw_grad <= 120.0 {
            rules::round2(raw_grad * 8.0)
        } else {
            raw_grad
        };
        let diff = if raw_diff > 0.0 && raw_diff < 8.0 {
            cfg.strategy.min_diff_1000.max(15.0)
        } else {
            raw_diff
        };
        (grad, diff)
    } else {
        let grad = if raw_grad > 150.0 {
            rules::round2((raw_grad / 8.0).clamp(40.0, 98.0))
        } else {
            raw_grad
        };
        let diff = if raw_diff >= 10.0 {
            cfg.strategy.min_diff_100.max(2.0)
        } else {
            raw_diff
        };
        (grad, diff)
    }
}

fn inspect_item_best_score(
    item: &LibraryItem,
    media_type: &str,
    cfg: &AppConfig,
    scorer: &Scorer,
) -> (f64, String, String) {
    let mut best = 0.0f64;
    let mut slot = String::from("默认");
    let mut cat = String::new();
    for src in item.effective_sources() {
        let paths = [
            src.strm_path.as_str(),
            src.cloud_path.as_str(),
            src.file_path.as_str(),
            item.path.as_str(),
        ];
        if cat.is_empty() {
            cat = category::locate(&cfg.host_sync.categories, media_type, &paths).unwrap_or_default();
        }
        let refs: Vec<&MediaStream> = item
            .streams
            .iter()
            .filter(|st| !src.id.is_empty() && st.media_source_id == src.id)
            .collect();
        let prof = MediaProfile::extract(src, &refs);
        let iso_prot = cfg
            .strategy
            .effective_for(media_type, &cat, &cfg.host_sync.categories, &cfg.rules)
            .iso_protected;
        let sc = scorer.score(&prof, &ReferenceBaseline::default(), &cat, iso_prot);
        if sc.withheld_reason.is_none() && sc.total >= best {
            best = sc.total;
            slot = sc.slot;
        }
    }
    (best, slot, cat)
}

pub fn search_pt_for_item(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    item_id: &str,
    custom_keyword: &str,
    enforce_graduate: bool,
) -> Result<Value> {
    let clock = Instant::now();
    let item = host.library_item_detail(item_id)?;
    let is_tv = item.item_type.eq_ignore_ascii_case("episode");
    let media_type = if is_tv { "tv" } else { "movie" };
    let tmdb_id = item.effective_tmdb().to_string();
    ensure!(!tmdb_id.is_empty() && tmdb_id != "0", "该库内条目缺少有效的 TMDB ID");

    let rule_id = active_pt_rule_id(cfg, media_type);
    let rule = rules::find_rule(cfg, &rule_id)?;
    let scorer = Scorer::new(rule)?;

    // Build existing source profiles
    let mut existing_versions: Vec<(String, MediaProfile, String)> = Vec::new();
    let mut category = String::new();
    for src in item.effective_sources() {
        let paths = [
            src.strm_path.as_str(),
            src.cloud_path.as_str(),
            src.file_path.as_str(),
            item.path.as_str(),
        ];
        if category.is_empty() {
            category = category::locate(&cfg.host_sync.categories, media_type, &paths)
                .unwrap_or_default();
        }
        let refs: Vec<&MediaStream> = item
            .streams
            .iter()
            .filter(|st| !src.id.is_empty() && st.media_source_id == src.id)
            .collect();
        let prof = MediaProfile::extract(src, &refs);
        let init = scorer.score(&prof, &ReferenceBaseline::default(), &category, false);
        if init.withheld_reason.is_none() {
            existing_versions.push((src.id.clone(), prof, init.slot));
        }
    }

    let eff = cfg
        .strategy
        .effective_for(media_type, &category, &cfg.host_sync.categories, &cfg.rules);
    if eff.skip_pt_wash {
        return Ok(json!({
            "title": item.name,
            "category": category,
            "candidates": [],
            "notice": "该分类已配置跳过 PT 主动洗版",
        }));
    }

    let (graduate, eff_min_diff) = effective_pt_thresholds(&rule.kind, cfg, is_tv);
    let highest_current = existing_versions
        .iter()
        .map(|(_, p, _)| {
            scorer
                .score(p, &ReferenceBaseline::default(), &category, eff.iso_protected)
                .total
        })
        .fold(0.0f64, f64::max);

    if enforce_graduate && highest_current >= graduate && custom_keyword.trim().is_empty() {
        return Ok(json!({
            "title": item.name,
            "category": category,
            "current_score": highest_current,
            "graduate_score": graduate,
            "candidates": [],
            "notice": format!("现有版本最高 {:.2} 分，已达到毕业线 {:.2} 分", highest_current, graduate),
        }));
    }

    // Determine PT search keyword (for TV episodes, look up series title from TMDB details if `item.name` is episode title)
    let search_kw = if !custom_keyword.trim().is_empty() {
        custom_keyword.trim().to_string()
    } else if is_tv {
        host.get_tmdb_details(&tmdb_id, "tv")
            .ok()
            .and_then(|v| {
                v["name"]
                    .as_str()
                    .or_else(|| v["title"].as_str())
                    .map(str::to_string)
            })
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| item.name.clone())
    } else {
        item.name.clone()
    };

    let torrents = host.search_pt_torrents(
        &search_kw,
        &cfg.pt_config.site_ids,
        cfg.pt_config.free_only,
    )?;
    let recognition = RecognitionEngine::new(cfg);
    let mut matched_torrents = Vec::new();
    let mut resolve_calls = 0usize;

    for tor in &torrents {
        if resolve_calls >= 12 || clock.elapsed() >= Duration::from_secs(36) {
            break;
        }
        if !torrent_eligible(tor, cfg) {
            continue;
        }
        let t_title = tor["title"].as_str().unwrap_or_default().trim();
        let dl_url = tor["download_url"].as_str().unwrap_or_default().trim();
        if t_title.is_empty()
            || !["https://", "http://", "magnet:", "mteam://"]
                .iter()
                .any(|p| dl_url.starts_with(p))
        {
            continue;
        }
        let size = tor["size"].as_f64().unwrap_or(0.0);
        let cand_prof = MediaProfile::extract(
            &LibrarySource {
                name: t_title.to_string(),
                size,
                ..Default::default()
            },
            &[],
        );

        let max_existing_sz = existing_versions
            .iter()
            .map(|(_, p, _)| p.size)
            .fold(0.0f64, f64::max);
        let cand_score = scorer.score(
            &cand_prof,
            &ReferenceBaseline {
                size: max_existing_sz,
                ..Default::default()
            },
            &category,
            eff.iso_protected,
        );
        if cand_score.excluded || cand_score.withheld_reason.is_some() {
            continue;
        }

        let same_slot_baseline = existing_versions
            .iter()
            .filter(|(_, _, sl)| *sl == cand_score.slot)
            .map(|(_, peer_p, _)| {
                // PT torrent candidates only carry `title` + `size` (no ffprobe stream `bitrate` yet).
                // Strip ffprobe `bitrate` / `video_bitrate` on the peer copy when candidate has none
                // so MediaTidy 1000-point rules compare both sides on identical dimensions!
                let mut peer_cmp = peer_p.clone();
                if cand_prof.bitrate == 0.0 && cand_prof.video_bitrate == 0.0 {
                    peer_cmp.bitrate = 0.0;
                    peer_cmp.video_bitrate = 0.0;
                }
                let mut shared_ref = ReferenceBaseline::default();
                shared_ref.observe(&cand_prof);
                shared_ref.observe(&peer_cmp);
                let ref_for_peer = if rule.kind == rules::SYMEDIA {
                    ReferenceBaseline {
                        size: cand_prof.size,
                        ..shared_ref
                    }
                } else {
                    shared_ref
                };
                scorer
                    .score(&peer_cmp, &ref_for_peer, &category, eff.iso_protected)
                    .total
            })
            .max_by(f64::total_cmp);

        if let Some(base) = same_slot_baseline {
            if cand_score.total <= base
                || rules::round2(cand_score.total - base) < eff_min_diff
            {
                continue;
            }
        }

        // Verify torrent actually belongs to the exact same TMDB ID (+ Season/Episode)!
        resolve_calls += 1;
        let Ok(ResolveOutcome::Matched(rec)) =
            recognition.resolve(host, t_title, "", media_type)
        else {
            continue;
        };
        if rec.tmdb_id != tmdb_id {
            continue;
        }
        if is_tv {
            let want_ep = (item.effective_season(), item.effective_episode());
            let covers_ep = match (rec.episode, rec.episode_end) {
                (Some((s, e_start)), Some(e_end)) => {
                    s == want_ep.0 && want_ep.1 >= e_start && want_ep.1 <= e_end
                }
                (Some(se), None) => se == want_ep,
                _ => false,
            };
            if !covers_ep {
                continue;
            }
        }

        let old_slot_ids: Vec<String> = existing_versions
            .iter()
            .filter(|(_, _, sl)| *sl == cand_score.slot)
            .map(|(sid, _, _)| sid.clone())
            .filter(|s| !s.is_empty())
            .collect();

        let mut req_payload = json!({
            "title": t_title,
            "site_id": tor["site_id"],
            "site_name": tor["site_name"],
            "download_url": dl_url,
            "size": tor["size"],
            "media_type": media_type,
            "tmdb_id": tmdb_id.parse::<u64>().ok(),
            "force_download": true,
            "defer_organize": true,
            "organize_conflict_strategy": cfg.pt_config.conflict_strategy,
        });
        if is_tv {
            req_payload["season"] = json!(item.effective_season());
            req_payload["episode"] = json!(item.effective_episode());
            req_payload["wanted_episodes"] = json!([item.effective_episode()]);
        }

        matched_torrents.push((
            t_title.to_string(),
            cand_score,
            same_slot_baseline,
            tor.clone(),
            req_payload,
            old_slot_ids,
        ));
    }

    matched_torrents.sort_by(|a, b| b.1.total.total_cmp(&a.1.total));

    let now = now_ts();
    let mut saved_candidates = Vec::new();
    for (t_title, sc, baseline, tor, req_payload, old_ids) in matched_torrents.into_iter().take(5) {
        let site_id = tor["site_id"].as_u64().unwrap_or(0);
        let site_name = tor["site_name"].as_str().unwrap_or_default().to_string();
        let seeders = tor["seeders"].as_u64().unwrap_or(0);
        let size = tor["size"].as_i64().unwrap_or(0);
        let dl_url = tor["download_url"].as_str().unwrap_or_default().to_string();
        let cand_key = format!("pt:{}:{}:{}", item.id, site_id, t_title);

        db.execute(
            "INSERT INTO pt_candidates (
                cand_key, item_id, media_type, tmdb_id, season, episode, title,
                category, slot, current_score, graduate_score, torrent_title,
                site_id, site_name, seeders, size, cand_score, download_url,
                request_json, old_source_ids_json, state, message, updated_at
             ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'pending','',?)
             ON CONFLICT(cand_key) DO UPDATE SET
                cand_score=excluded.cand_score, current_score=excluded.current_score,
                seeders=excluded.seeders, request_json=excluded.request_json,
                old_source_ids_json=excluded.old_source_ids_json,
                state=CASE WHEN pt_candidates.state='submitted' THEN 'submitted' ELSE 'pending' END,
                updated_at=excluded.updated_at",
            params![
                cand_key,
                item.id,
                media_type,
                tmdb_id,
                item.effective_season(),
                item.effective_episode(),
                if is_tv {
                    format!(
                        "{} S{:02}E{:02}",
                        search_kw,
                        item.effective_season(),
                        item.effective_episode()
                    )
                } else {
                    item.name.clone()
                },
                category,
                sc.slot,
                baseline.unwrap_or(highest_current),
                graduate,
                t_title,
                site_id as i64,
                site_name,
                seeders as i64,
                size,
                sc.total,
                dl_url,
                req_payload.to_string(),
                serde_json::to_string(&old_ids)?,
                now,
            ],
        )?;
        let cid: i64 = db.query_row(
            "SELECT id FROM pt_candidates WHERE cand_key = ?",
            [&cand_key],
            |r| r.get(0),
        )?;
        saved_candidates.push(json!({
            "id": cid,
            "torrent_title": t_title,
            "site_name": site_name,
            "seeders": seeders,
            "size": size,
            "slot": sc.slot,
            "cand_score": sc.total,
            "baseline_score": baseline,
            "breakdown": sc.breakdown,
        }));
    }

    if cfg.pt_config.auto_download && !settings.dry_run {
        if let Some(best) = saved_candidates.first() {
            if let Some(best_id) = best["id"].as_i64() {
                let _ = act_pt_candidate(db, host, cfg, settings, best_id, "download");
            }
        }
    }

    Ok(json!({
        "title": item.name,
        "search_keyword": search_kw,
        "category": category,
        "current_score": highest_current,
        "graduate_score": graduate,
        "rule_name": rule.name,
        "candidates": saved_candidates,
        "notice": if saved_candidates.is_empty() {
            "未找到满足促销、做种、TMDB 身份及分差门槛的 PT 升级候选"
        } else {
            "已筛出高分 PT 候选并加入队列"
        },
    }))
}

fn torrent_eligible(tor: &Value, cfg: &AppConfig) -> bool {
    let site_id = tor["site_id"].as_u64().unwrap_or(0);
    if site_id == 0 {
        return false;
    }
    if !cfg.pt_config.site_ids.is_empty() && !cfg.pt_config.site_ids.contains(&site_id) {
        return false;
    }
    if cfg.pt_config.free_only {
        let dl_factor = tor["downloadvolumefactor"].as_f64().unwrap_or(1.0);
        if dl_factor > 0.001 {
            return false;
        }
    }
    if cfg.pt_config.skip_hr && tor["hit_and_run"].as_bool() == Some(true) {
        return false;
    }
    tor["seeders"].as_u64().unwrap_or(0) >= cfg.pt_config.min_seeders
}

pub fn act_pt_candidate(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
    cand_id: i64,
    operation: &str,
) -> Result<Value> {
    if operation == "dismiss" {
        db.execute(
            "UPDATE pt_candidates SET state = 'dismissed', updated_at = ? WHERE id = ?",
            params![now_ts(), cand_id],
        )?;
        return Ok(json!({ "ok": true, "state": "dismissed" }));
    }
    if operation != "download" {
        bail!("不支持的 PT 候选操作: {operation}");
    }
    ensure!(
        !settings.dry_run,
        "当前处于「仅预览模式（干跑）」，请先关闭仅预览模式后再提交 PT 下载"
    );

    let (
        item_id,
        media_type,
        tmdb_id,
        season,
        episode,
        title,
        category,
        slot,
        current_score,
        torrent_title,
        cand_score,
        req_raw,
        old_ids_raw,
    ): (
        String,
        String,
        String,
        u32,
        u32,
        String,
        String,
        String,
        f64,
        String,
        f64,
        String,
        String,
    ) = db
        .query_row(
            "SELECT item_id, media_type, tmdb_id, season, episode, title, category, slot,
                    current_score, torrent_title, cand_score, request_json, old_source_ids_json
             FROM pt_candidates WHERE id = ?",
            [cand_id],
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
                ))
            },
        )
        .context("找不到该 PT 候选条目")?;

    // Re-verify library item identity before downloading
    let live_item = host.library_item_detail(&item_id)?;
    ensure!(
        live_item.effective_tmdb() == tmdb_id,
        "媒体库条目 TMDB 身份已变化，请重新搜种"
    );

    let req_json: Value = serde_json::from_str(&req_raw)?;
    let site_label = req_json["site_name"]
        .as_str()
        .or_else(|| req_json["indexer"].as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let size_bytes = req_json["size"].as_u64().unwrap_or(0);
    let download_id = host.create_media_download(req_json)?;
    let now = now_ts();

    db.execute(
        "UPDATE pt_candidates SET state = 'submitted', message = ?, updated_at = ? WHERE id = ?",
        params![format!("已提交暂缓整理下载 (任务 #{download_id})"), now, cand_id],
    )?;

    let rule_id = active_pt_rule_id(cfg, &media_type);
    let ledger_key = format!("pt:{item_id}:{download_id}");
    db.execute(
        "INSERT INTO wash_ledger (
            ledger_key, source_kind, media_type, tmdb_id, season, episode, item_id,
            title, category, slot, rule_id, download_id, old_source_ids_json,
            cand_score, old_score, state, message, created_at, updated_at
         ) VALUES (?,'pt',?,?,?,?,?,?,?,?,?,?,?,?,?,'downloading',?,?,?)
         ON CONFLICT(ledger_key) DO UPDATE SET state='downloading', updated_at=excluded.updated_at",
        params![
            ledger_key,
            media_type,
            tmdb_id,
            season,
            episode,
            item_id,
            title,
            category,
            slot,
            rule_id,
            download_id,
            old_ids_raw,
            cand_score,
            current_score,
            format!("PT 洗版下载中：{torrent_title}"),
            now,
            now,
        ],
    )?;

    let cur_lib_path = live_item
        .effective_sources()
        .first()
        .and_then(|s| {
            [s.strm_path.as_str(), s.cloud_path.as_str(), s.file_path.as_str(), live_item.path.as_str()]
                .into_iter()
                .find(|p| !p.trim().is_empty())
        })
        .unwrap_or(&live_item.path)
        .to_string();

    db::log_media_history(
        db,
        "PT洗版",
        &title,
        &torrent_title,
        &format!("PT 种子：{torrent_title}"),
        &if cur_lib_path.is_empty() {
            "下载完成后归档替换库内旧源".to_string()
        } else {
            format!("拟替换库内源：{cur_lib_path}")
        },
        &format!("{} · {}", if category.is_empty() { "PT升级" } else { &category }, slot),
        "submit_pt_download",
        "downloading",
        &format!("已提交暂缓整理下载 #{download_id}（{:.2} → {:.2}）", current_score, cand_score),
    );
    if settings.notify {
        let tpls = host.get_notification_templates();
        if tpls.download_started.enabled {
            let media_icon = if media_type == "tv" { "📺" } else { "🎬" };
            let se_tag = if media_type == "tv" && episode > 0 {
                format!("S{:02}E{:02}", season.max(1), episode)
            } else {
                String::new()
            };
            let size_str = if size_bytes >= 1_000_000_000 {
                format!("{:.2} GB", size_bytes as f64 / 1_000_000_000.0)
            } else if size_bytes >= 1_000_000 {
                format!("{:.1} MB", size_bytes as f64 / 1_000_000.0)
            } else {
                String::new()
            };
            let mut vars = std::collections::BTreeMap::new();
            vars.insert("media_icon", media_icon.to_string());
            vars.insert("display_title", title.clone());
            vars.insert("season_episode", se_tag);
            vars.insert("download_action", "PT洗版下载".to_string());
            vars.insert("torrent_title", torrent_title.clone());
            vars.insert("site", site_label);
            vars.insert("size", size_str);
            let nt_title =
                crate::host::render_mediary_template(&tpls.download_started.title, &vars);
            let mut nt_body =
                crate::host::render_mediary_template(&tpls.download_started.content, &vars);
            let extra = format!(
                "⭐ 评分: {current_score:.2} → {cand_score:.2} ({slot})\n🗓️ 时间: {}",
                crate::models::format_ts_cn(now)
            );
            if !nt_body.is_empty() {
                nt_body.push('\n');
            }
            nt_body.push_str(&extra);
            let img = host.resolve_tmdb_card_image(&tmdb_id, &media_type);
            host.notify(&nt_title, &nt_body, img.as_deref());
        }
    }

    Ok(json!({
        "ok": true,
        "download_id": download_id,
    }))
}

pub fn scheduled_pt_sweep(
    db: &mut Connection,
    host: &HostClient,
    cfg: &AppConfig,
    settings: &PluginSettings,
) -> Result<Value> {
    if !cfg.pt_config.enabled {
        return Ok(json!({ "checked": 0 }));
    }
    let quota = cfg.pt_config.batch_per_tick.clamp(1, 12);
    let mut checked = 0usize;
    let mut hits = 0usize;

    for media_type in ["movie", "tv"] {
        if checked >= quota {
            break;
        }
        let cursor_key = format!("pt_sweep_pos:{media_type}");
        let start = db::get_cursor(db, &cursor_key)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let Ok(page) = host.library_page(start, 6, media_type, "") else {
            continue;
        };
        let next = if page.items.is_empty() || start + page.items.len() >= page.total {
            0
        } else {
            start + page.items.len()
        };
        let _ = db::set_cursor(db, &cursor_key, &next.to_string());

        for summary in page.items {
            if checked >= quota {
                break;
            }
            let tmdb = summary.effective_tmdb();
            if tmdb.is_empty() || tmdb == "0" {
                continue;
            }
            let cool_key = format!(
                "pt_cool:{media_type}:{tmdb}:{}:{}",
                summary.effective_season(),
                summary.effective_episode()
            );
            let recent = db::get_cursor(db, &cool_key)
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|ts| now_ts().saturating_sub(ts) < 86_400);
            if recent {
                continue;
            }
            let _ = db::set_cursor(db, &cool_key, &now_ts().to_string());
            checked += 1;
            if let Ok(res) = search_pt_for_item(db, host, cfg, settings, &summary.id, "", true) {
                hits += res["candidates"].as_array().map_or(0, Vec::len);
            }
        }
    }
    Ok(json!({ "checked": checked, "candidates_found": hits }))
}

pub fn list_pt_candidates(db: &Connection, state_filter: &str, limit: usize) -> Result<Vec<Value>> {
    let mut sql = String::from(
        "SELECT id, item_id, media_type, tmdb_id, season, episode, title, category, slot,
                current_score, graduate_score, torrent_title, site_name, seeders, size,
                cand_score, state, message, updated_at
         FROM pt_candidates WHERE 1=1",
    );
    if !state_filter.is_empty() && state_filter != "all" {
        let safe = match state_filter {
            "pending" => "pending",
            "submitted" => "submitted",
            "dismissed" => "dismissed",
            _ => "pending",
        };
        sql.push_str(&format!(" AND state = '{safe}'"));
    }
    sql.push_str(&format!(
        " ORDER BY updated_at DESC, id DESC LIMIT {}",
        limit.clamp(1, 200)
    ));
    let mut stmt = db.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        let ts: u64 = r.get(18)?;
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "item_id": r.get::<_, String>(1)?,
            "media_type": r.get::<_, String>(2)?,
            "tmdb_id": r.get::<_, String>(3)?,
            "season": r.get::<_, u32>(4)?,
            "episode": r.get::<_, u32>(5)?,
            "title": r.get::<_, String>(6)?,
            "category": r.get::<_, String>(7)?,
            "slot": r.get::<_, String>(8)?,
            "current_score": r.get::<_, f64>(9)?,
            "graduate_score": r.get::<_, f64>(10)?,
            "torrent_title": r.get::<_, String>(11)?,
            "site_name": r.get::<_, String>(12)?,
            "seeders": r.get::<_, i64>(13)?,
            "size": r.get::<_, i64>(14)?,
            "cand_score": r.get::<_, f64>(15)?,
            "state": r.get::<_, String>(16)?,
            "message": r.get::<_, String>(17)?,
            "updated_at": format_ts_cn(ts),
        }))
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

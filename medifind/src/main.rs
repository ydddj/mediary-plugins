mod db;
mod discover;
mod engine;
mod host;
mod models;
mod providers;
mod rules;
mod tg_mtproto;

use anyhow::{anyhow, ensure, Result};
use chrono::Utc;
use db::Db;
use discover::DiscoverRequest;
use engine::{AggregateRequest, InspectRequest, TransferRequest};
use host::{HostClient, HostStatus, Subscription};
use models::{
    normalize_tg_level, AYClubConfig, AppConfig, HDHiveConfig, HistoryFilter, IgnoredEpisodeRule,
    NotificationConfig, PluginSettings, RSSFeedConfig, ResourceItem, RuleConfig, TGChannelEntry,
    TelegramConfig,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::env;
use std::io::{self, Read};
use std::path::PathBuf;

const SECRET_MASK: &str = "******";

fn main() {
    if let Err(err) = run() {
        eprintln!("{:#}", err);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let data_dir = env::var("MEDIARY_PLUGIN_DATA_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./data"));

    let action = env::var("MEDIARY_PLUGIN_ACTION").unwrap_or_default();
    let action = action.trim();
    if action.is_empty() {
        // Pure action_runtime plugin; output health JSON if invoked directly.
        println!(r#"{{"status":"ok","plugin":"medifind","engine":"rust"}}"#);
        return Ok(());
    }

    let settings = load_settings();
    let db = Db::open(&data_dir)?;
    let mut cfg = db.load_config()?;

    let mut raw_input = String::new();
    io::stdin()
        .take(4 * 1024 * 1024)
        .read_to_string(&mut raw_input)?;
    let input_val: Value = if raw_input.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&raw_input).unwrap_or_else(|_| json!({}))
    };

    let mc = HostClient::from_env().ok();

    let output: Value = match action {
        // --- Scheduled Actions ---
        "tick" => {
            let Some(_guard) = db.try_acquire_tick_lock() else {
                return Ok(println!(
                    r#"{{"notice":"上一轮追更巡检仍在执行中，本次自动跳过防并发冲突"}}"#
                ));
            };
            let sum = engine::run_tick(mc.as_ref(), &db, &settings, &mut cfg);
            db.save_config(&cfg)?;
            json!({
                "notice": sum.message,
                "report": sum,
            })
        }

        "daily_checkin" => {
            let res = engine::run_daily_checkin(mc.as_ref(), &db, &settings, &mut cfg, false);
            let _ = db.save_config(&cfg);
            let msg = res?;
            json!({ "notice": msg })
        }

        // --- Mediary Native Resource Search Integration (source_id: -10) ---
        "resource_search" => {
            let query = first_str(&input_val, &["query", "keyword", "title"]);
            let tmdb_id = first_str(&input_val, &["tmdb_id"]);
            let media_type = first_str(&input_val, &["media_type", "type"]);
            let season = first_u32(&input_val, &["season"]);
            let episode = first_u32(&input_val, &["episode"]);

            let agg = engine::aggregate_resources(
                mc.as_ref(),
                &db,
                &mut cfg,
                &AggregateRequest {
                    query: query.clone(),
                    tmdb_id,
                    media_type,
                    title: query,
                    season,
                    episode,
                    live_tg: true,
                    ..Default::default()
                },
            );
            let _ = db.save_config(&cfg);
            json!({
                "results": build_host_resource_results(&agg.items),
            })
        }

        "resource_transfer" => {
            let host = mc
                .as_ref()
                .ok_or_else(|| anyhow!("缺少 Mediary 插件 API 运行环境"))?;
            let slug = first_str(&input_val, &["slug", "link", "download_url"]);
            let title = first_str(&input_val, &["title"]);
            let tmdb_id = first_str(&input_val, &["tmdb_id"]);
            let media_type = first_str(&input_val, &["media_type"]);
            let source_type = first_str(&input_val, &["source_type"]);
            let channel_name = first_str(&input_val, &["channel_name"]);
            let pts = first_i32(&input_val, &["unlock_points"]);

            let res = engine::transfer_resource(
                host,
                &db,
                &settings,
                &mut cfg,
                &TransferRequest {
                    slug,
                    title,
                    tmdb_id: tmdb_id.clone(),
                    media_type: media_type.clone(),
                    source_type,
                    channel_name,
                    unlock_points: pts,
                    allow_unlock: settings.auto_unlock_hdhive,
                    only_missing_episodes: media_type.eq_ignore_ascii_case("tv")
                        && !tmdb_id.is_empty(),
                    trigger_reason: "原生资源搜索一键转存".into(),
                    ..Default::default()
                },
            );
            let _ = db.save_config(&cfg);
            let ok_res = res?;
            json!({ "notice": ok_res.notice })
        }

        // --- Workbench UI Actions ---
        "ui_bootstrap" | "dashboard" => {
            if !cfg.rules.default_target_cid.trim().is_empty()
                && cfg.rules.default_target_name.trim().is_empty()
            {
                if let Some(host) = mc.as_ref() {
                    let hs = host.get_host_status();
                    if let Some(dir_name) =
                        host.resolve_115_dir_name(&cfg.rules.default_target_cid, &hs)
                    {
                        cfg.rules.default_target_name = dir_name;
                        let _ = db.save_config(&cfg);
                    }
                }
            }
            let _ = db.write_dashboard(&cfg);
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "")
        }

        "ui_search_tmdb" => {
            let host = mc
                .as_ref()
                .ok_or_else(|| anyhow!("缺少 Mediary 宿主 API 运行环境"))?;
            let disc_req: DiscoverRequest = serde_json::from_value(input_val).unwrap_or_default();
            discover::discover_or_search_media(host, &db, &cfg, &disc_req)?
        }

        "ui_aggregate_search" => {
            let mut req: AggregateRequest = serde_json::from_value(input_val)?;
            if !req.live_tg {
                req.live_tg = true;
            }
            let agg = engine::aggregate_resources(mc.as_ref(), &db, &mut cfg, &req);
            let _ = db.save_config(&cfg);
            serde_json::to_value(agg)?
        }

        "ui_inspect_resource" => {
            let req: InspectRequest = serde_json::from_value(input_val)?;
            let resp = engine::inspect_resource(mc.as_ref(), &db, &settings, &mut cfg, &req);
            let _ = db.save_config(&cfg);
            serde_json::to_value(resp?)?
        }

        "ui_transfer_resource" => {
            let host = mc
                .as_ref()
                .ok_or_else(|| anyhow!("缺少 Mediary 宿主 API 运行环境"))?;
            let req: TransferRequest = serde_json::from_value(input_val)?;
            let res = engine::transfer_resource(host, &db, &settings, &mut cfg, &req);
            let _ = db.save_config(&cfg);
            let ok_res = res?;
            let mut boot = build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &ok_res.notice);
            boot["transfer_result"] = serde_json::to_value(ok_res)?;
            boot
        }

        "ui_browse_115" => {
            let host = mc
                .as_ref()
                .ok_or_else(|| anyhow!("缺少 Mediary 宿主 API 运行环境"))?;
            let cid = first_str(&input_val, &["cid"]);
            let offset = first_u32(&input_val, &["offset"]) as usize;
            let limit = first_u32(&input_val, &["limit"]) as usize;
            let page = host.open115_list_page(&cid, offset, limit, true)?;
            serde_json::to_value(page)?
        }

        "ui_run_tick" => {
            let Some(_guard) = db.try_acquire_tick_lock() else {
                return Ok(println!(
                    "{}",
                    serde_json::to_string(&build_bootstrap(
                        mc.as_ref(),
                        &db,
                        &settings,
                        &cfg,
                        "后台定时巡检正在运行中，请稍候刷新查看结果",
                    ))?
                ));
            };
            let sum = engine::run_tick(mc.as_ref(), &db, &settings, &mut cfg);
            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &sum.message)
        }

        "ui_daily_checkin" => {
            let force_hdhive = input_val
                .get("force_hdhive")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let res =
                engine::run_daily_checkin(mc.as_ref(), &db, &settings, &mut cfg, force_hdhive);
            let _ = db.save_config(&cfg);
            let msg = res?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_clear_tg_cache" => {
            let cleared = db.clear_tg_cache()?;
            for ch in &mut cfg.telegram.channels {
                ch.cached_count = 0;
                ch.last_msg_id = 0;
            }
            db.save_config(&cfg)?;
            build_bootstrap(
                mc.as_ref(),
                &db,
                &settings,
                &cfg,
                &format!("已清空 {} 条 Telegram 缓存资源并重置频道增量游标", cleared),
            )
        }

        "ui_reset_transfer_stats" => {
            cfg.transferred_count = 0;
            for ch in &mut cfg.telegram.channels {
                ch.transferred_count = 0;
            }
            db.save_config(&cfg)?;
            build_bootstrap(
                mc.as_ref(),
                &db,
                &settings,
                &cfg,
                "已清零累计已完成转存统计计数",
            )
        }

        "ui_chase_subscription" => {
            let host = mc
                .as_ref()
                .ok_or_else(|| anyhow!("缺少 Mediary 宿主 API 运行环境"))?;
            let sub = Subscription {
                tmdb_id: first_str(&input_val, &["tmdb_id"]),
                title: first_str(&input_val, &["title"]),
                media_type: first_str(&input_val, &["media_type"]),
                season: first_u32(&input_val, &["season"]),
                ..Default::default()
            };
            let res = engine::chase_single_subscription(host, &db, &settings, &mut cfg, &sub);
            let _ = db.save_config(&cfg);
            let (hit, msg) = res?;
            let prefix = if hit { "追更已触发转存：" } else { "追更检查：" };
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &format!("{}{}", prefix, msg))
        }

        "ui_retry_reviewing" => {
            let host = mc
                .as_ref()
                .ok_or_else(|| anyhow!("缺少 Mediary 宿主 API 运行环境"))?;
            let clean_slug = first_str(&input_val, &["clean_slug"]);
            let (retried, ok_cnt) = if !clean_slug.is_empty() {
                engine::retry_reviewing_queue(host, &db, &settings, &mut cfg, Some(&clean_slug), 1)
            } else {
                engine::retry_reviewing_queue(host, &db, &settings, &mut cfg, None, 10)
            };
            let _ = db.save_config(&cfg);
            build_bootstrap(
                mc.as_ref(),
                &db,
                &settings,
                &cfg,
                &format!("重试 {} 项，成功转存 {} 项", retried, ok_cnt),
            )
        }

        "ui_remove_reviewing" => {
            let clean_slug = first_str(&input_val, &["clean_slug"]);
            db.remove_reviewing(&clean_slug)?;
            let _ = db.write_dashboard(&cfg);
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "已移除待审记录")
        }

        "ui_history_list" => {
            let filter: HistoryFilter = serde_json::from_value(input_val).unwrap_or_default();
            db.query_records(&filter)
        }

        "ui_history_delete" => {
            let seqs: Vec<i64> = input_val
                .get("seqs")
                .or_else(|| input_val.get("ids"))
                .and_then(|x| x.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_i64())
                .collect();
            let clear_filtered = input_val
                .get("clear_filtered")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let unlock_dedupe = input_val
                .get("unlock_dedupe")
                .and_then(|x| x.as_bool())
                .unwrap_or(true);
            let filter: HistoryFilter = serde_json::from_value(input_val).unwrap_or_default();
            ensure!(
                !seqs.is_empty() || clear_filtered,
                "请先勾选要删除的记录，或确认清空当前筛选结果"
            );
            let (deleted, unlocked) = db.delete_records(
                &seqs,
                if clear_filtered { Some(&filter) } else { None },
                unlock_dedupe,
            )?;
            let is_unfiltered_transfer_clear = clear_filtered
                && filter.record_type.trim() != "task"
                && (filter.source.trim().is_empty() || filter.source.trim() == "all")
                && (filter.action_group.trim().is_empty() || filter.action_group.trim() == "all")
                && (filter.status.trim().is_empty() || filter.status.trim() == "all")
                && filter.keyword.trim().is_empty();
            if is_unfiltered_transfer_clear {
                cfg.transferred_count = 0;
                for ch in &mut cfg.telegram.channels {
                    ch.transferred_count = 0;
                }
                let _ = db.save_config(&cfg);
            } else {
                let _ = db.write_dashboard(&cfg);
            }
            let mut out = db.query_records(&filter);
            out["deleted"] = json!(deleted);
            out["unlocked"] = json!(unlocked);
            out["notice"] = json!(if unlocked > 0 {
                format!("已删除 {} 条记录，并同步解除 {} 条转存去重锁", deleted, unlocked)
            } else {
                format!("已删除 {} 条记录", deleted)
            });
            out
        }

        "ui_history_retry" => {
            let host = mc
                .as_ref()
                .ok_or_else(|| anyhow!("缺少 Mediary 宿主 API 运行环境"))?;
            let seqs: Vec<i64> = input_val
                .get("seqs")
                .or_else(|| input_val.get("ids"))
                .and_then(|x| x.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_i64())
                .collect();
            ensure!(!seqs.is_empty(), "请先选择要重新转存的历史记录");
            let filter: HistoryFilter = serde_json::from_value(input_val).unwrap_or_default();
            let targets = db.get_records_by_seqs(&seqs);
            ensure!(!targets.is_empty(), "未找到选中的历史记录");

            let mut retried = 0usize;
            let mut ok_cnt = 0usize;
            let mut errs = Vec::new();

            for rec in targets {
                retried += 1;
                if !rec.clean_slug.trim().is_empty() {
                    db.unmark_slug_transferred(&rec.clean_slug);
                }
                if !rec.tmdb_id.trim().is_empty() {
                    db.clear_in_flight_for_tmdb(&rec.tmdb_id);
                }
                if !rec.slug.trim().is_empty() {
                    let ck = rules::clean_slug(&rec.slug);
                    if !ck.is_empty() {
                        db.unmark_slug_transferred(&ck);
                    }
                    match engine::transfer_resource(
                        host,
                        &db,
                        &settings,
                        &mut cfg,
                        &TransferRequest {
                            slug: rec.slug.clone(),
                            title: rec.title.clone(),
                            tmdb_id: rec.tmdb_id.clone(),
                            media_type: rec.media_type.clone(),
                            source_type: rec.source.clone(),
                            channel_name: rec.channel_name.clone(),
                            allow_unlock: true,
                            only_missing_episodes: false,
                            trigger_reason: "历史记录重新转存".into(),
                            ..Default::default()
                        },
                    ) {
                        Ok(r) if r.status == "success" || r.status == "reviewing" => ok_cnt += 1,
                        Ok(r) => errs.push(format!("{}: {}", rec.title, r.notice)),
                        Err(e) => errs.push(format!("{}: {}", rec.title, e)),
                    }
                } else if !rec.tmdb_id.trim().is_empty() {
                    let sub = Subscription {
                        tmdb_id: rec.tmdb_id.clone(),
                        title: rec.title.clone(),
                        media_type: rec.media_type.clone(),
                        ..Default::default()
                    };
                    match engine::chase_single_subscription(host, &db, &settings, &mut cfg, &sub) {
                        Ok((true, _)) => ok_cnt += 1,
                        Ok((false, msg)) => errs.push(format!("{}: {}", rec.title, msg)),
                        Err(e) => errs.push(format!("{}: {}", rec.title, e)),
                    }
                } else {
                    errs.push(format!("{}: 旧版记录不含原始链接或 TMDB ID，请前往聚合搜片重新检索", rec.title));
                }
            }

            let _ = db.save_config(&cfg);
            let mut out = db.query_records(&filter);
            let mut msg = format!("重新转存执行完成：成功 {}/{} 项", ok_cnt, retried);
            if !errs.is_empty() {
                msg.push_str(&format!("（{}）", errs.join("；")));
            }
            out["retried"] = json!(retried);
            out["succeeded"] = json!(ok_cnt);
            out["notice"] = json!(msg);
            out
        }

        "ui_save_ignored_ep" => {
            let mut rule: IgnoredEpisodeRule = serde_json::from_value(input_val)?;
            rule.tmdb_id = rule.tmdb_id.trim().to_string();
            ensure!(!rule.tmdb_id.is_empty() && rule.season > 0, "请填写有效的 TMDB ID 和季号");
            if rule.media_type.is_empty() {
                rule.media_type = "tv".into();
            }
            let exists = cfg.ignored_episodes.iter().any(|r| {
                r.tmdb_id == rule.tmdb_id && r.season == rule.season && r.episode == rule.episode
            });
            if !exists {
                cfg.ignored_episodes.push(rule);
            }
            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "已添加缺集忽略规则")
        }

        "ui_delete_ignored_ep" => {
            let req: IgnoredEpisodeRule = serde_json::from_value(input_val)?;
            cfg.ignored_episodes.retain(|r| {
                !(r.tmdb_id == req.tmdb_id && r.season == req.season && r.episode == req.episode)
            });
            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "已移除忽略规则")
        }

        "ui_save_channels" => {
            #[derive(Deserialize)]
            struct SaveChannelsReq {
                #[serde(default)]
                custom_proxy: String,
                hdhive: HDHiveConfig,
                telegram: TelegramConfig,
                ayclub: AYClubConfig,
                #[serde(default)]
                notifications: Option<NotificationConfig>,
            }
            let mut req: SaveChannelsReq = serde_json::from_value(input_val)?;
            cfg.custom_proxy = req.custom_proxy.trim().to_string();
            if let Some(nc) = req.notifications {
                cfg.notifications = nc;
            }

            if req.hdhive.api_key == SECRET_MASK {
                req.hdhive.api_key = cfg.hdhive.api_key.clone();
            }
            if req.hdhive.refresh_token == SECRET_MASK {
                req.hdhive.refresh_token = cfg.hdhive.refresh_token.clone();
            }
            if req.hdhive.base_url.trim().is_empty() {
                req.hdhive.base_url = "https://hdhive.nextemby.com".into();
            } else {
                req.hdhive.base_url = req.hdhive.base_url.trim().trim_end_matches('/').to_string();
            }
            let same_hd_base = req
                .hdhive
                .base_url
                .eq_ignore_ascii_case(cfg.hdhive.base_url.trim().trim_end_matches('/'));
            req.hdhive.protocol = if same_hd_base {
                cfg.hdhive.protocol.clone()
            } else {
                String::new()
            };
            req.hdhive.username = if same_hd_base {
                cfg.hdhive.username.clone()
            } else {
                String::new()
            };
            req.hdhive.points = if same_hd_base { cfg.hdhive.points } else { 0 };
            req.hdhive.checked_in_today = cfg.hdhive.checked_in_today;
            req.hdhive.last_checkin_date = cfg.hdhive.last_checkin_date.clone();
            req.hdhive.last_checkin_result = cfg.hdhive.last_checkin_result.clone();
            cfg.hdhive = req.hdhive;

            if req.telegram.bot_token == SECRET_MASK {
                req.telegram.bot_token = cfg.telegram.bot_token.clone();
            }
            if req.telegram.api_hash == SECRET_MASK || req.telegram.api_hash.trim().is_empty() {
                req.telegram.api_hash = cfg.telegram.api_hash.clone();
            }
            if req.telegram.api_id.trim().is_empty() {
                req.telegram.api_id = cfg.telegram.api_id.clone();
            }
            req.telegram.is_logged_in = cfg.telegram.is_logged_in;
            req.telegram.tg_username = cfg.telegram.tg_username.clone();
            req.telegram.tg_phone = cfg.telegram.tg_phone.clone();
            req.telegram.pending_phone = cfg.telegram.pending_phone.clone();
            req.telegram.pending_code_hash = cfg.telegram.pending_code_hash.clone();
            req.telegram.pending_dc_id = cfg.telegram.pending_dc_id;
            req.telegram.password_required = cfg.telegram.password_required;
            req.telegram.password_hint = cfg.telegram.password_hint.clone();

            let prev_by_ch: std::collections::HashMap<String, TGChannelEntry> = cfg
                .telegram
                .channels
                .iter()
                .map(|c| {
                    (
                        providers::normalize_tg_channel(&c.identifier).to_lowercase(),
                        c.clone(),
                    )
                })
                .collect();

            let mut clean_chs = Vec::new();
            let mut seen_ch = HashSet::new();

            for (idx, mut ch) in req.telegram.channels.into_iter().enumerate() {
                let raw_id = if !ch.identifier.trim().is_empty() {
                    ch.identifier.trim().to_string()
                } else {
                    ch.name.trim().to_string()
                };
                let norm = providers::normalize_tg_channel(&raw_id);
                if norm.is_empty() || !seen_ch.insert(norm.to_lowercase()) {
                    continue;
                }
                ch.identifier = norm.clone();
                ch.level = normalize_tg_level(ch.level);
                if ch.id.trim().is_empty() {
                    ch.id = format!("ch-{}-{}", idx + 1, Utc::now().timestamp_subsec_millis());
                }
                if let Some(prev) = prev_by_ch.get(&norm.to_lowercase()) {
                    if ch.logo.is_empty() {
                        ch.logo = prev.logo.clone();
                    }
                    if (ch.name.trim().is_empty()
                        || providers::normalize_tg_channel(&ch.name) == norm)
                        && !prev.name.is_empty()
                        && prev.name != norm
                    {
                        ch.name = prev.name.clone();
                    }
                    ch.last_msg_id = prev.last_msg_id;
                    ch.cached_count = prev.cached_count;
                    ch.transferred_count = prev.transferred_count;
                    ch.last_sync_at = prev.last_sync_at.clone();
                    ch.last_error = prev.last_error.clone();
                }
                if ch.name.trim().is_empty() {
                    ch.name = norm;
                }
                clean_chs.push(ch);
            }
            req.telegram.channels = clean_chs;
            if req.telegram.per_channel_limit == 0 {
                req.telegram.per_channel_limit = 40;
            }
            cfg.telegram = req.telegram;

            if req.ayclub.nf_token == SECRET_MASK {
                req.ayclub.nf_token = cfg.ayclub.nf_token.clone();
            }
            req.ayclub.remaining_times = cfg.ayclub.remaining_times;
            req.ayclub.exhausted_date = cfg.ayclub.exhausted_date.clone();
            req.ayclub.last_checked_at = cfg.ayclub.last_checked_at.clone();
            req.ayclub.last_checked_status = cfg.ayclub.last_checked_status.clone();
            cfg.ayclub = req.ayclub;
            cfg.rules.sync_subscription_sources(&cfg.telegram.channels);

            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "已保存资源渠道配置")
        }

        "ui_tg_save_api" => {
            let api_id = first_str(&input_val, &["api_id"]);
            let api_hash = first_str(&input_val, &["api_hash"]);
            cfg.telegram.api_id = api_id.trim().to_string();
            if api_hash != SECRET_MASK {
                cfg.telegram.api_hash = api_hash.trim().to_string();
            }
            db.save_config(&cfg)?;
            let msg = if cfg.telegram.api_id.is_empty() {
                "已恢复使用内置默认 Telegram APPID (3212490)"
            } else {
                "已保存自定义 Telegram APPID 与 API Hash"
            };
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, msg)
        }

        "ui_tg_send_code" => {
            let phone = first_str(&input_val, &["phone"]);
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let msg = tg_mtproto::send_login_code(&db.data_dir, &mut cfg, &phone, &proxy_url)?;
            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_tg_sign_in" => {
            let code = first_str(&input_val, &["code", "password"]);
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let msg =
                tg_mtproto::sign_in_with_code_or_password(&db.data_dir, &mut cfg, &code, &proxy_url)?;
            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_tg_logout" => {
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let msg = tg_mtproto::logout_tg(&db.data_dir, &mut cfg, &proxy_url)?;
            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_tg_check" => {
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let res = tg_mtproto::check_tg_connection(&db.data_dir, &mut cfg, &proxy_url);
            let _ = db.save_config(&cfg);
            let msg = res?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_tg_channel_info" => {
            let id = first_str(&input_val, &["identifier"]);
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let meta = providers::resolve_tg_channel_info(&db.data_dir, &cfg, &id, &proxy_url)?;
            serde_json::to_value(meta)?
        }

        "ui_tg_resolve_and_save" => {
            let channel_id = first_str(&input_val, &["channel_id", "id"]);
            let raw_id = first_str(&input_val, &["identifier", "name"]);
            let req_level = input_val.get("level").and_then(|x| x.as_i64()).map(|v| v as i32);
            let req_enabled = input_val.get("enabled").and_then(|x| x.as_bool());
            let req_hide = input_val.get("hide_in_discover").and_then(|x| x.as_bool());

            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let meta = providers::resolve_tg_channel_info(&db.data_dir, &cfg, &raw_id, &proxy_url)?;
            let meta_norm = providers::normalize_tg_channel(&meta.identifier).to_lowercase();

            let mut matched_idx = None;
            if !channel_id.is_empty() {
                matched_idx = cfg
                    .telegram
                    .channels
                    .iter()
                    .position(|c| c.id == channel_id);
            }
            if matched_idx.is_none() && !meta_norm.is_empty() {
                matched_idx = cfg.telegram.channels.iter().position(|c| {
                    providers::normalize_tg_channel(&c.identifier).to_lowercase() == meta_norm
                });
            }

            if let Some(idx) = matched_idx {
                let ch = &mut cfg.telegram.channels[idx];
                ch.identifier = meta.identifier.clone();
                ch.name = meta.name.clone();
                if !meta.logo.is_empty() {
                    ch.logo = meta.logo.clone();
                }
                if let Some(lv) = req_level {
                    ch.level = normalize_tg_level(lv);
                }
                if let Some(en) = req_enabled {
                    ch.enabled = en;
                }
                if let Some(hd) = req_hide {
                    ch.hide_in_discover = hd;
                }
            } else {
                cfg.telegram.channels.push(TGChannelEntry {
                    id: if channel_id.is_empty() {
                        format!("ch-{}", Utc::now().timestamp_millis())
                    } else {
                        channel_id
                    },
                    identifier: meta.identifier.clone(),
                    name: meta.name.clone(),
                    logo: meta.logo.clone(),
                    level: normalize_tg_level(req_level.unwrap_or(0)),
                    enabled: req_enabled.unwrap_or(true),
                    hide_in_discover: req_hide.unwrap_or(false),
                    ..Default::default()
                });
            }

            // Order-preserving deduplication by normalized identifier
            let mut deduped: Vec<TGChannelEntry> = Vec::with_capacity(cfg.telegram.channels.len());
            for ch in cfg.telegram.channels.drain(..) {
                let k = providers::normalize_tg_channel(if !ch.identifier.trim().is_empty() {
                    &ch.identifier
                } else {
                    &ch.name
                })
                .to_lowercase();
                if k.is_empty() {
                    continue;
                }
                if let Some(prev) = deduped.iter_mut().find(|p| {
                    providers::normalize_tg_channel(if !p.identifier.trim().is_empty() {
                        &p.identifier
                    } else {
                        &p.name
                    })
                    .to_lowercase()
                        == k
                }) {
                    if prev.logo.is_empty() && !ch.logo.is_empty() {
                        prev.logo = ch.logo;
                    }
                    if !ch.name.is_empty() && ch.name != ch.identifier {
                        prev.name = ch.name;
                    }
                    prev.last_msg_id = prev.last_msg_id.max(ch.last_msg_id);
                    prev.cached_count = prev.cached_count.max(ch.cached_count);
                    prev.transferred_count = prev.transferred_count.max(ch.transferred_count);
                } else {
                    deduped.push(ch);
                }
            }
            cfg.telegram.channels = deduped;
            cfg.rules.sync_subscription_sources(&cfg.telegram.channels);
            db.save_config(&cfg)?;

            let mut boot = build_bootstrap(
                mc.as_ref(),
                &db,
                &settings,
                &cfg,
                &format!("已识别并保存频道：{}", meta.name),
            );
            boot["resolved"] = serde_json::to_value(meta)?;
            boot
        }

        "ui_tg_list_dialogs" => {
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let dialogs = tg_mtproto::list_joined_dialogs(&db.data_dir, &cfg, &proxy_url)?;
            json!({ "dialogs": dialogs })
        }

        "ui_hdhive_test" => {
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let ms = providers::hdhive_sync_me(&mut cfg, &proxy_url)?;
            db.save_config(&cfg)?;
            let msg = if cfg.hdhive.protocol == "relay" {
                format!(
                    "影巢中转连接成功：授权 {} · 剩余次数 {} ({}ms)",
                    cfg.hdhive.username, cfg.hdhive.points, ms
                )
            } else {
                format!(
                    "HDHive 连接成功：{}（{} 积分，{}ms）",
                    cfg.hdhive.username, cfg.hdhive.points, ms
                )
            };
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_tg_sync" => {
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let (added, _, errs) =
                providers::sync_all_tg_channels(&db, &mut cfg, true, &proxy_url);
            cfg.rules.sync_subscription_sources(&cfg.telegram.channels);
            db.save_config(&cfg)?;
            let mut msg = format!(
                "Telegram 频道增量抓取完成：新增 {} 条资源（共缓存 {} 条）",
                added,
                db.tg_cached_total()
            );
            if !errs.is_empty() {
                msg.push_str("；部分提示：");
                msg.push_str(&errs.join(" | "));
            }
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_tg_test_bot" => {
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            providers::send_tg_bot_message(
                &cfg.telegram.bot_token,
                &cfg.telegram.bot_chat_id,
                "<b>[MediFind]</b> Telegram Bot 通知测试成功！",
                &proxy_url,
            )?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "Telegram Bot 测试消息发送成功")
        }

        "ui_ayclub_refresh" => {
            let proxy_url = engine::effective_proxy(mc.as_ref(), &cfg);
            let res = providers::refresh_ayclub_quota(&mut cfg, &proxy_url);
            let _ = db.save_config(&cfg);
            let msg = format!("AYClub 状态：{}", res?);
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_test_mediary_notify" => {
            let host = mc
                .as_ref()
                .ok_or_else(|| anyhow!("缺少 Mediary 宿主 API 运行环境"))?;
            if let Some(nc_val) = input_val.get("notifications") {
                if let Ok(nc) = serde_json::from_value::<NotificationConfig>(nc_val.clone()) {
                    cfg.notifications = nc;
                    let _ = db.save_config(&cfg);
                }
            }
            let tpls = host.get_notification_templates();
            let target_lbl = if !cfg.rules.default_target_name.trim().is_empty() {
                format!("接收至 {}", cfg.rules.default_target_name.trim())
            } else if !cfg.rules.prefer_flowlink {
                "接收至待整理目录".to_string()
            } else {
                "FlowLink 自动整理".to_string()
            };
            let mut vars = std::collections::HashMap::new();
            vars.insert("media_icon", "📺".to_string());
            vars.insert("name", "庆余年".to_string());
            vars.insert("year", "2024".to_string());
            vars.insert("display_title", "庆余年 (2024)".to_string());
            vars.insert("season_episode", "S02E01-E36".to_string());
            vars.insert("download_action", "开始转存(测试)".to_string());
            vars.insert(
                "torrent_title",
                format!("庆余年 S02 2160p DV WEB-DL · {}", target_lbl),
            );
            vars.insert("site", "TG · MediFind 测试".to_string());
            vars.insert("size", "48.6 GB".to_string());
            let title = host::render_mediary_template(&tpls.download_started.title, &vars);
            let content = host::render_mediary_template(&tpls.download_started.content, &vars);
            let mut test_img: Option<String> = None;
            if cfg.notifications.include_poster {
                if let Ok(resolved) = host.resolve_tmdb("庆余年", "tv", 0) {
                    test_img = host::tmdb_card_image_https(
                        &resolved.backdrop_path,
                        &resolved.poster_path,
                    );
                }
            }
            host.notify_with_image(&title, &content, test_img.as_deref())?;
            build_bootstrap(
                mc.as_ref(),
                &db,
                &settings,
                &cfg,
                "已按 Mediary 主程序通知模版发送测试消息",
            )
        }

        "ui_save_rss" => {
            let reset_seen = input_val
                .get("reset_seen")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let mut feed: RSSFeedConfig = serde_json::from_value(input_val)?;
            feed.url = feed.url.trim().to_string();
            ensure!(!feed.url.is_empty(), "RSS 订阅地址不能为空");
            if feed.name.trim().is_empty() {
                feed.name = feed.url.clone();
            } else {
                feed.name = feed.name.trim().to_string();
            }
            if feed.refresh_interval.trim().is_empty() {
                feed.refresh_interval = "6H".into();
            }
            let is_edit = !feed.id.trim().is_empty()
                && cfg.rss_feeds.iter().any(|x| x.id == feed.id);
            if feed.id.trim().is_empty() {
                feed.id = format!("rss-{}", Utc::now().timestamp_millis());
            }
            if let Some(existing) = cfg.rss_feeds.iter_mut().find(|x| x.id == feed.id) {
                let url_changed = !existing.url.trim().eq_ignore_ascii_case(&feed.url);
                if reset_seen || url_changed {
                    feed.seen_guids.clear();
                    feed.last_refresh_at = 0;
                    feed.last_result = if url_changed {
                        "已更新订阅地址，等待拉取".into()
                    } else {
                        "已重置已读缓存，等待拉取".into()
                    };
                } else {
                    feed.seen_guids = existing.seen_guids.clone();
                    feed.last_refresh_at = existing.last_refresh_at;
                    feed.last_result = existing.last_result.clone();
                }
                *existing = feed.clone();
            } else {
                cfg.rss_feeds.push(feed.clone());
            }
            db.save_config(&cfg)?;
            let verb = if is_edit { "已修改 RSS 订阅源" } else { "已添加 RSS 订阅源" };
            build_bootstrap(
                mc.as_ref(),
                &db,
                &settings,
                &cfg,
                &format!("{}：{}", verb, feed.name),
            )
        }

        "ui_toggle_rss" => {
            let id = first_str(&input_val, &["id"]);
            if let Some(f) = cfg.rss_feeds.iter_mut().find(|x| x.id == id) {
                f.enabled = !f.enabled;
            }
            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "已切换 RSS 状态")
        }

        "ui_delete_rss" => {
            let id = first_str(&input_val, &["id"]);
            cfg.rss_feeds.retain(|x| x.id != id);
            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "已删除 RSS 订阅源")
        }

        "ui_sync_rss" => {
            let (cnt, errs) = engine::process_due_rss_feeds(mc.as_ref(), &db, &settings, &mut cfg, true);
            db.save_config(&cfg)?;
            let mut msg = format!("RSS 立即同步完成：共触发 {} 项动作", cnt);
            if !errs.is_empty() {
                msg.push_str(&format!("（{}）", errs.join("；")));
            }
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_save_rules" => {
            let mut rules_cfg: RuleConfig = serde_json::from_value(input_val)?;
            rules_cfg.default_target_cid = rules_cfg.default_target_cid.trim().to_string();
            rules_cfg.default_target_name = rules_cfg.default_target_name.trim().to_string();
            if rules_cfg.default_target_cid.is_empty() {
                rules_cfg.default_target_name.clear();
            } else {
                let cid_changed_manually = rules_cfg.default_target_cid
                    != cfg.rules.default_target_cid.trim()
                    && rules_cfg.default_target_name == cfg.rules.default_target_name.trim();
                if cid_changed_manually || rules_cfg.default_target_name.is_empty() {
                    if cid_changed_manually {
                        rules_cfg.default_target_name.clear();
                    }
                    if let Some(host) = mc.as_ref() {
                        let hs = host.get_host_status();
                        if let Some(dir_name) =
                            host.resolve_115_dir_name(&rules_cfg.default_target_cid, &hs)
                        {
                            rules_cfg.default_target_name = dir_name;
                        }
                    }
                }
            }
            rules_cfg.ensure_weights_initialized();
            rules_cfg.sync_subscription_sources(&cfg.telegram.channels);
            cfg.rules = rules_cfg;
            db.save_config(&cfg)?;
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "已保存资源权重与 115 转存落点设置")
        }

        "ui_test_rule" => {
            let raw_text = first_str(&input_val, &["text"]);
            let text = rules::strip_ignored_texts(&raw_text, &cfg.rules.ignored_texts);
            let size_str = first_str(&input_val, &["size_str"]);
            let mut mtype = first_str(&input_val, &["media_type"]);
            if mtype.is_empty() {
                mtype = "tv".into();
            }
            let source = first_str(&input_val, &["source"]);
            let pan_type = first_str(&input_val, &["pan_type"]);
            let unlocked = input_val
                .get("unlocked")
                .and_then(|x| x.as_bool())
                .unwrap_or(true);
            let pts = first_i32(&input_val, &["points"]);

            let pure = rules::extract_pure_title(&text);
            let attrs = rules::parse_movie_attributes(&text);
            let eps = rules::parse_shallow_episodes(&text, &mtype, None);
            let links = rules::extract_all_resource_links(&raw_text);
            let detected_pan = if !pan_type.is_empty() {
                pan_type
            } else if let Some(lk) = links.first() {
                lk.pan_type.clone()
            } else {
                "115".into()
            };
            let mut item = ResourceItem {
                title: if pure.is_empty() { text.clone() } else { pure.clone() },
                raw_title: raw_text,
                share_size: size_str,
                media_type: mtype,
                source_type: if source.is_empty() { "telegram".into() } else { source },
                pan_type: detected_pan,
                parsed_episodes: eps.clone(),
                movie_attributes: attrs.clone(),
                is_unlocked: unlocked,
                unlock_points: pts,
                ..Default::default()
            };
            let score_res = rules::score_resource(&mut item, &cfg.rules, &pure, "");
            json!({
                "pure_title": pure,
                "year": rules::extract_year(&text),
                "tmdb_id": rules::extract_tmdb_id(&text),
                "parsed_episodes": eps,
                "attributes": attrs,
                "links": links,
                "score": score_res,
            })
        }

        "ui_add_shield_share" => {
            let share_url = first_str(&input_val, &["share_url"]);
            let receive_code = first_str(&input_val, &["receive_code"]);
            let title = first_str(&input_val, &["title"]);
            let run_now = input_val
                .get("run_now")
                .and_then(|x| x.as_bool())
                .unwrap_or(true);

            let rec = engine::add_shield_share(&db, &share_url, &receive_code, &title)?;
            let mut added = 0usize;
            let mut step_err = None;
            if run_now {
                if let Some(host) = mc.as_ref() {
                    match engine::step_shield_share(host, &db, &rec.id, 12) {
                        Ok(n) => added = n,
                        Err(e) => step_err = Some(e.to_string()),
                    }
                }
            }
            let _ = db.write_dashboard(&cfg);
            let mut msg = format!(
                "已登记神盾分享 [{}]，新增索引 {} 个媒体文件",
                rec.title, added
            );
            if let Some(err_s) = step_err {
                msg.push_str(&format!("（提示：{}）", err_s));
            }
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, &msg)
        }

        "ui_step_shield_share" => {
            let host = mc
                .as_ref()
                .ok_or_else(|| anyhow!("缺少 Mediary 宿主 API 运行环境"))?;
            let id = first_str(&input_val, &["id"]);
            let max_dirs = first_u32(&input_val, &["max_dirs"]).max(15) as usize;
            let added = engine::step_shield_share(host, &db, &id, max_dirs)?;
            let _ = db.write_dashboard(&cfg);
            build_bootstrap(
                mc.as_ref(),
                &db,
                &settings,
                &cfg,
                &format!("神盾批次解析完成：本轮新收录 {} 个媒体文件", added),
            )
        }

        "ui_delete_shield_share" => {
            let id = first_str(&input_val, &["id"]);
            db.delete_shield_share(&id)?;
            let _ = db.write_dashboard(&cfg);
            build_bootstrap(mc.as_ref(), &db, &settings, &cfg, "已删除该神盾索引库及关联文件")
        }

        _ => return Err(anyhow!("不支持的 MediFind 插件动作: {}", action)),
    };

    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

fn build_bootstrap(
    mc: Option<&HostClient>,
    db: &Db,
    settings: &PluginSettings,
    cfg: &AppConfig,
    notice: &str,
) -> Value {
    let host_status = mc.map(|h| h.get_host_status()).unwrap_or_default();
    let (mut subs, subscriptions_error) = match mc {
        Some(h) => match h.list_subscriptions() {
            Ok(list) => (list, String::new()),
            Err(e) => (Vec::new(), e.to_string()),
        },
        None => (Vec::new(), String::new()),
    };
    subs.sort_by_key(|s| match s.status.trim().to_ascii_lowercase().as_str() {
        "completed" => 2,
        "paused" => 1,
        _ => 0,
    });

    let mut hd_dto = cfg.hdhive.clone();
    let has_hdhive_key = !hd_dto.api_key.trim().is_empty();
    if has_hdhive_key {
        hd_dto.api_key = SECRET_MASK.into();
    }
    if !hd_dto.refresh_token.trim().is_empty() {
        hd_dto.refresh_token = SECRET_MASK.into();
    }

    let mut tg_dto = cfg.telegram.clone();
    if !tg_mtproto::has_session_file(&db.data_dir) {
        tg_dto.is_logged_in = false;
    }
    let has_pending_code = !tg_dto.pending_code_hash.trim().is_empty();
    tg_dto.pending_code_hash.clear();
    if !tg_dto.api_hash.trim().is_empty() {
        tg_dto.api_hash = SECRET_MASK.into();
    }
    if !tg_dto.bot_token.trim().is_empty() {
        tg_dto.bot_token = SECRET_MASK.into();
    }

    let mut ay_dto = cfg.ayclub.clone();
    let has_ay_token = !ay_dto.nf_token.trim().is_empty();
    if has_ay_token {
        ay_dto.nf_token = SECRET_MASK.into();
    }

    json!({
        "notice": notice,
        "settings": settings,
        "host_status": HostStatus { ..host_status },
        "subscriptions": subs,
        "subscriptions_error": subscriptions_error,
        "state": {
            "custom_proxy": cfg.custom_proxy,
            "hdhive": hd_dto,
            "has_hdhive_key": has_hdhive_key,
            "telegram": tg_dto,
            "tg_has_pending_code": has_pending_code,
            "tg_cached_total": db.tg_cached_total(),
            "ayclub": ay_dto,
            "has_ayclub_token": has_ay_token,
            "rss_feeds": cfg.rss_feeds,
            "rules": cfg.rules,
            "notifications": cfg.notifications,
            "reviewing_queue": db.list_reviewing(),
            "ignored_episodes": cfg.ignored_episodes,
            "shield_shares": db.list_shield_shares(),
            "shield_files_total": db.shield_files_total(),
            "records": db.list_records(10),
            "transfer_history_total": db.record_counts().0,
            "task_history_total": db.record_counts().1,
            "dedupe_locks_total": db.transferred_slugs_count(),
            "transferred_count": cfg.transferred_count,
            "probe_cache_count": db.probe_cache_count(),
            "last_result": cfg.last_result,
        }
    })
}

fn build_host_resource_results(items: &[ResourceItem]) -> Vec<Value> {
    let mut out = Vec::new();
    let now_rfc = Utc::now().to_rfc3339();

    for it in items.iter().take(150) {
        let mut cat = "115";
        let mut action_kind = "transfer";
        let mut action_label = "转存整理".to_string();
        let mut pending_label = "转存中";

        match it.pan_type.as_str() {
            "ed2k" => cat = "ed2k",
            "magnet" => {
                cat = "磁力";
                action_kind = "download";
                action_label = "离线下载".into();
                pending_label = "提交中";
            }
            "radar" => {
                if !it.is_unlocked && it.unlock_points > 0 {
                    action_label = format!("解锁转存({}分)", it.unlock_points);
                }
            }
            _ => {}
        }

        let mut labels = vec![cat.to_string(), it.source_type.to_ascii_uppercase()];
        if !it.movie_attributes.resolution.is_empty() {
            labels.push(it.movie_attributes.resolution.clone());
        }
        if !it.movie_attributes.color.is_empty() {
            labels.push(it.movie_attributes.color.clone());
        }
        if it.shield_matched {
            labels.push("神盾匹配".into());
        }
        if !it.missing_eps_hit.is_empty() {
            labels.push("含缺集".into());
        }

        let mut desc_parts = Vec::new();
        if !it.parsed_episodes.is_empty() {
            desc_parts.push(it.parsed_episodes.clone());
        }
        if !it.share_size.is_empty() {
            desc_parts.push(it.share_size.clone());
        }
        if it.rule_score > 0.0 {
            desc_parts.push(format!("评分 {:.1}", it.rule_score));
        }
        if !it.remark.is_empty() {
            desc_parts.push(it.remark.clone());
        }

        out.push(json!({
            "title": it.title,
            "site_name": "MediFind",
            "site_id": -10,
            "size": it.size_bytes,
            "download_url": it.slug,
            "description": desc_parts.join(" · "),
            "publish_time": now_rfc,
            "category": cat,
            "uploader": it.channel_name,
            "seeders": it.rule_score as i32,
            "leechers": 0,
            "labels": labels,
            "hit_and_run": false,
            "source_kind": "plugin",
            "plugin_key": it.clean_slug,
            "plugin_payload": {
                "slug": it.slug,
                "title": it.title,
                "tmdb_id": it.tmdb_id,
                "media_type": it.media_type,
                "source_type": it.source_type,
                "channel_name": it.channel_name,
                "unlock_points": it.unlock_points,
            },
            "plugin_action_kind": action_kind,
            "plugin_action_label": action_label,
            "plugin_action_pending_label": pending_label,
            "parsed": {
                "title": it.title,
                "raw_title": it.title,
            }
        }));
    }
    out
}

fn load_settings() -> PluginSettings {
    env::var("MEDIARY_PLUGIN_SETTINGS_JSON")
        .ok()
        .and_then(|s| serde_json::from_str::<PluginSettings>(&s).ok())
        .unwrap_or_default()
}

fn first_str(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        match v.get(*k) {
            Some(Value::String(s)) if !s.trim().is_empty() => return s.trim().to_string(),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    String::new()
}

fn first_u32(v: &Value, keys: &[&str]) -> u32 {
    for k in keys {
        match v.get(*k) {
            Some(Value::Number(n)) => {
                if let Some(u) = n.as_u64() {
                    return u as u32;
                }
            }
            Some(Value::String(s)) => {
                if let Ok(u) = s.trim().parse::<u32>() {
                    return u;
                }
            }
            _ => {}
        }
    }
    0
}

fn first_i32(v: &Value, keys: &[&str]) -> i32 {
    for k in keys {
        match v.get(*k) {
            Some(Value::Number(n)) => {
                if let Some(i) = n.as_i64() {
                    return i as i32;
                }
            }
            Some(Value::String(s)) => {
                if let Ok(i) = s.trim().parse::<i32>() {
                    return i;
                }
            }
            _ => {}
        }
    }
    0
}

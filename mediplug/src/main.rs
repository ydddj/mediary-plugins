mod db;
mod github;
mod host;
mod models;
mod server;

use anyhow::{bail, ensure, Context, Result};
use github::{
    inspect_tar_gz_base64, inspect_tar_gz_file, parse_github_repo_spec, GithubClient,
};
use host::{fast_host_status, version_is_newer, HostClient};
use models::{
    is_masked_or_empty, mask_secret, now_ts, sanitize_source_id, AppConfig, CatalogPluginEntry,
    EngineSettings, GithubRepoBridge, HostStoreSnapshot, PluginSettings,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_STDIN_BYTES: u64 = 64 * 1024 * 1024;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = env::args().collect();
    let data_dir = env::var_os("MEDIARY_PLUGIN_DATA_DIR")
        .map_or_else(|| PathBuf::from("data"), PathBuf::from);
    let mut settings: PluginSettings = env::var("MEDIARY_PLUGIN_SETTINGS_JSON")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let (plugin_enabled, _, _, _) = host::inspect_local_plugin_state(&data_dir);
    settings.enabled = plugin_enabled && settings.enabled;

    if args.get(1).map(String::as_str) == Some("daemon") {
        match server::run_daemon(data_dir) {
            Ok(()) => return std::process::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("[mediplug] 本地 catalog.json 常驻进程异常退出: {err:#}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    match run_action(&data_dir, &settings) {
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

fn run_action(data_dir: &Path, settings: &PluginSettings) -> Result<Value> {
    let action = env::var("MEDIARY_PLUGIN_ACTION").unwrap_or_else(|_| "ui_bootstrap".to_string());
    let mut stdin_buf = Vec::new();
    io::stdin()
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut stdin_buf)?;
    ensure!(
        stdin_buf.len() as u64 <= MAX_STDIN_BYTES,
        "请求载荷超过 64 MB 上限"
    );
    let input: Value = if stdin_buf.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        serde_json::from_slice(&stdin_buf).context("输入参数不是合法的 JSON")?
    };

    dispatch(data_dir, &action, &input, settings)
}

fn dispatch(
    data_dir: &Path,
    action: &str,
    input: &Value,
    settings: &PluginSettings,
) -> Result<Value> {
    match action {
        // === Scheduled Actions ===
        "tick" => {
            let mut conn = db::open_db(data_dir)?;
            let is_manual = input["manual"].as_bool() == Some(true)
                || env::var("MEDIARY_PLUGIN_TRIGGER").ok().as_deref() == Some("manual");
            if !is_manual && !settings.auto_sync_releases {
                return Ok(json!({
                    "ok": true,
                    "skipped": true,
                    "notice": "已关闭定时自动同步 GitHub Releases",
                }));
            }
            let summary = sync_repos_workflow(data_dir, &mut conn, None, settings, true)?;
            Ok(summary)
        }

        "health_check" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let host = HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token);
            let (st, sources, store_snap) = if let Some(ref h) = host {
                let (st, srcs) = h.inspect_status(data_dir, &cfg.engine);
                let snap = if st.store_auth_ready {
                    h.fetch_plugin_store().unwrap_or_default()
                } else {
                    HostStoreSnapshot::default()
                };
                (st, srcs, snap)
            } else {
                (
                    fast_host_status(data_dir, &cfg.engine),
                    Vec::new(),
                    HostStoreSnapshot::default(),
                )
            };

            let msg = format!(
                "健康巡检完成：本地桥接={}，宿主源={}，可更新插件={}，冲突={}",
                if st.daemon_ready { "在线" } else { "未启动" },
                sources.len(),
                store_snap.updates_available,
                store_snap.conflicts.len()
            );
            let _ = db::set_kv(&conn, "last_sync_summary", &msg);
            db::write_dashboard_file(
                data_dir,
                &conn,
                &cfg,
                settings,
                st.daemon_ready,
                st.active_port,
                if st.store_auth_ready {
                    Some(sources.len())
                } else {
                    None
                },
                if st.store_auth_ready {
                    Some(store_snap.updates_available)
                } else {
                    None
                },
            );
            Ok(json!({
                "ok": true,
                "notice": msg,
                "daemon_ready": st.daemon_ready,
                "store_auth_ready": st.store_auth_ready,
                "host_sources": sources.len(),
                "updates_available": store_snap.updates_available,
                "conflicts": store_snap.conflicts.len(),
            }))
        }

        // === Workbench Bootstrap ===
        "ui_bootstrap" | "dashboard" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            db::write_catalog_snapshot_file(data_dir, &cfg);
            Ok(build_full_bootstrap(data_dir, &conn, &cfg, settings, true))
        }

        // === Native Host `/api/plugin-store/sources` Management ===
        "ui_save_host_source" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let host = HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token)
                .context("未检测到宿主 API 环境，请在 Mediary 插件中心打开工作台或配置管理员 Token")?;

            let src = &input["source"];
            let raw_id = src["id"]
                .as_str()
                .or_else(|| input["id"].as_str())
                .unwrap_or_default();
            let name = src["name"]
                .as_str()
                .or_else(|| input["name"].as_str())
                .unwrap_or_default();
            let url = src["url"]
                .as_str()
                .or_else(|| input["url"].as_str())
                .unwrap_or_default();
            let enabled = src
                .get("enabled")
                .and_then(Value::as_bool)
                .or_else(|| input.get("enabled").and_then(Value::as_bool))
                .unwrap_or(true);

            let clean_id = sanitize_source_id(raw_id);
            host.upsert_store_source(&clean_id, name, url, enabled)?;
            db::append_log(
                &conn,
                if name.trim().is_empty() {
                    &clean_id
                } else {
                    name.trim()
                },
                "宿主插件源",
                &clean_id,
                "save_source",
                "success",
                &format!("URL={url} · enabled={enabled}"),
            );
            let _ = db::set_kv(&conn, "last_sync_summary", &format!("已保存插件源 {clean_id}"));
            Ok(json!({
                "notice": format!("已同步保存插件源「{}」至宿主插件中心", if name.trim().is_empty() { &clean_id } else { name.trim() }),
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        "ui_toggle_host_source" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let host = HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token)
                .context("未检测到宿主 API 环境")?;
            let id = input["id"].as_str().unwrap_or_default().trim();
            let enabled = input["enabled"].as_bool().unwrap_or(true);
            ensure!(!id.is_empty(), "缺少插件源 ID");
            ensure!(id != "official", "官方内置源不可停用或修改");

            let (_, active_port) = host::probe_daemon_health(data_dir, &cfg.engine);
            let base_bridge = cfg.engine.effective_bridge_base_url(active_port);
            let list = host.list_store_sources(&base_bridge, &cfg.engine.unified_source_id)?;
            let existing = list
                .into_iter()
                .find(|s| s.id == id)
                .with_context(|| format!("宿主中未找到插件源: {id}"))?;

            host.upsert_store_source(&existing.id, &existing.name, &existing.url, enabled)?;
            db::append_log(
                &conn,
                &existing.name,
                "宿主插件源",
                &existing.id,
                if enabled { "enable_source" } else { "disable_source" },
                "success",
                &existing.url,
            );
            Ok(json!({
                "notice": format!("插件源「{}」已{}", existing.name, if enabled { "启用" } else { "停用" }),
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        "ui_delete_host_source" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let host = HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token)
                .context("未检测到宿主 API 环境")?;
            let id = input["id"].as_str().unwrap_or_default().trim();
            host.delete_store_source(id)?;
            db::append_log(
                &conn,
                id,
                "宿主插件源",
                "-",
                "delete_source",
                "success",
                "已从 Mediary /api/plugin-store/sources 移除",
            );
            Ok(json!({
                "notice": format!("已从宿主插件中心移除插件源: {id}"),
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        "ui_register_local_bridge" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let host = HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token)
                .context("未检测到宿主 API 环境")?;
            let (_, active_port) = host::probe_daemon_health(data_dir, &cfg.engine);
            let base_bridge = cfg.engine.effective_bridge_base_url(active_port);

            let repo_id = input["repo_id"].as_str().unwrap_or_default().trim();
            let fallback_proxy = host.host_proxy_url();
            let gh = GithubClient::new(&cfg.engine, &fallback_proxy);
            let (src_id, src_name, local_url, gist_key, gist_fname, doc) = if repo_id.is_empty() {
                (
                    cfg.engine.unified_source_id.clone(),
                    cfg.engine.unified_source_name.clone(),
                    format!("{base_bridge}/catalog.json"),
                    "gist_id:unified".to_string(),
                    "mediplug-unified-catalog.json".to_string(),
                    db::build_unified_catalog(&cfg),
                )
            } else {
                let repo = cfg
                    .github_repos
                    .iter()
                    .find(|r| r.id == repo_id)
                    .with_context(|| format!("未找到 GitHub 转换仓库: {repo_id}"))?;
                let doc = db::build_repo_catalog(&cfg, repo_id)
                    .with_context(|| format!("构建仓库索引失败: {repo_id}"))?;
                (
                    repo.id.clone(),
                    if repo.name.trim().is_empty() {
                        format!("GitHub: {}", repo.repo)
                    } else {
                        repo.name.clone()
                    },
                    format!("{base_bridge}/catalog/{}.json", repo.id),
                    format!("gist_id:{}", repo.id),
                    format!("mediplug-{}.json", repo.id),
                    doc,
                )
            };

            let registered_url = register_bridge_source_smart(
                &conn,
                &host,
                &gh,
                &src_id,
                &src_name,
                &local_url,
                &gist_key,
                &gist_fname,
                &doc,
            )?;
            db::append_log(
                &conn,
                &src_name,
                "桥接源注册",
                &src_id,
                "register_bridge",
                "success",
                &registered_url,
            );
            Ok(json!({
                "notice": format!("已注册桥接源至宿主插件中心: {registered_url}"),
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        "ui_install_store_plugin" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let host = HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token)
                .context("未检测到宿主 API 环境")?;
            let plugin_id = input["plugin_id"].as_str().unwrap_or_default().trim();
            let res = host.install_store_plugin(plugin_id)?;
            db::append_log(
                &conn,
                plugin_id,
                "插件中心安装",
                res["version"].as_str().unwrap_or("latest"),
                "install_plugin",
                "success",
                "已调用宿主 /api/plugin-store/:id/install 完成校验热安装",
            );
            Ok(json!({
                "install_result": res,
                "notice": format!("插件「{plugin_id}」已通过宿主原生安装器完成下载校验与热注册（首次安装请前往「已安装」页确认权限启用）"),
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        // === Plain GitHub Release Repository Bridge ===
        "ui_save_github_repo" => {
            let mut conn = db::open_db(data_dir)?;
            let incoming_val = if input.get("repo").is_some_and(Value::is_object) {
                input["repo"].clone()
            } else {
                input.clone()
            };
            let mut incoming: GithubRepoBridge =
                serde_json::from_value(incoming_val).context("GitHub 仓库配置参数无效")?;

            let (owner, repo_name) = parse_github_repo_spec(&incoming.repo)?;
            incoming.repo = format!("{owner}/{repo_name}");
            let fallback_id = sanitize_source_id(&format!("gh-{owner}-{repo_name}"));
            incoming.id = if incoming.id.trim().is_empty() {
                fallback_id
            } else {
                sanitize_source_id(&incoming.id)
            };
            ensure!(
                !incoming.id.is_empty() && incoming.id != "official",
                "仓库源 ID 无效且不可为 official"
            );
            if incoming.name.trim().is_empty() {
                incoming.name = format!("{owner}/{repo_name}");
            } else {
                incoming.name = incoming.name.trim().to_string();
            }
            if incoming.register_mode != "dedicated" {
                incoming.register_mode = "unified".to_string();
            }
            incoming.pinned_tag = incoming.pinned_tag.trim().to_string();
            incoming.asset_keyword = incoming.asset_keyword.trim().to_string();

            let target_id = incoming.id.clone();
            let cfg = db::edit_config(&mut conn, true, |c| {
                if let Some(ex) = c.github_repos.iter_mut().find(|r| r.id == target_id) {
                    incoming.plugins = ex.plugins.clone();
                    incoming.last_release_tag = ex.last_release_tag.clone();
                    incoming.last_release_url = ex.last_release_url.clone();
                    incoming.last_synced_at = ex.last_synced_at;
                    incoming.last_error = ex.last_error.clone();
                    *ex = incoming;
                } else {
                    c.github_repos.push(incoming);
                }
                Ok(true)
            })?;
            db::write_catalog_snapshot_file(data_dir, &cfg);

            let sync_now = input
                .get("sync_now")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if sync_now {
                let res = sync_repos_workflow(
                    data_dir,
                    &mut conn,
                    Some(&target_id),
                    settings,
                    false,
                )?;
                let final_cfg = db::load_config(&conn)?;
                return Ok(json!({
                    "sync": res,
                    "notice": res["notice"].as_str().unwrap_or("已解析 GitHub 仓库并刷新本地 catalog.json"),
                    "bootstrap": build_full_bootstrap(data_dir, &conn, &final_cfg, settings, true),
                }));
            }

            Ok(json!({
                "notice": format!("已保存 GitHub 仓库配置: {owner}/{repo_name}"),
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        "ui_sync_github_repo" => {
            let mut conn = db::open_db(data_dir)?;
            let target = input["id"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty() && *s != "all");
            let res = sync_repos_workflow(data_dir, &mut conn, target, settings, false)?;
            let cfg = db::load_config(&conn)?;
            Ok(json!({
                "sync": res,
                "notice": res["notice"],
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        "ui_delete_github_repo" => {
            let mut conn = db::open_db(data_dir)?;
            let id = input["id"].as_str().unwrap_or_default().trim().to_string();
            ensure!(!id.is_empty(), "缺少仓库 ID");
            let mut was_dedicated = false;
            let mut removed_repo_name = id.clone();
            let cfg = db::edit_config(&mut conn, true, |c| {
                if let Some(ex) = c.github_repos.iter().find(|r| r.id == id) {
                    was_dedicated = ex.register_mode == "dedicated";
                    removed_repo_name = ex.repo.clone();
                }
                c.github_repos.retain(|r| r.id != id);
                Ok(true)
            })?;
            db::write_catalog_snapshot_file(data_dir, &cfg);

            if was_dedicated && input["remove_host_source"].as_bool() != Some(false) {
                if let Some(host) =
                    HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token)
                {
                    let _ = host.delete_store_source(&id);
                }
            }
            db::append_log(
                &conn,
                &removed_repo_name,
                "GitHub 桥接仓",
                &id,
                "delete_repo",
                "success",
                "已移除仓库桥接并刷新本地 catalog.json",
            );
            Ok(json!({
                "notice": format!("已移除 GitHub 转换仓库: {removed_repo_name}"),
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        "ui_preview_local_catalog" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let repo_id = input["repo_id"].as_str().unwrap_or_default().trim();
            let doc = if repo_id.is_empty() {
                db::build_unified_catalog(&cfg)
            } else {
                db::build_repo_catalog(&cfg, repo_id)
                    .with_context(|| format!("未找到仓库: {repo_id}"))?
            };
            Ok(json!({
                "filename": if repo_id.is_empty() { "catalog.json".to_string() } else { format!("{repo_id}-catalog.json") },
                "catalog": doc,
                "content": serde_json::to_string_pretty(&doc)?,
            }))
        }

        // === Package Validator & Publisher `catalog.json` Builder ===
        "ui_inspect_archive" => {
            let mut conn = db::open_db(data_dir)?;
            let filename = input["filename"]
                .as_str()
                .unwrap_or("plugin-linux-amd64.tar.gz")
                .trim();
            let dl_url = input["download_url"].as_str().unwrap_or_default().trim();
            let inspection = if let Some(b64) = input["file_base64"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
            {
                inspect_tar_gz_base64(filename, b64, dl_url)?
            } else if let Some(local_p) = input["local_path"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
            {
                inspect_tar_gz_file(Path::new(local_p.trim()), dl_url)?
            } else {
                bail!("请上传插件 .tar.gz 压缩包或填写本地文件路径");
            };

            let add_to_hosted = input["add_to_hosted"].as_bool() == Some(true);
            let mut cfg = db::load_config(&conn)?;
            if add_to_hosted {
                ensure!(
                    inspection.valid_for_mediary_346,
                    "该压缩包未通过 Mediary 3.4.6 安装规范校验，请先修复问题后再挂载到本地源"
                );
                if let Some(ref entry) = inspection.catalog_entry {
                    cfg = merge_entry_into_hosted(&mut conn, entry.clone())?;
                    db::write_catalog_snapshot_file(data_dir, &cfg);
                    db::append_log(
                        &conn,
                        &entry.name,
                        "本地构建托管",
                        &entry.version,
                        "host_entry",
                        "success",
                        &format!("{} · sha256={}...", inspection.arch, &inspection.sha256[..12]),
                    );
                }
            }

            Ok(json!({
                "inspection": inspection,
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, false),
            }))
        }

        "ui_delete_hosted_entry" => {
            let mut conn = db::open_db(data_dir)?;
            let id = input["id"].as_str().unwrap_or_default().trim();
            let clear_all = input["clear_all"].as_bool() == Some(true);
            let cfg = db::edit_config(&mut conn, true, |c| {
                if clear_all {
                    c.hosted_entries.clear();
                } else {
                    c.hosted_entries.retain(|e| e.id != id);
                }
                Ok(true)
            })?;
            db::write_catalog_snapshot_file(data_dir, &cfg);
            Ok(json!({
                "notice": "已更新本地托管插件条目",
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, false),
            }))
        }

        // === Engine Settings, Notifications, Backup & Logs ===
        "ui_save_engine" => {
            let mut conn = db::open_db(data_dir)?;
            let inc_val = if input.get("engine").is_some_and(Value::is_object) {
                input["engine"].clone()
            } else {
                input.clone()
            };
            let clear_admin = input["clear_admin_token"].as_bool() == Some(true);
            let clear_gh = input["clear_github_token"].as_bool() == Some(true);

            let cfg = db::edit_config(&mut conn, true, |c| {
                let mut next: EngineSettings =
                    serde_json::from_value(inc_val).context("全局设置参数格式无效")?;
                if clear_admin {
                    next.admin_api_token.clear();
                } else if is_masked_or_empty(&next.admin_api_token) {
                    next.admin_api_token = c.engine.admin_api_token.clone();
                }
                if clear_gh {
                    next.github_token.clear();
                } else if is_masked_or_empty(&next.github_token) {
                    next.github_token = c.engine.github_token.clone();
                }
                c.engine = next.normalized();
                Ok(true)
            })?;
            db::write_catalog_snapshot_file(data_dir, &cfg);
            Ok(json!({
                "notice": "全局与鉴权设置已保存生效",
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        "ui_test_notify" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let host = HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token)
                .context("未检测到宿主 API 环境变量")?;
            host.notify(
                "🔔 MediPlug 插件源通知已连通",
                "MediPlug 已成功接入 Mediary 主程序通知中心。\n开启后将在监控的 GitHub Release 仓库发布新版本时自动推送提醒。",
            )?;
            Ok(json!({
                "ok": true,
                "notice": "测试通知已提交至 Mediary 主程序通知中心，请查看企业微信 / Telegram / Bark",
            }))
        }

        "ui_clear_logs" => {
            let conn = db::open_db(data_dir)?;
            conn.execute("DELETE FROM activity_logs", [])?;
            if input["clear_cache"].as_bool() == Some(true) {
                conn.execute("DELETE FROM release_asset_cache", [])?;
            }
            let cfg = db::load_config(&conn)?;
            Ok(json!({
                "notice": if input["clear_cache"].as_bool() == Some(true) {
                    "已清空操作记录与 Release 校验缓存"
                } else {
                    "已清空操作历史记录"
                },
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, false),
            }))
        }

        "ui_export_config" => {
            let conn = db::open_db(data_dir)?;
            let mut cfg = db::load_config(&conn)?;
            let include_secrets = input["include_secrets"].as_bool() == Some(true);
            if !include_secrets {
                cfg.engine.admin_api_token = mask_secret(&cfg.engine.admin_api_token);
                cfg.engine.github_token = mask_secret(&cfg.engine.github_token);
            }
            Ok(json!({
                "filename": format!("mediplug-backup-{}.json", chrono::Local::now().format("%Y%m%d")),
                "content": serde_json::to_string_pretty(&cfg)?,
            }))
        }

        "ui_import_config" => {
            let mut conn = db::open_db(data_dir)?;
            let raw = input["content"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .context("导入配置 JSON 内容为空")?;
            let imported: AppConfig =
                serde_json::from_str(raw).context("解析导入的配置文件失败")?;
            let cfg = db::edit_config(&mut conn, true, |c| {
                let keep_admin = c.engine.admin_api_token.clone();
                let keep_gh = c.engine.github_token.clone();
                *c = imported;
                if is_masked_or_empty(&c.engine.admin_api_token) {
                    c.engine.admin_api_token = keep_admin;
                }
                if is_masked_or_empty(&c.engine.github_token) {
                    c.engine.github_token = keep_gh;
                }
                c.engine = c.engine.clone().normalized();
                Ok(true)
            })?;
            db::write_catalog_snapshot_file(data_dir, &cfg);
            Ok(json!({
                "notice": format!("成功导入配置：包含 {} 个 GitHub 转换仓库", cfg.github_repos.len()),
                "bootstrap": build_full_bootstrap(data_dir, &conn, &cfg, settings, true),
            }))
        }

        other => bail!("未知的插件动作: {other}"),
    }
}

fn register_bridge_source_smart(
    conn: &Connection,
    host: &HostClient,
    gh: &GithubClient,
    src_id: &str,
    src_name: &str,
    preferred_url: &str,
    gist_kv_key: &str,
    gist_filename: &str,
    doc: &models::CatalogDocument,
) -> Result<String> {
    match host.upsert_store_source(src_id, src_name, preferred_url, true) {
        Ok(_) => Ok(preferred_url.to_string()),
        Err(local_err) => {
            let err_msg = format!("{local_err:#}");
            if err_msg.contains("HTTPS")
                || err_msg.contains("https")
                || err_msg.contains("公网")
                || preferred_url.starts_with("http://127.0.0.1")
            {
                let gist_url = gh.publish_gist_catalog(
                    conn,
                    gist_kv_key,
                    gist_filename,
                    &format!("MediPlug Auto-Converted Catalog ({src_name})"),
                    doc,
                )?;
                host.upsert_store_source(src_id, src_name, &gist_url, true)?;
                return Ok(gist_url);
            }
            Err(local_err)
        }
    }
}

fn merge_entry_into_hosted(
    conn: &mut Connection,
    incoming: CatalogPluginEntry,
) -> Result<AppConfig> {
    db::edit_config(conn, true, |c| {
        if let Some(ex) = c.hosted_entries.iter_mut().find(|e| e.id == incoming.id) {
            let mut merged_arts = ex.artifacts.clone();
            for (arch, art) in incoming.artifacts {
                merged_arts.insert(arch, art);
            }
            *ex = CatalogPluginEntry {
                artifacts: merged_arts,
                ..incoming
            };
        } else {
            c.hosted_entries.push(incoming);
        }
        Ok(true)
    })
}

fn sync_repos_workflow(
    data_dir: &Path,
    conn: &mut Connection,
    specific_id: Option<&str>,
    settings: &PluginSettings,
    is_scheduled: bool,
) -> Result<Value> {
    let cfg = db::load_config(conn)?;
    let host = HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token);
    let fallback_proxy = host
        .as_ref()
        .map(HostClient::host_proxy_url)
        .unwrap_or_default();
    let gh = GithubClient::new(&cfg.engine, &fallback_proxy);

    let targets: Vec<GithubRepoBridge> = cfg
        .github_repos
        .iter()
        .filter(|r| {
            if let Some(id) = specific_id {
                r.id == id
            } else {
                r.enabled
            }
        })
        .cloned()
        .collect();

    if targets.is_empty() {
        return Ok(json!({
            "ok": true,
            "synced": 0,
            "failed": 0,
            "notice": "当前暂无待同步的 GitHub 仓库",
        }));
    }

    let mut old_versions: HashMap<String, String> = HashMap::new();
    for r in &cfg.github_repos {
        for p in &r.plugins {
            old_versions.insert(p.id.clone(), p.version.clone());
        }
    }

    let mut synced = 0usize;
    let mut failed = 0usize;
    let mut upgraded_notices: Vec<String> = Vec::new();
    let mut last_err_msg = String::new();

    for bridge in targets {
        match gh.sync_repo(conn, &bridge, &cfg.engine) {
            Ok((tag, rel_url, plugins)) => {
                synced += 1;
                for p in &plugins {
                    if let Some(prev_v) = old_versions.get(&p.id) {
                        if !prev_v.is_empty() && version_is_newer(&p.version, prev_v) {
                            upgraded_notices.push(format!("{} ({prev_v} -> {})", p.name, p.version));
                        }
                    }
                }
                let p_summary = plugins
                    .iter()
                    .map(|p| format!("{}@{}", p.id, p.version))
                    .collect::<Vec<_>>()
                    .join(", ");
                let bid = bridge.id.clone();
                let _ = db::edit_config(conn, true, |c| {
                    if let Some(ex) = c.github_repos.iter_mut().find(|r| r.id == bid) {
                        ex.last_release_tag = tag.clone();
                        ex.last_release_url = rel_url.clone();
                        ex.last_synced_at = now_ts();
                        ex.last_error.clear();
                        ex.plugins = plugins;
                    }
                    Ok(true)
                });
                db::append_log(
                    conn,
                    &bridge.repo,
                    "GitHub Release 转换",
                    &tag,
                    "sync_release",
                    "success",
                    &format!("解析成功: {p_summary}"),
                );
            }
            Err(err) => {
                failed += 1;
                let msg = format!("{err:#}");
                last_err_msg = msg.clone();
                let bid = bridge.id.clone();
                let _ = db::edit_config(conn, false, |c| {
                    if let Some(ex) = c.github_repos.iter_mut().find(|r| r.id == bid) {
                        ex.last_synced_at = now_ts();
                        ex.last_error = msg.clone();
                    }
                    Ok(true)
                });
                db::append_log(
                    conn,
                    &bridge.repo,
                    "GitHub Release 转换",
                    "-",
                    "sync_release",
                    "error",
                    &msg,
                );
            }
        }
    }

    let updated_cfg = db::load_config(conn)?;
    db::write_catalog_snapshot_file(data_dir, &updated_cfg);

    // Auto-register unified/dedicated bridge sources into Mediary `/api/plugin-store/sources`
    let mut registered_sources = 0usize;
    if settings.auto_register_bridge && synced > 0 {
        if let Some(ref h) = host {
            let (_, active_port) = host::probe_daemon_health(data_dir, &updated_cfg.engine);
            let base_bridge = updated_cfg.engine.effective_bridge_base_url(active_port);

            let has_unified = updated_cfg
                .github_repos
                .iter()
                .any(|r| r.enabled && r.register_mode != "dedicated" && !r.plugins.is_empty())
                || !updated_cfg.hosted_entries.is_empty();

            if has_unified {
                let doc = db::build_unified_catalog(&updated_cfg);
                if register_bridge_source_smart(
                    conn,
                    h,
                    &gh,
                    &updated_cfg.engine.unified_source_id,
                    &updated_cfg.engine.unified_source_name,
                    &format!("{base_bridge}/catalog.json"),
                    "gist_id:unified",
                    "mediplug-unified-catalog.json",
                    &doc,
                )
                .is_ok()
                {
                    registered_sources += 1;
                }
            }

            for r in updated_cfg
                .github_repos
                .iter()
                .filter(|r| r.enabled && r.register_mode == "dedicated" && !r.plugins.is_empty())
            {
                let sname = if r.name.trim().is_empty() {
                    format!("GitHub: {}", r.repo)
                } else {
                    r.name.clone()
                };
                if let Some(doc) = db::build_repo_catalog(&updated_cfg, &r.id) {
                    if register_bridge_source_smart(
                        conn,
                        h,
                        &gh,
                        &r.id,
                        &sname,
                        &format!("{base_bridge}/catalog/{}.json", r.id),
                        &format!("gist_id:{}", r.id),
                        &format!("mediplug-{}.json", r.id),
                        &doc,
                    )
                    .is_ok()
                    {
                        registered_sources += 1;
                    }
                }
            }
        }
    }

    if settings.notify && !upgraded_notices.is_empty() {
        if let Some(ref h) = host {
            let _ = h.notify(
                "🚀 MediPlug 发现第三方插件新版本",
                &format!(
                    "以下接入插件已同步最新 Release，可直接前往 Mediary「插件中心」一键热升级：\n• {}",
                    upgraded_notices.join("\n• ")
                ),
            );
        }
    }

    if specific_id.is_some() && synced == 0 && failed > 0 {
        bail!("同步 GitHub Release 失败: {last_err_msg}");
    }

    let summary_text = if failed == 0 {
        if registered_sources > 0 {
            format!("成功同步 {synced} 个 GitHub 仓库并自动注册至宿主「插件中心」")
        } else {
            format!("成功同步 {synced} 个 GitHub 仓库，本地 catalog.json 已就绪")
        }
    } else {
        format!("同步完成：成功 {synced} 个，失败 {failed} 个（{last_err_msg}）")
    };
    let _ = db::set_kv(conn, "last_sync_summary", &summary_text);

    if is_scheduled {
        let (daemon_ready, active_port) = host::probe_daemon_health(data_dir, &updated_cfg.engine);
        db::write_dashboard_file(
            data_dir,
            conn,
            &updated_cfg,
            settings,
            daemon_ready,
            active_port,
            None,
            None,
        );
    }

    Ok(json!({
        "ok": failed == 0,
        "synced": synced,
        "failed": failed,
        "registered_sources": registered_sources,
        "upgraded": upgraded_notices,
        "notice": summary_text,
    }))
}

fn build_full_bootstrap(
    data_dir: &Path,
    conn: &Connection,
    cfg: &AppConfig,
    settings: &PluginSettings,
    fetch_store: bool,
) -> Value {
    let host = HostClient::from_env_with_admin(data_dir, &cfg.engine.admin_api_token);
    let (host_status, host_sources, store_snapshot) = if let Some(ref h) = host {
        let (st, srcs) = h.inspect_status(data_dir, &cfg.engine);
        let snap = if fetch_store && st.store_auth_ready {
            h.fetch_plugin_store().unwrap_or_default()
        } else {
            HostStoreSnapshot::default()
        };
        (st, srcs, snap)
    } else {
        (
            fast_host_status(data_dir, &cfg.engine),
            Vec::new(),
            HostStoreSnapshot::default(),
        )
    };

    db::write_dashboard_file(
        data_dir,
        conn,
        cfg,
        settings,
        host_status.daemon_ready,
        host_status.active_port,
        if host_status.store_auth_ready {
            Some(host_sources.len())
        } else {
            None
        },
        if fetch_store && host_status.store_auth_ready {
            Some(store_snapshot.updates_available)
        } else {
            None
        },
    );

    let mut masked_engine = cfg.engine.clone();
    masked_engine.admin_api_token = mask_secret(&masked_engine.admin_api_token);
    masked_engine.github_token = mask_secret(&masked_engine.github_token);

    let unified_doc = db::build_unified_catalog(cfg);
    let logs = db::list_logs(conn, 120, "");
    let last_sync_summary =
        db::get_kv(conn, "last_sync_summary").unwrap_or_else(|| "就绪".to_string());

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "settings": {
            "$enabled": settings.enabled,
            "auto_sync_releases": settings.auto_sync_releases,
            "auto_register_bridge": settings.auto_register_bridge,
            "notify": settings.notify,
        },
        "host_status": host_status,
        "engine": masked_engine,
        "host_sources": host_sources,
        "github_repos": cfg.github_repos,
        "hosted_entries": cfg.hosted_entries,
        "unified_catalog": unified_doc,
        "store": store_snapshot,
        "logs": logs,
        "last_sync_summary": last_sync_summary,
    })
}

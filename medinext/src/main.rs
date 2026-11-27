mod db;
mod engine;
mod hook;
mod host;
mod migrate;
mod models;
mod p115;
mod p115_crypto;

use anyhow::{bail, ensure, Context, Result};
use host::{fast_host_status, HostClient, HostStatus};
use models::{
    mask_secret, now_ts, Account115, AppConfig, EngineSettings, PluginSettings, PolicyTemplate,
    ResolveRequest, RiskConfig, UserPolicy,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::env;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_STDIN_BYTES: u64 = 16 * 1024 * 1024;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = env::args().collect();
    let data_dir = env::var_os("MEDIARY_PLUGIN_DATA_DIR")
        .map_or_else(|| PathBuf::from("data"), PathBuf::from);
    let mut settings: PluginSettings = env::var("MEDIARY_PLUGIN_SETTINGS_JSON")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let (plugin_enabled, _, _, _, _) = host::inspect_local_plugin_state(&data_dir);
    settings.enabled = plugin_enabled && settings.enabled;

    if args.get(1).map(String::as_str) == Some("daemon") {
        match hook::run_daemon(data_dir, settings) {
            Ok(()) => return std::process::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("[medinext] playback_hook 常驻进程异常退出: {err:#}");
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
        "请求载荷超过 16 MB 上限"
    );
    let input: Value = if stdin_buf.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        serde_json::from_slice(&stdin_buf).context("输入参数不是合法的 JSON")?
    };

    let res = dispatch(data_dir, &action, &input, settings)?;
    if let Ok(conn) = db::open_db(data_dir) {
        if let Ok(cfg) = db::load_config(&conn) {
            db::write_dashboard_file(data_dir, &conn, &cfg, settings);
        }
    }
    Ok(res)
}

fn dispatch(
    data_dir: &Path,
    action: &str,
    input: &Value,
    settings: &PluginSettings,
) -> Result<Value> {
    match action {
        // === Internal / CLI Playback Decision & Lifecycle Actions ===
        "resolve_302" | "playback_resolve" => {
            let req: ResolveRequest =
                serde_json::from_value(input.clone()).context("解析 resolve_302 请求参数失败")?;
            let decision = engine::resolve_playback(data_dir, settings, req)?;
            Ok(serde_json::to_value(decision)?)
        }

        "prefetch_302" | "playback_prefetch" => {
            let mut req: ResolveRequest =
                serde_json::from_value(input.clone()).context("解析 prefetch_302 请求参数失败")?;
            req.prefetch = true;
            let decision = engine::resolve_playback(data_dir, settings, req)?;
            Ok(serde_json::to_value(decision)?)
        }

        "playback_event" => {
            let conn = db::open_db(data_dir)?;
            let ev = input["event"]
                .as_str()
                .or_else(|| input["type"].as_str())
                .unwrap_or("playback.progress");
            let username = input["username"].as_str().unwrap_or_default().trim();
            let dev_id = input["device_id"].as_str().unwrap_or_default().trim();
            let host_skey = input["session_key"]
                .as_str()
                .or_else(|| input["session_id"].as_str())
                .unwrap_or_default()
                .trim();
            if ev == "playback.stopped" || ev == "stop" {
                if !host_skey.is_empty() {
                    let _ = conn.execute(
                        "DELETE FROM active_sessions WHERE host_session_key = ?1",
                        params![host_skey],
                    );
                }
                if !username.is_empty() && !dev_id.is_empty() {
                    let key = format!("{username}:{dev_id}");
                    let _ = conn.execute(
                        "DELETE FROM active_sessions WHERE session_key = ?1",
                        params![key],
                    );
                }
            } else if !username.is_empty() {
                db::touch_active_session(
                    &conn,
                    host_skey,
                    username,
                    input["user_id"].as_str().unwrap_or_default(),
                    dev_id,
                    input["device_name"].as_str().unwrap_or_default(),
                    input["client"].as_str().unwrap_or_default(),
                    input["user_agent"].as_str().unwrap_or_default(),
                    input["client_ip"].as_str().unwrap_or_default(),
                    input["item_id"].as_str().unwrap_or_default(),
                    input["item_name"].as_str().unwrap_or_default(),
                    input["provider"].as_str().unwrap_or_default(),
                );
            }
            Ok(json!({ "ok": true, "handled": true }))
        }

        // === Scheduled Actions ===
        "tick" => {
            let mut conn = db::open_db(data_dir)?;
            engine::run_scheduled_tick(&mut conn, settings)
        }

        "health_check" => {
            let mut conn = db::open_db(data_dir)?;
            engine::run_health_check(&mut conn, settings)
        }

        // === Workbench UI Actions ===
        "ui_bootstrap" | "dashboard" => {
            let mut conn = db::open_db(data_dir)?;
            db::prune_stale_data(&conn);
            let host = HostClient::from_env();
            engine::sync_active_sessions_now(&conn, host.as_ref(), 0);
            let cfg = db::edit_config(&mut conn, false, |c| Ok(engine::sync_same_uid_lb_quotas(c)))
                .unwrap_or_else(|_| db::load_config(&conn).unwrap_or_default());
            let valid_lb_names: Vec<String> = cfg.lb_accounts.iter().map(|a| a.name.clone()).collect();
            db::prune_orphaned_lb_states(&conn, &valid_lb_names);
            let host_status = host
                .map(|h| h.host_status(data_dir))
                .unwrap_or_else(|| fast_host_status(data_dir));
            Ok(build_bootstrap(&conn, &cfg, settings, host_status))
        }

        "ui_save_engine" => {
            let mut conn = db::open_db(data_dir)?;
            let incoming = &input["engine"];
            let cfg = db::edit_config(&mut conn, true, |c| {
                let mut next: EngineSettings =
                    serde_json::from_value(incoming.clone()).context("引擎设置格式无效")?;
                next.cache_ttl_secs = next.cache_ttl_secs.clamp(60, 7200);
                next.p115_cooldown_ms = next.p115_cooldown_ms.min(5000);
                c.engine = next;
                Ok(true)
            })?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        // === 115 QR Login & Account Pool Management ===
        "ui_qr_start" => {
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let app = input["app"].as_str().unwrap_or("tv");
            p115::qr_start(app, &cfg.engine.p115_proxy_url)
        }

        "ui_qr_poll" => {
            let mut conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            let uid = input["uid"].as_str().unwrap_or_default();
            let time_val = input["time"].as_str().unwrap_or_default();
            let sign = input["sign"].as_str().unwrap_or_default();
            let app = input["app"].as_str().unwrap_or("tv");
            let poll_res = p115::qr_poll(uid, time_val, sign, app, &cfg.engine.p115_proxy_url)?;

            if poll_res["status"].as_str() == Some("success") {
                let bind_kind = input["bind_kind"].as_str().unwrap_or_default();
                let bind_name = input["bind_name"].as_str().unwrap_or_default().trim();
                let cookie_str = poll_res["cookie"].as_str().unwrap_or_default().to_string();
                let acc_token = poll_res["access_token"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let ref_token = poll_res["refresh_token"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let p_uid = poll_res["p115_uid"].as_str().unwrap_or_default().to_string();

                if !bind_name.is_empty() && (!cookie_str.is_empty() || !acc_token.is_empty()) {
                    let updated = db::edit_config(&mut conn, true, |c| {
                        match bind_kind {
                            "source" => {
                                if let Some(ex) =
                                    c.source_accounts.iter_mut().find(|a| a.name == bind_name)
                                {
                                    ex.cookies = cookie_str;
                                    ex.access_token = acc_token;
                                    ex.refresh_token = ref_token;
                                    ex.uid = p_uid;
                                    ex.app = app.to_string();
                                    ex.valid = true;
                                    ex.enabled = true;
                                } else {
                                    c.source_accounts.push(Account115 {
                                        name: bind_name.to_string(),
                                        role: "source".to_string(),
                                        cookies: cookie_str,
                                        access_token: acc_token,
                                        refresh_token: ref_token,
                                        uid: p_uid,
                                        app: app.to_string(),
                                        enabled: true,
                                        valid: true,
                                        ..Default::default()
                                    });
                                }
                            }
                            "lb" => {
                                if let Some(ex) =
                                    c.lb_accounts.iter_mut().find(|a| a.name == bind_name)
                                {
                                    ex.cookies = cookie_str;
                                    ex.access_token = acc_token;
                                    ex.refresh_token = ref_token;
                                    ex.uid = p_uid;
                                    ex.app = app.to_string();
                                    ex.valid = true;
                                    ex.enabled = true;
                                } else {
                                    c.lb_accounts.push(Account115 {
                                        name: bind_name.to_string(),
                                        role: "lb".to_string(),
                                        cookies: cookie_str,
                                        access_token: acc_token,
                                        refresh_token: ref_token,
                                        uid: p_uid,
                                        app: app.to_string(),
                                        cache_path: "/Next".to_string(),
                                        quota_enabled: true,
                                        quota_gb: 5120,
                                        enabled: true,
                                        valid: true,
                                        ..Default::default()
                                    });
                                }
                            }
                            "user" => {
                                if let Some(u) = c
                                    .users
                                    .iter_mut()
                                    .find(|x| x.username.eq_ignore_ascii_case(bind_name))
                                {
                                    u.cookies = cookie_str;
                                    u.access_token = acc_token;
                                    u.p115_uid = p_uid;
                                    u.route_mode = "own_cookie".to_string();
                                }
                            }
                            _ => {}
                        }
                        Ok(true)
                    })?;
                    return Ok(json!({
                        "poll": poll_res,
                        "bound": true,
                        "bootstrap": build_bootstrap(&conn, &updated, settings, fast_host_status(data_dir)),
                    }));
                }
            }
            Ok(json!({ "poll": poll_res, "bound": false }))
        }

        "ui_save_account" => {
            let mut conn = db::open_db(data_dir)?;
            let mut acc: Account115 =
                serde_json::from_value(input["account"].clone()).context("账号参数无效")?;
            acc.name = acc.name.trim().to_string();
            ensure!(!acc.name.is_empty(), "账号名称不能为空");
            let orig_name = input["original_name"]
                .as_str()
                .unwrap_or(&acc.name)
                .trim()
                .to_string();
            let is_lb = acc.role == "lb";

            let cfg = db::edit_config(&mut conn, true, |c| {
                let list = if is_lb {
                    &mut c.lb_accounts
                } else {
                    &mut c.source_accounts
                };
                if let Some(existing) = list.iter_mut().find(|a| a.name == orig_name) {
                    if acc.cookies.trim().is_empty() || acc.cookies.contains("******") {
                        acc.cookies = existing.cookies.clone();
                    }
                    if acc.access_token.trim().is_empty() || acc.access_token.contains("******") {
                        acc.access_token = existing.access_token.clone();
                        acc.refresh_token = existing.refresh_token.clone();
                    }
                    if acc.recycle_bin_code.contains("******") {
                        acc.recycle_bin_code = existing.recycle_bin_code.clone();
                    }
                    if acc.quota_usage_bytes == 0 && existing.quota_usage_bytes > 0 {
                        acc.quota_usage_bytes = existing.quota_usage_bytes;
                    }
                    if acc.uid.is_empty() {
                        acc.uid = p115::extract_uid_from_cookie(&acc.cookies);
                        if acc.uid.is_empty() {
                            acc.uid = existing.uid.clone();
                        }
                    }
                    acc.valid = true;
                    *existing = acc;
                } else {
                    if acc.uid.is_empty() {
                        acc.uid = p115::extract_uid_from_cookie(&acc.cookies);
                    }
                    if is_lb && acc.cache_path.trim().is_empty() {
                        acc.cache_path = "/Next".to_string();
                    }
                    acc.valid = true;
                    list.push(acc);
                }
                Ok(true)
            })?;
            if is_lb {
                db::mark_lb_cooldown(&conn, &orig_name, 0, "");
            }
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_delete_account" => {
            let mut conn = db::open_db(data_dir)?;
            let name = input["name"].as_str().unwrap_or_default().trim();
            let role = input["role"].as_str().unwrap_or("lb");
            let cfg = db::edit_config(&mut conn, true, |c| {
                if role == "source" {
                    c.source_accounts.retain(|a| a.name != name);
                } else {
                    c.lb_accounts.retain(|a| a.name != name);
                }
                Ok(true)
            })?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_probe_account" => {
            let mut conn = db::open_db(data_dir)?;
            let name = input["name"].as_str().unwrap_or_default().trim();
            let role = input["role"].as_str().unwrap_or("lb");
            let cfg = db::load_config(&conn)?;
            let list = if role == "source" {
                &cfg.source_accounts
            } else {
                &cfg.lb_accounts
            };
            let acc = list
                .iter()
                .find(|a| a.name == name)
                .context("未找到指定 115 账号")?
                .clone();
            let proxy = if !acc.proxy_url.is_empty() {
                acc.proxy_url.as_str()
            } else {
                cfg.engine.p115_proxy_url.as_str()
            };
            let (creds, drive_label) = p115::fetch_upload_credentials(
                &acc.cookies,
                proxy,
                cfg.engine.p115_cooldown_ms.min(300),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            let _ = db::set_kv(
                &conn,
                &format!("ucreds:{}", creds.user_id),
                &serde_json::to_string(&creds).unwrap_or_default(),
            );
            db::mark_lb_cooldown(&conn, name, 0, "");
            let updated = db::edit_config(&mut conn, false, |c| {
                let target_list = if role == "source" {
                    &mut c.source_accounts
                } else {
                    &mut c.lb_accounts
                };
                if let Some(a) = target_list.iter_mut().find(|x| x.name == name) {
                    a.uid = creds.user_id.clone();
                    a.user_key = creds.user_key.clone();
                    a.valid = true;
                    if !drive_label.is_empty() {
                        a.drive_info = drive_label.clone();
                    }
                }
                engine::sync_same_uid_lb_quotas(c);
                Ok(true)
            })?;
            Ok(json!({
                "notice": format!("探活成功：UID={} {}", creds.user_id, drive_label),
                "bootstrap": build_bootstrap(&conn, &updated, settings, fast_host_status(data_dir)),
            }))
        }

        "ui_reset_cooldown" => {
            let conn = db::open_db(data_dir)?;
            let name = input["name"].as_str().unwrap_or_default().trim();
            db::mark_lb_cooldown(&conn, name, 0, "");
            let cfg = db::load_config(&conn)?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_clean_quota" => {
            let mut conn = db::open_db(data_dir)?;
            let name = input["name"].as_str().unwrap_or_default().trim().to_string();
            let force_all = input["force_all"].as_bool() == Some(true);
            let cfg = db::load_config(&conn)?;
            let lb = cfg
                .lb_accounts
                .iter()
                .find(|a| a.name == name)
                .context("未找到指定负载均衡账号")?
                .clone();
            let proxy = if !lb.proxy_url.is_empty() {
                lb.proxy_url.as_str()
            } else {
                cfg.engine.p115_proxy_url.as_str()
            };
            let limit = lb.quota_gb.saturating_mul(1024 * 1024 * 1024);
            let (del_cnt, freed, rb_ok) = engine::clean_target_quota(
                &mut conn,
                &lb.name,
                &lb.cookies,
                &lb.access_token,
                &lb.uid,
                &lb.recycle_bin_code,
                proxy,
                cfg.engine.p115_cooldown_ms,
                lb.quota_usage_bytes,
                limit,
                force_all,
            )?;
            let eff_uid = if !lb.uid.trim().is_empty() {
                lb.uid.trim().to_string()
            } else {
                p115::extract_uid_from_cookie(&lb.cookies)
            };
            let updated = db::edit_config(&mut conn, false, |c| {
                for a in &mut c.lb_accounts {
                    let a_uid = if !a.uid.trim().is_empty() {
                        a.uid.trim().to_string()
                    } else {
                        p115::extract_uid_from_cookie(&a.cookies)
                    };
                    if a.name == name || (!eff_uid.is_empty() && a_uid == eff_uid) {
                        if force_all {
                            a.quota_usage_bytes = 0;
                        } else {
                            a.quota_usage_bytes = a.quota_usage_bytes.saturating_sub(freed);
                        }
                    }
                }
                Ok(true)
            })?;
            Ok(json!({
                "notice": format!("清理完成：删除 {del_cnt} 个转存文件，回收站清空={}", if rb_ok { "成功" } else { "跳过/无需" }),
                "bootstrap": build_bootstrap(&conn, &updated, settings, fast_host_status(data_dir)),
            }))
        }

        // === Users & Policy Templates (Native Mediary 3.4.6 `media-service:read`) ===
        "ui_sync_users" => {
            let mut conn = db::open_db(data_dir)?;
            let host = HostClient::from_env()
                .context("当前未注入宿主 API 凭据，请在 Mediary 插件工作台内操作")?;
            let host_users = host.list_users()?;
            if let Ok(raw) = serde_json::to_string(&host_users) {
                let _ = db::set_kv(&conn, "host_users_cache", &raw);
            }
            let count = host_users.len();
            let updated = db::edit_config(&mut conn, true, |c| {
                let default_tpl = c
                    .templates
                    .first()
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                for hu in host_users {
                    if let Some(ex) = c
                        .users
                        .iter_mut()
                        .find(|u| u.username.eq_ignore_ascii_case(&hu.username))
                    {
                        if !hu.user_id.is_empty() {
                            ex.emby_id = hu.user_id;
                        }
                        ex.is_admin = hu.is_admin;
                        ex.is_disabled = hu.is_disabled;
                    } else {
                        c.users.push(UserPolicy {
                            username: hu.username,
                            emby_id: hu.user_id,
                            is_admin: hu.is_admin,
                            is_disabled: hu.is_disabled,
                            route_mode: if hu.is_admin {
                                "direct".to_string()
                            } else {
                                "template".to_string()
                            },
                            template: default_tpl.clone(),
                            cache_path: "/EmbyCache".to_string(),
                            ..Default::default()
                        });
                    }
                }
                Ok(true)
            })?;
            Ok(json!({
                "notice": format!("已从 Mediary 原生媒体服务同步 {count} 位观影用户"),
                "bootstrap": build_bootstrap(&conn, &updated, settings, fast_host_status(data_dir)),
            }))
        }

        "ui_save_user" => {
            let mut conn = db::open_db(data_dir)?;
            let mut user: UserPolicy =
                serde_json::from_value(input["user"].clone()).context("用户参数无效")?;
            user.username = user.username.trim().to_string();
            ensure!(!user.username.is_empty(), "用户名不能为空");
            let cfg = db::edit_config(&mut conn, true, |c| {
                if let Some(ex) = c
                    .users
                    .iter_mut()
                    .find(|x| x.username.eq_ignore_ascii_case(&user.username))
                {
                    if user.cookies.trim().is_empty() || user.cookies.contains("******") {
                        if input["clear_cookie"].as_bool() != Some(true) {
                            user.cookies = ex.cookies.clone();
                            user.access_token = ex.access_token.clone();
                        }
                    }
                    if user.p115_uid.is_empty() {
                        user.p115_uid = p115::extract_uid_from_cookie(&user.cookies);
                    }
                    *ex = user;
                } else {
                    user.p115_uid = p115::extract_uid_from_cookie(&user.cookies);
                    c.users.push(user);
                }
                Ok(true)
            })?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_delete_user" => {
            let mut conn = db::open_db(data_dir)?;
            let username = input["username"].as_str().unwrap_or_default().trim();
            let cfg = db::edit_config(&mut conn, true, |c| {
                c.users
                    .retain(|u| !u.username.eq_ignore_ascii_case(username));
                Ok(true)
            })?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_save_template" => {
            let mut conn = db::open_db(data_dir)?;
            let mut tpl: PolicyTemplate =
                serde_json::from_value(input["template"].clone()).context("策略模板格式无效")?;
            tpl.name = tpl.name.trim().to_string();
            ensure!(!tpl.name.is_empty(), "模板名称不能为空");
            let cfg = db::edit_config(&mut conn, true, |c| {
                if let Some(ex) = c.templates.iter_mut().find(|t| t.name == tpl.name) {
                    *ex = tpl;
                } else {
                    c.templates.push(tpl);
                }
                Ok(true)
            })?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_delete_template" => {
            let mut conn = db::open_db(data_dir)?;
            let name = input["name"].as_str().unwrap_or_default().trim();
            let cfg = db::edit_config(&mut conn, true, |c| {
                ensure!(c.templates.len() > 1, "至少需保留 1 个默认策略模板");
                c.templates.retain(|t| t.name != name);
                Ok(true)
            })?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        // === Risk Control, Sessions, UA & Cache ===
        "ui_save_risk" => {
            let mut conn = db::open_db(data_dir)?;
            let risk: RiskConfig =
                serde_json::from_value(input["risk"].clone()).context("风控配置格式无效")?;
            let cfg = db::edit_config(&mut conn, true, |c| {
                c.risk = risk;
                Ok(true)
            })?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_toggle_ua_ban" => {
            let conn = db::open_db(data_dir)?;
            let ua_name = input["ua_name"].as_str().unwrap_or_default().trim();
            let banned = input["banned"].as_bool() == Some(true);
            ensure!(!ua_name.is_empty(), "缺少 ua_name");
            conn.execute(
                "INSERT INTO ua_stats (ua_name, hit_count, last_seen_at, is_banned)
                 VALUES (?1, 0, ?2, ?3)
                 ON CONFLICT(ua_name) DO UPDATE SET is_banned = excluded.is_banned",
                params![ua_name, now_ts(), if banned { 1 } else { 0 }],
            )?;
            let cfg = db::load_config(&conn)?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_toggle_device_ban" => {
            let conn = db::open_db(data_dir)?;
            let username = input["username"].as_str().unwrap_or_default().trim();
            let device_id = input["device_id"].as_str().unwrap_or_default().trim();
            let banned = input["banned"].as_bool() == Some(true);
            if banned {
                conn.execute(
                    "UPDATE known_devices SET is_banned = 1 WHERE username = ?1 AND device_id = ?2",
                    params![username, device_id],
                )?;
                let skey = format!("{username}:{device_id}");
                let host = HostClient::from_env();
                let _ = engine::kick_session_everywhere(&conn, host.as_ref(), &skey);
            } else if input["remove"].as_bool() == Some(true) {
                conn.execute(
                    "DELETE FROM known_devices WHERE username = ?1 AND device_id = ?2",
                    params![username, device_id],
                )?;
            } else {
                conn.execute(
                    "UPDATE known_devices SET is_banned = 0 WHERE username = ?1 AND device_id = ?2",
                    params![username, device_id],
                )?;
            }
            let cfg = db::load_config(&conn)?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_kick_session" => {
            let conn = db::open_db(data_dir)?;
            let session_key = input["session_key"].as_str().unwrap_or_default().trim();
            let host = HostClient::from_env();
            let (_host_kicked, notice) =
                engine::kick_session_everywhere(&conn, host.as_ref(), session_key);
            let cfg = db::load_config(&conn)?;
            Ok(json!({
                "notice": notice,
                "bootstrap": build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)),
            }))
        }

        "ui_test_resolve" => {
            let req: ResolveRequest =
                serde_json::from_value(input["request"].clone()).context("测试请求格式无效")?;
            let decision = engine::resolve_playback(data_dir, settings, req)?;
            let conn = db::open_db(data_dir)?;
            let cfg = db::load_config(&conn)?;
            Ok(json!({
                "decision": decision,
                "bootstrap": build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)),
            }))
        }

        "ui_test_notify" => {
            let host = HostClient::from_env().ok_or_else(|| {
                anyhow::anyhow!("未检测到宿主 API 环境变量，请通过 Mediary 插件中心调用")
            })?;
            host.notify_with_image(
                "🔔 MediNext 播放与风控通知已连通",
                "MediNext 已成功接入 Mediary 主程序通知中心。\n开启后将自动推送多号负载播放调度与风控拦截告警。",
                None,
            )?;
            Ok(json!({
                "ok": true,
                "notice": "测试通知已提交至 Mediary 主程序通知中心，请查看企业微信/Telegram/Bark",
            }))
        }

        "ui_clear_cache" => {
            let conn = db::open_db(data_dir)?;
            let target = input["target"].as_str().unwrap_or("links");
            match target {
                "sha1_pool" => {
                    conn.execute("DELETE FROM sha1_pool", [])?;
                }
                "all" => {
                    conn.execute("DELETE FROM link_cache", [])?;
                    conn.execute("DELETE FROM sha1_pool", [])?;
                }
                _ => {
                    conn.execute("DELETE FROM link_cache", [])?;
                }
            }
            let cfg = db::load_config(&conn)?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        "ui_clear_history" => {
            let conn = db::open_db(data_dir)?;
            conn.execute("DELETE FROM playback_history", [])?;
            let cfg = db::load_config(&conn)?;
            Ok(build_bootstrap(&conn, &cfg, settings, fast_host_status(data_dir)))
        }

        // === NextEmby One-Click Import & Config Export ===
        "ui_import_nextemby" => {
            let conn = db::open_db(data_dir)?;
            let current = db::load_config(&conn)?;
            let (merged, summary) = migrate::import_nextemby_payload(
                data_dir,
                input["source_path"].as_str(),
                input["file_base64"].as_str(),
                input["filename"].as_str(),
                current,
            )?;
            db::save_config(&conn, &merged)?;
            Ok(json!({
                "summary": summary,
                "bootstrap": build_bootstrap(&conn, &merged, settings, fast_host_status(data_dir)),
            }))
        }

        "ui_export_config" => {
            let conn = db::open_db(data_dir)?;
            let mut cfg = db::load_config(&conn)?;
            let include_secrets = input["include_secrets"].as_bool() == Some(true);
            if !include_secrets {
                for a in cfg
                    .source_accounts
                    .iter_mut()
                    .chain(cfg.lb_accounts.iter_mut())
                {
                    a.cookies = mask_secret(&a.cookies);
                    a.access_token = mask_secret(&a.access_token);
                    a.refresh_token = String::new();
                    a.recycle_bin_code = mask_secret(&a.recycle_bin_code);
                }
                for u in &mut cfg.users {
                    u.cookies = mask_secret(&u.cookies);
                    u.access_token = mask_secret(&u.access_token);
                }
            }
            Ok(json!({
                "filename": format!("medinext-backup-{}.json", chrono::Local::now().format("%Y%m%d")),
                "content": serde_json::to_string_pretty(&cfg)?,
            }))
        }

        other => bail!("未知的插件动作: {other}"),
    }
}

fn build_bootstrap(
    conn: &rusqlite::Connection,
    cfg: &AppConfig,
    settings: &PluginSettings,
    host_status: HostStatus,
) -> Value {
    let lb_states = db::load_lb_states(conn);
    let active_by_prov = db::active_sessions_by_provider(conn, "");
    let sessions = db::list_active_sessions(conn);

    let mut active_by_user: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for s in &sessions {
        if let Some(u) = s["username"].as_str() {
            *active_by_user.entry(u.to_ascii_lowercase()).or_insert(0) += 1;
        }
    }

    let devices = db::list_known_devices(conn);
    let mut dev_count_by_user: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for d in &devices {
        if d["is_banned"].as_bool() != Some(true) {
            if let Some(u) = d["username"].as_str() {
                *dev_count_by_user.entry(u.to_ascii_lowercase()).or_insert(0) += 1;
            }
        }
    }

    let source_views: Vec<Value> = cfg
        .source_accounts
        .iter()
        .map(|a| {
            let act = *active_by_prov.get(&a.name).unwrap_or(&0);
            a.sanitized_view(0, "", 0, act)
        })
        .collect();

    let lb_views: Vec<Value> = cfg
        .lb_accounts
        .iter()
        .map(|a| {
            let st = lb_states.get(&a.name);
            let act = *active_by_prov.get(&a.name).unwrap_or(&0);
            a.sanitized_view(
                st.map(|s| s.cooldown_until).unwrap_or(0),
                st.map(|s| s.last_error.as_str()).unwrap_or(""),
                st.map(|s| s.call_count).unwrap_or(0),
                act,
            )
        })
        .collect();

    let user_views: Vec<Value> = cfg
        .users
        .iter()
        .map(|u| {
            let key = u.username.to_ascii_lowercase();
            u.sanitized_view(
                *active_by_user.get(&key).unwrap_or(&0),
                *dev_count_by_user.get(&key).unwrap_or(&0),
            )
        })
        .collect();

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "settings": settings,
        "host_status": host_status,
        "engine": cfg.engine,
        "source_accounts": source_views,
        "lb_accounts": lb_views,
        "templates": cfg.templates,
        "users": user_views,
        "risk": cfg.risk,
        "active_sessions": sessions,
        "known_devices": devices,
        "ua_stats": db::list_ua_stats(conn),
        "stats": db::compute_stats_summary(conn),
        "history": db::list_playback_history(conn, 100, ""),
        "last_migrated_from": cfg.last_migrated_from,
        "last_tick_result": db::get_kv(conn, "last_tick_result").unwrap_or_default(),
        "last_health_result": db::get_kv(conn, "last_health_result").unwrap_or_default(),
    })
}

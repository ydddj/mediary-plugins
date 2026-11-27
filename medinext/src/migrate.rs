use crate::models::{now_ts, Account115, AppConfig, PolicyTemplate, UserPolicy};
use crate::p115::extract_uid_from_cookie;
use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn unquote_json_str(raw: &str) -> String {
    let t = raw.trim();
    if t.starts_with('"') && t.ends_with('"') {
        if let Ok(s) = serde_json::from_str::<String>(t) {
            return s;
        }
    }
    t.to_string()
}

fn normalize_route_label(raw: &str, has_real_cookie: bool) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    if has_real_cookie {
        return "own_cookie".to_string();
    }
    if lower.contains("源盘") || lower.contains("直连") || lower.contains("直通") || lower == "direct" {
        return "direct".to_string();
    }
    if lower.contains("负载") || lower == "load_balance" {
        return "load_balance".to_string();
    }
    "template".to_string()
}

fn convert_nextemby_quota_gb(raw_quota: i64) -> u64 {
    if raw_quota <= 0 {
        return 0;
    }
    // In NextEmby v4.x, `transfer_quota` (e.g. 5) is configured in TB when <= 50
    if raw_quota <= 50 {
        (raw_quota as u64) * 1024
    } else {
        raw_quota as u64
    }
}

pub fn import_nextemby_payload(
    data_dir: &Path,
    source_path: Option<&str>,
    file_base64: Option<&str>,
    filename: Option<&str>,
    mut cfg: AppConfig,
) -> Result<(AppConfig, Value)> {
    if let Some(b64) = file_base64.map(str::trim).filter(|s| !s.is_empty()) {
        let clean_b64 = b64
            .split_once(",")
            .map(|(_, r)| r)
            .unwrap_or(b64)
            .trim();
        let bytes = B64.decode(clean_b64).context("解码上传文件 Base64 失败")?;
        ensure!(!bytes.is_empty(), "上传文件内容为空");
        let label = filename.unwrap_or("上传文件").to_string();
        if bytes.starts_with(b"SQLite format 3") {
            let tmp_db = data_dir.join("import_nextemby_tmp.db");
            fs::write(&tmp_db, &bytes)?;
            let res = import_from_sqlite_file(&tmp_db, &label, &mut cfg);
            let _ = fs::remove_file(&tmp_db);
            let stats = res?;
            return Ok((cfg, stats));
        } else {
            let stats = import_from_json_bytes(&bytes, &label, &mut cfg)?;
            return Ok((cfg, stats));
        }
    }

    if let Some(p) = source_path.map(str::trim).filter(|s| !s.is_empty()) {
        let path = Path::new(p);
        ensure!(path.is_file(), "未找到指定的配置文件路径: {p}（若路径在宿主机且未挂载进容器，请点击「选择本地文件上传导入」）");
        let head = fs::read(path).context("读取导入文件失败")?;
        if head.starts_with(b"SQLite format 3") {
            let stats = import_from_sqlite_file(path, p, &mut cfg)?;
            return Ok((cfg, stats));
        } else {
            let stats = import_from_json_bytes(&head, p, &mut cfg)?;
            return Ok((cfg, stats));
        }
    }

    bail!("请上传 NextEmby 的 config.db / .bak 备份文件，或填写容器内可读路径")
}

fn import_from_sqlite_file(db_path: &Path, label: &str, cfg: &mut AppConfig) -> Result<Value> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("打开 NextEmby SQLite 数据库失败")?;

    let mut imported_sources = 0usize;
    let mut imported_lbs = 0usize;
    let mut imported_users = 0usize;
    let mut imported_templates = 0usize;

    // 1. Settings
    if let Ok(mut stmt) = conn.prepare("SELECT key, value, category FROM settings") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1).unwrap_or_default(),
                r.get::<_, String>(2).unwrap_or_default(),
            ))
        }) {
            for (k, raw_v, cat) in rows.flatten() {
                let v = unquote_json_str(&raw_v);
                match (cat.as_str(), k.as_str()) {
                    ("emby", "template_policies") => {
                        if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&raw_v)
                            .or_else(|_| serde_json::from_str::<Vec<Value>>(&v))
                        {
                            for item in arr {
                                let name = item["name"].as_str().unwrap_or_default().trim().to_string();
                                if name.is_empty() {
                                    continue;
                                }
                                let tpl = PolicyTemplate {
                                    name: name.clone(),
                                    max_streams: item["max_streams"].as_u64().unwrap_or(3) as u32,
                                    max_devices: item["max_devices"].as_u64().unwrap_or(5) as u32,
                                    default_route: normalize_route_label(
                                        item["default_route"].as_str().unwrap_or("load_balance"),
                                        false,
                                    ),
                                    quota_gb: 0,
                                };
                                if let Some(ex) = cfg.templates.iter_mut().find(|t| t.name == name) {
                                    *ex = tpl;
                                } else {
                                    cfg.templates.push(tpl);
                                }
                                imported_templates += 1;
                            }
                        }
                    }
                    ("system", "p115_proxy") if !v.is_empty() => {
                        cfg.engine.p115_proxy_url = v;
                    }
                    ("system", "fallback_small_file_size") => {
                        if let Ok(mb) = v.parse::<u64>() {
                            cfg.engine.small_file_direct_mb = mb;
                        }
                    }
                    ("system", "max_daily_transfers") => {
                        if let Ok(n) = v.parse::<u32>() {
                            cfg.engine.max_daily_transfers = n;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // 2. Master / Source Accounts (`accounts` table)
    if let Ok(mut stmt) = conn.prepare(
        "SELECT username, COALESCE(cookies,''), COALESCE(proxy,''), COALESCE(recycle_bin_code,''), COALESCE(drive_info,''), COALESCE(sort_order,0)
         FROM accounts ORDER BY sort_order ASC",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            let cookies = unquote_json_str(&r.get::<_, String>(1)?);
            let uid = extract_uid_from_cookie(&cookies);
            Ok(Account115 {
                name: r.get::<_, String>(0)?.trim().to_string(),
                role: "source".to_string(),
                cookies,
                uid,
                app: "tv".to_string(),
                proxy_url: unquote_json_str(&r.get::<_, String>(2)?),
                recycle_bin_code: unquote_json_str(&r.get::<_, String>(3)?),
                drive_info: unquote_json_str(&r.get::<_, String>(4)?),
                sort_order: r.get::<_, i32>(5).unwrap_or(0),
                enabled: true,
                valid: true,
                ..Default::default()
            })
        }) {
            for acc in rows.flatten() {
                if acc.name.is_empty() {
                    continue;
                }
                if let Some(ex) = cfg.source_accounts.iter_mut().find(|a| a.name == acc.name) {
                    *ex = acc;
                } else {
                    cfg.source_accounts.push(acc);
                }
                imported_sources += 1;
            }
        }
    }

    // 3. Load-Balance Accounts (`lb_accounts` table)
    if let Ok(mut stmt) = conn.prepare(
        "SELECT username, COALESCE(cookies,''), COALESCE(proxy,''), COALESCE(cache_path,'/Next'),
                COALESCE(recycle_bin_code,''), COALESCE(transfer_quota,0), COALESCE(transfer_quota_enabled,0),
                COALESCE(transfer_quota_usage,0), COALESCE(is_valid,1), COALESCE(is_enabled,1),
                COALESCE(drive_info,''), COALESCE(sort_order,0)
         FROM lb_accounts ORDER BY sort_order ASC",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            let cookies = unquote_json_str(&r.get::<_, String>(1)?);
            let uid = extract_uid_from_cookie(&cookies);
            let raw_q: i64 = r.get(5).unwrap_or(0);
            Ok(Account115 {
                name: r.get::<_, String>(0)?.trim().to_string(),
                role: "lb".to_string(),
                cookies,
                uid,
                app: "tv".to_string(),
                proxy_url: unquote_json_str(&r.get::<_, String>(2)?),
                cache_path: {
                    let cp = unquote_json_str(&r.get::<_, String>(3)?);
                    if cp.is_empty() { "/Next".to_string() } else { cp }
                },
                recycle_bin_code: unquote_json_str(&r.get::<_, String>(4)?),
                quota_gb: convert_nextemby_quota_gb(raw_q),
                quota_enabled: r.get::<_, i64>(6).unwrap_or(0) != 0,
                quota_usage_bytes: r.get::<_, i64>(7).unwrap_or(0).max(0) as u64,
                valid: r.get::<_, i64>(8).unwrap_or(1) != 0,
                enabled: r.get::<_, i64>(9).unwrap_or(1) != 0,
                drive_info: unquote_json_str(&r.get::<_, String>(10)?),
                sort_order: r.get::<_, i32>(11).unwrap_or(0),
                ..Default::default()
            })
        }) {
            for acc in rows.flatten() {
                if acc.name.is_empty() {
                    continue;
                }
                if let Some(ex) = cfg.lb_accounts.iter_mut().find(|a| a.name == acc.name) {
                    *ex = acc;
                } else {
                    cfg.lb_accounts.push(acc);
                }
                imported_lbs += 1;
            }
        }
    }

    // 4. Users (`users` table)
    if let Ok(mut stmt) = conn.prepare(
        "SELECT username, COALESCE(emby_id,''), COALESCE(cookies,''), COALESCE(p115_uid,''),
                COALESCE(expiry_date,''), COALESCE(cache_path,'/EmbyCache'), COALESCE(is_admin,0),
                COALESCE(template,''), COALESCE(recycle_bin_code,''), COALESCE(transfer_quota,0),
                COALESCE(transfer_quota_enabled,0), COALESCE(transfer_quota_usage,0),
                COALESCE(remark,''), COALESCE(is_disabled,0), COALESCE(is_banned,0)
         FROM users",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            let raw_cookie = unquote_json_str(&r.get::<_, String>(2)?);
            let raw_uid = unquote_json_str(&r.get::<_, String>(3)?);
            let has_real_cookie = raw_cookie.contains("UID=") && raw_cookie.contains("CID=");
            let uid = if has_real_cookie {
                extract_uid_from_cookie(&raw_cookie)
            } else if raw_uid.bytes().all(|b| b.is_ascii_digit()) {
                raw_uid.clone()
            } else {
                String::new()
            };
            let route_hint = if !raw_cookie.is_empty() && !has_real_cookie {
                &raw_cookie
            } else {
                &raw_uid
            };
            let route_mode = normalize_route_label(route_hint, has_real_cookie);
            let raw_q: i64 = r.get(9).unwrap_or(0);
            Ok(UserPolicy {
                username: r.get::<_, String>(0)?.trim().to_string(),
                emby_id: unquote_json_str(&r.get::<_, String>(1)?),
                cookies: if has_real_cookie { raw_cookie } else { String::new() },
                p115_uid: uid,
                route_mode,
                expiry_date: unquote_json_str(&r.get::<_, String>(4)?),
                cache_path: {
                    let cp = unquote_json_str(&r.get::<_, String>(5)?);
                    if cp.is_empty() { "/EmbyCache".to_string() } else { cp }
                },
                is_admin: r.get::<_, i64>(6).unwrap_or(0) != 0,
                template: unquote_json_str(&r.get::<_, String>(7)?),
                recycle_bin_code: unquote_json_str(&r.get::<_, String>(8)?),
                quota_gb: convert_nextemby_quota_gb(raw_q),
                quota_enabled: r.get::<_, i64>(10).unwrap_or(0) != 0,
                quota_usage_bytes: r.get::<_, i64>(11).unwrap_or(0).max(0) as u64,
                remark: unquote_json_str(&r.get::<_, String>(12)?),
                is_disabled: r.get::<_, i64>(13).unwrap_or(0) != 0,
                is_banned: r.get::<_, i64>(14).unwrap_or(0) != 0,
                ..Default::default()
            })
        }) {
            for u in rows.flatten() {
                if u.username.is_empty() {
                    continue;
                }
                if let Some(ex) = cfg
                    .users
                    .iter_mut()
                    .find(|x| x.username.eq_ignore_ascii_case(&u.username))
                {
                    *ex = u;
                } else {
                    cfg.users.push(u);
                }
                imported_users += 1;
            }
        }
    }

    cfg.last_migrated_from = format!("{label} @ {}", chrono::Local::now().format("%Y-%m-%d %H:%M"));
    cfg.updated_at = now_ts();

    Ok(json!({
        "source": label,
        "imported_sources": imported_sources,
        "imported_lbs": imported_lbs,
        "imported_users": imported_users,
        "imported_templates": imported_templates,
    }))
}

fn import_from_json_bytes(bytes: &[u8], label: &str, cfg: &mut AppConfig) -> Result<Value> {
    let v: Value = serde_json::from_slice(bytes).context("解析 JSON 备份文件失败")?;
    // Support both MediNext native export and NextEmby JSON backup
    if v.get("engine").is_some() && v.get("lb_accounts").is_some() {
        let mut parsed: AppConfig = serde_json::from_value(v)?;
        parsed.last_migrated_from = format!("{label} @ {}", chrono::Local::now().format("%Y-%m-%d %H:%M"));
        parsed.updated_at = now_ts();
        let summary = json!({
            "source": label,
            "imported_sources": parsed.source_accounts.len(),
            "imported_lbs": parsed.lb_accounts.len(),
            "imported_users": parsed.users.len(),
            "imported_templates": parsed.templates.len(),
        });
        *cfg = parsed;
        return Ok(summary);
    }

    let data = v.get("data").filter(|x| x.is_object()).unwrap_or(&v);
    let mut imported_sources = 0usize;
    let mut imported_lbs = 0usize;
    let mut imported_users = 0usize;

    if let Some(sp) = data["storage_providers"].as_object() {
        for (name, item) in sp {
            let cookies = item["cookies"].as_str().unwrap_or_default().to_string();
            let acc = Account115 {
                name: name.clone(),
                role: "source".to_string(),
                uid: extract_uid_from_cookie(&cookies),
                cookies,
                app: "tv".to_string(),
                enabled: true,
                valid: true,
                ..Default::default()
            };
            if let Some(ex) = cfg.source_accounts.iter_mut().find(|a| a.name == *name) {
                *ex = acc;
            } else {
                cfg.source_accounts.push(acc);
            }
            imported_sources += 1;
        }
    }

    if let Some(lbp) = data["load_balance_providers"].as_object() {
        for (name, item) in lbp {
            let cookies = item["cookies"].as_str().unwrap_or_default().to_string();
            let raw_q = item["transfer_quota"].as_i64().unwrap_or(0);
            let acc = Account115 {
                name: name.clone(),
                role: "lb".to_string(),
                uid: extract_uid_from_cookie(&cookies),
                cookies,
                app: "tv".to_string(),
                cache_path: item["cache_path"].as_str().unwrap_or("/Next").to_string(),
                quota_gb: convert_nextemby_quota_gb(raw_q),
                quota_enabled: item["transfer_quota_enabled"].as_bool() == Some(true),
                enabled: item["is_enabled"].as_bool().unwrap_or(true),
                valid: item["is_valid"].as_bool().unwrap_or(true),
                ..Default::default()
            };
            if let Some(ex) = cfg.lb_accounts.iter_mut().find(|a| a.name == *name) {
                *ex = acc;
            } else {
                cfg.lb_accounts.push(acc);
            }
            imported_lbs += 1;
        }
    }

    if let Some(users_map) = data["users"].as_object() {
        for (uname, item) in users_map {
            let raw_cookie = item["cookies"].as_str().unwrap_or_default().to_string();
            let has_real = raw_cookie.contains("UID=") && raw_cookie.contains("CID=");
            let u = UserPolicy {
                username: uname.clone(),
                emby_id: item["emby_id"].as_str().unwrap_or_default().to_string(),
                cookies: if has_real { raw_cookie.clone() } else { String::new() },
                p115_uid: if has_real { extract_uid_from_cookie(&raw_cookie) } else { String::new() },
                route_mode: normalize_route_label(&raw_cookie, has_real),
                expiry_date: item["expiry_date"].as_str().unwrap_or_default().to_string(),
                template: item["template"].as_str().unwrap_or_default().to_string(),
                cache_path: item["cache_path"].as_str().unwrap_or("/EmbyCache").to_string(),
                is_admin: item["is_admin"].as_bool() == Some(true),
                ..Default::default()
            };
            if let Some(ex) = cfg.users.iter_mut().find(|x| x.username == *uname) {
                *ex = u;
            } else {
                cfg.users.push(u);
            }
            imported_users += 1;
        }
    }

    cfg.last_migrated_from = format!("{label} @ {}", chrono::Local::now().format("%Y-%m-%d %H:%M"));
    cfg.updated_at = now_ts();

    Ok(json!({
        "source": label,
        "imported_sources": imported_sources,
        "imported_lbs": imported_lbs,
        "imported_users": imported_users,
        "imported_templates": cfg.templates.len(),
    }))
}

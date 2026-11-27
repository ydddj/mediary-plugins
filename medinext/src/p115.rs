use crate::models::{now_ts, Account115};
use crate::p115_crypto::{
    ecdh_aes_decrypt, hex_upper, m115_rsa_decrypt, m115_rsa_encrypt, make_upload_init_request,
    pickcode_to_id,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const DEFAULT_115_UA: &str = "Mozilla/5.0 115Browser/27.0.5.7";

#[derive(Debug)]
pub enum P115Error {
    Risk405(String),
    AuthInvalid(String),
    Other(String),
}

impl std::fmt::Display for P115Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Risk405(m) => write!(f, "115 风控限制 (405): {m}"),
            Self::AuthInvalid(m) => write!(f, "115 凭证失效: {m}"),
            Self::Other(m) => write!(f, "{m}"),
        }
    }
}
impl std::error::Error for P115Error {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileMeta115 {
    pub file_id: String,
    pub pick_code: String,
    pub sha1: String,
    pub size: u64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadLink115 {
    pub url: String,
    pub file_name: String,
    pub file_size: u64,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadCredentials {
    pub user_id: String,
    pub user_key: String,
}

static UID_RATE_GATE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn throttle_uid(bucket: &str, uid: &str, cooldown_ms: u64) {
    if uid.trim().is_empty() || cooldown_ms == 0 {
        return;
    }
    let gate_key = format!("{bucket}:{}", uid.trim());
    let map_mutex = UID_RATE_GATE.get_or_init(|| Mutex::new(HashMap::new()));
    let wait_dur = {
        let mut guard = map_mutex.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let target = Duration::from_millis(cooldown_ms.min(2000));
        if let Some(&last) = guard.get(&gate_key) {
            let elapsed = now.saturating_duration_since(last);
            if elapsed < target {
                let remain = target - elapsed;
                guard.insert(gate_key, now + remain);
                Some(remain)
            } else {
                guard.insert(gate_key, now);
                None
            }
        } else {
            guard.insert(gate_key, now);
            None
        }
    };
    if let Some(d) = wait_dur {
        std::thread::sleep(d);
    }
}

pub fn extract_uid_from_cookie(cookie: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?:^|;\s*)UID=([0-9]+)_").unwrap());
    if let Some(cap) = re.captures(cookie) {
        return cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
    }
    String::new()
}

pub fn build_agent(proxy_url: &str, timeout_secs: u64) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(timeout_secs.max(4)))
        .redirects(3);
    if !proxy_url.trim().is_empty() {
        if let Ok(p) = ureq::Proxy::new(proxy_url.trim()) {
            b = b.proxy(p);
        }
    }
    b.build()
}

fn check_405_or_errno(status: u16, v: &Value) -> Result<(), P115Error> {
    if status == 405 || status == 403 {
        return Err(P115Error::Risk405(format!("HTTP {status}")));
    }
    let errno = v["errno"]
        .as_i64()
        .or_else(|| v["errNo"].as_i64())
        .or_else(|| v["code"].as_i64())
        .unwrap_or(0);
    let msg = v["error"]
        .as_str()
        .or_else(|| v["message"].as_str())
        .or_else(|| v["msg"].as_str())
        .unwrap_or("");
    if errno == 99 || errno == 990001 || msg.contains("重新登录") || msg.contains("未登录") {
        return Err(P115Error::AuthInvalid(if msg.is_empty() {
            format!("errno={errno}")
        } else {
            msg.to_string()
        }));
    }
    if errno == 405 || msg.contains("高频") || msg.contains("操作太快") || msg.contains("风控") {
        return Err(P115Error::Risk405(if msg.is_empty() {
            format!("errno={errno}")
        } else {
            msg.to_string()
        }));
    }
    Ok(())
}

// === 115 QR Code Login (Web Cookie & OpenAPI) ===

pub fn qr_start(app: &str, proxy_url: &str) -> Result<Value> {
    let app_name = match app.trim() {
        "web" | "mac" | "linux" | "windows" | "android" | "ios" | "tv" | "alipaymini"
        | "wechatmini" | "qandroid" | "clouddrive" => app.trim(),
        _ => "tv",
    };
    let agent = build_agent(proxy_url, 20);
    let token_url = format!("https://qrcodeapi.115.com/api/1.0/{app_name}/1.0/token/");
    let resp: Value = agent
        .get(&token_url)
        .set("User-Agent", DEFAULT_115_UA)
        .set("Referer", "https://115.com/")
        .call()
        .context("请求 115 二维码令牌失败")?
        .into_json()?;
    ensure!(
        resp["state"].as_i64() == Some(1),
        "获取二维码失败: {}",
        resp["message"].as_str().unwrap_or("未知返回")
    );
    let data = &resp["data"];
    let uid = data["uid"].as_str().unwrap_or_default().trim().to_string();
    ensure!(!uid.is_empty(), "二维码响应缺少 uid");
    let time_val = data["time"]
        .as_i64()
        .map(|n| n.to_string())
        .or_else(|| data["time"].as_str().map(str::to_string))
        .unwrap_or_default();
    let sign = data["sign"].as_str().unwrap_or_default().to_string();
    let qr_text = data["qrcode"].as_str().unwrap_or_default().to_string();

    let img_url = format!("https://qrcodeapi.115.com/api/1.0/web/1.0/qrcode?uid={uid}");
    let mut image_data_url = String::new();
    if let Ok(img_resp) = agent
        .get(&img_url)
        .set("User-Agent", DEFAULT_115_UA)
        .set("Referer", "https://115.com/")
        .call()
    {
        let mut bytes = Vec::new();
        if img_resp.into_reader().take(512 * 1024).read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
            image_data_url = format!("data:image/png;base64,{}", B64.encode(&bytes));
        }
    }

    Ok(json!({
        "uid": uid,
        "time": time_val,
        "sign": sign,
        "app": app_name,
        "qrcode_text": qr_text,
        "image_base64": image_data_url,
    }))
}

pub fn qr_poll(uid: &str, time_val: &str, sign: &str, app: &str, proxy_url: &str) -> Result<Value> {
    let app_name = if app.trim().is_empty() { "tv" } else { app.trim() };
    let agent = build_agent(proxy_url, 25);
    let ts_ms = (now_ts() as u128) * 1000;
    let status_resp: Value = agent
        .get("https://qrcodeapi.115.com/get/status/")
        .query("uid", uid.trim())
        .query("time", time_val.trim())
        .query("sign", sign.trim())
        .query("_", &ts_ms.to_string())
        .set("User-Agent", DEFAULT_115_UA)
        .set("Referer", "https://115.com/")
        .call()
        .context("轮询二维码状态失败")?
        .into_json()?;
    if status_resp["state"].as_i64() != Some(1) {
        return Ok(json!({
            "status": "error",
            "message": status_resp["message"].as_str().unwrap_or("二维码状态接口异常"),
        }));
    }
    let code = status_resp["data"]["status"].as_i64().unwrap_or(0);
    match code {
        0 => Ok(json!({ "status": "waiting", "message": "请使用 115 App 扫描二维码" })),
        1 => Ok(json!({ "status": "scanned", "message": "已扫码，请在手机上点击确认登录" })),
        -1 | -2 => Ok(json!({ "status": "expired", "message": "二维码已过期或已取消，请刷新重试" })),
        2 => {
            if app_name == "clouddrive" {
                let open_resp: Value = agent
                    .post("https://passportapi.115.com/open/deviceCodeToToken")
                    .set("User-Agent", DEFAULT_115_UA)
                    .send_form(&[("uid", uid.trim()), ("app", app_name)])
                    .context("兑换 115 OpenAPI 令牌失败")?
                    .into_json()?;
                if open_resp["state"].as_i64() != Some(1) {
                    bail!(
                        "OpenAPI 登录失败: {}",
                        open_resp["message"].as_str().unwrap_or("未知错误")
                    );
                }
                let d = &open_resp["data"];
                Ok(json!({
                    "status": "success",
                    "mode": "openapi",
                    "access_token": d["access_token"].as_str().or_else(|| d["accessToken"].as_str()).unwrap_or_default(),
                    "refresh_token": d["refresh_token"].as_str().or_else(|| d["refreshToken"].as_str()).unwrap_or_default(),
                    "p115_uid": d["uid"].as_str().or_else(|| d["user_id"].as_str()).unwrap_or_default(),
                }))
            } else {
                let login_url = format!("https://passportapi.115.com/app/1.0/{app_name}/1.0/login/qrcode/");
                let login_resp: Value = agent
                    .post(&login_url)
                    .set("User-Agent", DEFAULT_115_UA)
                    .set("Referer", "https://115.com/")
                    .send_form(&[("app", app_name), ("account", uid.trim())])
                    .context("获取 115 登录 Cookie 失败")?
                    .into_json()?;
                if login_resp["state"].as_i64() != Some(1) {
                    bail!(
                        "登录确认失败: {}",
                        login_resp["message"].as_str().unwrap_or("未知错误")
                    );
                }
                let cookie_obj = &login_resp["data"]["cookie"];
                let mut pairs = Vec::new();
                if let Some(map) = cookie_obj.as_object() {
                    for k in ["UID", "CID", "SEID", "KID"] {
                        if let Some(v) = map.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                            pairs.push(format!("{k}={v}"));
                        }
                    }
                    if pairs.is_empty() {
                        for (k, v) in map {
                            if let Some(s) = v.as_str() {
                                pairs.push(format!("{k}={s}"));
                            }
                        }
                    }
                } else if let Some(s) = cookie_obj.as_str() {
                    pairs.push(s.to_string());
                }
                let cookie_str = pairs.join("; ");
                let p115_uid = extract_uid_from_cookie(&cookie_str);
                Ok(json!({
                    "status": "success",
                    "mode": "cookie",
                    "cookie": cookie_str,
                    "p115_uid": p115_uid,
                }))
            }
        }
        _ => Ok(json!({ "status": "waiting", "message": "等待扫码响应..." })),
    }
}

// === Account Probe, Upload Credentials & Metadata ===

pub fn fetch_upload_credentials(
    cookie: &str,
    proxy_url: &str,
    cooldown_ms: u64,
) -> std::result::Result<(UploadCredentials, String), P115Error> {
    let uid_hint = extract_uid_from_cookie(cookie);
    throttle_uid("uploadinfo", &uid_hint, cooldown_ms);
    let agent = build_agent(proxy_url, 12);
    let resp = agent
        .get("https://proapi.115.com/app/uploadinfo")
        .set("Cookie", cookie.trim())
        .set("User-Agent", DEFAULT_115_UA)
        .call();
    match resp {
        Ok(r) => {
            let status = r.status();
            let v: Value = r.into_json().map_err(|e| P115Error::Other(e.to_string()))?;
            check_405_or_errno(status, &v)?;
            let user_id = v["user_id"]
                .as_u64()
                .map(|n| n.to_string())
                .or_else(|| v["user_id"].as_str().map(str::to_string))
                .unwrap_or(uid_hint);
            let user_key = v["userkey"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            if user_id.is_empty() || user_key.is_empty() {
                return Err(P115Error::AuthInvalid("uploadinfo 未返回有效 user_id / userkey".to_string()));
            }
            let name_info = v["user_name"].as_str().unwrap_or_default().to_string();
            let space_info = fetch_space_summary(cookie, proxy_url).unwrap_or_default();
            let drive_label = match (!name_info.is_empty(), !space_info.is_empty()) {
                (true, true) => format!("{name_info} · {space_info}"),
                (false, true) => space_info,
                (true, false) => name_info,
                (false, false) => String::new(),
            };
            Ok((UploadCredentials { user_id, user_key }, drive_label))
        }
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            if code == 405 || code == 403 {
                Err(P115Error::Risk405(format!("uploadinfo HTTP {code}")))
            } else {
                Err(P115Error::Other(format!("uploadinfo HTTP {code}: {}", &body[..body.len().min(120)])))
            }
        }
        Err(e) => Err(P115Error::Other(format!("连接 115 uploadinfo 失败: {e}"))),
    }
}

/// Fetches 115 cloud storage remaining/total capacity (`可用 XX TB / 共 XX TB`).
pub fn fetch_space_summary(cookie: &str, proxy_url: &str) -> Option<String> {
    if cookie.trim().is_empty() {
        return None;
    }
    let agent = build_agent(proxy_url, 8);
    let v: Value = agent
        .get("https://webapi.115.com/files/index_info")
        .set("Cookie", cookie.trim())
        .set("User-Agent", DEFAULT_115_UA)
        .set("Referer", "https://115.com/")
        .call()
        .ok()?
        .into_json()
        .ok()?;
    let sp = v.get("data")?.get("space_info")?;
    let remain = sp["all_remain"]["size_format"]
        .as_str()
        .unwrap_or_default()
        .trim();
    let total = sp["all_total"]["size_format"]
        .as_str()
        .unwrap_or_default()
        .trim();
    if !remain.is_empty() && !total.is_empty() {
        Some(format!("可用 {remain} / {total}"))
    } else {
        None
    }
}

pub fn fetch_file_meta_by_pickcode(
    acc: &Account115,
    pickcode: &str,
    global_proxy: &str,
    cooldown_ms: u64,
) -> std::result::Result<FileMeta115, P115Error> {
    let proxy = if !acc.proxy_url.trim().is_empty() {
        acc.proxy_url.as_str()
    } else {
        global_proxy
    };
    let uid = if !acc.uid.is_empty() {
        acc.uid.clone()
    } else {
        extract_uid_from_cookie(&acc.cookies)
    };
    throttle_uid("meta", &uid, cooldown_ms);

    let file_id = pickcode_to_id(pickcode)
        .map(|n| n.to_string())
        .ok_or_else(|| P115Error::Other(format!("无法从提取码推导文件 ID: {pickcode}")))?;

    let agent = build_agent(proxy, 15);
    if !acc.cookies.trim().is_empty() {
        let resp = agent
            .get("https://webapi.115.com/files/file")
            .query("file_id", &file_id)
            .set("Cookie", acc.cookies.trim())
            .set("User-Agent", DEFAULT_115_UA)
            .set("Referer", "https://115.com/")
            .call();
        match resp {
            Ok(r) => {
                let status = r.status();
                let v: Value = r.into_json().map_err(|e| P115Error::Other(e.to_string()))?;
                check_405_or_errno(status, &v)?;
                let row = v["data"]
                    .as_array()
                    .and_then(|a| a.first())
                    .or_else(|| v["data"].as_object().map(|_| &v["data"]))
                    .ok_or_else(|| P115Error::Other("115 files/file 未返回文件信息".to_string()))?;
                let sha1 = row["sha1"]
                    .as_str()
                    .or_else(|| row["sha"].as_str())
                    .or_else(|| row["file_sha1"].as_str())
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                let name = row["file_name"]
                    .as_str()
                    .or_else(|| row["n"].as_str())
                    .or_else(|| row["name"].as_str())
                    .unwrap_or_default()
                    .to_string();
                let size = row["file_size"]
                    .as_u64()
                    .or_else(|| row["file_size"].as_str().and_then(|s| s.parse().ok()))
                    .or_else(|| row["s"].as_u64())
                    .or_else(|| row["s"].as_str().and_then(|s| s.parse().ok()))
                    .unwrap_or(0);
                let pc = row["pick_code"]
                    .as_str()
                    .or_else(|| row["pc"].as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(pickcode)
                    .to_string();
                return Ok(FileMeta115 {
                    file_id,
                    pick_code: pc,
                    sha1,
                    size,
                    name,
                });
            }
            Err(ureq::Error::Status(code, _)) if code == 405 || code == 403 => {
                return Err(P115Error::Risk405(format!("files/file HTTP {code}")));
            }
            Err(e) => {
                if acc.access_token.trim().is_empty() {
                    return Err(P115Error::Other(format!("查询 115 文件元数据失败: {e}")));
                }
            }
        }
    }

    if !acc.access_token.trim().is_empty() {
        let resp = agent
            .post("https://proapi.115.com/open/ufile/downurl")
            .set("Authorization", &format!("Bearer {}", acc.access_token.trim()))
            .set("User-Agent", DEFAULT_115_UA)
            .send_form(&[("pick_code", pickcode.trim())])
            .map_err(|e| P115Error::Other(format!("OpenAPI 查询文件信息失败: {e}")))?;
        let v: Value = resp.into_json().map_err(|e| P115Error::Other(e.to_string()))?;
        check_405_or_errno(200, &v)?;
        if let Some(map) = v["data"].as_object() {
            if let Some((fid, entry)) = map.iter().next() {
                let name = entry["file_name"].as_str().unwrap_or_default().to_string();
                let size = entry["file_size"]
                    .as_u64()
                    .or_else(|| entry["file_size"].as_str().and_then(|s| s.parse().ok()))
                    .unwrap_or(0);
                let sha1 = entry["sha1"].as_str().unwrap_or_default().to_ascii_uppercase();
                return Ok(FileMeta115 {
                    file_id: fid.clone(),
                    pick_code: pickcode.to_string(),
                    sha1,
                    size,
                    name,
                });
            }
        }
    }

    Err(P115Error::AuthInvalid("账号未配置可用的 115 Cookie 或 OpenAPI Token".to_string()))
}

// === Direct 302 Download Link Resolution (m115 RSA + OpenAPI + Web Fallback) ===

pub fn get_download_link(
    cookie: &str,
    access_token: &str,
    uid_hint: &str,
    pickcode: &str,
    client_ua: &str,
    proxy_url: &str,
    cooldown_ms: u64,
) -> std::result::Result<DownloadLink115, P115Error> {
    let pc = pickcode.trim();
    if pc.is_empty() {
        return Err(P115Error::Other("pickcode 为空".to_string()));
    }
    let ua = if client_ua.trim().is_empty() {
        DEFAULT_115_UA
    } else {
        client_ua.trim()
    };
    let uid = if !uid_hint.trim().is_empty() {
        uid_hint.trim().to_string()
    } else {
        extract_uid_from_cookie(cookie)
    };
    throttle_uid("downurl", &uid, cooldown_ms);
    let agent = build_agent(proxy_url, 12);

    // 1. OpenAPI path (when access_token is configured and cookie is empty)
    if !access_token.trim().is_empty() && cookie.trim().is_empty() {
        let resp = agent
            .post("https://proapi.115.com/open/ufile/downurl")
            .set("Authorization", &format!("Bearer {}", access_token.trim()))
            .set("User-Agent", ua)
            .send_form(&[("pick_code", pc)]);
        match resp {
            Ok(r) => {
                let status = r.status();
                let v: Value = r.into_json().map_err(|e| P115Error::Other(e.to_string()))?;
                check_405_or_errno(status, &v)?;
                if let Some(link) = extract_downurl_from_data(&v["data"], ua) {
                    return Ok(link);
                }
            }
            Err(ureq::Error::Status(code, _)) if code == 405 || code == 403 => {
                return Err(P115Error::Risk405(format!("OpenAPI downurl HTTP {code}")));
            }
            Err(e) => return Err(P115Error::Other(format!("OpenAPI 获取直链失败: {e}"))),
        }
    }

    if cookie.trim().is_empty() {
        return Err(P115Error::AuthInvalid("未配置 115 Cookie".to_string()));
    }

    // 2. Primary m115 encrypted proapi (/app/chrome/downurl) -> supports 80GB+ 4K Remux & binds UA!
    let ts = now_ts();
    let plain_payload = json!({ "pickcode": pc }).to_string();
    let encrypted = m115_rsa_encrypt(plain_payload.as_bytes());
    let down_url = format!("https://proapi.115.com/app/chrome/downurl?t={ts}");

    match agent
        .post(&down_url)
        .set("Cookie", cookie.trim())
        .set("User-Agent", ua)
        .send_form(&[("data", encrypted.as_str())])
    {
        Ok(r) => {
            let status = r.status();
            let v: Value = r.into_json().map_err(|e| P115Error::Other(e.to_string()))?;
            check_405_or_errno(status, &v)?;
            if v["state"].as_bool() == Some(true) || v["state"].as_i64() == Some(1) {
                if let Some(cipher_str) = v["data"].as_str() {
                    let decrypted = m115_rsa_decrypt(cipher_str)
                        .map_err(|e| P115Error::Other(format!("解密 downurl 失败: {e}")))?;
                    let data_val: Value = serde_json::from_slice(&decrypted)
                        .map_err(|e| P115Error::Other(format!("解析 downurl JSON 失败: {e}")))?;
                    if let Some(link) = extract_downurl_from_data(&data_val, ua) {
                        return Ok(link);
                    }
                }
            }
        }
        Err(ureq::Error::Status(code, _)) if code == 405 || code == 403 => {
            return Err(P115Error::Risk405(format!("proapi downurl HTTP {code}")));
        }
        Err(_) => {}
    }

    // 3. Fallback for <=200MB or special aid=12 files: webapi /files/download
    let web_resp = agent
        .get("https://webapi.115.com/files/download")
        .query("pickcode", pc)
        .set("Cookie", cookie.trim())
        .set("User-Agent", ua)
        .set("Referer", "https://115.com/")
        .call();
    match web_resp {
        Ok(r) => {
            let status = r.status();
            let v: Value = r.into_json().map_err(|e| P115Error::Other(e.to_string()))?;
            check_405_or_errno(status, &v)?;
            if let Some(url) = v["file_url"].as_str().filter(|s| !s.is_empty()) {
                let mut headers = HashMap::new();
                headers.insert("User-Agent".to_string(), ua.to_string());
                return Ok(DownloadLink115 {
                    url: url.to_string(),
                    file_name: v["file_name"].as_str().unwrap_or_default().to_string(),
                    file_size: v["file_size"]
                        .as_u64()
                        .or_else(|| v["file_size"].as_str().and_then(|s| s.parse().ok()))
                        .unwrap_or(0),
                    headers,
                });
            }
            Err(P115Error::Other(
                v["msg"]
                    .as_str()
                    .or_else(|| v["error"].as_str())
                    .unwrap_or("115 未返回有效直链")
                    .to_string(),
            ))
        }
        Err(ureq::Error::Status(code, _)) if code == 405 || code == 403 => {
            Err(P115Error::Risk405(format!("webapi download HTTP {code}")))
        }
        Err(e) => Err(P115Error::Other(format!("获取 115 直链失败: {e}"))),
    }
}

fn extract_downurl_from_data(data_val: &Value, ua: &str) -> Option<DownloadLink115> {
    let map = data_val.as_object()?;
    for (_fid, entry) in map {
        let url_str = entry["url"]["url"]
            .as_str()
            .or_else(|| entry["url"].as_str())
            .filter(|s| !s.trim().is_empty())?;
        let file_name = entry["file_name"].as_str().unwrap_or_default().to_string();
        let file_size = entry["file_size"]
            .as_u64()
            .or_else(|| entry["file_size"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
        let mut headers = HashMap::new();
        headers.insert("User-Agent".to_string(), ua.to_string());
        return Some(DownloadLink115 {
            url: url_str.to_string(),
            file_name,
            file_size,
            headers,
        });
    }
    None
}

// === Folder Resolution, Rapid Transfer (initupload) & Range SHA1 ===

pub fn ensure_folder_cid(
    cookie: &str,
    access_token: &str,
    folder_path: &str,
    proxy_url: &str,
    cooldown_ms: u64,
) -> std::result::Result<String, P115Error> {
    let clean = folder_path.trim().trim_matches('/');
    if clean.is_empty() {
        return Ok("0".to_string());
    }
    let uid = extract_uid_from_cookie(cookie);
    let agent = build_agent(proxy_url, 20);
    let mut current_cid = "0".to_string();

    for seg in clean.split('/').map(str::trim).filter(|s| !s.is_empty()) {
        throttle_uid("folder", &uid, cooldown_ms);
        let mut found_cid = None;

        if !cookie.trim().is_empty() {
            let list_resp = agent
                .get("https://webapi.115.com/files")
                .query("aid", "1")
                .query("cid", &current_cid)
                .query("show_dir", "1")
                .query("limit", "200")
                .query("offset", "0")
                .set("Cookie", cookie.trim())
                .set("User-Agent", DEFAULT_115_UA)
                .call();
            if let Ok(r) = list_resp {
                let status = r.status();
                if let Ok(v) = r.into_json::<Value>() {
                    check_405_or_errno(status, &v)?;
                    if let Some(arr) = v["data"].as_array() {
                        for item in arr {
                            let is_dir = item.get("cid").is_some() && item.get("fid").is_none();
                            let name = item["n"].as_str().unwrap_or_default();
                            if is_dir && name == seg {
                                found_cid = item["cid"]
                                    .as_str()
                                    .map(str::to_string)
                                    .or_else(|| item["cid"].as_u64().map(|n| n.to_string()));
                                break;
                            }
                        }
                    }
                }
            }
            if found_cid.is_none() {
                throttle_uid("folder", &uid, cooldown_ms);
                let add_resp = agent
                    .post("https://webapi.115.com/files/add")
                    .set("Cookie", cookie.trim())
                    .set("User-Agent", DEFAULT_115_UA)
                    .send_form(&[("pid", current_cid.as_str()), ("cname", seg)]);
                if let Ok(r) = add_resp {
                    if let Ok(v) = r.into_json::<Value>() {
                        found_cid = v["cid"]
                            .as_str()
                            .map(str::to_string)
                            .or_else(|| v["cid"].as_u64().map(|n| n.to_string()))
                            .or_else(|| v["file_id"].as_str().map(str::to_string));
                    }
                }
            }
        } else if !access_token.trim().is_empty() {
            let add_resp = agent
                .post("https://proapi.115.com/open/folder/add")
                .set("Authorization", &format!("Bearer {}", access_token.trim()))
                .set("User-Agent", DEFAULT_115_UA)
                .send_form(&[("pid", current_cid.as_str()), ("file_name", seg)]);
            if let Ok(r) = add_resp {
                if let Ok(v) = r.into_json::<Value>() {
                    found_cid = v["data"]["file_id"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| v["data"]["file_id"].as_u64().map(|n| n.to_string()));
                }
            }
        }

        match found_cid.filter(|c| !c.is_empty() && c != "0") {
            Some(c) => current_cid = c,
            None => return Err(P115Error::Other(format!("无法定位或创建缓存目录段: {seg}"))),
        }
    }
    Ok(current_cid)
}

pub fn fetch_range_sha1_from_url(
    url: &str,
    user_agent: &str,
    range_spec: &str,
    proxy_url: &str,
) -> Result<String> {
    let clean_range = range_spec.trim().trim_start_matches("bytes=");
    let (start_s, end_s) = clean_range
        .split_once('-')
        .context("无效的 sign_check 区间格式")?;
    let start: usize = start_s.trim().parse()?;
    let end: usize = end_s.trim().parse()?;
    ensure!(end >= start && end - start <= 16 * 1024 * 1024, "sign_check 区间大小越界");
    let expected_len = end - start + 1;

    let agent = build_agent(proxy_url, 22);
    let resp = agent
        .get(url)
        .set("User-Agent", if user_agent.is_empty() { DEFAULT_115_UA } else { user_agent })
        .set("Range", &format!("bytes={start}-{end}"))
        .set("Accept-Encoding", "identity")
        .call()
        .map_err(|e| anyhow!("拉取源盘 Range 校验块失败: {e}"))?;

    let mut buf = Vec::with_capacity(expected_len);
    resp.into_reader()
        .take(expected_len as u64)
        .read_to_end(&mut buf)?;
    ensure!(
        buf.len() == expected_len,
        "源盘返回校验块长度不匹配（期望 {expected_len}，实际 {}）",
        buf.len()
    );
    let mut sha = Sha1::new();
    sha.update(&buf);
    Ok(hex_upper(&sha.finalize()))
}

#[derive(Debug, Clone)]
pub struct RapidTransferOutcome {
    pub pick_code: String,
    pub file_id: String,
}

/// Executes 115 rapid transfer (`status=2` direct hit, or `status=7` secondary range SHA1 check).
pub fn rapid_transfer<F>(
    cookie: &str,
    access_token: &str,
    creds: Option<&UploadCredentials>,
    target_cid: &str,
    file_name: &str,
    file_size: u64,
    file_sha1: &str,
    proxy_url: &str,
    cooldown_ms: u64,
    mut fetch_sign_val: F,
) -> std::result::Result<RapidTransferOutcome, P115Error>
where
    F: FnMut(&str) -> Result<String>,
{
    let sha1_up = file_sha1.trim().to_ascii_uppercase();
    if sha1_up.len() != 40 || file_size == 0 {
        return Err(P115Error::Other("缺少有效的 40 位文件 SHA1 或文件大小，无法执行秒传".to_string()));
    }
    let cid = if target_cid.trim().is_empty() { "0" } else { target_cid.trim() };
    let target = format!("U_1_{cid}");
    let agent = build_agent(proxy_url, 22);

    // Path A: Cookie + uploadinfo ECDH protocol
    if !cookie.trim().is_empty() {
        let owned_creds;
        let uc = match creds {
            Some(c) if !c.user_id.is_empty() && !c.user_key.is_empty() => c,
            _ => {
                let (c, _) = fetch_upload_credentials(cookie, proxy_url, cooldown_ms)?;
                owned_creds = c;
                &owned_creds
            }
        };

        let call_init = |sign_key: &str, sign_val: &str, do_throttle: bool| -> std::result::Result<Value, P115Error> {
            if do_throttle {
                throttle_uid("initupload", &uc.user_id, cooldown_ms);
            }
            let ts = now_ts() as u32;
            let (k_ec, body) = make_upload_init_request(
                &uc.user_id,
                &uc.user_key,
                &sha1_up,
                file_name,
                file_size,
                &target,
                sign_key,
                sign_val,
                ts,
            );
            let url = format!("https://uplb.115.com/4.0/initupload.php?k_ec={}", urlencoding_simple(&k_ec));
            let r = agent
                .post(&url)
                .set("Cookie", cookie.trim())
                .set(
                    "User-Agent",
                    "Mozilla/5.0 115disk/36.0.0 115Browser/36.0.0 115wangpan_android/36.0.0",
                )
                .set("Content-Type", "application/x-www-form-urlencoded")
                .send_bytes(&body)
                .map_err(|e| match e {
                    ureq::Error::Status(405 | 403, _) => P115Error::Risk405("initupload HTTP 405".to_string()),
                    other => P115Error::Other(format!("请求 initupload 失败: {other}")),
                })?;
            let mut raw = Vec::new();
            r.into_reader()
                .take(512 * 1024)
                .read_to_end(&mut raw)
                .map_err(|e| P115Error::Other(e.to_string()))?;
            let decrypted = ecdh_aes_decrypt(&raw)
                .map_err(|e| P115Error::Other(format!("解密 initupload 响应失败: {e}")))?;
            serde_json::from_slice(&decrypted)
                .map_err(|e| P115Error::Other(format!("解析 initupload JSON 失败: {e}")))
        };

        let mut resp = call_init("", "", true)?;
        check_405_or_errno(200, &resp)?;
        if resp["status"].as_i64() == Some(7) {
            let sign_key = resp["sign_key"].as_str().unwrap_or_default().to_string();
            let sign_check = resp["sign_check"].as_str().unwrap_or_default().to_string();
            let sign_val = fetch_sign_val(&sign_check)
                .map_err(|e| P115Error::Other(format!("计算秒传二次校验 sign_val 失败: {e}")))?;
            resp = call_init(&sign_key, &sign_val, false)?;
            check_405_or_errno(200, &resp)?;
        }
        if resp["status"].as_i64() == Some(2) {
            let pc = resp["pickcode"]
                .as_str()
                .or_else(|| resp["pick_code"].as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            if !pc.is_empty() {
                let fid = pickcode_to_id(&pc).map(|n| n.to_string()).unwrap_or_default();
                return Ok(RapidTransferOutcome {
                    pick_code: pc,
                    file_id: fid,
                });
            }
        }
        return Err(P115Error::Other(format!(
            "115 未命中秒传 (status={})",
            resp["status"]
        )));
    }

    // Path B: OpenAPI upload/init
    if !access_token.trim().is_empty() {
        let call_open = |sign_key: &str, sign_val: &str| -> std::result::Result<Value, P115Error> {
            let size_s = file_size.to_string();
            let mut form = vec![
                ("fileid", sha1_up.as_str()),
                ("file_name", file_name),
                ("file_size", size_s.as_str()),
                ("target", target.as_str()),
                ("topupload", "1"),
            ];
            if !sign_key.is_empty() {
                form.push(("sign_key", sign_key));
            }
            if !sign_val.is_empty() {
                form.push(("sign_val", sign_val));
            }
            let r = agent
                .post("https://proapi.115.com/open/upload/init")
                .set("Authorization", &format!("Bearer {}", access_token.trim()))
                .set("User-Agent", DEFAULT_115_UA)
                .send_form(&form)
                .map_err(|e| P115Error::Other(format!("OpenAPI upload/init 失败: {e}")))?;
            r.into_json().map_err(|e| P115Error::Other(e.to_string()))
        };

        let mut resp = call_open("", "")?;
        check_405_or_errno(200, &resp)?;
        if resp["data"]["status"].as_i64() == Some(7) {
            let sk = resp["data"]["sign_key"].as_str().unwrap_or_default().to_string();
            let sc = resp["data"]["sign_check"].as_str().unwrap_or_default().to_string();
            let sv = fetch_sign_val(&sc).map_err(|e| P115Error::Other(e.to_string()))?;
            resp = call_open(&sk, &sv)?;
            check_405_or_errno(200, &resp)?;
        }
        if resp["data"]["status"].as_i64() == Some(2) {
            let pc = resp["data"]["pick_code"]
                .as_str()
                .or_else(|| resp["data"]["pickcode"].as_str())
                .unwrap_or_default()
                .to_string();
            if !pc.is_empty() {
                let fid = pickcode_to_id(&pc).map(|n| n.to_string()).unwrap_or_default();
                return Ok(RapidTransferOutcome {
                    pick_code: pc,
                    file_id: fid,
                });
            }
        }
        return Err(P115Error::Other("OpenAPI 未命中秒传".to_string()));
    }

    Err(P115Error::AuthInvalid("目标账号缺少 Cookie 或 OpenAPI 令牌".to_string()))
}

fn urlencoding_simple(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// === Quota File Deletion & Recycle Bin Cleanup ===

pub fn delete_115_files(
    cookie: &str,
    access_token: &str,
    file_ids: &[String],
    proxy_url: &str,
    cooldown_ms: u64,
) -> Result<usize> {
    let clean: Vec<&str> = file_ids
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "0" && s.bytes().all(|b| b.is_ascii_digit()))
        .collect();
    if clean.is_empty() {
        return Ok(0);
    }
    let uid = extract_uid_from_cookie(cookie);
    let agent = build_agent(proxy_url, 25);
    let mut removed = 0usize;

    for chunk in clean.chunks(100) {
        throttle_uid("delete", &uid, cooldown_ms);
        if !cookie.trim().is_empty() {
            let mut form: Vec<(String, String)> = Vec::with_capacity(chunk.len() + 1);
            form.push(("ignore_warn".to_string(), "1".to_string()));
            for (idx, fid) in chunk.iter().enumerate() {
                form.push((format!("fid[{idx}]"), (*fid).to_string()));
            }
            let refs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let v: Value = agent
                .post("https://webapi.115.com/rb/delete")
                .set("Cookie", cookie.trim())
                .set("User-Agent", DEFAULT_115_UA)
                .send_form(&refs)?
                .into_json()?;
            if v["state"].as_bool() == Some(true) || v["state"].as_i64() == Some(1) {
                removed += chunk.len();
            }
        } else if !access_token.trim().is_empty() {
            let joined = chunk.join(",");
            let v: Value = agent
                .post("https://proapi.115.com/open/ufile/delete")
                .set("Authorization", &format!("Bearer {}", access_token.trim()))
                .set("User-Agent", DEFAULT_115_UA)
                .send_form(&[("file_ids", joined.as_str())])?
                .into_json()?;
            if v["state"].as_bool() == Some(true) || v["state"].as_i64() == Some(1) {
                removed += chunk.len();
            }
        }
    }
    Ok(removed)
}

pub fn clean_recycle_bin(
    cookie: &str,
    access_token: &str,
    recycle_password: &str,
    proxy_url: &str,
    cooldown_ms: u64,
) -> Result<()> {
    let uid = extract_uid_from_cookie(cookie);
    throttle_uid("recycle", &uid, cooldown_ms);
    let agent = build_agent(proxy_url, 25);

    if !access_token.trim().is_empty() {
        let v: Value = agent
            .post("https://proapi.115.com/open/rb/del")
            .set("Authorization", &format!("Bearer {}", access_token.trim()))
            .set("User-Agent", DEFAULT_115_UA)
            .send_form(&[])?
            .into_json()?;
        if v["state"].as_bool() == Some(true) || v["state"].as_i64() == Some(1) {
            return Ok(());
        }
    }

    if !cookie.trim().is_empty() {
        let pwd = if recycle_password.trim().is_empty() {
            "000000"
        } else {
            recycle_password.trim()
        };
        let v: Value = agent
            .post("https://webapi.115.com/rb/clean")
            .set("Cookie", cookie.trim())
            .set("User-Agent", DEFAULT_115_UA)
            .send_form(&[("password", pwd)])?
            .into_json()?;
        ensure!(
            v["state"].as_bool() == Some(true) || v["state"].as_i64() == Some(1),
            "清空 115 回收站未成功: {}",
            v["error"].as_str().or_else(|| v["msg"].as_str()).unwrap_or("请核对回收站安全密钥")
        );
        return Ok(());
    }

    bail!("缺少可用的 115 凭证，无法清理回收站")
}

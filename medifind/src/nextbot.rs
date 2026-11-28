use crate::db::Db;
use crate::engine::{self, TransferRequest};
use crate::host::HostClient;
use crate::models::{
    default_nextbot_template, AppConfig, MovieAttributes, NextBotConfig, PluginSettings,
    ResourceItem,
};
use crate::providers::build_http_agent;
use crate::rules;
use crate::tg_mtproto;
use anyhow::{anyhow, ensure, Result};
use base64::engine::general_purpose::{STANDARD as B64_STD, URL_SAFE_NO_PAD as B64_URL};
use base64::Engine;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

const NEXTFIND_LINK_SECRET_KEY: &str = "NextFind_V1_Link_Secret_Obfuscation_Key_8f9a2b";
const RADAR_DEDUPE_TTL_SECS: i64 = 7 * 86400;
const MAX_RADAR_PER_TICK: usize = 10;

static RE_BOT_115: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(https?://(?:[a-z0-9.]*\.)?(?:115|115cdn|anxia)\.com/s/[^\s'"<>#)\]}]+|115://[A-Za-z0-9]+(?:[?#][A-Za-z0-9=&_-]+)?)"#).unwrap()
});
static RE_BOT_ED2K: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(ed2k://\|file\|[^|\s]+\|\d+\|[A-Fa-f0-9]{32}\|(?:[^\s"'<>]*\|)?/)"#).unwrap()
});
static RE_BOT_MAGNET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(magnet:\?xt=urn:btih:[A-Za-z0-9]+[^\s"'<>]*)"#).unwrap()
});
static RE_SHARE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"/s/([a-zA-Z0-9_-]+(?:\?password=[a-zA-Z0-9]+)?)"#).unwrap()
});
static RE_ED2K_SPLIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(ed2k://\|file\|[^|]+\|)(.*?)(\|/)$"#).unwrap()
});
static RE_MAGNET_SPLIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(magnet:\?xt=)([^&]+)(.*)$"#).unwrap()
});

#[derive(Debug, Clone, Default)]
pub struct NextBotMessageData {
    pub notification_type: String,
    pub media_type: String,
    pub base_title: String,
    pub ep_part: String,
    pub tmdb_id: String,
    pub vote_average: String,
    pub theme: String,
    pub region: String,
    pub overview: String,
    pub media_tags: String,
    pub size_str: String,
    pub total_size_str: String,
    pub trigger_reason_folder: String,
    pub source: String,
    pub slug: String,
    pub poster_urls: Vec<String>,
    pub was_encrypted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyBotResult {
    pub title: String,
    pub avatar: String,
    pub corrected_chat_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotIncomingLink {
    pub slug: String,
    pub title: String,
    pub tmdb_id: String,
    pub media_type: String,
    pub source_kind: &'static str,
}

pub fn mask_bot_token(token: &str) -> String {
    let t = token.trim();
    if t.is_empty() {
        return String::new();
    }
    if let Some((prefix, _)) = t.split_once(':') {
        if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) {
            return format!("{}:********", prefix);
        }
    }
    "********".to_string()
}

pub fn is_masked_bot_token(token: &str) -> bool {
    let t = token.trim();
    t == "********" || t.ends_with(":********")
}

pub fn resolve_saved_bot_token(
    input_token: &str,
    bot_id: &str,
    chat_id: &str,
    saved_bots: &[NextBotConfig],
) -> String {
    let raw = input_token.trim();
    if !is_masked_bot_token(raw) {
        return raw.to_string();
    }
    let bid = bot_id.trim();
    let cid = chat_id.trim();
    let prefix = raw.split_once(':').map(|(p, _)| p).unwrap_or("");

    if !bid.is_empty() {
        if let Some(b) = saved_bots.iter().find(|b| b.id.trim() == bid && !b.bot_token.trim().is_empty()) {
            return b.bot_token.trim().to_string();
        }
    }
    if !prefix.is_empty() {
        let want = format!("{}:", prefix);
        if let Some(b) = saved_bots.iter().find(|b| b.bot_token.trim().starts_with(&want)) {
            return b.bot_token.trim().to_string();
        }
    }
    if !cid.is_empty() {
        if let Some(b) = saved_bots.iter().find(|b| b.chat_id.trim() == cid && !b.bot_token.trim().is_empty()) {
            return b.bot_token.trim().to_string();
        }
    }
    String::new()
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn map_tmdb_country_to_region(origin_country: &[String], original_language: &str) -> &'static str {
    let joined = origin_country
        .iter()
        .map(|s| s.trim().to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(",");
    let lang = original_language.trim().to_ascii_lowercase();
    if joined.contains("CN") {
        return "大陆";
    }
    if joined.contains("HK") || joined.contains("TW") {
        return "港台";
    }
    if joined.contains("JP") || lang == "ja" {
        return "日本";
    }
    if joined.contains("KR") || lang == "ko" {
        return "韩国";
    }
    if joined.contains("IN")
        || joined.contains("TH")
        || joined.contains("VN")
        || joined.contains("SG")
        || matches!(lang.as_str(), "hi" | "th" | "vi")
    {
        return "亚太";
    }
    if !joined.is_empty() || matches!(lang.as_str(), "en" | "fr" | "de" | "es" | "it") {
        return "欧美";
    }
    ""
}

pub fn map_tmdb_genres_to_theme(genre_ids: &[i32], genre_names: &[String]) -> String {
    let names: Vec<&str> = genre_names
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .take(3)
        .collect();
    if !names.is_empty() {
        return names.join(" / ");
    }
    let mut labels: Vec<&'static str> = Vec::new();
    for &gid in genre_ids {
        let lbl = match gid {
            28 | 12 | 10759 => "动作冒险",
            16 => "动漫",
            35 => "喜剧",
            80 | 53 => "惊悚犯罪",
            99 => "纪录片",
            18 => "剧情",
            14 | 878 | 10765 => "科幻奇幻",
            27 | 9648 => "恐怖悬疑",
            10749 | 10751 => "爱情家庭",
            10752 | 10768 => "战争历史",
            36 => "历史",
            _ => continue,
        };
        if !labels.contains(&lbl) {
            labels.push(lbl);
            if labels.len() >= 2 {
                break;
            }
        }
    }
    labels.join(" / ")
}

pub fn format_movie_attrs_tags(attrs: &MovieAttributes) -> String {
    let mut parts = Vec::new();
    for v in [
        attrs.resolution.trim(),
        attrs.remux.trim(),
        attrs.web_source.trim(),
        attrs.color.trim(),
        attrs.audio.trim(),
        attrs.subtitle.trim(),
        attrs.ext.trim(),
    ] {
        if !v.is_empty() {
            parts.push(format!("[{}]", v));
        }
    }
    parts.join(" ")
}

fn xor_text(value: &str, key: &str) -> String {
    let key_chars: Vec<char> = key.chars().collect();
    if key_chars.is_empty() {
        return value.to_string();
    }
    value
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let k = key_chars[i % key_chars.len()];
            char::from_u32((c as u32) ^ (k as u32)).unwrap_or(c)
        })
        .collect()
}

pub fn encrypt_nextfind_link(slug: &str, chat_id: &str) -> String {
    let raw = slug.trim();
    if raw.is_empty() {
        return String::new();
    }
    let enc_payload = |core: &str| -> String {
        let payload = if !chat_id.trim().is_empty() {
            format!("{}|{}", chat_id.trim(), core)
        } else {
            core.to_string()
        };
        let xored = xor_text(&payload, NEXTFIND_LINK_SECRET_KEY);
        B64_URL.encode(xored.as_bytes())
    };
    if let Some(caps) = RE_ED2K_SPLIT.captures(raw) {
        let enc = enc_payload(&caps[2]);
        return format!("{}{}{}", &caps[1], enc, &caps[3]);
    }
    if let Some(caps) = RE_MAGNET_SPLIT.captures(raw) {
        let enc = enc_payload(&caps[2]);
        return format!("{}{}{}", &caps[1], enc, &caps[3]);
    }
    let core = RE_SHARE_PATH
        .captures(raw)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .unwrap_or(raw);
    format!("https://115cdn.com/s/{}", enc_payload(core))
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in s.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

pub fn build_custom_message(bot: &NextBotConfig, data: &NextBotMessageData) -> String {
    let tpl = if bot.template_config.is_empty() {
        default_nextbot_template()
    } else {
        bot.template_config.clone()
    };

    let media_icon = match data.media_type.trim().to_ascii_lowercase().as_str() {
        "tv" | "series" => "📺",
        "movie" => "🎬",
        _ => "📦",
    };
    let media_type_cn = match data.media_type.trim().to_ascii_lowercase().as_str() {
        "tv" | "series" => "电视剧",
        "movie" => "电影",
        _ => "其他",
    };
    let mut lines: Vec<String> = Vec::new();
    let tid = data.tmdb_id.trim();

    for field in &tpl {
        if !field.enabled {
            continue;
        }
        match field.id.as_str() {
            "title" => {
                let base = if data.base_title.trim().is_empty() {
                    "未知资源"
                } else {
                    data.base_title.trim()
                };
                let mut t = html_escape(base);
                if field.append_episodes && !data.ep_part.trim().is_empty() {
                    t.push(' ');
                    t.push_str(&html_escape(data.ep_part.trim()));
                }
                lines.push(format!("{} <b>{}：</b>{}", media_icon, media_type_cn, t));
            }
            "download_episodes" => {
                if !data.ep_part.trim().is_empty() {
                    lines.push(format!(
                        "📥 <b>转存入库:</b> {}",
                        html_escape(data.ep_part.trim())
                    ));
                }
            }
            "tmdb_id" => {
                if !tid.is_empty() && tid != "0" {
                    lines.push(format!("🍿 <b>TMDB ID:</b> {}", html_escape(tid)));
                }
            }
            "rating" => {
                if !data.vote_average.trim().is_empty() && data.vote_average.trim() != "0" && data.vote_average.trim() != "0.0" {
                    lines.push(format!(
                        "⭐️ <b>评分:</b> {}",
                        html_escape(data.vote_average.trim())
                    ));
                }
            }
            "theme" => {
                if !data.theme.trim().is_empty() {
                    lines.push(format!("🎭 <b>题材:</b> {}", html_escape(data.theme.trim())));
                }
            }
            "region" => {
                if !data.region.trim().is_empty() {
                    lines.push(format!("📂 <b>地区:</b> {}", html_escape(data.region.trim())));
                }
            }
            "overview" => {
                let ov = data.overview.trim();
                if !ov.is_empty() {
                    lines.push(format!(
                        "📝 <b>剧情简介:</b>\n{}",
                        html_escape(&truncate_chars(ov, 180))
                    ));
                }
            }
            "quality" => {
                if !data.media_tags.trim().is_empty() {
                    lines.push(format!(
                        "🎞️ <b>质量:</b> {}",
                        html_escape(data.media_tags.trim())
                    ));
                }
            }
            "size" => {
                let sz = if field.show_total_size && !data.total_size_str.trim().is_empty() {
                    data.total_size_str.trim()
                } else if !data.size_str.trim().is_empty() {
                    data.size_str.trim()
                } else {
                    data.total_size_str.trim()
                };
                if !sz.is_empty() {
                    lines.push(format!("💾 <b>大小:</b> {}", html_escape(sz)));
                }
            }
            "trigger" => {
                if !data.trigger_reason_folder.trim().is_empty() {
                    lines.push(format!(
                        "🎯 <b>触发:</b> {}",
                        html_escape(data.trigger_reason_folder.trim())
                    ));
                }
            }
            "channel" => {
                if !data.source.trim().is_empty() {
                    lines.push(format!("📢 <b>频道:</b> {}", html_escape(data.source.trim())));
                }
            }
            "link" => {
                let raw_slug = data.slug.trim();
                if !raw_slug.is_empty() {
                    if data.was_encrypted {
                        lines.push("🔗 <b>链接:</b> <i>[安全限制：受保护资源禁止二次转发]</i>".into());
                    } else {
                        let out_link = if field.encrypt_link {
                            encrypt_nextfind_link(raw_slug, &bot.chat_id)
                        } else {
                            raw_slug.to_string()
                        };
                        let esc_link = html_escape(&out_link);
                        if out_link.starts_with("http://") || out_link.starts_with("https://") {
                            lines.push(format!(
                                "🔗 <b>链接:</b> <a href=\"{}\">{}</a>",
                                esc_link, esc_link
                            ));
                        } else {
                            lines.push(format!("🔗 <b>链接:</b> <code>{}</code>", esc_link));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut body = if lines.is_empty() {
        html_escape(if !data.base_title.trim().is_empty() {
            data.base_title.trim()
        } else if !data.notification_type.trim().is_empty() {
            data.notification_type.trim()
        } else {
            "MediFind 通知"
        })
    } else {
        lines.join("\n")
    };

    if bot.enable_signature && !bot.custom_signature.trim().is_empty() {
        body.push_str("\n\n");
        body.push_str(&html_escape(bot.custom_signature.trim()));
    }
    body
}

fn read_response_body(resp: ureq::Response, max_bytes: u64) -> String {
    let mut buf = Vec::new();
    let _ = resp.into_reader().take(max_bytes).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn tg_post_json(agent: &ureq::Agent, url: &str, payload: &Value) -> Result<(u16, Value)> {
    let mut last_status = 0u16;
    let mut last_val = json!({});
    for attempt in 0..3 {
        match agent.post(url).send_json(payload.clone()) {
            Ok(resp) => {
                let st = resp.status();
                let txt = read_response_body(resp, 1 << 20);
                let val = serde_json::from_str::<Value>(&txt).unwrap_or_else(|_| json!({ "raw": txt }));
                return Ok((st, val));
            }
            Err(ureq::Error::Status(st, resp)) => {
                let txt = read_response_body(resp, 1 << 20);
                let val = serde_json::from_str::<Value>(&txt).unwrap_or_else(|_| json!({ "raw": txt }));
                last_status = st;
                last_val = val.clone();
                if st == 429 && attempt < 2 {
                    let wait = val
                        .get("parameters")
                        .and_then(|p| p.get("retry_after"))
                        .and_then(|x| x.as_u64())
                        .unwrap_or(3)
                        .min(10);
                    std::thread::sleep(Duration::from_secs(wait + 1));
                    continue;
                }
                return Ok((st, val));
            }
            Err(e) => {
                if attempt == 2 {
                    return Err(anyhow!("请求 Telegram Bot API 失败: {}", e));
                }
                std::thread::sleep(Duration::from_millis(600));
            }
        }
    }
    Ok((last_status, last_val))
}

fn tg_get_json(agent: &ureq::Agent, url: &str, query: &[(&str, &str)]) -> Result<(u16, Value)> {
    let mut req = agent.get(url);
    for (k, v) in query {
        req = req.query(k, v);
    }
    match req.call() {
        Ok(resp) => {
            let st = resp.status();
            let txt = read_response_body(resp, 2 << 20);
            let val = serde_json::from_str::<Value>(&txt).unwrap_or_else(|_| json!({ "raw": txt }));
            Ok((st, val))
        }
        Err(ureq::Error::Status(st, resp)) => {
            let txt = read_response_body(resp, 2 << 20);
            let val = serde_json::from_str::<Value>(&txt).unwrap_or_else(|_| json!({ "raw": txt }));
            Ok((st, val))
        }
        Err(e) => Err(anyhow!("请求 Telegram Bot API 失败: {}", e)),
    }
}

fn explain_tg_bot_error(status: u16, val: &Value) -> String {
    let desc = val
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim();
    let low = desc.to_ascii_lowercase();
    if low.contains("bot can't send messages to the bot") {
        return "目标 Chat ID 配置错误，Telegram 禁止机器人向机器人发消息".into();
    }
    if status == 401 {
        return "Bot Token 未授权或已失效，请检查配置".into();
    }
    if status == 404 {
        return "Bot Token 无效，请检查配置".into();
    }
    if !desc.is_empty() {
        format!("Telegram 返回错误 ({}): {}", status, desc)
    } else {
        format!("Telegram 请求失败: HTTP {}", status)
    }
}

fn is_fatal_bot_error(status: u16, val: &Value) -> bool {
    if matches!(status, 401 | 403 | 404) {
        return true;
    }
    let low = val
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    low.contains("chat not found") || low.contains("bot can't send messages to the bot")
}

pub fn send_message_html(
    agent: &ureq::Agent,
    bot_token: &str,
    chat_id: &str,
    html_text: &str,
) -> Result<()> {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token.trim());
    let payload = json!({
        "chat_id": chat_id.trim(),
        "text": html_text,
        "parse_mode": "HTML",
        "disable_web_page_preview": true,
    });
    let (st, val) = tg_post_json(agent, &url, &payload)?;
    if st == 200 && val.get("ok").and_then(|x| x.as_bool()) == Some(true) {
        return Ok(());
    }
    Err(anyhow!("{}", explain_tg_bot_error(st, &val)))
}

fn send_photo_multipart(
    agent: &ureq::Agent,
    bot_token: &str,
    chat_id: &str,
    image_bytes: &[u8],
    filename: &str,
    caption_html: &str,
) -> Result<()> {
    let boundary = format!("----MediFindNextBot{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(1));
    let mut body: Vec<u8> = Vec::with_capacity(image_bytes.len() + 1024);

    let mut push_field = |name: &str, value: &str| {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", name).as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    };
    push_field("chat_id", chat_id.trim());
    push_field("caption", caption_html);
    push_field("parse_mode", "HTML");

    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"photo\"; filename=\"{}\"\r\nContent-Type: image/jpeg\r\n\r\n",
            filename
        )
        .as_bytes(),
    );
    body.extend_from_slice(image_bytes);
    body.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());

    let url = format!("https://api.telegram.org/bot{}/sendPhoto", bot_token.trim());
    let ctype = format!("multipart/form-data; boundary={}", boundary);
    match agent
        .post(&url)
        .set("Content-Type", &ctype)
        .send_bytes(&body)
    {
        Ok(resp) => {
            let st = resp.status();
            let txt = read_response_body(resp, 1 << 20);
            let val = serde_json::from_str::<Value>(&txt).unwrap_or_else(|_| json!({}));
            if st == 200 && val.get("ok").and_then(|x| x.as_bool()) == Some(true) {
                return Ok(());
            }
            Err(anyhow!("{}", explain_tg_bot_error(st, &val)))
        }
        Err(ureq::Error::Status(st, resp)) => {
            let txt = read_response_body(resp, 1 << 20);
            let val = serde_json::from_str::<Value>(&txt).unwrap_or_else(|_| json!({}));
            Err(anyhow!("{}", explain_tg_bot_error(st, &val)))
        }
        Err(e) => Err(anyhow!("上传海报图片流失败: {}", e)),
    }
}

pub fn send_photo_or_fallback(
    agent: &ureq::Agent,
    bot_token: &str,
    chat_id: &str,
    photo_url: &str,
    caption_html: &str,
) -> Result<bool> {
    // Telegram photo caption hard limit is 1024 characters
    if caption_html.chars().count() > 1000 {
        return Ok(false);
    }
    let url = format!("https://api.telegram.org/bot{}/sendPhoto", bot_token.trim());
    let payload = json!({
        "chat_id": chat_id.trim(),
        "photo": photo_url.trim(),
        "caption": caption_html,
        "parse_mode": "HTML",
    });
    let (st, val) = tg_post_json(agent, &url, &payload)?;
    if st == 200 && val.get("ok").and_then(|x| x.as_bool()) == Some(true) {
        return Ok(true);
    }
    if is_fatal_bot_error(st, &val) {
        return Err(anyhow!("{}", explain_tg_bot_error(st, &val)));
    }
    // Retry by downloading image bytes and uploading via multipart/form-data
    if let Ok(resp) = agent.get(photo_url.trim()).call() {
        if resp.status() == 200 {
            let mut img = Vec::new();
            let _ = resp.into_reader().take(8 << 20).read_to_end(&mut img);
            if img.len() > 256 {
                let fname = photo_url
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("poster.jpg");
                if send_photo_multipart(agent, bot_token, chat_id, &img, fname, caption_html).is_ok()
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

pub fn send_bot_notification(
    agent: &ureq::Agent,
    bot: &NextBotConfig,
    data: &NextBotMessageData,
) -> Result<()> {
    let token = bot.bot_token.trim();
    let chat_id = bot.chat_id.trim();
    ensure!(!token.is_empty() && !chat_id.is_empty(), "Bot Token 或 Chat ID 为空");
    let text = build_custom_message(bot, data);
    if bot.enable_poster {
        for p in &data.poster_urls {
            let u = p.trim();
            if u.is_empty() {
                continue;
            }
            match send_photo_or_fallback(agent, token, chat_id, u, &text) {
                Ok(true) => return Ok(()),
                Ok(false) => continue,
                Err(e) => return Err(e),
            }
        }
    }
    send_message_html(agent, token, chat_id, &text)
}

pub fn verify_bot_chat(
    data_dir: &Path,
    cfg: &AppConfig,
    bot_token: &str,
    raw_chat_id: &str,
    proxy_url: &str,
) -> Result<VerifyBotResult> {
    let token = bot_token.trim();
    let chat_id = raw_chat_id.trim();
    ensure!(!token.is_empty() && !chat_id.is_empty(), "Bot Token 和 接收方 Chat ID 不能为空");

    let bot_id = token.split(':').next().unwrap_or("").trim();
    ensure!(
        !bot_id.is_empty() && token.contains(':'),
        "Bot Token 格式无效（应类似 123456:ABC...）"
    );
    ensure!(
        chat_id != bot_id,
        "验证失败：请勿输入机器人的 ID。接收私聊通知请填写您个人的 Telegram 账号 ID。"
    );

    let agent = build_http_agent(proxy_url, 14);
    let get_chat_url = format!("https://api.telegram.org/bot{}/getChat", token);

    let mut corrected = chat_id.to_string();
    let (mut st, mut val) = tg_get_json(&agent, &get_chat_url, &[("chat_id", &corrected)])?;

    let is_ok = |v: &Value| v.get("ok").and_then(|x| x.as_bool()) == Some(true);
    let is_not_found = |v: &Value| {
        v.get("error_code").and_then(|x| x.as_i64()) == Some(400)
            && v.get("description")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .contains("chat not found")
    };

    if !is_ok(&val) && is_not_found(&val) {
        if chat_id.chars().all(|c| c.is_ascii_digit()) {
            let cand = format!("-100{}", chat_id);
            if let Ok((st2, v2)) = tg_get_json(&agent, &get_chat_url, &[("chat_id", &cand)]) {
                st = st2;
                val = v2;
                if is_ok(&val) {
                    corrected = cand;
                }
            }
        } else if !chat_id.starts_with('-') && !chat_id.starts_with('@') {
            let cand = format!("@{}", chat_id);
            if let Ok((st2, v2)) = tg_get_json(&agent, &get_chat_url, &[("chat_id", &cand)]) {
                st = st2;
                val = v2;
                if is_ok(&val) {
                    corrected = cand;
                }
            }
        }

        // Fallback 3: resolve Chinese/display channel name via logged-in MTProto dialogs
        if !is_ok(&val) && tg_mtproto::has_session_file(data_dir) {
            if let Ok(cands) = tg_mtproto::find_dialog_chat_ids_by_name(data_dir, cfg, chat_id, proxy_url)
            {
                for cand in cands {
                    if let Ok((st2, v2)) = tg_get_json(&agent, &get_chat_url, &[("chat_id", &cand)]) {
                        if is_ok(&v2) {
                            st = st2;
                            val = v2;
                            corrected = cand;
                            break;
                        }
                    }
                }
            }
        }
    }

    if !is_ok(&val) {
        let code = val.get("error_code").and_then(|x| x.as_i64()).unwrap_or(st as i64);
        let desc = val
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        if code == 400 && desc.to_ascii_lowercase().contains("chat not found") {
            return Err(anyhow!(
                "找不到该频道/群组。公开频道请确认 @用户名；私密频道请填数字 ID（或先登录上方 TG 账号按中文名识别）且确保机器人已在内。"
            ));
        } else if matches!(code, 400 | 403) {
            return Err(anyhow!("权限不足：请先将机器人加入该频道/群组。"));
        } else if code == 401 {
            return Err(anyhow!("Bot Token 无效，请检查"));
        }
        return Err(anyhow!("验证失败: {}", if desc.is_empty() { "未知错误" } else { desc }));
    }

    let chat_obj = val.get("result").cloned().unwrap_or_else(|| json!({}));
    let chat_type = chat_obj.get("type").and_then(|x| x.as_str()).unwrap_or("unknown");

    // Verify bot membership & channel admin permission for channels/groups
    if matches!(chat_type, "channel" | "group" | "supergroup") {
        let member_url = format!("https://api.telegram.org/bot{}/getChatMember", token);
        let (_, mval) = tg_get_json(
            &agent,
            &member_url,
            &[("chat_id", &corrected), ("user_id", bot_id)],
        )?;
        ensure!(
            is_ok(&mval),
            "权限不足：请先将机器人加入该频道/群组。"
        );
        let bstat = mval
            .get("result")
            .and_then(|r| r.get("status"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        ensure!(
            !matches!(bstat, "left" | "kicked"),
            "权限不足：机器人不在该频道/群组中或已被踢出。"
        );
        ensure!(
            chat_type != "channel" || bstat == "administrator",
            "权限不足：机器人必须被设置为频道管理员才能发送通知。"
        );
    }

    let title_base = if chat_type == "private" {
        let first = chat_obj.get("first_name").and_then(|x| x.as_str()).unwrap_or("");
        let last = chat_obj.get("last_name").and_then(|x| x.as_str()).unwrap_or("");
        let full = format!("{} {}", first, last).trim().to_string();
        if !full.is_empty() {
            full
        } else {
            chat_obj
                .get("username")
                .and_then(|x| x.as_str())
                .unwrap_or("Private Chat")
                .to_string()
        }
    } else {
        chat_obj
            .get("title")
            .and_then(|x| x.as_str())
            .or_else(|| chat_obj.get("username").and_then(|x| x.as_str()))
            .unwrap_or("Group/Channel")
            .to_string()
    };
    let resolved_id = chat_obj
        .get("id")
        .and_then(|x| x.as_i64())
        .map(|n| n.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| corrected.clone());
    let title = format!("{} ({})", title_base, resolved_id);

    // Fetch small avatar if available
    let mut avatar = String::new();
    if let Some(fid) = chat_obj
        .get("photo")
        .and_then(|p| p.get("small_file_id"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    {
        let file_url = format!("https://api.telegram.org/bot{}/getFile", token);
        if let Ok((_, fval)) = tg_get_json(&agent, &file_url, &[("file_id", fid)]) {
            if let Some(fpath) = fval
                .get("result")
                .and_then(|r| r.get("file_path"))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
            {
                let img_url = format!("https://api.telegram.org/file/bot{}/{}", token, fpath);
                if let Ok(resp) = agent.get(&img_url).call() {
                    if resp.status() == 200 {
                        let mut bytes = Vec::new();
                        let _ = resp.into_reader().take(256 << 10).read_to_end(&mut bytes);
                        if !bytes.is_empty() {
                            avatar = format!("data:image/jpeg;base64,{}", B64_STD.encode(&bytes));
                        }
                    }
                }
            }
        }
    }

    Ok(VerifyBotResult {
        title,
        avatar,
        corrected_chat_id: resolved_id,
    })
}

pub fn test_bot_template(bot: &NextBotConfig, proxy_url: &str) -> Result<String> {
    ensure!(
        !bot.bot_token.trim().is_empty() && !bot.chat_id.trim().is_empty(),
        "Bot Token 和 接收方 Chat ID 不能为空"
    );
    let agent = build_http_agent(proxy_url, 18);
    let test_data = NextBotMessageData {
        notification_type: "入库通知".into(),
        media_type: "tv".into(),
        base_title: "绝命毒师 (2008)".into(),
        ep_part: "S01E01-E07".into(),
        tmdb_id: "1396".into(),
        vote_average: "9.3".into(),
        theme: "惊悚犯罪".into(),
        region: "欧美".into(),
        overview: "这是一段测试剧情简介：一名高中化学老师在得知自己身患绝症后，为了给家人留下财产，踏上了意想不到的命运之路。".into(),
        media_tags: "[2160p] [WEB-DL] [SDR]".into(),
        size_str: "14.44 GB".into(),
        total_size_str: "101.08 GB".into(),
        trigger_reason_folder: "测试触发 → 待洗版".into(),
        source: "MediFind FindBOT 测试通道".into(),
        slug: "https://115.com/s/swtest123456?password=abcd".into(),
        poster_urls: vec![
            "https://image.tmdb.org/t/p/w1280/tsRy63Mu5cu8etL1X7ZLyf7UP1M.jpg".into(),
        ],
        was_encrypted: false,
    };
    send_bot_notification(&agent, bot, &test_data)?;
    Ok("FindBOT 测试消息发送成功，请前往 Telegram 查收".into())
}

pub fn has_active_transfer_bots(cfg: &AppConfig) -> bool {
    cfg.notifications.telegram_bots.iter().any(|b| {
        !b.is_paused
            && b.is_bot_transfer
            && !b.bot_token.trim().is_empty()
            && !is_masked_bot_token(&b.bot_token)
    })
}

pub fn notify_transfer_bots(cfg: &AppConfig, data: &NextBotMessageData, proxy_url: &str) {
    let targets: Vec<&NextBotConfig> = cfg
        .notifications
        .telegram_bots
        .iter()
        .filter(|b| {
            !b.is_paused
                && b.library_notification_enabled
                && !b.bot_token.trim().is_empty()
                && !b.chat_id.trim().is_empty()
        })
        .collect();
    if targets.is_empty() {
        return;
    }
    let agent = build_http_agent(proxy_url, 15);
    for bot in targets {
        let _ = send_bot_notification(&agent, bot, data);
    }
}

pub fn notify_radar_bots(
    db: &Db,
    mc: Option<&HostClient>,
    cfg: &AppConfig,
    items: &[ResourceItem],
    proxy_url: &str,
) -> usize {
    let targets: Vec<&NextBotConfig> = cfg
        .notifications
        .telegram_bots
        .iter()
        .filter(|b| {
            !b.is_paused
                && b.receive_radar
                && !b.bot_token.trim().is_empty()
                && !b.chat_id.trim().is_empty()
        })
        .collect();
    if targets.is_empty() || items.is_empty() {
        return 0;
    }

    let agent = build_http_agent(proxy_url, 15);
    let mut pushed = 0usize;

    for it in items {
        if pushed >= MAX_RADAR_PER_TICK {
            break;
        }
        let ck = if !it.clean_slug.trim().is_empty() {
            it.clean_slug.trim().to_string()
        } else {
            rules::clean_slug(&it.slug)
        };
        if ck.is_empty() || db.is_slug_transferred(&ck) {
            continue;
        }
        let radar_key = format!("nextbot_radar:{}", ck);
        if db.is_in_flight(&radar_key, RADAR_DEDUPE_TTL_SECS) {
            continue;
        }

        let mut base_title = if !it.title.trim().is_empty() {
            rules::clean_display_title(&it.title)
        } else {
            rules::clean_display_title(&it.raw_title)
        };
        let mut vote_average = String::new();
        let mut theme = String::new();
        let mut region = String::new();
        let mut overview = String::new();
        let mut poster_urls = Vec::new();
        let mtype = if it.media_type.eq_ignore_ascii_case("tv") || !it.parsed_episodes.is_empty() {
            "tv"
        } else {
            "movie"
        };

        if let Some(host) = mc {
            if !it.tmdb_id.trim().is_empty() {
                if let Ok(det) = host.get_tmdb_details(it.tmdb_id.trim(), mtype) {
                    let dt = if !det.title.trim().is_empty() {
                        det.title.trim()
                    } else {
                        det.name.trim()
                    };
                    let dy = if det.release_date.len() >= 4 {
                        &det.release_date[..4]
                    } else if det.first_air_date.len() >= 4 {
                        &det.first_air_date[..4]
                    } else {
                        it.year.trim()
                    };
                    if !dt.is_empty() {
                        base_title = if dy.len() == 4 {
                            format!("{} ({})", dt, dy)
                        } else {
                            dt.to_string()
                        };
                    }
                    if det.vote_average > 0.0 {
                        vote_average = format!("{:.1}", det.vote_average);
                    }
                    let gnames: Vec<String> = det.genres.iter().map(|g| g.name.clone()).collect();
                    theme = map_tmdb_genres_to_theme(&det.genre_ids, &gnames);
                    region = map_tmdb_country_to_region(&det.origin_country, &det.original_language)
                        .to_string();
                    overview = det.overview;
                    if let Some(p) = crate::host::tmdb_card_image_https(&det.backdrop_path, &det.poster_path)
                    {
                        poster_urls.push(p);
                    }
                    if let Some(p2) = crate::host::tmdb_poster_https(&det.poster_path) {
                        if !poster_urls.contains(&p2) {
                            poster_urls.push(p2);
                        }
                    }
                }
            }
        }
        if !it.year.trim().is_empty() && !base_title.contains(it.year.trim()) {
            base_title = format!("{} ({})", base_title, it.year.trim());
        }

        let data = NextBotMessageData {
            notification_type: "资源雷达".into(),
            media_type: mtype.into(),
            base_title,
            ep_part: it.parsed_episodes.clone(),
            tmdb_id: it.tmdb_id.clone(),
            vote_average,
            theme,
            region,
            overview,
            media_tags: format_movie_attrs_tags(&it.movie_attributes),
            size_str: it.share_size.clone(),
            total_size_str: it.share_size.clone(),
            trigger_reason_folder: "资源雷达发现新片".into(),
            source: if !it.channel_name.trim().is_empty() {
                it.channel_name.clone()
            } else {
                "Telegram 频道".into()
            },
            slug: it.slug.clone(),
            poster_urls,
            was_encrypted: false,
        };

        let mut sent_any = false;
        for bot in &targets {
            if send_bot_notification(&agent, bot, &data).is_ok() {
                sent_any = true;
            }
        }
        if sent_any {
            db.mark_in_flight(&radar_key);
            pushed += 1;
        }
    }

    pushed
}

pub fn extract_bot_resource_links(text: &str) -> Option<BotIncomingLink> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    let (slug, start_idx, source_kind) = if let Some(m) = RE_BOT_115.find(raw) {
        (m.as_str().to_string(), m.start(), "115")
    } else if let Some(m) = RE_BOT_ED2K.find(raw) {
        (m.as_str().to_string(), m.start(), "ed2k")
    } else if let Some(m) = RE_BOT_MAGNET.find(raw) {
        (m.as_str().to_string(), m.start(), "magnet")
    } else {
        return None;
    };

    let before = raw[..start_idx].trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | '：' | '-' | '—' | '•')
    });
    let fallback = raw
        .replace(&slug, "")
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：' | '-'))
        .to_string();
    let raw_title = if !before.is_empty() {
        before.lines().next().unwrap_or(before).trim().to_string()
    } else if !fallback.is_empty() {
        fallback.lines().next().unwrap_or(&fallback).trim().to_string()
    } else {
        "未知资源".to_string()
    };

    let tmdb_id = rules::extract_tmdb_id(raw);
    let pairs = rules::extract_episode_pairs(raw, 1);
    let media_type = if !pairs.is_empty()
        || raw.contains("剧集")
        || raw.contains("全集")
        || raw.contains("电视剧")
    {
        "tv".to_string()
    } else {
        "movie".to_string()
    };

    Some(BotIncomingLink {
        slug,
        title: rules::clean_display_title(&raw_title),
        tmdb_id,
        media_type,
        source_kind,
    })
}

pub fn is_bot_sender_allowed(bot_chat_id: &str, sender_id: &str, msg_chat_id: &str) -> bool {
    let allowed = bot_chat_id.trim();
    if allowed.is_empty() {
        return true;
    }
    let sid = sender_id.trim();
    let cid = msg_chat_id.trim();
    if sid == allowed || cid == allowed {
        return true;
    }
    if allowed.starts_with("-100") && cid == allowed.trim_start_matches("-100") {
        return true;
    }
    false
}

pub fn poll_bot_transfers(
    mc: &HostClient,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
    proxy_url: &str,
) -> (usize, Vec<String>) {
    let bot_indices: Vec<usize> = cfg
        .notifications
        .telegram_bots
        .iter()
        .enumerate()
        .filter(|(_, b)| {
            !b.is_paused
                && b.is_bot_transfer
                && !b.bot_token.trim().is_empty()
                && !is_masked_bot_token(&b.bot_token)
        })
        .map(|(i, _)| i)
        .collect();
    if bot_indices.is_empty() {
        return (0, Vec::new());
    }

    let agent = build_http_agent(proxy_url, 14);
    let mut transferred = 0usize;
    let mut errors = Vec::new();

    for idx in bot_indices {
        let token = cfg.notifications.telegram_bots[idx].bot_token.trim().to_string();
        let allowed_chat = cfg.notifications.telegram_bots[idx].chat_id.trim().to_string();
        let offset = cfg.notifications.telegram_bots[idx].last_update_id;

        let url = format!("https://api.telegram.org/bot{}/getUpdates", token);
        let offset_str = if offset > 0 {
            (offset + 1).to_string()
        } else {
            String::new()
        };
        let mut params: Vec<(&str, &str)> = vec![
            ("limit", "30"),
            ("timeout", "0"),
            ("allowed_updates", r#"["message","channel_post"]"#),
        ];
        if !offset_str.is_empty() {
            params.push(("offset", &offset_str));
        }

        let (st, val) = match tg_get_json(&agent, &url, &params) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("FindBOT 拉取消息失败: {}", e));
                continue;
            }
        };
        if st != 200 || val.get("ok").and_then(|x| x.as_bool()) != Some(true) {
            errors.push(format!("FindBOT getUpdates: {}", explain_tg_bot_error(st, &val)));
            continue;
        }
        let Some(updates) = val.get("result").and_then(|r| r.as_array()) else {
            continue;
        };

        let mut max_uid = offset;
        let mut pending_msgs: Vec<(String, String, String)> = Vec::new();
        for upd in updates {
            let uid = upd.get("update_id").and_then(|x| x.as_i64()).unwrap_or(0);
            if uid > max_uid {
                max_uid = uid;
            }
            let msg = match upd.get("message").or_else(|| upd.get("channel_post")) {
                Some(m) => m,
                None => continue,
            };
            let msg_chat_id = msg
                .get("chat")
                .and_then(|c| c.get("id"))
                .and_then(|x| x.as_i64())
                .map(|n| n.to_string())
                .unwrap_or_default();
            let sender_id = msg
                .get("from")
                .and_then(|u| u.get("id"))
                .and_then(|x| x.as_i64())
                .map(|n| n.to_string())
                .unwrap_or_default();
            let text = msg
                .get("text")
                .or_else(|| msg.get("caption"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if msg_chat_id.is_empty() || text.is_empty() {
                continue;
            }
            if !is_bot_sender_allowed(&allowed_chat, &sender_id, &msg_chat_id) {
                continue;
            }
            pending_msgs.push((msg_chat_id, sender_id, text));
        }

        if max_uid > offset {
            cfg.notifications.telegram_bots[idx].last_update_id = max_uid;
        }

        for (reply_chat_id, _sender_id, text) in pending_msgs {
            if text.starts_with("/start") || text.starts_with("/help") {
                let _ = send_message_html(
                    &agent,
                    &token,
                    &reply_chat_id,
                    "🤖 <b>MediFind FindBOT 已就绪</b>\n\n直接发送或转发包含 <code>115</code> 分享链接、<code>ed2k://</code> 或 <code>magnet:?</code> 的消息，我将自动识别片名与 TMDB 信息并转存到 115。",
                );
                continue;
            }

            let Some(mut link) = extract_bot_resource_links(&text) else {
                continue;
            };

            if link.tmdb_id.is_empty() && !link.title.is_empty() && link.title != "未知资源" {
                let pure = rules::extract_pure_title(&link.title);
                let q = if pure.is_empty() { &link.title } else { &pure };
                if let Ok(res) = mc.resolve_tmdb(q, &link.media_type, 0) {
                    if !res.effective_id().is_empty() {
                        link.tmdb_id = res.effective_id().to_string();
                        link.media_type = res.effective_type().to_string();
                        if !res.effective_title().is_empty() {
                            link.title = res.effective_title().to_string();
                        }
                    }
                }
            }

            let ack = if !link.tmdb_id.is_empty() {
                format!(
                    "🔍 识别到资源: <b>{}</b> (TMDB: <code>{}</code>)，正在为您转存...",
                    html_escape(&link.title),
                    html_escape(&link.tmdb_id)
                )
            } else {
                format!(
                    "🔍 识别到资源: <b>{}</b>，正在为您转存...",
                    html_escape(&link.title)
                )
            };
            let _ = send_message_html(&agent, &token, &reply_chat_id, &ack);

            let res = engine::transfer_resource(
                mc,
                db,
                settings,
                cfg,
                &TransferRequest {
                    slug: link.slug,
                    title: link.title.clone(),
                    tmdb_id: link.tmdb_id.clone(),
                    media_type: link.media_type.clone(),
                    source_type: link.source_kind.to_string(),
                    channel_name: "FindBOT 转存".into(),
                    allow_unlock: settings.auto_unlock_hdhive,
                    trigger_reason: "BOT转存".into(),
                    ..Default::default()
                },
            );

            match res {
                Ok(tr) if tr.status == "success" => {
                    transferred += 1;
                    let ep_line = if !tr.episodes.is_empty() {
                        format!("\n集数: <code>{}</code>", html_escape(&tr.episodes))
                    } else {
                        String::new()
                    };
                    let reply = format!(
                        "✅ <b>转存成功！</b>\n标题: {}{}\n说明: {}",
                        html_escape(&link.title),
                        ep_line,
                        html_escape(&tr.notice)
                    );
                    let _ = send_message_html(&agent, &token, &reply_chat_id, &reply);
                }
                Ok(tr) if tr.status == "reviewing" => {
                    transferred += 1;
                    let reply = format!(
                        "⏳ <b>已加入 115 待审重试队列</b>\n标题: {}\n说明: {}",
                        html_escape(&link.title),
                        html_escape(&tr.notice)
                    );
                    let _ = send_message_html(&agent, &token, &reply_chat_id, &reply);
                }
                Ok(tr) => {
                    let reply = format!(
                        "ℹ️ <b>已处理</b>\n标题: {}\n说明: {}",
                        html_escape(&link.title),
                        html_escape(&tr.notice)
                    );
                    let _ = send_message_html(&agent, &token, &reply_chat_id, &reply);
                }
                Err(e) => {
                    errors.push(format!("FindBOT 转存失败 [{}]: {}", link.title, e));
                    let reply = format!("❌ <b>转存失败:</b> {}", html_escape(&e.to_string()));
                    let _ = send_message_html(&agent, &token, &reply_chat_id, &reply);
                }
            }
        }
    }

    (transferred, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nextbot_message_builder_encryption_and_parsers() {
        let mut bot = NextBotConfig {
            id: "b1".into(),
            bot_token: "123456:ABC_SECRET".into(),
            chat_id: "-1001234567890".into(),
            enable_signature: true,
            custom_signature: "- Powered by MediFind".into(),
            ..Default::default()
        };
        if let Some(size_f) = bot.template_config.iter_mut().find(|f| f.id == "size") {
            size_f.show_total_size = true;
        }
        if let Some(link_f) = bot.template_config.iter_mut().find(|f| f.id == "link") {
            link_f.encrypt_link = true;
        }

        let data = NextBotMessageData {
            notification_type: "入库通知".into(),
            media_type: "tv".into(),
            base_title: "绝命毒师 (2008)".into(),
            ep_part: "S01E01-E07".into(),
            tmdb_id: "1396".into(),
            vote_average: "9.3".into(),
            theme: "惊悚犯罪".into(),
            region: "欧美".into(),
            overview: "测试剧情 <script>alert(1)</script>".into(),
            media_tags: "[4K] [WEB-DL] [DV]".into(),
            size_str: "14.44 GB".into(),
            total_size_str: "101.08 GB".into(),
            trigger_reason_folder: "订阅自动追更 → 待洗版".into(),
            source: "TG · 爱影频道".into(),
            slug: "https://115.com/s/swabcdef123?password=9999".into(),
            ..Default::default()
        };

        let msg = build_custom_message(&bot, &data);
        assert!(msg.contains("📺 <b>电视剧：</b>绝命毒师 (2008) S01E01-E07"));
        assert!(msg.contains("📥 <b>转存入库:</b> S01E01-E07"));
        assert!(msg.contains("🍿 <b>TMDB ID:</b> 1396"));
        assert!(msg.contains("💾 <b>大小:</b> 101.08 GB"));
        assert!(msg.contains("https://115cdn.com/s/"));
        assert!(!msg.contains("swabcdef123"));
        assert!(msg.contains("- Powered by MediFind"));
        // Overview disabled by default
        assert!(!msg.contains("剧情简介"));

        // Verify XOR+Base64URL round-trip parity with NextFind's LINK_SECRET_KEY
        let enc_url = encrypt_nextfind_link(
            "https://115.com/s/swabcdef123?password=9999",
            "-1001234567890",
        );
        let b64_part = enc_url.strip_prefix("https://115cdn.com/s/").unwrap();
        let raw_bytes = B64_URL.decode(b64_part).unwrap();
        let dec = xor_text(&String::from_utf8(raw_bytes).unwrap(), NEXTFIND_LINK_SECRET_KEY);
        assert_eq!(dec, "-1001234567890|swabcdef123?password=9999");

        // Verify region/theme mapping & masking
        assert_eq!(map_tmdb_country_to_region(&["CN".into()], "zh"), "大陆");
        assert_eq!(map_tmdb_country_to_region(&["US".into()], "en"), "欧美");
        assert_eq!(map_tmdb_genres_to_theme(&[80, 18], &[]), "惊悚犯罪 / 剧情");
        assert_eq!(mask_bot_token("123456:ABC_SECRET"), "123456:********");
        assert!(is_masked_bot_token("123456:********"));

        // Verify incoming BOT transfer link extraction & auth
        let parsed = extract_bot_resource_links(
            "沙丘2 [tmdbid-693134]\nhttps://115cdn.com/s/sw998877?password=abcd",
        )
        .unwrap();
        assert_eq!(parsed.source_kind, "115");
        assert_eq!(parsed.tmdb_id, "693134");
        assert_eq!(parsed.slug, "https://115cdn.com/s/sw998877?password=abcd");
        assert!(is_bot_sender_allowed("10001", "10001", "10001"));
        assert!(!is_bot_sender_allowed("10001", "99999", "99999"));
    }
}

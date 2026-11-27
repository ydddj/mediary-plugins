use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn today_ymd() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub fn now_datetime_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub fn flex_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?.unwrap_or(Value::Null);
    Ok(match v {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    })
}

pub fn flex_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?.unwrap_or(Value::Null);
    Ok(match v {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
            .unwrap_or(0),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map(|f| f.max(0.0) as u64)
            .unwrap_or(0),
        _ => 0,
    })
}

pub fn flex_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(flex_u64(deserializer)?.min(u32::MAX as u64) as u32)
}

pub fn mask_secret(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 10 {
        return "******".to_string();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}******{tail}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSettings {
    #[serde(rename = "$enabled")]
    pub enabled: bool,
    pub auto_clean_quota: bool,
    pub notify: bool,
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_clean_quota: true,
            notify: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    #[serde(deserialize_with = "flex_u64")]
    pub cache_ttl_secs: u64,
    #[serde(deserialize_with = "flex_u64")]
    pub negative_cache_secs: u64,
    #[serde(deserialize_with = "flex_u64")]
    pub p115_cooldown_ms: u64,
    #[serde(deserialize_with = "flex_u64")]
    pub risk_405_cooldown_secs: u64,
    #[serde(deserialize_with = "flex_u64")]
    pub small_file_direct_mb: u64,
    #[serde(deserialize_with = "flex_u32")]
    pub max_daily_transfers: u32,
    pub fallback_on_fail: bool,
    pub fallback_for_guest: bool,
    /// 播放剧集时后台自动预秒传下一集至同一负载小号并预签直链缓存
    pub enable_next_ep_prewarm: bool,
    /// 详情页停留小号负载直链预热
    pub enable_detail_prefetch: bool,
    #[serde(deserialize_with = "flex_u64")]
    pub detail_prefetch_delay_ms: u64,
    /// 播放出流调度通知（推送至主程序通知中心，按会话去重）
    pub notify_playback: bool,
    /// 风控拦截告警通知（UA 拦截 / 并发超限 / 设备超限 / 到期拦截）
    pub notify_risk_intercept: bool,
    #[serde(deserialize_with = "null_default")]
    pub guest_route: String,
    #[serde(deserialize_with = "null_default")]
    pub p115_proxy_url: String,
    #[serde(deserialize_with = "null_default")]
    pub expired_video_url: String,
    #[serde(deserialize_with = "null_default")]
    pub concurrency_video_url: String,
    #[serde(deserialize_with = "null_default")]
    pub banned_video_url: String,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            cache_ttl_secs: 1800,
            negative_cache_secs: 60,
            p115_cooldown_ms: 250,
            risk_405_cooldown_secs: 1800,
            small_file_direct_mb: 200,
            max_daily_transfers: 0,
            fallback_on_fail: true,
            fallback_for_guest: true,
            enable_next_ep_prewarm: true,
            enable_detail_prefetch: true,
            detail_prefetch_delay_ms: 0,
            notify_playback: false,
            notify_risk_intercept: true,
            guest_route: "load_balance".to_string(),
            p115_proxy_url: String::new(),
            expired_video_url: String::new(),
            concurrency_video_url: String::new(),
            banned_video_url: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Account115 {
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    /// "source" or "lb"
    #[serde(deserialize_with = "null_default")]
    pub role: String,
    #[serde(deserialize_with = "null_default")]
    pub cookies: String,
    #[serde(deserialize_with = "null_default")]
    pub access_token: String,
    #[serde(deserialize_with = "null_default")]
    pub refresh_token: String,
    #[serde(deserialize_with = "flex_id")]
    pub uid: String,
    #[serde(deserialize_with = "null_default")]
    pub user_key: String,
    #[serde(deserialize_with = "null_default")]
    pub app: String,
    #[serde(deserialize_with = "null_default")]
    pub proxy_url: String,
    #[serde(deserialize_with = "null_default")]
    pub cache_path: String,
    #[serde(deserialize_with = "flex_id")]
    pub cache_cid: String,
    pub quota_enabled: bool,
    #[serde(deserialize_with = "flex_u64")]
    pub quota_gb: u64,
    #[serde(deserialize_with = "flex_u64")]
    pub quota_usage_bytes: u64,
    #[serde(deserialize_with = "null_default")]
    pub recycle_bin_code: String,
    pub enabled: bool,
    pub valid: bool,
    #[serde(deserialize_with = "null_default")]
    pub drive_info: String,
    pub sort_order: i32,
}

impl Account115 {
    pub fn sanitized_view(
        &self,
        cooldown_until: i64,
        last_error: &str,
        call_count: u64,
        active_sessions: usize,
    ) -> Value {
        let now = now_ts();
        serde_json::json!({
            "name": self.name,
            "role": self.role,
            "has_cookie": !self.cookies.trim().is_empty(),
            "cookie_masked": mask_secret(&self.cookies),
            "has_open_token": !self.access_token.trim().is_empty(),
            "uid": self.uid,
            "app": if self.app.is_empty() { "tv" } else { &self.app },
            "proxy_url": self.proxy_url,
            "cache_path": if self.cache_path.is_empty() { "/Next" } else { &self.cache_path },
            "cache_cid": self.cache_cid,
            "quota_enabled": self.quota_enabled,
            "quota_gb": self.quota_gb,
            "quota_usage_bytes": self.quota_usage_bytes,
            "has_recycle_code": !self.recycle_bin_code.trim().is_empty(),
            "enabled": self.enabled,
            "valid": self.valid,
            "drive_info": self.drive_info,
            "sort_order": self.sort_order,
            "in_cooldown": cooldown_until > now,
            "cooldown_remaining_secs": (cooldown_until - now).max(0),
            "last_error": last_error,
            "call_count": call_count,
            "active_sessions": active_sessions,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyTemplate {
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "flex_u32")]
    pub max_streams: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub max_devices: u32,
    /// "load_balance" | "own_cookie" | "direct"
    #[serde(deserialize_with = "null_default")]
    pub default_route: String,
    #[serde(deserialize_with = "flex_u64")]
    pub quota_gb: u64,
}

impl Default for PolicyTemplate {
    fn default() -> Self {
        Self {
            name: "默认策略".to_string(),
            max_streams: 3,
            max_devices: 5,
            default_route: "load_balance".to_string(),
            quota_gb: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserPolicy {
    #[serde(deserialize_with = "null_default")]
    pub username: String,
    #[serde(deserialize_with = "flex_id")]
    pub emby_id: String,
    pub is_admin: bool,
    pub is_banned: bool,
    pub is_disabled: bool,
    /// "template" | "load_balance" | "own_cookie" | "direct"
    #[serde(deserialize_with = "null_default")]
    pub route_mode: String,
    #[serde(deserialize_with = "null_default")]
    pub template: String,
    /// YYYY-MM-DD or empty for permanent
    #[serde(deserialize_with = "null_default")]
    pub expiry_date: String,
    #[serde(deserialize_with = "null_default")]
    pub cookies: String,
    #[serde(deserialize_with = "null_default")]
    pub access_token: String,
    #[serde(deserialize_with = "flex_id")]
    pub p115_uid: String,
    #[serde(deserialize_with = "null_default")]
    pub user_key: String,
    #[serde(deserialize_with = "null_default")]
    pub cache_path: String,
    #[serde(deserialize_with = "flex_id")]
    pub cache_cid: String,
    pub quota_enabled: bool,
    #[serde(deserialize_with = "flex_u64")]
    pub quota_gb: u64,
    #[serde(deserialize_with = "flex_u64")]
    pub quota_usage_bytes: u64,
    #[serde(deserialize_with = "null_default")]
    pub recycle_bin_code: String,
    #[serde(deserialize_with = "flex_u32")]
    pub max_streams_override: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub max_devices_override: u32,
    #[serde(deserialize_with = "null_default")]
    pub remark: String,
    #[serde(deserialize_with = "null_default")]
    pub last_ip: String,
}

impl UserPolicy {
    pub fn sanitized_view(&self, active_streams: usize, device_count: usize) -> Value {
        let expired = !self.is_admin
            && !self.expiry_date.trim().is_empty()
            && self.expiry_date.trim() < today_ymd().as_str();
        serde_json::json!({
            "username": self.username,
            "emby_id": self.emby_id,
            "is_admin": self.is_admin,
            "is_banned": self.is_banned,
            "is_disabled": self.is_disabled,
            "is_expired": expired,
            "route_mode": if self.route_mode.is_empty() { "template" } else { &self.route_mode },
            "template": self.template,
            "expiry_date": self.expiry_date,
            "has_own_cookie": !self.cookies.trim().is_empty() || !self.access_token.trim().is_empty(),
            "cookie_masked": mask_secret(&self.cookies),
            "p115_uid": self.p115_uid,
            "cache_path": if self.cache_path.is_empty() { "/EmbyCache" } else { &self.cache_path },
            "quota_enabled": self.quota_enabled,
            "quota_gb": self.quota_gb,
            "quota_usage_bytes": self.quota_usage_bytes,
            "max_streams_override": self.max_streams_override,
            "max_devices_override": self.max_devices_override,
            "remark": self.remark,
            "last_ip": self.last_ip,
            "active_streams": active_streams,
            "device_count": device_count,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskConfig {
    /// "blacklist" or "whitelist"
    #[serde(deserialize_with = "null_default")]
    pub ua_mode: String,
    #[serde(deserialize_with = "null_default")]
    pub banned_ua_keywords: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub allowed_ua_keywords: Vec<String>,
    pub ip_lock_enabled: bool,
    #[serde(deserialize_with = "flex_u32")]
    pub ip_lock_window_mins: u32,
    #[serde(deserialize_with = "flex_u32")]
    pub ip_lock_max_ips: u32,
    pub auto_disable_expired: bool,
    pub enforce_device_limit: bool,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            ua_mode: "blacklist".to_string(),
            banned_ua_keywords: vec![
                "python-requests".to_string(),
                "Scrapy".to_string(),
                "Aria2".to_string(),
                "Thunder".to_string(),
                "IDM".to_string(),
            ],
            allowed_ua_keywords: vec![
                "Infuse".to_string(),
                "SenPlayer".to_string(),
                "VidHub".to_string(),
                "Hills".to_string(),
                "Yamby".to_string(),
                "Forward".to_string(),
                "AfuseKt".to_string(),
                "Emby".to_string(),
                "Mediary".to_string(),
                "Rex".to_string(),
                "MPV".to_string(),
                "Filmly".to_string(),
                "CapyPlayer".to_string(),
                "Lenna".to_string(),
                "Conflux".to_string(),
                "HamHub".to_string(),
            ],
            ip_lock_enabled: false,
            ip_lock_window_mins: 30,
            ip_lock_max_ips: 3,
            auto_disable_expired: true,
            enforce_device_limit: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub engine: EngineSettings,
    pub source_accounts: Vec<Account115>,
    pub lb_accounts: Vec<Account115>,
    pub templates: Vec<PolicyTemplate>,
    pub users: Vec<UserPolicy>,
    pub risk: RiskConfig,
    #[serde(deserialize_with = "null_default")]
    pub last_migrated_from: String,
    pub updated_at: i64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            engine: EngineSettings::default(),
            source_accounts: Vec::new(),
            lb_accounts: Vec::new(),
            templates: vec![PolicyTemplate {
                name: "vvvvvip".to_string(),
                max_streams: 3,
                max_devices: 5,
                default_route: "load_balance".to_string(),
                quota_gb: 0,
            }],
            users: Vec::new(),
            risk: RiskConfig::default(),
            last_migrated_from: String::new(),
            updated_at: now_ts(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ResolveRequest {
    pub user_id: String,
    pub username: String,
    pub is_admin: bool,
    pub item_id: String,
    pub media_source_id: String,
    pub pick_code: String,
    pub sha1: String,
    pub file_size: u64,
    pub file_name: String,
    pub session_id: String,
    pub client: String,
    pub device_id: String,
    pub device_name: String,
    pub user_agent: String,
    pub client_ip: String,
    pub bypass_cache: bool,
    pub prefetch: bool,
}

impl<'de> Deserialize<'de> for ResolveRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Option::<Value>::deserialize(deserializer)?.unwrap_or(Value::Null);
        let str_of = |keys: &[&str]| -> String {
            for k in keys {
                if let Some(val) = v.get(*k) {
                    let s = match val {
                        Value::String(s) => s.trim().to_string(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => String::new(),
                    };
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
            String::new()
        };
        let u64_of = |keys: &[&str]| -> u64 {
            for k in keys {
                if let Some(val) = v.get(*k) {
                    let n = match val {
                        Value::Number(num) => num
                            .as_u64()
                            .or_else(|| num.as_f64().map(|f| f.max(0.0) as u64))
                            .unwrap_or(0),
                        Value::String(s) => s
                            .trim()
                            .parse::<f64>()
                            .map(|f| f.max(0.0) as u64)
                            .unwrap_or(0),
                        _ => 0,
                    };
                    if n > 0 {
                        return n;
                    }
                }
            }
            0
        };
        let bool_of = |key: &str| -> bool {
            match v.get(key) {
                Some(Value::Bool(b)) => *b,
                Some(Value::Number(n)) => n.as_i64().is_some_and(|x| x != 0),
                Some(Value::String(s)) => matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                ),
                _ => false,
            }
        };

        Ok(Self {
            user_id: str_of(&["user_id", "userid"]),
            username: str_of(&["username", "user_name"]),
            is_admin: bool_of("is_admin"),
            item_id: str_of(&["item_id"]),
            media_source_id: str_of(&["media_source_id", "mediasourceid"]),
            pick_code: str_of(&["pick_code", "pickcode"]),
            sha1: str_of(&["sha1"]),
            file_size: u64_of(&["file_size", "size"]),
            file_name: str_of(&["file_name", "name"]),
            session_id: str_of(&["session_id", "session_key"]),
            client: str_of(&["client"]),
            device_id: str_of(&["device_id", "deviceid"]),
            device_name: str_of(&["device_name", "device"]),
            user_agent: str_of(&["user_agent", "ua"]),
            client_ip: str_of(&["client_ip", "ip"]),
            bypass_cache: bool_of("bypass_cache"),
            prefetch: bool_of("prefetch")
                || bool_of("is_prefetch")
                || str_of(&["mode", "purpose", "trigger"])
                    .to_ascii_lowercase()
                    .contains("prefetch"),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveDecision {
    pub handled: bool,
    /// "redirect" | "deny" | "pass"
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fallback_video_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
    pub status_code: u16,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    pub provider: String,
    pub mode_label: String,
    pub cache_hit: bool,
    pub elapsed_ms: u64,
    pub reason: String,
}

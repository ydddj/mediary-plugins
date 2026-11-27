use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn flex_id<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    match Value::deserialize(de)? {
        Value::String(s) => Ok(s.trim().to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Null => Ok(String::new()),
        _ => Err(D::Error::custom("expected string or number id")),
    }
}

pub fn null_default<'de, T: Deserialize<'de> + Default, D: Deserializer<'de>>(
    de: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}

pub fn flex_u32<'de, D: Deserializer<'de>>(de: D) -> Result<u32, D::Error> {
    match Value::deserialize(de)? {
        Value::Null => Ok(0),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(u.min(u32::MAX as u64) as u32)
            } else if let Some(i) = n.as_i64() {
                Ok(if i > 0 { i.min(u32::MAX as i64) as u32 } else { 0 })
            } else if let Some(f) = n.as_f64() {
                Ok(if f > 0.0 { f as u32 } else { 0 })
            } else {
                Ok(0)
            }
        }
        Value::String(s) => Ok(s.trim().parse::<i64>().ok().filter(|&v| v > 0).unwrap_or(0) as u32),
        _ => Ok(0),
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSettings {
    #[serde(rename = "$enabled")]
    pub enabled: bool,
    pub auto_chase: bool,
    pub auto_unlock_hdhive: bool,
    pub notify: bool,
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_chase: true,
            auto_unlock_hdhive: true,
            notify: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MovieAttributes {
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub resolution: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub remux: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub web_source: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub color: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub audio: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub group: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub ext: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub subtitle: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceItem {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub slug: String,
    #[serde(deserialize_with = "null_default")]
    pub clean_slug: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub raw_title: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub remark: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "flex_id")]
    pub tmdb_id: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub media_type: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "flex_id")]
    pub year: String,
    #[serde(deserialize_with = "null_default")]
    pub source_type: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub channel_id: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub channel_name: String,
    #[serde(deserialize_with = "null_default")]
    pub pan_type: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub share_code: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub receive_code: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub share_size: String,
    #[serde(default)]
    pub size_bytes: i64,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub parsed_episodes: String,
    #[serde(default)]
    pub movie_attributes: MovieAttributes,
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "null_default")]
    pub custom_keywords: Vec<String>,
    pub is_unlocked: bool,
    pub unlock_points: i32,
    pub rule_score: f64,
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "null_default")]
    pub rule_reasons: Vec<String>,
    #[serde(default)]
    pub shield_matched: bool,
    #[serde(default)]
    pub shield_diff_gb: f64,
    #[serde(default)]
    pub local_in_library: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "null_default")]
    pub missing_eps_hit: Vec<String>,
    #[serde(default)]
    pub is_reviewing: bool,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub created_at: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub message_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareFileNode {
    #[serde(deserialize_with = "flex_id")]
    pub id: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "flex_id")]
    pub parent_id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub path: String,
    pub size: i64,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub sha1: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub pick_code: String,
    pub is_dir: bool,
    pub is_video: bool,
    pub is_subtitle: bool,
    pub depth: usize,
    pub season: u32,
    pub episode: u32,
    pub episode_end: u32,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub episode_label: String,
    pub is_missing_in_db: bool,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub direct_link: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProbeCacheEntry {
    pub clean_slug: String,
    pub share_code: String,
    pub receive_code: String,
    pub share_url: String,
    pub title: String,
    pub parsed_episodes: String,
    pub movie_attributes: MovieAttributes,
    #[serde(deserialize_with = "null_default")]
    pub files: Vec<ShareFileNode>,
    pub is_unlocked: bool,
    pub unlock_points: i32,
    pub is_valid: bool,
    pub is_reviewing: bool,
    pub updated_ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HDHiveConfig {
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_checkin: bool,
    #[serde(deserialize_with = "null_default")]
    pub protocol: String,
    #[serde(deserialize_with = "null_default")]
    pub base_url: String,
    #[serde(deserialize_with = "null_default")]
    pub api_key: String,
    #[serde(deserialize_with = "null_default")]
    pub refresh_token: String,
    pub access_expire_at: i64,
    pub max_unlock_points: i32,
    #[serde(deserialize_with = "null_default")]
    pub username: String,
    pub points: i32,
    pub checked_in_today: bool,
    #[serde(deserialize_with = "null_default")]
    pub last_checkin_date: String,
    #[serde(deserialize_with = "null_default")]
    pub last_checkin_result: String,
}

impl Default for HDHiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_checkin: true,
            protocol: String::new(),
            base_url: "https://hdhive.nextemby.com".to_string(),
            api_key: String::new(),
            refresh_token: String::new(),
            access_expire_at: 0,
            max_unlock_points: 10,
            username: String::new(),
            points: 0,
            checked_in_today: false,
            last_checkin_date: String::new(),
            last_checkin_result: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TGChannelEntry {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub identifier: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub logo: String,
    pub level: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub hide_in_discover: bool,
    pub last_msg_id: i64,
    pub cached_count: usize,
    pub transferred_count: usize,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub last_sync_at: String,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub last_error: String,
}

/// Converts legacy 1/2/3 levels into NextFind v3.2.8 bitmask values (0=手动, 16=追更, 32=洗版, 64=入新, 99=全量).
pub fn normalize_tg_level(level: i32) -> i32 {
    match level {
        0 => 0,
        1 => 16,
        2 => 48,
        3 | 99 => 99,
        n if n < 0 => 0,
        n => {
            let mask = n & (16 | 32 | 64);
            if mask != 0 {
                mask | 16
            } else {
                16
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "null_default")]
    pub api_id: String,
    #[serde(deserialize_with = "null_default")]
    pub api_hash: String,
    pub is_logged_in: bool,
    #[serde(deserialize_with = "null_default")]
    pub tg_username: String,
    #[serde(deserialize_with = "null_default")]
    pub tg_phone: String,
    #[serde(deserialize_with = "null_default")]
    pub pending_phone: String,
    #[serde(deserialize_with = "null_default")]
    pub pending_code_hash: String,
    pub pending_dc_id: i32,
    pub password_required: bool,
    #[serde(deserialize_with = "null_default")]
    pub password_hint: String,
    #[serde(deserialize_with = "null_default")]
    pub channels: Vec<TGChannelEntry>,
    pub per_channel_limit: usize,
    pub expand_telegraph: bool,
    #[serde(deserialize_with = "null_default")]
    pub bot_token: String,
    #[serde(deserialize_with = "null_default")]
    pub bot_chat_id: String,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_id: String::new(),
            api_hash: String::new(),
            is_logged_in: false,
            tg_username: String::new(),
            tg_phone: String::new(),
            pending_phone: String::new(),
            pending_code_hash: String::new(),
            pending_dc_id: 0,
            password_required: false,
            password_hint: String::new(),
            channels: Vec::new(),
            per_channel_limit: 40,
            expand_telegraph: true,
            bot_token: String::new(),
            bot_chat_id: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AYClubConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "null_default")]
    pub base_url: String,
    #[serde(deserialize_with = "null_default")]
    pub nf_token: String,
    #[serde(deserialize_with = "null_default")]
    pub tg_user_id: String,
    pub remaining_times: Option<i32>,
    #[serde(deserialize_with = "null_default")]
    pub exhausted_date: String,
    #[serde(deserialize_with = "null_default")]
    pub last_checked_at: String,
    #[serde(deserialize_with = "null_default")]
    pub last_checked_status: String,
}

impl Default for AYClubConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://api.ayclub.vip:5050/api/next_find".to_string(),
            nf_token: String::new(),
            tg_user_id: String::new(),
            remaining_times: None,
            exhausted_date: String::new(),
            last_checked_at: String::new(),
            last_checked_status: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RSSFeedConfig {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(deserialize_with = "null_default")]
    pub refresh_interval: String,
    pub auto_subscribe: bool,
    pub auto_transfer: bool,
    pub last_refresh_at: i64,
    #[serde(deserialize_with = "null_default")]
    pub seen_guids: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub last_result: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RuleChipItem {
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub is_custom_size: bool,
    #[serde(default)]
    pub value: i64,
    #[serde(default)]
    pub min: f64,
    #[serde(default)]
    pub max: f64,
}

impl RuleChipItem {
    pub fn simple(name: &str, enabled: bool) -> Self {
        Self {
            name: name.to_string(),
            enabled,
            ..Default::default()
        }
    }

    pub fn size_range(name: &str, min: f64, max: f64, enabled: bool) -> Self {
        Self {
            name: name.to_string(),
            enabled,
            is_custom_size: false,
            value: 0,
            min,
            max,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SubSourceItem {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BlockFilterGroup {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub on_transfer: bool,
    pub on_rss_subscribe: bool,
    pub on_checkin: bool,
    pub include_poster: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            on_transfer: true,
            on_rss_subscribe: true,
            on_checkin: true,
            include_poster: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleConfig {
    #[serde(deserialize_with = "null_default")]
    pub resource_types: Vec<RuleChipItem>,
    #[serde(deserialize_with = "null_default")]
    pub subscription_sources: Vec<SubSourceItem>,
    #[serde(deserialize_with = "null_default")]
    pub weights: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub resolution_items: Vec<RuleChipItem>,
    #[serde(deserialize_with = "null_default")]
    pub color_items: Vec<RuleChipItem>,
    #[serde(deserialize_with = "null_default")]
    pub audio_items: Vec<RuleChipItem>,
    #[serde(deserialize_with = "null_default")]
    pub extension_items: Vec<RuleChipItem>,
    #[serde(deserialize_with = "null_default")]
    pub size_items: Vec<RuleChipItem>,
    #[serde(deserialize_with = "null_default")]
    pub group_items: Vec<RuleChipItem>,
    #[serde(deserialize_with = "null_default")]
    pub block_filters: Vec<BlockFilterGroup>,
    #[serde(deserialize_with = "null_default")]
    pub ignored_texts: Vec<String>,
    pub cutoff_score: f64,
    pub min_score: f64,
    #[serde(deserialize_with = "null_default")]
    pub default_target_cid: String,
    #[serde(deserialize_with = "null_default")]
    pub default_target_name: String,
    pub prefer_flowlink: bool,
}

impl Default for RuleConfig {
    fn default() -> Self {
        Self {
            resource_types: default_resource_type_items(),
            subscription_sources: default_sub_source_items(),
            weights: default_weight_categories(),
            resolution_items: vec![
                RuleChipItem::simple("4K", true),
                RuleChipItem::simple("1080P", true),
                RuleChipItem::simple("720P及以下", false),
            ],
            color_items: vec![
                RuleChipItem::simple("DV&HDR", true),
                RuleChipItem::simple("DV", true),
                RuleChipItem::simple("HDR10", true),
                RuleChipItem::simple("HDR", true),
                RuleChipItem::simple("HLG", true),
                RuleChipItem::simple("SDR", true),
            ],
            audio_items: vec![
                RuleChipItem::simple("TRUEHD", true),
                RuleChipItem::simple("DTS-HD MA", true),
                RuleChipItem::simple("DTS-X", true),
                RuleChipItem::simple("DTS-HD", true),
                RuleChipItem::simple("DTS", true),
                RuleChipItem::simple("EAC3", true),
                RuleChipItem::simple("AC3", true),
                RuleChipItem::simple("FLAC", true),
                RuleChipItem::simple("AAC", true),
            ],
            extension_items: default_extension_items(),
            size_items: default_size_items(),
            group_items: vec![
                RuleChipItem::simple("特效", true),
                RuleChipItem::simple("双语", true),
                RuleChipItem::simple("简繁", true),
                RuleChipItem::simple("中字", true),
                RuleChipItem::simple("杜比", true),
                RuleChipItem::simple("高码", true),
                RuleChipItem::simple("收藏", true),
                RuleChipItem::simple("国粤", true),
            ],
            block_filters: vec![
                BlockFilterGroup { id: "bf-1".into(), tags: vec!["枪版".into()] },
                BlockFilterGroup { id: "bf-2".into(), tags: vec!["偷拍".into()] },
                BlockFilterGroup { id: "bf-3".into(), tags: vec!["CAM".into()] },
                BlockFilterGroup { id: "bf-4".into(), tags: vec!["TS清晰".into()] },
                BlockFilterGroup { id: "bf-5".into(), tags: vec!["HC字幕".into()] },
                BlockFilterGroup { id: "bf-6".into(), tags: vec!["博彩".into()] },
            ],
            ignored_texts: Vec::new(),
            cutoff_score: 85.0,
            min_score: 0.0,
            default_target_cid: String::new(),
            default_target_name: String::new(),
            prefer_flowlink: true,
        }
    }
}

pub fn default_resource_type_items() -> Vec<RuleChipItem> {
    vec![
        RuleChipItem::simple("115", true),
        RuleChipItem::simple("ED2K", true),
        RuleChipItem::simple("MAGNET", true),
    ]
}

pub fn default_sub_source_items() -> Vec<SubSourceItem> {
    vec![
        SubSourceItem { id: "shield".into(), name: "私有神盾".into(), enabled: true },
        SubSourceItem { id: "telegram".into(), name: "Telegram".into(), enabled: true },
        SubSourceItem { id: "hdhive".into(), name: "影巢/HDHive".into(), enabled: true },
        SubSourceItem { id: "ayclub".into(), name: "AYClub".into(), enabled: true },
    ]
}

pub fn default_weight_categories() -> Vec<String> {
    vec![
        "resolutions".into(),
        "colors".into(),
        "audios".into(),
        "extensions".into(),
        "sizes".into(),
        "groups".into(),
    ]
}

pub fn default_extension_items() -> Vec<RuleChipItem> {
    vec![
        RuleChipItem::simple("MKV", true),
        RuleChipItem::simple("MP4", true),
        RuleChipItem::simple("M2TS", true),
        RuleChipItem::simple("TS", false),
        RuleChipItem::simple("ISO", false),
        RuleChipItem::simple("RMVB", false),
        RuleChipItem::simple("AVI", false),
        RuleChipItem::simple("MOV", false),
        RuleChipItem::simple("WMV", false),
        RuleChipItem::simple("小众格式", false),
    ]
}

pub fn default_size_items() -> Vec<RuleChipItem> {
    vec![
        RuleChipItem::simple("由大到小", true),
        RuleChipItem::size_range("40G以上", 40.0, 115.0, true),
        RuleChipItem::size_range("20-40G", 20.0, 40.0, true),
        RuleChipItem::size_range("10-20G", 10.0, 20.0, true),
        RuleChipItem::size_range("5-10G", 5.0, 10.0, true),
        RuleChipItem::size_range("0-5G", 0.0, 5.0, true),
        RuleChipItem::size_range("115G以上", 115.0, 999999.0, false),
        RuleChipItem {
            name: "小于_MB".into(),
            enabled: false,
            is_custom_size: true,
            value: 0,
            min: 0.0,
            max: 0.0,
        },
    ]
}

impl RuleConfig {
    pub fn ensure_weights_initialized(&mut self) {
        let canonical_cats = default_weight_categories();
        let mut clean_weights = Vec::new();
        for w in &self.weights {
            let k = w.trim().to_ascii_lowercase();
            if canonical_cats.iter().any(|c| c == &k) && !clean_weights.contains(&k) {
                clean_weights.push(k);
            }
        }
        for c in canonical_cats {
            if !clean_weights.contains(&c) {
                clean_weights.push(c);
            }
        }
        self.weights = clean_weights;

        if self.resource_types.is_empty() {
            self.resource_types = default_resource_type_items();
        }
        if self.size_items.is_empty() {
            self.size_items = default_size_items();
        } else if !self.size_items.iter().any(|x| x.is_custom_size) {
            self.size_items.push(RuleChipItem {
                name: "小于_MB".into(),
                enabled: false,
                is_custom_size: true,
                value: 0,
                min: 0.0,
                max: 0.0,
            });
        }

        let def = RuleConfig::default();
        if self.resolution_items.is_empty() {
            self.resolution_items = def.resolution_items;
        }
        if self.color_items.is_empty() {
            self.color_items = def.color_items;
        }
        if self.audio_items.is_empty() {
            self.audio_items = def.audio_items;
        }
        if self.extension_items.is_empty() {
            self.extension_items = default_extension_items();
        }
        if self.subscription_sources.is_empty() {
            self.subscription_sources = default_sub_source_items();
        }
    }

    pub fn sync_subscription_sources(&mut self, channels: &[TGChannelEntry]) {
        let mut active_tg: Vec<(String, String)> = Vec::new();
        let mut seen_norm = std::collections::HashSet::new();
        for ch in channels {
            if !ch.enabled {
                continue;
            }
            let raw_key = if !ch.identifier.trim().is_empty() {
                ch.identifier.trim()
            } else {
                ch.name.trim()
            };
            let norm = crate::providers::normalize_tg_channel(raw_key).to_lowercase();
            if norm.is_empty() || !seen_norm.insert(norm.clone()) {
                continue;
            }
            let disp = if !ch.name.trim().is_empty() {
                ch.name.trim().to_string()
            } else {
                raw_key.to_string()
            };
            active_tg.push((format!("telegram:{}", norm), disp));
        }

        let active_tg_map: std::collections::HashMap<String, String> =
            active_tg.iter().cloned().collect();
        let legacy_tg_enabled = self
            .subscription_sources
            .iter()
            .find(|s| s.id.eq_ignore_ascii_case("telegram"))
            .map(|s| s.enabled)
            .unwrap_or(true);

        let base_defs = [
            ("shield", "私有神盾"),
            ("hdhive", "影巢/HDHive"),
            ("ayclub", "AYClub"),
        ];

        let mut merged: Vec<SubSourceItem> = Vec::new();
        let mut inserted_ids = std::collections::HashSet::new();

        for s in &self.subscription_sources {
            let sid_low = s.id.trim().to_lowercase();
            if sid_low.is_empty() {
                continue;
            }
            if sid_low == "nextfind" || sid_low == "shield" {
                if inserted_ids.insert("shield".to_string()) {
                    merged.push(SubSourceItem {
                        id: "shield".into(),
                        name: "私有神盾".into(),
                        enabled: s.enabled,
                    });
                }
            } else if sid_low == "hdhive" {
                if inserted_ids.insert("hdhive".to_string()) {
                    merged.push(SubSourceItem {
                        id: "hdhive".into(),
                        name: "影巢/HDHive".into(),
                        enabled: s.enabled,
                    });
                }
            } else if sid_low == "ayclub" {
                if inserted_ids.insert("ayclub".to_string()) {
                    merged.push(SubSourceItem {
                        id: "ayclub".into(),
                        name: "AYClub".into(),
                        enabled: s.enabled,
                    });
                }
            } else if sid_low == "telegram" {
                if active_tg.is_empty() {
                    if inserted_ids.insert("telegram".to_string()) {
                        merged.push(SubSourceItem {
                            id: "telegram".into(),
                            name: "Telegram".into(),
                            enabled: s.enabled,
                        });
                    }
                } else {
                    for (tg_id, tg_name) in &active_tg {
                        if !self
                            .subscription_sources
                            .iter()
                            .any(|existing| existing.id.trim().eq_ignore_ascii_case(tg_id))
                            && inserted_ids.insert(tg_id.clone())
                        {
                            merged.push(SubSourceItem {
                                id: tg_id.clone(),
                                name: tg_name.clone(),
                                enabled: s.enabled,
                            });
                        }
                    }
                }
            } else if sid_low.starts_with("telegram:") {
                if let Some(latest_name) = active_tg_map.get(&sid_low) {
                    if inserted_ids.insert(sid_low.clone()) {
                        merged.push(SubSourceItem {
                            id: sid_low,
                            name: latest_name.clone(),
                            enabled: s.enabled,
                        });
                    }
                }
            }
        }

        for (bid, bname) in base_defs {
            if inserted_ids.insert(bid.to_string()) {
                merged.push(SubSourceItem {
                    id: bid.into(),
                    name: bname.into(),
                    enabled: true,
                });
            }
        }

        if active_tg.is_empty() {
            if inserted_ids.insert("telegram".to_string()) {
                merged.push(SubSourceItem {
                    id: "telegram".into(),
                    name: "Telegram".into(),
                    enabled: legacy_tg_enabled,
                });
            }
        } else {
            for (tg_id, tg_name) in active_tg {
                if inserted_ids.insert(tg_id.clone()) {
                    merged.push(SubSourceItem {
                        id: tg_id,
                        name: tg_name,
                        enabled: legacy_tg_enabled,
                    });
                }
            }
        }

        self.subscription_sources = merged;
    }

    pub fn find_sub_source_for_item(&self, item: &ResourceItem) -> Option<(usize, &SubSourceItem)> {
        let st = item.source_type.trim().to_ascii_lowercase();
        if st == "telegram" {
            let ch_key = if !item.channel_id.trim().is_empty() {
                item.channel_id.trim().to_lowercase()
            } else {
                crate::providers::normalize_tg_channel(&item.channel_name).to_lowercase()
            };
            if !ch_key.is_empty() {
                let target_id = format!("telegram:{}", ch_key);
                if let Some(found) = self
                    .subscription_sources
                    .iter()
                    .enumerate()
                    .find(|(_, s)| s.id.trim().eq_ignore_ascii_case(&target_id))
                {
                    return Some(found);
                }
            }
            let disp = item
                .channel_name
                .trim()
                .trim_start_matches("TG · ")
                .trim_start_matches('@')
                .trim();
            if !disp.is_empty() {
                if let Some(found) = self.subscription_sources.iter().enumerate().find(|(_, s)| {
                    s.id.to_ascii_lowercase().starts_with("telegram:")
                        && s.name.trim().eq_ignore_ascii_case(disp)
                }) {
                    return Some(found);
                }
            }
            return self
                .subscription_sources
                .iter()
                .enumerate()
                .find(|(_, s)| s.id.trim().eq_ignore_ascii_case("telegram"));
        }
        self.subscription_sources
            .iter()
            .enumerate()
            .find(|(_, s)| s.id.trim().eq_ignore_ascii_case(&st))
    }

    pub fn is_item_auto_sub_enabled(&self, item: &ResourceItem) -> bool {
        self.find_sub_source_for_item(item)
            .map(|(_, s)| s.enabled)
            .unwrap_or(true)
    }
}

impl AppConfig {
    /// Checks whether a candidate item is allowed to be used by automatic subscription chase.
    /// For Telegram items, requires the channel to be enabled AND its 5-tier level to include `追更` (bit 16) or `全量` (99).
    pub fn is_tg_channel_chase_allowed(&self, item: &ResourceItem) -> bool {
        if !item.source_type.trim().eq_ignore_ascii_case("telegram") {
            return true;
        }
        if !self.telegram.enabled {
            return false;
        }
        let item_norm = if !item.channel_id.trim().is_empty() {
            crate::providers::normalize_tg_channel(&item.channel_id).to_lowercase()
        } else {
            crate::providers::normalize_tg_channel(&item.channel_name).to_lowercase()
        };
        let disp = item
            .channel_name
            .trim()
            .trim_start_matches("TG · ")
            .trim_start_matches('@')
            .trim();

        for ch in &self.telegram.channels {
            let ch_raw = if !ch.identifier.trim().is_empty() {
                ch.identifier.trim()
            } else {
                ch.name.trim()
            };
            let ch_norm = crate::providers::normalize_tg_channel(ch_raw).to_lowercase();
            let id_hit = !item_norm.is_empty() && ch_norm == item_norm;
            let name_hit = !disp.is_empty() && ch.name.trim().eq_ignore_ascii_case(disp);
            if id_hit || name_hit {
                if !ch.enabled {
                    return false;
                }
                let lvl = normalize_tg_level(ch.level);
                return lvl == 99 || (lvl & 16) != 0;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IgnoredEpisodeRule {
    #[serde(deserialize_with = "flex_id")]
    pub tmdb_id: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub media_type: String,
    pub season: u32,
    pub episode: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewingItem {
    #[serde(deserialize_with = "null_default")]
    pub slug: String,
    #[serde(deserialize_with = "null_default")]
    pub clean_slug: String,
    #[serde(deserialize_with = "null_default")]
    pub share_code: String,
    #[serde(deserialize_with = "null_default")]
    pub receive_code: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "flex_id")]
    pub tmdb_id: String,
    #[serde(deserialize_with = "null_default")]
    pub media_type: String,
    #[serde(deserialize_with = "null_default")]
    pub source_type: String,
    #[serde(deserialize_with = "null_default")]
    pub target_cid: String,
    pub retry_count: i32,
    #[serde(deserialize_with = "null_default")]
    pub last_error: String,
    pub created_ts: i64,
    pub last_tried_ts: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShieldShareRecord {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub share_code: String,
    #[serde(deserialize_with = "null_default")]
    pub receive_code: String,
    #[serde(deserialize_with = "null_default")]
    pub share_url: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub status: String,
    pub parsed_files: usize,
    pub parsed_folders: usize,
    #[serde(deserialize_with = "null_default")]
    pub processed_cids: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub pending_cids: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub dir_paths: std::collections::HashMap<String, String>,
    #[serde(deserialize_with = "null_default")]
    pub last_error: String,
    pub updated_ts: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShieldIndexedFile {
    pub share_code: String,
    pub receive_code: String,
    pub file_id: String,
    pub parent_id: String,
    pub rel_path: String,
    pub file_name: String,
    pub file_size: i64,
    pub sha1: String,
    pub pure_title: String,
    pub tmdb_id: String,
    pub media_type: String,
    pub parsed_episodes: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ActivityRecord {
    pub seq: i64,
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub record_type: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub source: String,
    #[serde(deserialize_with = "null_default")]
    pub channel_name: String,
    #[serde(deserialize_with = "null_default")]
    pub slug: String,
    #[serde(deserialize_with = "null_default")]
    pub clean_slug: String,
    #[serde(deserialize_with = "flex_id")]
    pub tmdb_id: String,
    #[serde(deserialize_with = "null_default")]
    pub media_type: String,
    #[serde(deserialize_with = "null_default")]
    pub episodes: String,
    #[serde(deserialize_with = "null_default")]
    pub action: String,
    #[serde(deserialize_with = "null_default")]
    pub status: String,
    #[serde(deserialize_with = "null_default")]
    pub detail: String,
    #[serde(deserialize_with = "null_default")]
    pub updated_at: String,
}

impl ActivityRecord {
    pub fn effective_record_type(&self) -> &'static str {
        let rt = self.record_type.trim().to_ascii_lowercase();
        if rt == "task" {
            return "task";
        }
        if rt == "transfer" {
            return "transfer";
        }
        let act = self.action.trim();
        if matches!(act, "每日签到" | "自动订阅" | "巡检任务") {
            "task"
        } else {
            "transfer"
        }
    }

    pub fn source_group(&self) -> &'static str {
        let s = self.source.trim().to_ascii_lowercase();
        if s == "telegram" || s.starts_with("tg") || !self.channel_name.trim().is_empty() && s == "115" {
            "telegram"
        } else if s == "hdhive" || s.contains("relay") {
            "hdhive"
        } else if s == "ayclub" {
            "ayclub"
        } else if s == "shield" {
            "shield"
        } else if s == "rss" || s.starts_with("rss:") {
            "rss"
        } else if s == "system" {
            "system"
        } else if s == "manual" {
            "manual"
        } else if matches!(s.as_str(), "115" | "ed2k" | "magnet") {
            if !self.channel_name.trim().is_empty() && self.channel_name != "手动直链" {
                "telegram"
            } else {
                "manual"
            }
        } else {
            "other"
        }
    }

    pub fn action_group(&self) -> &'static str {
        let a = self.action.trim();
        if a.contains("追更") {
            "chase"
        } else if a.contains("入新") || a.contains("全量") {
            "tg_auto"
        } else if a.contains("待审") {
            "review"
        } else if a.contains("RSS") {
            "rss"
        } else {
            "manual"
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HistoryFilter {
    #[serde(deserialize_with = "null_default")]
    pub record_type: String,
    #[serde(deserialize_with = "null_default")]
    pub source: String,
    #[serde(deserialize_with = "null_default")]
    pub action_group: String,
    #[serde(deserialize_with = "null_default")]
    pub status: String,
    #[serde(deserialize_with = "null_default")]
    pub keyword: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    #[serde(deserialize_with = "null_default")]
    pub custom_proxy: String,
    pub hdhive: HDHiveConfig,
    pub telegram: TelegramConfig,
    pub ayclub: AYClubConfig,
    #[serde(deserialize_with = "null_default")]
    pub rss_feeds: Vec<RSSFeedConfig>,
    pub rules: RuleConfig,
    pub notifications: NotificationConfig,
    #[serde(deserialize_with = "null_default")]
    pub ignored_episodes: Vec<IgnoredEpisodeRule>,
    pub transferred_count: usize,
    pub subscription_cursor: usize,
    #[serde(deserialize_with = "null_default")]
    pub last_result: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        let mut rules = RuleConfig::default();
        rules.ensure_weights_initialized();
        Self {
            custom_proxy: String::new(),
            hdhive: HDHiveConfig::default(),
            telegram: TelegramConfig::default(),
            ayclub: AYClubConfig::default(),
            rss_feeds: Vec::new(),
            rules,
            notifications: NotificationConfig::default(),
            ignored_episodes: Vec::new(),
            transferred_count: 0,
            subscription_cursor: 0,
            last_result: "就绪（尚未执行巡检）".to_string(),
        }
    }
}

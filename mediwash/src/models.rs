use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn flex_id<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    match Value::deserialize(de)? {
        Value::String(s) => Ok(s.trim().to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Null => Ok(String::new()),
        _ => Err(D::Error::custom("需要字符串或数字 ID")),
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
        Value::String(s) => {
            Ok(s.trim().parse::<i64>().ok().filter(|&v| v > 0).unwrap_or(0) as u32)
        }
        _ => Ok(0),
    }
}

pub fn flex_u64<'de, D: Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    match Value::deserialize(de)? {
        Value::Null => Ok(0),
        Value::Number(n) => Ok(n
            .as_u64()
            .or_else(|| n.as_i64().filter(|&v| v > 0).map(|v| v as u64))
            .or_else(|| n.as_f64().filter(|&v| v > 0.0).map(|v| v as u64))
            .unwrap_or(0)),
        Value::String(s) => Ok(s.trim().parse::<u64>().unwrap_or(0)),
        _ => Ok(0),
    }
}

pub fn flex_f64<'de, D: Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    match Value::deserialize(de)? {
        Value::Null => Ok(0.0),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => Ok(s.trim().parse::<f64>().unwrap_or(0.0)),
        _ => Ok(0.0),
    }
}

pub fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Formats a Unix timestamp into `YYYY-MM-DD HH:MM:SS` in UTC+8 without external crates.
pub fn format_ts_cn(ts: u64) -> String {
    if ts == 0 {
        return String::new();
    }
    let local = ts + 8 * 3600;
    let secs_of_day = local % 86400;
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let sec = secs_of_day % 60;

    let z = (local / 86400) as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02} {hour:02}:{min:02}:{sec:02}")
}

fn default_true() -> bool {
    true
}

fn default_reject_mode() -> String {
    "recycle_bin".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSettings {
    #[serde(rename = "$enabled")]
    pub enabled: bool,
    pub dry_run: bool,
    pub auto_delete_old: bool,
    pub notify: bool,
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            dry_run: true,
            auto_delete_old: false,
            notify: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(default)]
pub struct CategoryRule {
    pub media_type: String,
    pub path: String,
    pub iso_zone: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSyncState {
    pub categories: Vec<CategoryRule>,
    pub category_source: String,
    pub movie_rename_format: String,
    pub tv_rename_format: String,
    pub filter_keywords: Vec<String>,
    pub open115_target_folder: String,
    pub open115_target_cid: String,
    pub open115_offline_folder: String,
    pub open115_offline_cid: String,
    pub transfer_source_folder: String,
    pub transfer_source_cid: String,
    pub transfer_target_folder: String,
    pub transfer_target_cid: String,
    #[serde(default)]
    pub transfer_task_name: String,
    #[serde(default = "default_true")]
    pub has_flowlink_transfer_template: bool,
    pub protected_cids: Vec<String>,
    pub last_synced: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CategoryOverride {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub category_prefix: String,
    pub rule_id: String,
    pub min_diff: Option<f64>,
    pub keep_per_slot: Option<usize>,
    pub skip_library_wash: bool,
    pub skip_pt_wash: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UnifiedStrategy {
    pub movie_rule_id: String,
    pub tv_rule_id: String,
    pub min_diff_100: f64,
    pub min_diff_1000: f64,
    pub keep_per_slot: usize,
    #[serde(default = "default_true")]
    pub protect_iso_zones: bool,
    pub overrides: Vec<CategoryOverride>,
}

impl Default for UnifiedStrategy {
    fn default() -> Self {
        Self {
            movie_rule_id: "mediatidy:ultimate-precision".to_string(),
            tv_rule_id: "mediatidy:ultimate-precision".to_string(),
            min_diff_100: 2.0,
            min_diff_1000: 15.0,
            keep_per_slot: 1,
            protect_iso_zones: true,
            overrides: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EffectivePolicy {
    pub rule_id: String,
    pub min_diff: f64,
    pub keep_per_slot: usize,
    pub iso_protected: bool,
    pub skip_library_wash: bool,
    pub skip_pt_wash: bool,
}

impl UnifiedStrategy {
    pub fn effective_for(
        &self,
        media_type: &str,
        category: &str,
        categories: &[CategoryRule],
        rules: &[ScoringRule],
    ) -> EffectivePolicy {
        let mut rule_id = if media_type == "tv" {
            self.tv_rule_id.clone()
        } else {
            self.movie_rule_id.clone()
        };
        let mut custom_diff = None;
        let mut keep_per_slot = self.keep_per_slot.max(1);
        let mut skip_library_wash = false;
        let mut skip_pt_wash = false;

        // Pick most specific matching category override
        let mut best_depth = 0usize;
        for ov in &self.overrides {
            if !ov.media_type.is_empty() && ov.media_type != "all" && ov.media_type != media_type {
                continue;
            }
            let prefix = ov.category_prefix.trim().trim_matches('/');
            let matched = if prefix.is_empty() {
                true
            } else {
                category == prefix
                    || category
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            };
            if matched {
                let depth = if prefix.is_empty() {
                    1
                } else {
                    prefix.split('/').count() + 1
                };
                if depth >= best_depth {
                    best_depth = depth;
                    if !ov.rule_id.is_empty() && rules.iter().any(|r| r.id == ov.rule_id) {
                        rule_id = ov.rule_id.clone();
                    }
                    if ov.min_diff.is_some() {
                        custom_diff = ov.min_diff;
                    }
                    if let Some(k) = ov.keep_per_slot {
                        keep_per_slot = k.max(1);
                    }
                    skip_library_wash = ov.skip_library_wash;
                    skip_pt_wash = ov.skip_pt_wash;
                }
            }
        }

        if !rules.iter().any(|r| r.id == rule_id) {
            if let Some(fallback) = rules
                .iter()
                .find(|r| r.kind == "mediatidy")
                .or_else(|| rules.first())
            {
                rule_id = fallback.id.clone();
            }
        }

        let is_symedia = rules
            .iter()
            .find(|r| r.id == rule_id)
            .is_some_and(|r| r.kind == "symedia");
        let min_diff = custom_diff.unwrap_or(if is_symedia {
            self.min_diff_100
        } else {
            self.min_diff_1000
        });

        let iso_protected = self.protect_iso_zones
            && (category.contains("蓝光原盘")
                || category.to_ascii_lowercase().contains("iso")
                || categories
                    .iter()
                    .any(|c| c.media_type == media_type && c.path == category && c.iso_zone));

        EffectivePolicy {
            rule_id,
            min_diff: min_diff.max(0.0),
            keep_per_slot,
            iso_protected,
            skip_library_wash,
            skip_pt_wash,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StagingPipeline {
    pub id: i64,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// "auto" | "movie" | "tv"
    pub media_type: String,
    pub source_cid: String,
    pub source_name: String,
    pub target_cid: String,
    pub target_name: String,
    #[serde(default = "default_true")]
    pub recursive: bool,
    #[serde(default = "default_true")]
    pub move_subtitles: bool,
    #[serde(default = "default_true")]
    pub auto_admit: bool,
    #[serde(default = "default_true")]
    pub wash_enabled: bool,
    /// "recycle_bin" | "skip"
    #[serde(default = "default_reject_mode")]
    pub reject_mode: String,
    /// "smart" | "always" | "filename_only"
    pub probe_mode: String,
    pub rule_id_override: String,
}

impl Default for StagingPipeline {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            enabled: true,
            media_type: "auto".to_string(),
            source_cid: String::new(),
            source_name: String::new(),
            target_cid: String::new(),
            target_name: String::new(),
            recursive: true,
            move_subtitles: true,
            auto_admit: true,
            wash_enabled: true,
            reject_mode: default_reject_mode(),
            probe_mode: "smart".to_string(),
            rule_id_override: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PtWashConfig {
    pub enabled: bool,
    pub movie_rule_id: String,
    pub tv_rule_id: String,
    pub movie_graduate: f64,
    pub tv_graduate: f64,
    pub min_score_diff: f64,
    #[serde(default = "default_true")]
    pub free_only: bool,
    #[serde(default = "default_true")]
    pub skip_hr: bool,
    pub min_seeders: u64,
    pub site_ids: Vec<u64>,
    pub batch_per_tick: usize,
    pub auto_download: bool,
    /// "overwrite" | "skip"
    pub conflict_strategy: String,
}

impl Default for PtWashConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            movie_rule_id: String::new(),
            tv_rule_id: String::new(),
            movie_graduate: 75.0,
            tv_graduate: 65.0,
            min_score_diff: 3.0,
            free_only: true,
            skip_hr: true,
            min_seeders: 2,
            site_ids: Vec::new(),
            batch_per_tick: 4,
            auto_download: false,
            conflict_strategy: "overwrite".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScoringRule {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub builtin: bool,
    pub source: String,
    pub body: Value,
    pub raw: String,
}

impl Default for ScoringRule {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: "symedia".to_string(),
            builtin: false,
            source: "custom".to_string(),
            body: Value::Null,
            raw: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleSubscription {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cache: String,
    pub last_sync: u64,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManualBinding {
    pub pattern_key: String,
    pub title: String,
    pub media_type: String,
    pub tmdb_id: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub revision: i64,
    pub host_sync: HostSyncState,
    pub strategy: UnifiedStrategy,
    pub pipelines: Vec<StagingPipeline>,
    pub pt_config: PtWashConfig,
    pub rules: Vec<ScoringRule>,
    pub subscriptions: BTreeMap<String, RuleSubscription>,
    pub local_matching_yaml: String,
    pub local_tmdb_yaml: String,
    pub manual_bindings: BTreeMap<String, ManualBinding>,
}

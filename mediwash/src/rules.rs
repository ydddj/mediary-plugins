use crate::host::{LibrarySource, MediaStream};
use crate::models::{now_ts, AppConfig, ScoringRule};
use anyhow::{bail, ensure, Context, Result};
use regex::{Regex, RegexBuilder};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::LazyLock;
use std::time::Duration;

pub const SYMEDIA: &str = "symedia";
pub const MEDIATIDY: &str = "mediatidy";

const PRESET_SYMEDIA_V5: &str = include_str!("../presets/symedia-v5.json");
const PRESET_MEDIATIDY_ULTIMATE: &str = include_str!("../presets/mediatidy-ultimate.yaml");

pub const SYM_OBJECT_GROUPS: &[(&str, &str)] = &[
    ("videoFormat", "分辨率"),
    ("resourceType", "片源类型"),
    ("videoCodec", "视频编码"),
    ("audioCodec", "音频编码"),
    ("color_depth", "色彩深度"),
    ("frame_rate", "帧率"),
    ("dolby_vision", "杜比视界"),
    ("dynamic_range", "动态范围"),
    ("subtitle", "字幕"),
    ("file_extension", "容器后缀"),
];

pub const SYM_ALL_DIMENSIONS: &[(&str, &str, &str)] = &[
    ("videoFormat", "videoFormat_weight", "分辨率"),
    ("resourceType", "resourceType_weight", "片源类型"),
    ("videoCodec", "videoCodec_weight", "视频编码"),
    ("audioCodec", "audioCodec_weight", "音频编码"),
    ("color_depth", "color_depth_weight", "色彩深度"),
    ("frame_rate", "frame_rate_weight", "帧率"),
    ("dolby_vision", "dolby_vision_weight", "杜比视界"),
    ("dynamic_range", "dynamic_range_weight", "动态范围"),
    ("subtitle", "subtitle_weight", "字幕"),
    ("file_extension", "file_extension_weight", "容器后缀"),
    ("releaseGroup", "releaseGroup_weight", "制作组"),
    ("custom_attributes", "custom_weight", "自定义属性"),
];

#[allow(dead_code)]
pub const TIDY_DIMENSIONS: &[(&str, &str)] = &[
    ("resolution", "分辨率"),
    ("source", "片源"),
    ("video_codec", "视频编码"),
    ("audio_codec", "音频编码"),
    ("hdr", "动态范围"),
    ("color_depth", "色彩深度"),
    ("frame_rate", "帧率"),
    ("subtitle", "字幕"),
    ("file_extension", "容器后缀"),
    ("file_size", "文件体积"),
    ("bitrate", "总码率"),
    ("video_bitrate", "视频码率"),
];

pub fn round2(v: f64) -> f64 {
    if !v.is_finite() {
        0.0
    } else {
        (v * 100.0).round() / 100.0
    }
}

fn compile_ci_regex(pattern: &str) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(256 * 1024)
        .build()
        .with_context(|| format!("无效正则表达式: {pattern}"))
}

pub fn default_rules() -> Result<Vec<ScoringRule>> {
    let mut out = parse_rules(SYMEDIA, PRESET_SYMEDIA_V5, "preset")?;
    out.extend(parse_rules(MEDIATIDY, PRESET_MEDIATIDY_ULTIMATE, "preset")?);
    for r in &mut out {
        r.builtin = true;
    }
    Ok(out)
}

/// Normalizes a Symedia `MetaWeightRule` JSON object to match Symedia 1.0.59.25 `MetaWeightRule.py`
/// (including `effect` -> `dolby_vision`/`dynamic_range` migration, 36 required fields, and `version_control_attribute_options`).
pub fn normalize_symedia_body(mut body: Value) -> Result<Value> {
    // Unwrap `{ "rule": { ... } }` envelope exported by some Symedia endpoints
    if let Some(inner) = body.get("rule").filter(|x| x.is_object()).cloned() {
        let outer_name = body["name"].as_str().map(str::to_string);
        let outer_id = body["id"].as_str().map(str::to_string);
        body = inner;
        if body["name"].as_str().unwrap_or_default().is_empty() {
            if let Some(n) = outer_name {
                body["name"] = json!(n);
            }
        }
        if body["id"].as_str().unwrap_or_default().is_empty() {
            if let Some(id) = outer_id {
                body["id"] = json!(id);
            }
        }
    }

    let obj = body
        .as_object_mut()
        .context("Symedia 规则顶层必须是 JSON 对象")?;

    // Migrate legacy `effect` -> `dolby_vision` + `dynamic_range` (per Symedia MetaWeightRule.py lines 884-926)
    if let Some(effect) = obj.get("effect").and_then(Value::as_object).cloned() {
        let bl = effect
            .get("blacklist")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let wl = effect
            .get("whitelist")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let eff_w = obj
            .get("effect_weight")
            .and_then(Value::as_f64)
            .unwrap_or(1.0);
        if !obj.contains_key("dolby_vision") {
            let mut dv = Map::new();
            dv.insert("blacklist".into(), bl.clone());
            dv.insert("whitelist".into(), wl.clone());
            for k in [
                "dolby_vision_p7",
                "dolby_vision_p5",
                "dolby_vision_p8",
                "dolby_vision",
            ] {
                if let Some(v) = effect.get(k) {
                    dv.insert(k.into(), v.clone());
                }
            }
            obj.insert("dolby_vision".into(), Value::Object(dv));
        }
        if !obj.contains_key("dynamic_range") {
            let mut dr = Map::new();
            dr.insert("blacklist".into(), bl);
            dr.insert("whitelist".into(), wl);
            for k in ["hdr10_plus", "hdr10", "hdr", "hlg", "sdr"] {
                if let Some(v) = effect.get(k) {
                    dr.insert(k.into(), v.clone());
                }
            }
            obj.insert("dynamic_range".into(), Value::Object(dr));
        }
        obj.entry("dolby_vision_weight")
            .or_insert_with(|| json!(eff_w));
        obj.entry("dynamic_range_weight")
            .or_insert_with(|| json!(eff_w));
    }

    // Backfill any missing required fields from Symedia V5 preset template so exports satisfy Symedia's 36-field validator
    let preset_template: Value = serde_json::from_str(PRESET_SYMEDIA_V5)?;
    if let Some(tpl) = preset_template.as_object() {
        for (k, v) in tpl {
            if k == "name" || k == "id" || k == "version_control_attribute_options" {
                continue;
            }
            obj.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }

    for &(group_key, _) in SYM_OBJECT_GROUPS {
        if let Some(g) = obj.get_mut(group_key).and_then(Value::as_object_mut) {
            g.entry("blacklist").or_insert_with(|| json!([]));
            g.entry("whitelist").or_insert_with(|| json!([]));
        }
    }
    for list_key in [
        "releaseGroup",
        "releaseGroup_blacklist",
        "releaseGroup_whitelist",
        "custom_attributes",
        "custom_attributes_blacklist",
        "custom_attributes_whitelist",
        "file_size_blacklist",
        "file_size_whitelist",
        "always_override_blacklist",
        "always_override_whitelist",
        "version_control_versions",
    ] {
        obj.entry(list_key).or_insert_with(|| json!([]));
    }
    obj.entry("source_file_extract_metadata")
        .or_insert_with(|| json!(true));
    obj.entry("dest_file_extract_metadata")
        .or_insert_with(|| json!(true));
    obj.entry("version_control_enabled")
        .or_insert_with(|| json!(false));
    obj.entry("version_control_delete_invalid_dest_files")
        .or_insert_with(|| json!(false));

    // Regenerate `version_control_attribute_options` for 100% lossless Symedia UI import
    let mut attr_options = Vec::new();
    for &(group_key, label) in SYM_OBJECT_GROUPS {
        let mut opts = Vec::new();
        if let Some(g) = obj.get(group_key).and_then(Value::as_object) {
            for (item_k, item_v) in g {
                if matches!(item_k.as_str(), "blacklist" | "whitelist") || !item_v.is_object() {
                    continue;
                }
                let item_label = item_v["name"].as_str().unwrap_or(item_k);
                opts.push(json!({ "value": item_k, "label": item_label }));
            }
        }
        attr_options.push(json!({
            "attribute": group_key,
            "label": label,
            "options": opts,
        }));
    }
    for (list_key, label) in [
        ("releaseGroup", "制作组"),
        ("custom_attributes", "自定义属性"),
    ] {
        let mut opts = Vec::new();
        for (idx, item_v) in obj
            .get(list_key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let name = item_v["name"]
                .as_str()
                .or_else(|| item_v["pattern"].as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| idx.to_string());
            opts.push(json!({ "value": name, "label": name }));
        }
        attr_options.push(json!({
            "attribute": list_key,
            "label": label,
            "options": opts,
        }));
    }
    obj.insert(
        "version_control_attribute_options".into(),
        Value::Array(attr_options),
    );
    Ok(body)
}

pub fn parse_rules(kind: &str, raw: &str, source: &str) -> Result<Vec<ScoringRule>> {
    let cleaned = raw.trim_start_matches('\u{feff}').trim();
    ensure!(!cleaned.is_empty(), "规则内容不能为空");
    let parsed: Value = match kind {
        SYMEDIA => serde_json::from_str(cleaned).context("Symedia JSON 解析失败")?,
        MEDIATIDY => serde_yaml::from_str(cleaned).context("MediaTidy YAML 解析失败")?,
        _ => bail!("不支持的评分规则格式: {kind}"),
    };

    let entries: Vec<Value> = match kind {
        SYMEDIA => {
            if let Some(arr) = parsed.as_array() {
                arr.clone()
            } else {
                vec![parsed]
            }
        }
        MEDIATIDY => {
            if let Some(arr) = parsed.get("quality").and_then(Value::as_array) {
                arr.clone()
            } else if parsed.get("scoring").is_some() {
                vec![parsed]
            } else {
                bail!("MediaTidy YAML 缺少顶层 quality 列表或 scoring 字段");
            }
        }
        _ => unreachable!(),
    };
    ensure!(
        !entries.is_empty() && entries.len() <= 40,
        "单次导入评分方案数量须在 1~40 之间"
    );

    let mut out = Vec::new();
    let mut seen_ids = BTreeSet::new();
    for entry in entries {
        let normalized = if kind == SYMEDIA {
            normalize_symedia_body(entry)?
        } else {
            ensure!(
                entry["scoring"]
                    .as_object()
                    .is_some_and(|m| !m.is_empty()),
                "MediaTidy 方案缺少 scoring 配置"
            );
            entry
        };
        let name = normalized["name"].as_str().unwrap_or_default().trim().to_string();
        ensure!(!name.is_empty(), "评分规则缺少 name 名称");

        let base_id = if kind == MEDIATIDY {
            let slug = normalized["id"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&name)
                .trim();
            format!("mediatidy:{slug}")
        } else {
            let slug = normalized["id"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&name)
                .trim();
            if source == "preset" {
                "symedia:preset-v5".to_string()
            } else {
                format!("symedia:{slug}")
            }
        };

        let serialized_raw = if kind == SYMEDIA {
            serde_json::to_string_pretty(&normalized)?
        } else {
            serde_yaml::to_string(&json!({ "quality": [&normalized] }))?
        };

        let rule = ScoringRule {
            id: base_id,
            name,
            kind: kind.to_string(),
            builtin: source == "preset",
            source: source.to_string(),
            body: normalized,
            raw: serialized_raw,
        };
        // Validate regexes & slots compile cleanly
        Scorer::new(&rule)?;
        ensure!(seen_ids.insert(rule.id.clone()), "存在重复的评分规则 ID: {}", rule.id);
        out.push(rule);
    }
    Ok(out)
}

pub fn rebuild_rule_raw(rule: &mut ScoringRule) -> Result<()> {
    if rule.kind == SYMEDIA {
        rule.body["name"] = json!(rule.name);
        rule.body = normalize_symedia_body(rule.body.clone())?;
        rule.raw = serde_json::to_string_pretty(&rule.body)?;
    } else {
        rule.body["name"] = json!(rule.name);
        rule.raw = serde_yaml::to_string(&json!({ "quality": [&rule.body] }))?;
    }
    Scorer::new(rule)?;
    Ok(())
}

/// Extracts YAML body starting from `mr:` or `tmdb_override:` even if preceded by prose documentation.
pub fn extract_aux_yaml(raw: &str, marker: &str) -> String {
    let stripped = raw.trim_start_matches('\u{feff}');
    if stripped.trim().is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = stripped.lines().collect();
    if let Some(pos) = lines.iter().position(|l| l.trim_end() == marker || l.trim_start().starts_with(marker)) {
        lines[pos..].join("\n")
    } else {
        stripped.to_string()
    }
}

pub fn inspect_aux_yaml(kind: &str, raw: &str) -> Result<Value> {
    if raw.trim().is_empty() {
        return Ok(json!({ "counts": {}, "warnings": [] }));
    }
    let marker = match kind {
        "matching" => "mr:",
        "tmdb" => "tmdb_override:",
        _ => bail!("未知辅助规则类型: {kind}"),
    };
    let body = extract_aux_yaml(raw, marker);
    let val: Value = serde_yaml::from_str(&body).context("辅助规则 YAML 语法错误")?;
    let keys: &[&str] = if kind == "matching" {
        &["mr", "mp", "output_mr", "groups", "streaming", "region"]
    } else {
        &["tmdb_override", "tmdb_blacklist"]
    };
    let mut counts = BTreeMap::new();
    let mut warnings = Vec::new();
    for &k in keys {
        if let Some(arr) = val[k].as_array() {
            counts.insert(k, arr.len());
            if matches!(k, "mr" | "output_mr") {
                for (idx, item) in arr.iter().enumerate() {
                    if let Some(pat) = item["pattern"].as_str() {
                        if let Err(e) = crate::identity::compile_fancy(pat) {
                            warnings.push(format!("{k} #{}: {e}", idx + 1));
                        }
                    }
                }
            }
        }
    }
    ensure!(counts.values().any(|&n| n > 0), "未检测到有效的 {marker} 规则条目");
    Ok(json!({ "counts": counts, "warnings": warnings }))
}

pub fn sync_subscriptions(db: &mut Connection) -> Result<Value> {
    let cfg = crate::db::load_config(db)?;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .redirects(3)
        .build();
    let mut report = Vec::new();
    let mut ok_count = 0usize;
    let mut fail_count = 0usize;

    for (sub_id, sub) in cfg.subscriptions.iter().filter(|(_, s)| s.enabled) {
        let fetched = (|| -> Result<(String, Vec<ScoringRule>)> {
            let parsed_url = url::Url::parse(sub.url.trim()).context("无效订阅 URL")?;
            ensure!(
                parsed_url.scheme() == "https" && parsed_url.username().is_empty(),
                "远程订阅必须使用 HTTPS URL"
            );
            let resp = agent.get(parsed_url.as_str()).call().context("下载远程订阅失败")?;
            let mut content = String::new();
            resp.into_reader()
                .take(3 * 1024 * 1024)
                .read_to_string(&mut content)?;
            ensure!(!content.trim().is_empty(), "远程订阅返回空内容");
            let parsed_rules = if sub.kind == MEDIATIDY {
                parse_rules(MEDIATIDY, &content, "subscription")?
            } else {
                inspect_aux_yaml(&sub.kind, &content)?;
                Vec::new()
            };
            Ok((content, parsed_rules))
        })();

        match fetched {
            Ok((content, parsed_rules)) => {
                ok_count += 1;
                report.push(json!({ "id": sub_id, "name": sub.name, "ok": true, "error": "" }));
                crate::db::edit_config(db, true, |live_cfg| {
                    let Some(target) = live_cfg.subscriptions.get_mut(sub_id) else {
                        return Ok(false);
                    };
                    target.last_sync = now_ts();
                    target.error.clear();
                    let changed = target.cache != content;
                    target.cache = content;
                    for mut r in parsed_rules {
                        r.id = format!("sub:{sub_id}:{}", r.id);
                        r.name = format!("[订阅] {}", r.name);
                        if let Some(existing) = live_cfg.rules.iter_mut().find(|x| x.id == r.id) {
                            *existing = r;
                        } else {
                            live_cfg.rules.push(r);
                        }
                    }
                    Ok(changed)
                })?;
            }
            Err(err) => {
                fail_count += 1;
                let msg = format!("{err:#}");
                report.push(json!({ "id": sub_id, "name": sub.name, "ok": false, "error": msg }));
                crate::db::edit_config(db, false, |live_cfg| {
                    if let Some(target) = live_cfg.subscriptions.get_mut(sub_id) {
                        target.error = msg;
                    }
                    Ok(false)
                })?;
            }
        }
    }

    crate::db::log_task_history(
        db,
        "远程订阅",
        "规则订阅同步",
        "-",
        "sync_rules",
        if fail_count == 0 { "done" } else { "warning" },
        &format!("成功 {ok_count}，失败 {fail_count}"),
    );
    Ok(json!({
        "ok_count": ok_count,
        "fail_count": fail_count,
        "sources": report,
    }))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubtitleTrack {
    pub lang: String,
    pub format: String,
    pub title: String,
    pub synthetic_tag: String,
}

/// Rich technical profile extracted from MediaInfo streams + paths + filename tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaProfile {
    pub display_name: String,
    pub full_haystack: String,
    pub resolution: String,
    pub hdr: String,
    pub dolby_vision_tag: String,
    pub fps: String,
    pub source: String,
    pub codec: String,
    pub depth: String,
    pub extension: String,
    pub audio_tags: Vec<String>,
    pub audio_count: usize,
    pub subtitles: Vec<SubtitleTrack>,
    pub size: f64,
    pub bitrate: f64,
    pub video_bitrate: f64,
    pub probed: bool,
}

impl MediaProfile {
    pub fn extract(src: &LibrarySource, streams: &[&MediaStream]) -> Self {
        // Combine name + all paths so container `.iso`/`.mkv`, HDR, group, and bitrate tokens are never missed
        let path_candidates = [
            src.name.as_str(),
            src.cloud_path.as_str(),
            src.file_path.as_str(),
            src.strm_path.as_str(),
        ];
        let display_name = path_candidates
            .iter()
            .copied()
            .find(|s| !s.trim().is_empty())
            .unwrap_or_default()
            .to_string();

        let combined_raw = path_candidates
            .iter()
            .filter(|s| !s.trim().is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let low = combined_raw.to_lowercase();

        let video = streams
            .iter()
            .copied()
            .find(|s| s.stream_type.eq_ignore_ascii_case("video"));

        // 1. Container extension (checks name -> cloud_path -> file_path, ignoring `.strm` wrapper when upstream exists)
        let extension = extract_extension(&src.name)
            .or_else(|| extract_extension(&src.cloud_path))
            .or_else(|| extract_extension(&src.file_path))
            .or_else(|| {
                // Check token like `.iso` or ` BluRay.ISO ` in title
                static EXT_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
                    Regex::new(r"(?i)(?:^|[.\s_-])(mkv|mp4|iso|m2ts|ts|avi|rmvb|wmv|mov|webm)(?:$|[.\s_-])").unwrap()
                });
                EXT_TOKEN_RE
                    .captures(&src.name)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().to_ascii_lowercase())
            })
            .unwrap_or_default();

        // 2. Video codec
        let raw_vcodec = video.map_or("", |v| v.codec.as_str()).to_ascii_lowercase();
        let codec = if raw_vcodec.contains("av1") || low.contains("av1") {
            "AV1"
        } else if matches!(raw_vcodec.as_str(), "hevc" | "h265" | "h.265")
            || low.contains("h265")
            || low.contains("h.265")
            || low.contains("hevc")
            || low.contains("x265")
        {
            "H.265"
        } else if matches!(raw_vcodec.as_str(), "avc" | "h264" | "h.264")
            || low.contains("h264")
            || low.contains("h.264")
            || low.contains("x264")
            || low.contains("avc")
        {
            "H.264"
        } else if raw_vcodec.contains("vp9") || low.contains("vp9") {
            "VP9"
        } else if raw_vcodec.contains("vc1") || low.contains("vc-1") || low.contains("vc1") {
            "VC-1"
        } else if low.contains("xvid") {
            "Xvid"
        } else if low.contains("divx") {
            "DivX"
        } else if raw_vcodec.contains("mpeg4") {
            "MPEG-4"
        } else if raw_vcodec.contains("mpeg2") {
            "MPEG-2"
        } else {
            ""
        }
        .to_string();

        // 3. Resolution
        static RES_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)\b(4320p|8k|2160p|4k|uhd|1440p|1080p|1080i|720p|576p|480p|360p)\b")
                .unwrap()
        });
        let measured_res = video.and_then(|v| match (v.width, v.height) {
            (7000.., _) | (_, 3900..) => Some("4320p"),
            (3600.., _) | (_, 1950..) => Some("2160p"),
            (2400.., _) | (_, 1300..) => Some("1440p"),
            (1800.., _) | (_, 980..) => Some(if low.contains("1080i") { "1080i" } else { "1080p" }),
            (1180.., _) | (_, 680..) => Some("720p"),
            (_, 540..) => Some("576p"),
            (_, 400..) => Some("480p"),
            (_, 280..) => Some("360p"),
            _ => None,
        });
        let resolution = measured_res
            .map(str::to_string)
            .or_else(|| {
                RES_RE.captures(&low).and_then(|c| c.get(1)).map(|m| {
                    match m.as_str() {
                        "8k" => "4320p",
                        "4k" | "uhd" => "2160p",
                        other => other,
                    }
                    .to_string()
                })
            })
            .unwrap_or_default();

        // 4. HDR & Dolby Vision (both combined `hdr` for MediaTidy AND separate `dolby_vision_tag` / `hdr` for Symedia dual DV+HDR10!)
        let stream_hdr = video.map_or("", |v| v.hdr_format.as_str()).to_lowercase();
        let hdr_probe_str = format!("{stream_hdr} {low}");
        static DV_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)(?:\b(?:dv|dovi)\b|dolby[ ._-]?vision)(?:[ ._-]*(?:profile[ ._-]*|p)([578]))?").unwrap()
        });
        let dolby_vision_tag = if let Some(cap) = DV_RE.captures(&hdr_probe_str) {
            match cap.get(1).map(|m| m.as_str()) {
                Some("7") => "Dolby Vision P7",
                Some("5") => "Dolby Vision P5",
                Some("8") => "Dolby Vision P8",
                _ => "Dolby Vision",
            }
            .to_string()
        } else {
            String::new()
        };

        let has_hdr_vivid = hdr_probe_str.contains("hdrvivid")
            || hdr_probe_str.contains("hdr vivid")
            || hdr_probe_str.contains("cuva");
        let base_hdr = if hdr_probe_str.contains("hdr10+") || hdr_probe_str.contains("hdr10plus") {
            "HDR10+"
        } else if hdr_probe_str.contains("hdr10") {
            "HDR10"
        } else if hdr_probe_str.contains("hlg") {
            "HLG"
        } else if has_hdr_vivid || Regex::new(r"(?i)\bhdr\b").unwrap().is_match(&hdr_probe_str) {
            "HDR"
        } else if hdr_probe_str.contains("sdr")
            || (dolby_vision_tag.is_empty()
                && (video.is_some()
                    || !resolution.is_empty()
                    || !codec.is_empty()
                    || !extension.is_empty()))
        {
            "SDR"
        } else {
            ""
        };
        let hdr = if !dolby_vision_tag.is_empty() {
            dolby_vision_tag.clone()
        } else {
            base_hdr.to_string()
        };

        // 5. Frame rate
        static FPS_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)\b(120|119\.88|60|59\.94|50|30|29\.97|25|24|23\.976|23\.98)\s*fps\b")
                .unwrap()
        });
        let fps = video
            .and_then(|v| v.fps())
            .map(|r| {
                if (r - 23.976).abs() < 0.05 {
                    "24fps".to_string()
                } else if (r - 59.94).abs() < 0.1 {
                    "60fps".to_string()
                } else if (r - 119.88).abs() < 0.2 {
                    "120fps".to_string()
                } else {
                    format!("{}fps", r.round() as u32)
                }
            })
            .or_else(|| {
                FPS_RE
                    .captures(&low)
                    .and_then(|c| c.get(1))
                    .map(|m| format!("{}fps", m.as_str()))
            })
            .unwrap_or_default();

        // 6. Color depth
        let depth = video
            .filter(|v| v.bit_depth > 0)
            .map(|v| format!("{}-bit", v.bit_depth))
            .or_else(|| {
                if low.contains("12bit") || low.contains("12-bit") {
                    Some("12-bit".to_string())
                } else if low.contains("10bit")
                    || low.contains("10-bit")
                    || low.contains("hi10p")
                    || !dolby_vision_tag.is_empty()
                    || matches!(base_hdr, "HDR10+" | "HDR10" | "HLG" | "HDR")
                {
                    Some("10-bit".to_string())
                } else if low.contains("8bit") || low.contains("8-bit") {
                    Some("8-bit".to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default();

        // 7. Source / Release type (produces canonical tags matching BOTH MediaTidy & Symedia)
        let is_uhd = low.contains("uhd") || low.contains("ultra hd") || resolution == "2160p";
        let is_bluray =
            low.contains("bluray") || low.contains("blu-ray") || low.contains("bdmv") || extension == "iso";
        let source = if low.contains("remux") {
            if is_uhd {
                "Remux+UHD BluRay"
            } else {
                "Remux+BluRay"
            }
        } else if low.contains("bdrip") {
            "BDRip"
        } else if is_bluray {
            if is_uhd {
                "UHD BluRay"
            } else {
                "BluRay"
            }
        } else if low.contains("web-dl") || low.contains("webdl") {
            "WEB-DL"
        } else if low.contains("webrip") || low.contains("web-rip") {
            "WEBRip"
        } else if low.contains("hdtv") {
            "HDTV"
        } else if low.contains("dvdrip") {
            "DVDRip"
        } else {
            "Unknown"
        }
        .to_string();

        // 8. Audio tracks (from streams + filename fallback)
        let mut audio_tags = Vec::new();
        let audio_streams: Vec<&&MediaStream> = streams
            .iter()
            .filter(|s| s.stream_type.eq_ignore_ascii_case("audio"))
            .collect();
        for a in &audio_streams {
            audio_tags.push(canonical_audio_from_stream(a));
        }
        if audio_tags.is_empty() {
            audio_tags.extend(extract_audio_from_filename(&low));
        }
        static MULTI_AUDIO_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})\s*audios?\b").unwrap());
        let explicit_audios = MULTI_AUDIO_RE
            .captures(&low)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<usize>().ok())
            .unwrap_or(0);
        let audio_count = audio_streams.len().max(explicit_audios).max(usize::from(
            low.contains("国粤") || low.contains("双音轨") || low.contains("多音轨"),
        ) * 2);

        // 9. Subtitle tracks (from streams + filename fallback, producing Symedia & MediaTidy compatible tags)
        let mut subtitles: Vec<SubtitleTrack> = streams
            .iter()
            .filter(|s| s.stream_type.eq_ignore_ascii_case("subtitle"))
            .map(|s| build_subtitle_track(&s.language, &s.codec, &s.title))
            .collect();
        if subtitles.is_empty() {
            subtitles.extend(extract_subtitles_from_filename(&low));
        }

        // 10. Bitrate (stream / container / filename e.g. `38.2Mbps`)
        static BITRATE_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)\b(\d+(?:\.\d+)?)\s*mbps\b").unwrap());
        let name_bitrate = BITRATE_RE
            .captures(&low)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<f64>().ok())
            .map(|mbps| mbps * 1_000_000.0)
            .unwrap_or(0.0);
        let v_bitrate = video.map_or(0.0, |v| v.bitrate);
        let total_bitrate = if src.bitrate > 0.0 {
            src.bitrate
        } else if v_bitrate > 0.0 {
            v_bitrate
        } else {
            name_bitrate
        };

        let mut haystack_parts = vec![
            combined_raw,
            resolution.clone(),
            hdr.clone(),
            base_hdr.to_string(),
            dolby_vision_tag.clone(),
            fps.clone(),
            source.clone(),
            codec.clone(),
            depth.clone(),
        ];
        if source.contains("UHD BluRay") {
            haystack_parts.push("Ultra HD Blu-Ray".to_string());
        } else if source.contains("BluRay") {
            haystack_parts.push("Blu-Ray".to_string());
        }
        if !extension.is_empty() {
            haystack_parts.push(format!(".{extension}"));
        }
        if audio_count > 0 {
            haystack_parts.push(format!("{audio_count}Audios"));
        }
        haystack_parts.extend(audio_tags.iter().cloned());
        haystack_parts.extend(subtitles.iter().map(|s| s.synthetic_tag.clone()));

        Self {
            display_name,
            full_haystack: haystack_parts.join(" "),
            resolution,
            hdr,
            dolby_vision_tag,
            fps,
            source,
            codec,
            depth,
            extension,
            audio_tags,
            audio_count,
            subtitles,
            size: src.size,
            bitrate: total_bitrate,
            video_bitrate: if v_bitrate > 0.0 {
                v_bitrate
            } else if name_bitrate > 0.0 {
                name_bitrate * 0.9
            } else {
                0.0
            },
            probed: video.is_some(),
        }
    }

    pub fn summary_json(&self) -> Value {
        json!({
            "resolution": self.resolution,
            "hdr": self.hdr,
            "fps": self.fps,
            "source": self.source,
            "codec": self.codec,
            "depth": self.depth,
            "extension": self.extension,
            "audio": self.audio_tags,
            "subtitle_count": self.subtitles.len(),
            "size_gb": round2(self.size / 1_000_000_000.0),
            "bitrate_mbps": round2(self.bitrate / 1_000_000.0),
            "video_bitrate_mbps": round2(self.video_bitrate / 1_000_000.0),
            "probed": self.probed,
        })
    }
}

fn extract_extension(path_or_name: &str) -> Option<String> {
    let trimmed = path_or_name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (_, ext) = trimmed.rsplit_once('.')?;
    let low = ext.trim().to_ascii_lowercase();
    matches!(
        low.as_str(),
        "mkv" | "mp4" | "iso" | "m2ts" | "ts" | "avi" | "rmvb" | "wmv" | "mpeg" | "mpg" | "mov" | "webm" | "flv"
    )
    .then_some(low)
}

fn canonical_audio_from_stream(s: &MediaStream) -> String {
    let c = s.codec.to_ascii_lowercase();
    let t = format!("{} {}", s.title, s.channel_layout).to_ascii_lowercase();
    let atmos = c.contains("atmos") || t.contains("atmos");
    let dtsx = c.contains("dts:x") || c.contains("dtsx") || t.contains("dts:x") || t.contains("dtsx");
    let ch = if s.channels >= 8 || t.contains("7.1") {
        "7.1"
    } else if s.channels >= 6 || t.contains("5.1") {
        "5.1"
    } else if s.channels == 2 || t.contains("2.0") {
        "2.0"
    } else {
        ""
    };

    if dtsx {
        return "DTS:X".to_string();
    }
    if c.contains("truehd") || t.contains("truehd") {
        if atmos {
            return if !ch.is_empty() {
                format!("TrueHD Dolby Atmos {ch}")
            } else {
                "TrueHD Dolby Atmos 7.1".to_string()
            };
        }
        return if !ch.is_empty() {
            format!("TrueHD {ch}")
        } else {
            "Dolby TrueHD".to_string()
        };
    }
    if c.contains("dts") {
        if t.contains("ma") || c.contains("hd_ma") || c.contains("dts-hd ma") {
            return if !ch.is_empty() {
                format!("DTS-HD MA {ch}")
            } else {
                "DTS-HD MA".to_string()
            };
        }
        if t.contains("hra") || t.contains("dts-hd") || c.contains("dts-hd") {
            return "DTS-HD".to_string();
        }
        return "DTS".to_string();
    }
    if c.contains("eac3") || c.contains("ddp") || t.contains("dd+") || t.contains("dolby digital plus") {
        if atmos {
            return "DDP Dolby Atmos 5.1 | Dolby Digital Plus Atmos".to_string();
        }
        return if ch == "5.1" || ch == "7.1" {
            "DDP 5.1 | Dolby Digital Plus".to_string()
        } else {
            "Dolby Digital Plus".to_string()
        };
    }
    if c.contains("ac3") || t.contains("dolby digital") {
        return "Dolby Digital AC3".to_string();
    }
    if c.contains("pcm") || c.contains("lpcm") {
        return "PCM".to_string();
    }
    if c.contains("flac") {
        return "FLAC".to_string();
    }
    if c.contains("opus") {
        return "Opus".to_string();
    }
    if c.contains("aac") {
        return "AAC".to_string();
    }
    if c.contains("mp3") {
        return "MP3".to_string();
    }
    s.codec.clone()
}

fn extract_audio_from_filename(low: &str) -> Vec<String> {
    let mut out = Vec::new();
    if low.contains("dts:x") || low.contains("dts-x") || low.contains("dtsx") {
        out.push("DTS:X".to_string());
    }
    if low.contains("truehd") {
        if low.contains("atmos") {
            if low.contains("5.1") {
                out.push("TrueHD Dolby Atmos 5.1".to_string());
            } else {
                out.push("TrueHD Dolby Atmos 7.1".to_string());
            }
        } else if low.contains("7.1") {
            out.push("TrueHD 7.1".to_string());
        } else if low.contains("5.1") {
            out.push("TrueHD 5.1".to_string());
        } else {
            out.push("Dolby TrueHD".to_string());
        }
    }
    if low.contains("dts-hd") || low.contains("dtshd") {
        if low.contains("ma") {
            if low.contains("7.1") {
                out.push("DTS-HD MA 7.1".to_string());
            } else if low.contains("5.1") {
                out.push("DTS-HD MA 5.1".to_string());
            } else {
                out.push("DTS-HD MA".to_string());
            }
        } else {
            out.push("DTS-HD".to_string());
        }
    } else if Regex::new(r"(?i)\bdts\b").unwrap().is_match(low) {
        out.push("DTS".to_string());
    }
    if low.contains("ddp") || low.contains("dd+") || low.contains("eac3") || low.contains("e-ac-3") {
        if low.contains("atmos") {
            out.push("DDP Dolby Atmos 5.1 | Dolby Digital Plus Atmos".to_string());
        } else {
            out.push("DDP 5.1 | Dolby Digital Plus".to_string());
        }
    } else if low.contains("ac3") || low.contains("dd5.1") || low.contains("dd2.0") {
        out.push("Dolby Digital AC3".to_string());
    }
    if low.contains("lpcm") || low.contains("pcm") {
        out.push("PCM".to_string());
    }
    if low.contains("flac") {
        out.push("FLAC".to_string());
    }
    if low.contains("opus") {
        out.push("Opus".to_string());
    }
    if low.contains("aac") {
        out.push("AAC".to_string());
    }
    if low.contains("mp3") {
        out.push("MP3".to_string());
    }
    out
}

fn build_subtitle_track(raw_lang: &str, raw_codec: &str, raw_title: &str) -> SubtitleTrack {
    let l = raw_lang.to_ascii_lowercase();
    let c = raw_codec.to_ascii_lowercase();
    let t = raw_title.to_lowercase();

    let fmt = if c.contains("pgs") || c.contains("sup") || t.contains("pgs") {
        "PGS"
    } else if c.contains("ass") || c.contains("ssa") || t.contains("ass") || t.contains("ssa") {
        "ASS"
    } else {
        "SRT"
    };

    let is_bilingual = t.contains("双语")
        || t.contains("中英")
        || t.contains("简英")
        || t.contains("繁英")
        || t.contains("chs&eng")
        || t.contains("bilingual");
    let is_effects = t.contains("特效") || t.contains("effect");

    let lang = if t.contains("繁")
        || t.contains("cht")
        || t.contains("big5")
        || matches!(l.as_str(), "zh-tw" | "zh-hk" | "zht" | "cht")
    {
        "zh-TW"
    } else if l == "yue" || t.contains("粤") {
        "yue"
    } else if l.starts_with("zh")
        || matches!(l.as_str(), "chi" | "zho" | "chs" | "cn")
        || t.contains("简")
        || t.contains("中")
        || t.contains("chs")
    {
        "zh-CN"
    } else if l.starts_with("en") || t.contains("eng") || t.contains("英文") {
        "en"
    } else if l.starts_with("ja") || l == "jpn" || t.contains("日") {
        "ja"
    } else if l.starts_with("ko") || l == "kor" || t.contains("韩") {
        "ko"
    } else if l.is_empty() {
        "und"
    } else {
        raw_lang
    };

    let synthetic_tag = match (lang, is_bilingual, is_effects) {
        ("zh-CN" | "zh-TW" | "yue", true, true) => format!("{fmt} 简英特效 {fmt} 中英特效 {fmt} 中文双语特效"),
        ("zh-CN" | "zh-TW" | "yue", false, true) => format!("{fmt} 简中特效 {fmt} 中文特效"),
        ("zh-CN" | "zh-TW" | "yue", true, false) => format!("{fmt} 简英双语 {fmt} 中英双语 {fmt} 中文双语"),
        ("zh-TW" | "yue", false, false) => format!("{fmt} 繁体中文"),
        ("zh-CN", false, false) => format!("{fmt} 简体中文"),
        ("en", _, _) => format!("{fmt} 英文 {fmt} English"),
        ("ja", _, _) => format!("{fmt} 日文"),
        ("ko", _, _) => format!("{fmt} 韩文"),
        _ => format!("{fmt} {raw_title}"),
    };

    SubtitleTrack {
        lang: lang.to_string(),
        format: fmt.to_string(),
        title: raw_title.to_string(),
        synthetic_tag,
    }
}

fn extract_subtitles_from_filename(low: &str) -> Vec<SubtitleTrack> {
    let mut out = Vec::new();
    let fmt = if low.contains("pgs") || low.contains("sup") {
        "PGS"
    } else if low.contains("ass") || low.contains("ssa") || low.contains("特效") {
        "ASS"
    } else {
        "SRT"
    };
    let effects = low.contains("特效");
    let bilingual = low.contains("中英") || low.contains("简英") || low.contains("双语") || low.contains("chs&eng");
    if effects && bilingual {
        out.push(build_subtitle_track("zh-CN", fmt, "简英双语特效"));
    } else if effects {
        out.push(build_subtitle_track("zh-CN", fmt, "简体中文特效"));
    } else if bilingual {
        out.push(build_subtitle_track("zh-CN", fmt, "中英双语"));
    } else if low.contains("中字")
        || low.contains("简中")
        || low.contains("简体")
        || low.contains("chs")
        || low.contains("chinese")
    {
        out.push(build_subtitle_track("zh-CN", fmt, "简体中文"));
    } else if low.contains("繁中") || low.contains("繁体") || low.contains("cht") {
        out.push(build_subtitle_track("zh-TW", fmt, "繁体中文"));
    }
    out
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReferenceBaseline {
    pub size: f64,
    pub bitrate: f64,
    pub video_bitrate: f64,
}

impl ReferenceBaseline {
    pub fn observe(&mut self, p: &MediaProfile) {
        self.size = self.size.max(p.size);
        self.bitrate = self.bitrate.max(p.bitrate);
        self.video_bitrate = self.video_bitrate.max(p.video_bitrate);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoreBreakdownItem {
    pub dimension: String,
    pub matched: String,
    pub points: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoreResult {
    pub total: f64,
    pub slot: String,
    pub excluded: bool,
    pub withheld_reason: Option<String>,
    pub breakdown: Vec<ScoreBreakdownItem>,
}

impl ScoreResult {
    fn push(&mut self, dimension: &str, matched: &str, raw_points: f64) {
        let pts = round2(raw_points);
        self.breakdown.push(ScoreBreakdownItem {
            dimension: dimension.to_string(),
            matched: matched.to_string(),
            points: pts,
        });
        self.total = round2(self.total + pts);
    }
}

struct CompiledSymItem {
    key: String,
    label: String,
    regex: Regex,
    score: f64,
}

struct CompiledFilter {
    blacklist: Vec<Regex>,
    whitelist: Vec<Regex>,
}

impl CompiledFilter {
    fn from_lists(bl: &Value, wl: &Value) -> Result<Self> {
        let compile_arr = |arr: &Value| -> Result<Vec<Regex>> {
            let mut out = Vec::new();
            for v in arr.as_array().into_iter().flatten() {
                if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    // Expand common Symedia category shorthand `儿童动漫` to also match Mediary `动漫/儿童` & `儿童动画`
                    let expanded = if s == "儿童动漫" {
                        "儿童动漫|动漫/儿童|儿童动画".to_string()
                    } else {
                        s.to_string()
                    };
                    out.push(compile_ci_regex(&expanded)?);
                }
            }
            Ok(out)
        };
        Ok(Self {
            blacklist: compile_arr(bl)?,
            whitelist: compile_arr(wl)?,
        })
    }

    fn allows(&self, context_path: &str) -> bool {
        if self.blacklist.iter().any(|rx| rx.is_match(context_path)) {
            return false;
        }
        if !self.whitelist.is_empty() && !self.whitelist.iter().any(|rx| rx.is_match(context_path)) {
            return false;
        }
        true
    }
}

pub struct Scorer<'a> {
    pub rule: &'a ScoringRule,
    sym_groups: BTreeMap<&'static str, (Vec<CompiledSymItem>, CompiledFilter)>,
    sym_size_filter: Option<CompiledFilter>,
    sym_override_filter: Option<CompiledFilter>,
    sym_slot_regexes: Vec<Option<Regex>>,
}

impl<'a> Scorer<'a> {
    pub fn new(rule: &'a ScoringRule) -> Result<Self> {
        let mut sym_groups = BTreeMap::new();
        let mut sym_size_filter = None;
        let mut sym_override_filter = None;
        let mut sym_slot_regexes = Vec::new();

        if rule.kind == SYMEDIA {
            let b = &rule.body;
            for &(dim_key, _weight_key, _label) in SYM_ALL_DIMENSIONS {
                let (entries, filter) = if matches!(dim_key, "releaseGroup" | "custom_attributes") {
                    let filter = CompiledFilter::from_lists(
                        &b[format!("{dim_key}_blacklist")],
                        &b[format!("{dim_key}_whitelist")],
                    )?;
                    let list: Vec<(String, &Value)> = b[dim_key]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .map(|(i, v)| {
                            let k = v["name"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                                .unwrap_or_else(|| i.to_string());
                            (k, v)
                        })
                        .collect();
                    (list, filter)
                } else {
                    let grp = &b[dim_key];
                    let filter = CompiledFilter::from_lists(&grp["blacklist"], &grp["whitelist"])?;
                    let list: Vec<(String, &Value)> = grp
                        .as_object()
                        .into_iter()
                        .flatten()
                        .filter(|(k, v)| !matches!(k.as_str(), "blacklist" | "whitelist") && v.is_object())
                        .map(|(k, v)| (k.clone(), v))
                        .collect();
                    (list, filter)
                };

                let mut compiled = Vec::new();
                for (item_key, item_val) in entries {
                    let pat_raw = item_val["pattern"].as_str().unwrap_or_default().trim();
                    if pat_raw.is_empty() {
                        continue;
                    }
                    let pat = match dim_key {
                        "file_extension" => {
                            let ext = pat_raw.trim_start_matches('.');
                            format!(r"(?:^|\.)(?:{})(?:$|\s)", regex::escape(ext))
                        }
                        "releaseGroup" => {
                            format!(r"(?:^|[^a-zA-Z0-9])(?:{pat_raw})(?:$|[^a-zA-Z0-9])")
                        }
                        _ => pat_raw.to_string(),
                    };
                    compiled.push(CompiledSymItem {
                        label: item_val["name"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .unwrap_or(&item_key)
                            .to_string(),
                        key: item_key,
                        regex: compile_ci_regex(&pat)?,
                        score: item_val["score"].as_f64().unwrap_or(0.0),
                    });
                }
                sym_groups.insert(dim_key, (compiled, filter));
            }

            sym_size_filter = Some(CompiledFilter::from_lists(
                &b["file_size_blacklist"],
                &b["file_size_whitelist"],
            )?);
            sym_override_filter = Some(CompiledFilter::from_lists(
                &b["always_override_blacklist"],
                &b["always_override_whitelist"],
            )?);

            for slot in b["version_control_versions"].as_array().into_iter().flatten() {
                let rx_s = slot["file_name_regex"].as_str().unwrap_or_default().trim();
                sym_slot_regexes.push(if rx_s.is_empty() {
                    None
                } else {
                    Some(compile_ci_regex(rx_s)?)
                });
            }
        } else {
            let ver = &rule.body["version"];
            if ver["enabled"].as_bool() == Some(true) {
                if let Some(slots) = ver["slots"].as_array().filter(|s| !s.is_empty()) {
                    let mut names = BTreeSet::new();
                    for s in slots {
                        let n = s["name"].as_str().unwrap_or_default().trim();
                        ensure!(!n.is_empty() && names.insert(n), "MediaTidy 版本槽位名称为空或重复: {n}");
                    }
                }
            }
        }

        Ok(Self {
            rule,
            sym_groups,
            sym_size_filter,
            sym_override_filter,
            sym_slot_regexes,
        })
    }

    pub fn wants_source_probe(&self) -> bool {
        if self.rule.kind == SYMEDIA {
            self.rule.body["source_file_extract_metadata"]
                .as_bool()
                .unwrap_or(true)
        } else {
            self.rule.body["source_extract_metadata"]
                .as_bool()
                .unwrap_or(true)
        }
    }

    pub fn score(
        &self,
        p: &MediaProfile,
        baseline: &ReferenceBaseline,
        category: &str,
        iso_protected: bool,
    ) -> ScoreResult {
        if self.rule.kind == SYMEDIA {
            self.score_symedia(p, baseline, category, iso_protected)
        } else {
            self.score_mediatidy(p, baseline, iso_protected)
        }
    }

    fn score_symedia(
        &self,
        p: &MediaProfile,
        baseline: &ReferenceBaseline,
        category: &str,
        iso_protected: bool,
    ) -> ScoreResult {
        let b = &self.rule.body;
        let filter_ctx = format!("{category} {}", p.display_name);
        let mut res = ScoreResult {
            slot: "默认".to_string(),
            ..Default::default()
        };
        let mut matched_keys: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();

        for &(dim_key, weight_key, label) in SYM_ALL_DIMENSIONS {
            let weight = b[weight_key].as_f64().unwrap_or(0.0);
            let Some((items, filter)) = self.sym_groups.get(dim_key) else {
                continue;
            };
            let target_text = match dim_key {
                "videoFormat" => format!("{} {}", p.resolution, p.display_name),
                "resourceType" => format!("{} {}", p.source, p.full_haystack),
                "videoCodec" => format!("{} {}", p.codec, p.display_name),
                "audioCodec" => format!("{} {}", p.audio_tags.join(" "), p.display_name),
                "color_depth" => format!("{} {}", p.depth, p.display_name),
                "frame_rate" => format!("{} {}", p.fps, p.display_name),
                "dolby_vision" => format!("{} {}", p.dolby_vision_tag, p.display_name),
                "dynamic_range" => format!("{} {}", p.hdr, p.full_haystack),
                "subtitle" => format!(
                    "{} {}",
                    p.subtitles
                        .iter()
                        .map(|s| s.synthetic_tag.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    p.display_name
                ),
                "file_extension" => format!(".{}", p.extension),
                _ => p.full_haystack.clone(),
            };

            let hits: Vec<&CompiledSymItem> = items
                .iter()
                .filter(|it| it.regex.is_match(&target_text))
                .collect();
            matched_keys.insert(dim_key, hits.iter().map(|it| it.key.as_str()).collect());

            if weight != 0.0 && filter.allows(&filter_ctx) {
                if let Some(best) = hits.into_iter().max_by(|a, b| a.score.total_cmp(&b.score)) {
                    // Protect `.iso` in designated Blu-ray ISO categories from Symedia's -100 `.iso` penalty
                    let effective_item_score =
                        if dim_key == "file_extension" && iso_protected && p.extension == "iso" && best.score < 0.0 {
                            10.0
                        } else {
                            best.score
                        };
                    res.push(label, &best.label, effective_item_score * weight);
                }
            }
        }

        if p.size > 0.0
            && p.size > baseline.size
            && self
                .sym_size_filter
                .as_ref()
                .is_none_or(|f| f.allows(&filter_ctx))
        {
            let pts =
                b["file_size_score"].as_f64().unwrap_or(0.0) * b["file_size_weight"].as_f64().unwrap_or(0.0);
            if pts != 0.0 {
                res.push("文件体积", "大于对比版本", pts);
            }
        }

        let ov_pts = b["always_override_score"].as_f64().unwrap_or(0.0)
            * b["always_override_weight"].as_f64().unwrap_or(0.0);
        if ov_pts != 0.0
            && self
                .sym_override_filter
                .as_ref()
                .is_none_or(|f| f.allows(&filter_ctx))
        {
            res.push("总是覆盖", "规则强制加权", ov_pts);
        }

        // Symedia Version Control (`version_control_versions`)
        if b["version_control_enabled"].as_bool() == Some(true) {
            let cond_ok = |cond: &Value| -> bool {
                let attr = cond["attribute"].as_str().unwrap_or_default().trim();
                if attr.is_empty() {
                    return false;
                }
                let Some(hit_set) = matched_keys.get(attr) else {
                    return false;
                };
                if hit_set.is_empty() {
                    return false;
                }
                let vals: Vec<&str> = cond["values"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .collect();
                vals.is_empty() || vals.iter().any(|want| hit_set.contains(want))
            };

            let matched_slot = b["version_control_versions"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .find(|(idx, slot)| {
                    let inc_ok = slot["include_conditions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .all(cond_ok);
                    let exc_hit = slot["exclude_conditions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(cond_ok);
                    let rx_ok = self
                        .sym_slot_regexes
                        .get(*idx)
                        .and_then(|o| o.as_ref())
                        .is_none_or(|rx| rx.is_match(&p.full_haystack));
                    inc_ok && !exc_hit && rx_ok
                });

            if let Some((_, s)) = matched_slot {
                res.slot = s["name"].as_str().unwrap_or("默认").to_string();
            } else {
                res.slot = "未匹配".to_string();
                res.withheld_reason = Some("未命中 Symedia 版本控制槽位".to_string());
            }
        }
        res
    }

    fn score_mediatidy(
        &self,
        p: &MediaProfile,
        baseline: &ReferenceBaseline,
        iso_protected: bool,
    ) -> ScoreResult {
        let b = &self.rule.body;
        let mut res = ScoreResult {
            slot: "默认".to_string(),
            ..Default::default()
        };

        // 1. Hard Exclusions (`exclude`)
        let excl = &b["exclude"];
        if str_in_array(&excl["resolutions"], &p.resolution)
            || str_in_array(&excl["codecs"], &p.codec)
            || str_in_array(&excl["hdr_types"], &p.hdr)
        {
            res.excluded = true;
            res.withheld_reason = Some("命中 MediaTidy 硬排除条件".to_string());
        }

        // 2. 27-Slot Classification (`version`)
        let ver = &b["version"];
        if ver["enabled"].as_bool() == Some(true) {
            if let Some(slots) = ver["slots"].as_array().filter(|s| !s.is_empty()) {
                let matching: Vec<&Value> = slots
                    .iter()
                    .filter(|s| {
                        let m = &s["match"];
                        wildcard_match(&m["resolution"], &p.resolution)
                            && wildcard_match(&m["hdr"], &p.hdr)
                            && wildcard_match(&m["frame_rate"], &p.fps)
                            && wildcard_match(&m["source"], &p.source)
                    })
                    .collect();
                // HFR slot priority over standard HDR/SDR slot at same resolution
                let hfr_specific: Vec<&Value> = matching
                    .iter()
                    .copied()
                    .filter(|s| {
                        s["match"]["frame_rate"]
                            .as_array()
                            .is_some_and(|a| !a.is_empty())
                    })
                    .collect();
                let effective = if !hfr_specific.is_empty() {
                    hfr_specific
                } else {
                    matching
                };
                match effective.as_slice() {
                    [single] => {
                        res.slot = single["name"].as_str().unwrap_or("默认").to_string();
                    }
                    [] => {
                        res.slot = "未匹配".to_string();
                        if ver["unmatched"].as_str() != Some("keep") && res.withheld_reason.is_none() {
                            res.withheld_reason = Some("未命中任何版本槽位".to_string());
                        }
                    }
                    many => {
                        res.slot = many[0]["name"].as_str().unwrap_or("默认").to_string();
                    }
                }
            } else if let Some(dims) = ver["dimensions"].as_array().filter(|d| !d.is_empty()) {
                let parts: Vec<&str> = dims
                    .iter()
                    .filter_map(|k| match k.as_str()? {
                        "resolution" => Some(p.resolution.as_str()),
                        "hdr" => Some(p.hdr.as_str()),
                        "frame_rate" => Some(p.fps.as_str()),
                        "source" => Some(p.source.as_str()),
                        _ => None,
                    })
                    .filter(|s| !s.is_empty() && *s != "Unknown")
                    .collect();
                if parts.is_empty() {
                    res.slot = "未匹配".to_string();
                    if res.withheld_reason.is_none() {
                        res.withheld_reason = Some("缺少分槽维度信息".to_string());
                    }
                } else {
                    res.slot = parts.join("-");
                }
            }
        }

        // 3. Non-bitrate priority dimensions
        let scoring = &b["scoring"];
        for (dim_key, label, val) in [
            ("resolution", "分辨率", p.resolution.as_str()),
            ("source", "片源", p.source.as_str()),
            ("video_codec", "视频编码", p.codec.as_str()),
            ("hdr", "动态范围", p.hdr.as_str()),
            ("color_depth", "色彩深度", p.depth.as_str()),
            ("frame_rate", "帧率", p.fps.as_str()),
            ("file_extension", "容器后缀", p.extension.as_str()),
        ] {
            let dim = &scoring[dim_key];
            if dim["enabled"].as_bool() != Some(true) {
                continue;
            }
            let weight = dim["weight"].as_f64().unwrap_or(0.0);
            let Some(prio) = dim["priority"].as_array() else {
                continue;
            };
            if dim_key == "file_extension" && iso_protected && val == "iso" {
                res.push(label, "iso (原盘保护)", 10.0 * weight);
                continue;
            }
            if val.is_empty() {
                res.push(label, "未知", 0.0);
            } else {
                let norm_val = norm_token(val);
                let idx = prio
                    .iter()
                    .position(|x| norm_token(x.as_str().unwrap_or_default()) == norm_val)
                    .or_else(|| prio.iter().position(|x| x.as_str() == Some("Unknown")));
                let base_score = idx.map_or(0.5, |i| rank_points(i, prio.len()));
                res.push(label, val, base_score * weight);
            }
        }

        // 4. Audio Codec (supports both object `{ codecs, atmos }` and plain string entries from MuRongYun8)
        let audio_dim = &scoring["audio_codec"];
        if audio_dim["enabled"].as_bool() == Some(true) {
            let weight = audio_dim["weight"].as_f64().unwrap_or(0.0);
            if let Some(prio) = audio_dim["priority"].as_array() {
                let hit = prio.iter().enumerate().find(|(_, entry)| {
                    p.audio_tags.iter().any(|got_raw| {
                        let got = norm_token(got_raw);
                        if entry["atmos"].as_bool() == Some(true) && !got.contains("atmos") {
                            return false;
                        }
                        if let Some(s) = entry.as_str() {
                            return got.contains(&norm_token(s));
                        }
                        entry["codecs"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .any(|want| got.contains(&norm_token(want)))
                    })
                });
                let pts = hit.map_or(0.0, |(i, _)| rank_points(i, prio.len()));
                res.push(
                    "音频编码",
                    p.audio_tags.first().map_or("无数据", String::as_str),
                    pts * weight,
                );
            }
        }

        // 5. Subtitle (supports both object `{ languages, formats, score }` and plain string entries like `"PGS 简英特效"`)
        let sub_dim = &scoring["subtitle"];
        if sub_dim["enabled"].as_bool() == Some(true) {
            let weight = sub_dim["weight"].as_f64().unwrap_or(0.0);
            if let Some(prio) = sub_dim["priority"].as_array() {
                let mut best_pts = 0.0f64;
                let mut matched_desc = "无字幕数据";
                for (i, entry) in prio.iter().enumerate() {
                    let is_hit = p.subtitles.iter().any(|trk| {
                        if let Some(text_rule) = entry.as_str() {
                            return match_string_subtitle_rule(text_rule, trk);
                        }
                        wildcard_match(&entry["languages"], &trk.lang)
                            && wildcard_match(&entry["formats"], &trk.format)
                            && (entry["bilingual"].as_bool() != Some(true)
                                || trk.synthetic_tag.contains("双语"))
                            && (entry["effects"].as_bool() != Some(true)
                                || trk.synthetic_tag.contains("特效"))
                    });
                    if is_hit {
                        let pts = entry["score"]
                            .as_f64()
                            .filter(|&v| v > 0.0)
                            .unwrap_or_else(|| rank_points(i, prio.len()));
                        if pts > best_pts {
                            best_pts = pts;
                            matched_desc = p
                                .subtitles
                                .first()
                                .map(|s| s.synthetic_tag.as_str())
                                .unwrap_or("命中字幕");
                        }
                    }
                }
                res.push("字幕", matched_desc, best_pts * weight);
            }
        }

        // 6. Logarithmic Bitrate Group (70%: file_size 20 + bitrate 25 + video_bitrate 25)
        for (dim_key, label, val, ref_val, unit_div, unit_str) in [
            ("file_size", "文件体积", p.size, baseline.size, 1_000_000_000.0, "GB"),
            ("bitrate", "总码率", p.bitrate, baseline.bitrate, 1_000_000.0, "Mbps"),
            (
                "video_bitrate",
                "视频码率",
                p.video_bitrate,
                baseline.video_bitrate,
                1_000_000.0,
                "Mbps",
            ),
        ] {
            let dim = &scoring[dim_key];
            if dim["enabled"].as_bool() != Some(true) {
                continue;
            }
            let weight = dim["weight"].as_f64().unwrap_or(0.0);
            let pts = if val > 0.0 && ref_val > 0.0 {
                (9.0 + 1.5 * (val / ref_val).log2()).clamp(0.5, 10.0)
            } else {
                0.0
            };
            let disp = if val > 0.0 {
                format!("{:.2} {}", val / unit_div, unit_str)
            } else {
                "未知".to_string()
            };
            res.push(label, &disp, pts * weight);
        }

        res
    }
}

fn match_string_subtitle_rule(rule_str: &str, trk: &SubtitleTrack) -> bool {
    let r = rule_str.to_lowercase();
    let combined = format!("{} {} {}", trk.format, trk.title, trk.synthetic_tag).to_lowercase();
    for fmt in ["pgs", "ass", "srt"] {
        if r.contains(fmt) && !trk.format.eq_ignore_ascii_case(fmt) {
            return false;
        }
    }
    if (r.contains("简") || r.contains("中")) && !matches!(trk.lang.as_str(), "zh-CN" | "zh" | "yue") {
        if !(r.contains("繁") && trk.lang == "zh-TW") {
            return false;
        }
    }
    if r.contains("繁") && trk.lang != "zh-TW" {
        return false;
    }
    if r.contains("eng") && !r.contains("简") && !r.contains("中") && !r.contains("繁") && trk.lang != "en" {
        return false;
    }
    if r.contains("特效") && !combined.contains("特效") {
        return false;
    }
    if (r.contains("简英") || r.contains("中英") || r.contains("双语")) && !combined.contains("双语") && !combined.contains("简英") {
        return false;
    }
    true
}

fn rank_points(index: usize, len: usize) -> f64 {
    if len <= 1 {
        10.0
    } else {
        (10.0 - (index as f64) * 9.0 / ((len - 1) as f64)).clamp(1.0, 10.0)
    }
}

fn norm_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '+')
        .flat_map(char::to_lowercase)
        .collect()
}

fn str_in_array(arr: &Value, needle: &str) -> bool {
    !needle.is_empty()
        && arr.as_array().is_some_and(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .any(|s| norm_token(s) == norm_token(needle))
        })
}

fn wildcard_match(arr: &Value, actual: &str) -> bool {
    arr.as_array().is_none_or(|list| list.is_empty()) || str_in_array(arr, actual)
}

pub fn find_rule<'a>(cfg: &'a AppConfig, rule_id: &str) -> Result<&'a ScoringRule> {
    cfg.rules
        .iter()
        .find(|r| r.id == rule_id)
        .or_else(|| cfg.rules.first())
        .context("找不到可用的评分规则")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symedia_preset_roundtrip_iso_penalty_and_kids_blacklist() {
        let rules = default_rules().unwrap();
        let sym = rules.iter().find(|r| r.kind == SYMEDIA).unwrap();
        let scorer = Scorer::new(sym).unwrap();

        // Source name without `.iso`, but `cloud_path` ends with `.iso`!
        let iso_p = MediaProfile::extract(
            &LibrarySource {
                name: "Film.2024.2160p.BluRay.HEVC.TrueHD.7.1.Atmos".into(),
                cloud_path: "媒体库/电影/华语电影/2020-2030/Film.iso".into(),
                size: 45_000_000_000.0,
                ..Default::default()
            },
            &[],
        );
        assert_eq!(iso_p.extension, "iso");
        let regular_score = scorer.score(&iso_p, &ReferenceBaseline::default(), "电影/华语电影/2020-2030", false);
        let iso_zone_score = scorer.score(&iso_p, &ReferenceBaseline::default(), "电影/技术专区/蓝光原盘", true);
        assert!(iso_zone_score.total - regular_score.total > 40.0);

        // `儿童动漫` subtitle blacklist matches Mediary's `动漫/儿童` category!
        let ep = MediaProfile::extract(
            &LibrarySource {
                name: "Bluey.S01E01.2160p.WEB-DL.H265.DDP5.1.ASS.简英特效.mkv".into(),
                size: 1_000_000_000.0,
                ..Default::default()
            },
            &[],
        );
        let normal_tv = scorer.score(&ep, &ReferenceBaseline::default(), "电视剧/欧美剧", false);
        let kids_tv = scorer.score(&ep, &ReferenceBaseline::default(), "动漫/儿童", false);
        assert!(normal_tv.total > kids_tv.total);
    }

    #[test]
    fn mediatidy_27_slots_and_string_subtitle_entries_work() {
        let rules = default_rules().unwrap();
        let tidy = rules.iter().find(|r| r.kind == MEDIATIDY).unwrap();
        let scorer = Scorer::new(tidy).unwrap();

        let low = MediaProfile::extract(
            &LibrarySource {
                name: "光阴之外.S01E49.2160p.HDR10.WEB-DL.H265.AAC.mkv".into(),
                size: 800_000_000.0,
                bitrate: 5_600_000.0,
                ..Default::default()
            },
            &[],
        );
        let high = MediaProfile::extract(
            &LibrarySource {
                name: "光阴之外.S01E49.2160p.HDR10.WEB-DL.H265.DDP5.1.ASS.简英特效.mkv".into(),
                size: 2_800_000_000.0,
                bitrate: 19_400_000.0,
                ..Default::default()
            },
            &[],
        );
        let mut ref_base = ReferenceBaseline::default();
        ref_base.observe(&low);
        ref_base.observe(&high);
        let s_low = scorer.score(&low, &ref_base, "动漫/国漫", false);
        let s_high = scorer.score(&high, &ref_base, "动漫/国漫", false);
        assert_eq!(s_low.slot, "4K-HDR");
        assert_eq!(s_high.slot, "4K-HDR");
        assert!(s_high.total - s_low.total > 120.0);

        // Unprobed SDR release without explicit `.SDR.` token must land in `4K-SDR` (never `未匹配`)!
        let unprobed_sdr = MediaProfile::extract(
            &LibrarySource {
                name: "六朝 (2026) - S01E09 - 2160p.WEB-DL.H265.DDP 5.1.mkv".into(),
                size: 873_924_355.0,
                ..Default::default()
            },
            &[],
        );
        assert_eq!(unprobed_sdr.hdr, "SDR");
        let s_sdr = scorer.score(&unprobed_sdr, &ReferenceBaseline::default(), "电视剧/国产剧", false);
        assert_eq!(s_sdr.slot, "4K-SDR");
        assert!(s_sdr.withheld_reason.is_none());

        // HDRVivid / CUVA domestic streaming HDR tags map cleanly to `4K-HDR`
        let hdr_vivid = MediaProfile::extract(
            &LibrarySource {
                name: "凤起微月 (2026) - S01E21 - 2160p.WEB-DL HDRVivid.H265 10bit.AAC.mp4".into(),
                size: 950_000_000.0,
                ..Default::default()
            },
            &[],
        );
        assert_eq!(hdr_vivid.hdr, "HDR");
        assert_eq!(
            scorer.score(&hdr_vivid, &ReferenceBaseline::default(), "电视剧/国产剧", false).slot,
            "4K-HDR"
        );
    }
}

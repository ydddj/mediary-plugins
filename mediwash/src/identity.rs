use crate::host::HostClient;
use crate::models::AppConfig;
use crate::rules::extract_aux_yaml;
use anyhow::Result;
use fancy_regex::{Regex as FancyRegex, RegexBuilder as FancyBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

pub const PRESET_MATCHING_YAML: &str = include_str!("../presets/matching.yaml");
pub const PRESET_TMDB_YAML: &str = include_str!("../presets/tmdb.yaml");

pub fn compile_fancy(pattern: &str) -> Result<FancyRegex> {
    FancyBuilder::new(&format!("(?i:{pattern})"))
        .backtrack_limit(100_000)
        .build()
        .map_err(Into::into)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedFilename {
    pub title: String,
    pub year: Option<u32>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub episode_end: Option<u32>,
    pub explicit_tmdb: Option<u32>,
    pub inferred_type: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecognizedMedia {
    pub media_type: String,
    pub tmdb_id: String,
    pub title: String,
    pub year: Option<u32>,
    pub episode: Option<(u32, u32)>,
    pub episode_end: Option<u32>,
    pub matched_by: String,
}

#[derive(Debug)]
pub enum ResolveOutcome {
    Matched(RecognizedMedia),
    Blacklisted { tmdb_id: String, reason: String },
    Unresolved(String),
}

struct RuleLayer {
    label: &'static str,
    matching: Value,
    tmdb: Value,
    mr_patterns: Vec<(FancyRegex, Value)>,
}

impl RuleLayer {
    fn from_yaml(label: &'static str, matching_raw: &str, tmdb_raw: &str) -> Self {
        let matching = if matching_raw.trim().is_empty() {
            Value::Null
        } else {
            serde_yaml::from_str(&extract_aux_yaml(matching_raw, "mr:")).unwrap_or(Value::Null)
        };
        let tmdb = if tmdb_raw.trim().is_empty() {
            Value::Null
        } else {
            serde_yaml::from_str(&extract_aux_yaml(tmdb_raw, "tmdb_override:"))
                .unwrap_or(Value::Null)
        };
        let mr_patterns = matching["mr"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let pat = r["pattern"].as_str()?;
                let rx = compile_fancy(pat).ok()?;
                Some((rx, r.clone()))
            })
            .collect();
        Self {
            label,
            matching,
            tmdb,
            mr_patterns,
        }
    }
}

pub struct RecognitionEngine {
    layers: Vec<RuleLayer>,
    manual_bindings: std::collections::BTreeMap<String, crate::models::ManualBinding>,
}

pub fn strip_invisible_chars(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            !matches!(
                c,
                '\u{200B}'..='\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{206F}'
                    | '\u{FEFF}'
            ) && !c.is_control()
        })
        .collect()
}

pub fn norm_title_key(s: &str) -> String {
    strip_invisible_chars(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Parses title, year, season/episode, and explicit `[tmdbid-12345]` from filename + optional parent folder context.
pub fn parse_media_name(filename: &str, rel_folder: &str) -> ParsedFilename {
    static TMDB_TAG_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(?:tmdbid|tmdb)[=:\s_-]*(\d{1,9})").unwrap());
    static SE_RANGE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\bS(\d{1,2})\s*E(\d{1,4})\s*(?:[-~]\s*E?(\d{1,4})|E(\d{1,4}))\b").unwrap()
    });
    static SE_SINGLE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\bS(\d{1,2})\s*[._-]?E(\d{1,4})\b").unwrap());
    static EP_ONLY_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:\bEP?\s*(\d{1,4})\b|第\s*(\d{1,4})\s*[集话話期]|\[(\d{2,4})(?:v\d)?\])")
            .unwrap()
    });
    static TRAILING_PAREN_EP_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[\(（]\s*(\d{1,3})\s*[\)）]\s*$").unwrap());
    static FOLDER_SEASON_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:\bSeason\s*(\d{1,2})\b|\bS(\d{1,2})\b|第\s*(\d{1,2})\s*季)").unwrap()
    });
    static YEAR_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b(19\d{2}|20\d{2})\b").unwrap());
    static TECH_CUT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(4320p|2160p|1440p|1080[pi]|720p|576p|480p|4k|uhd|web[ -]?dl|webrip|blu[ -]?ray|bdrip|remux|hdtv|h\.?26[45]|x26[45]|hevc|av1|dv|dovi|hdrvivid|hdr10|sdr)\b").unwrap()
    });

    let clean_file = strip_invisible_chars(filename);
    let clean_folder = strip_invisible_chars(rel_folder);
    let filename = clean_file.as_str();
    let rel_folder = clean_folder.as_str();

    let folder_is_movie = rel_folder.split('/').any(|s| {
        matches!(
            s.trim(),
            "电影" | "动画电影" | "外语电影" | "华语电影" | "剧场版"
        )
    });
    let folder_is_tv = !folder_is_movie
        && rel_folder.split('/').any(|s| {
            matches!(
                s.trim(),
                "电视剧" | "剧集" | "国产剧" | "日韩剧" | "欧美剧" | "港台剧" | "番剧" | "综艺" | "国漫" | "日漫"
            )
        });

    let combined_ctx = if rel_folder.trim().is_empty() {
        filename.to_string()
    } else {
        format!("{rel_folder}/{filename}")
    };
    let explicit_tmdb = TMDB_TAG_RE
        .captures(&combined_ctx)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u32>().ok())
        .filter(|&id| id > 0);

    let stem = filename
        .rsplit_once('.')
        .map_or(filename, |(s, _)| s)
        .replace(['.', '_'], " ");

    let tech_pos = TECH_CUT_RE.find(&stem).map(|m| m.start());
    let pre_tech_stem = &stem[..tech_pos.unwrap_or(stem.len())];

    let mut season = None;
    let mut episode = None;
    let mut episode_end = None;
    let mut ep_cut_pos = None;

    if let Some(cap) = SE_RANGE_RE.captures(&stem) {
        season = cap.get(1).and_then(|m| m.as_str().parse().ok());
        episode = cap.get(2).and_then(|m| m.as_str().parse().ok());
        episode_end = cap
            .get(3)
            .or_else(|| cap.get(4))
            .and_then(|m| m.as_str().parse().ok());
        ep_cut_pos = cap.get(0).map(|m| m.start());
    } else if let Some(cap) = SE_SINGLE_RE.captures(&stem) {
        season = cap.get(1).and_then(|m| m.as_str().parse().ok());
        episode = cap.get(2).and_then(|m| m.as_str().parse().ok());
        ep_cut_pos = cap.get(0).map(|m| m.start());
    } else if let Some(cap) = EP_ONLY_RE.captures(&stem) {
        let ep_val = cap
            .get(1)
            .or_else(|| cap.get(2))
            .or_else(|| cap.get(3))
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .filter(|&e| e > 0 && !(1900..=2099).contains(&e));
        if let Some(ep) = ep_val {
            let folder_s = FOLDER_SEASON_RE
                .captures(rel_folder)
                .and_then(|fc| fc.get(1).or_else(|| fc.get(2)).or_else(|| fc.get(3)))
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .unwrap_or(1);
            season = Some(folder_s);
            episode = Some(ep);
            ep_cut_pos = cap.get(0).map(|m| m.start());
        }
    } else if let Some(cap) = (!folder_is_movie)
        .then(|| TRAILING_PAREN_EP_RE.captures(pre_tech_stem))
        .flatten()
    {
        // Classic HK/Asia TV naming like `爱情全保 (1).mkv` ~ `爱情全保 (20).mkv`
        // Guard: ignore `(1)` duplicate copy suffixes when inside `/电影/` or when a 4-digit year `(2024)` already precedes it!
        let prefix = &pre_tech_stem[..cap.get(0).map_or(0, |m| m.start())];
        let has_year_before = YEAR_RE
            .find_iter(prefix)
            .any(|ym| prefix[..ym.start()].chars().any(char::is_alphanumeric));
        if !has_year_before && prefix.chars().any(char::is_alphanumeric) {
            if let Some(ep) = cap
                .get(1)
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .filter(|&e| (1..=999).contains(&e))
            {
                let folder_s = FOLDER_SEASON_RE
                    .captures(rel_folder)
                    .and_then(|fc| fc.get(1).or_else(|| fc.get(2)).or_else(|| fc.get(3)))
                    .and_then(|m| m.as_str().parse::<u32>().ok())
                    .unwrap_or(1);
                season = Some(folder_s);
                episode = Some(ep);
                ep_cut_pos = cap.get(0).map(|m| m.start());
            }
        }
    } else if let Some(bare_num) = (!folder_is_movie)
        .then(|| stem.trim().parse::<u32>().ok())
        .flatten()
    {
        // Bare episode file like `01.mkv` or `1089.mkv` inside a show folder (exclude 1900+ years)
        if (1..=1899).contains(&bare_num) && !rel_folder.trim().is_empty() {
            let folder_s = FOLDER_SEASON_RE
                .captures(rel_folder)
                .and_then(|fc| fc.get(1).or_else(|| fc.get(2)).or_else(|| fc.get(3)))
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .unwrap_or(1);
            season = Some(folder_s);
            episode = Some(bare_num);
            ep_cut_pos = Some(0);
        }
    }

    let cut = [ep_cut_pos, tech_pos]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(stem.len());
    let mut raw_title = stem[..cut].to_string();

    // If filename before `S01E01` has no title (e.g. `S01E01.2160p.mkv` inside `庆余年 (2019)/Season 01`), pull title from parent folder!
    if clean_title_noise(&raw_title).is_empty() && !rel_folder.trim().is_empty() {
        for seg in rel_folder.split('/').rev() {
            let seg_clean = seg.replace(['.', '_'], " ");
            if FOLDER_SEASON_RE.is_match(&seg_clean)
                && clean_title_noise(&FOLDER_SEASON_RE.replace_all(&seg_clean, "")).is_empty()
            {
                continue;
            }
            let folder_cut = TECH_CUT_RE
                .find(&seg_clean)
                .map(|m| m.start())
                .unwrap_or(seg_clean.len());
            let candidate = &seg_clean[..folder_cut];
            if !clean_title_noise(candidate).is_empty() {
                raw_title = candidate.to_string();
                break;
            }
        }
    }

    // Extract year (must appear after at least 1 title character so numeric titles like `1917` / `2046` stay intact!)
    let year_match = YEAR_RE
        .find_iter(&raw_title)
        .find(|m| raw_title[..m.start()].chars().any(char::is_alphanumeric));
    let year = year_match
        .and_then(|m| m.as_str().parse::<u32>().ok())
        .or_else(|| {
            YEAR_RE
                .find_iter(rel_folder)
                .next()
                .and_then(|m| m.as_str().parse::<u32>().ok())
        });
    if let Some(ym) = year_match {
        raw_title.truncate(ym.start());
    }
    let title = clean_title_noise(&raw_title);
    let inferred_type = if episode.is_some() || folder_is_tv {
        "tv"
    } else {
        "movie"
    };

    ParsedFilename {
        title,
        year,
        season,
        episode,
        episode_end,
        explicit_tmdb,
        inferred_type,
    }
}

fn clean_title_noise(s: &str) -> String {
    static BRACKET_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\[(?:tmdbid|tmdb)[^\]]*\]|\{(?:tmdbid|tmdb)[^}]*\}|\((?:tmdbid|tmdb)[^)]*\)",
        )
        .unwrap()
    });
    let cleaned = strip_invisible_chars(s);
    let without_tags = BRACKET_TAG_RE.replace_all(&cleaned, " ");
    without_tags
        .trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '-' | '_' | '.' | '[' | ']' | '(' | ')' | '【' | '】' | '（' | '）'
                )
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl RecognitionEngine {
    pub fn new(cfg: &AppConfig) -> Self {
        let mut layers = Vec::new();
        // Precedence 1: Local user overrides
        if !cfg.local_matching_yaml.trim().is_empty() || !cfg.local_tmdb_yaml.trim().is_empty() {
            layers.push(RuleLayer::from_yaml(
                "本地覆盖",
                &cfg.local_matching_yaml,
                &cfg.local_tmdb_yaml,
            ));
        }
        // Precedence 2: Remote subscriptions (never overwrites local)
        for sub in cfg
            .subscriptions
            .values()
            .filter(|s| s.enabled && !s.cache.is_empty())
        {
            match sub.kind.as_str() {
                "matching" => layers.push(RuleLayer::from_yaml("远程订阅", &sub.cache, "")),
                "tmdb" => layers.push(RuleLayer::from_yaml("远程订阅", "", &sub.cache)),
                _ => {}
            }
        }
        // Precedence 3: Built-in verified presets
        layers.push(RuleLayer::from_yaml(
            "内置预设",
            PRESET_MATCHING_YAML,
            PRESET_TMDB_YAML,
        ));
        Self {
            layers,
            manual_bindings: cfg.manual_bindings.clone(),
        }
    }

    /// Applies `mr` preprocessing & binding + `tmdb_override` without touching the network.
    pub fn preprocess(
        &self,
        filename: &str,
        rel_folder: &str,
        preferred_type: &str,
    ) -> (ParsedFilename, Option<(u32, String, String)>) {
        // Check manual user bindings first (by folder key or filename title key)
        let initial = parse_media_name(filename, rel_folder);
        for key_candidate in [norm_title_key(rel_folder), norm_title_key(&initial.title)] {
            if !key_candidate.is_empty() {
                if let Some(mb) = self.manual_bindings.get(&key_candidate) {
                    if let Ok(tid) = mb.tmdb_id.parse::<u32>() {
                        let mut out = initial.clone();
                        if let Some(s) = mb.season {
                            out.season = Some(s);
                        }
                        if let Some(e) = mb.episode {
                            out.episode = Some(e);
                        }
                        if !mb.title.is_empty() {
                            out.title = mb.title.clone();
                        }
                        return (out, Some((tid, mb.media_type.clone(), "人工绑定".to_string())));
                    }
                }
            }
        }

        if let Some(tid) = initial.explicit_tmdb {
            let mt = if preferred_type == "movie" || preferred_type == "tv" {
                preferred_type.to_string()
            } else {
                initial.inferred_type.to_string()
            };
            return (initial, Some((tid, mt, "路径内置 TMDB 标签".to_string())));
        }

        let mut cleaned_name = filename.to_string();
        for layer in &self.layers {
            for (rx, rule) in &layer.mr_patterns {
                if let Some(bind) = rule.get("bind") {
                    let bind_media = bind["media"].as_str().unwrap_or_default();
                    if (preferred_type == "movie" || preferred_type == "tv")
                        && !bind_media.is_empty()
                        && bind_media != preferred_type
                    {
                        continue;
                    }
                    if let Ok(Some(caps)) = rx.captures(&cleaned_name) {
                        let mut parsed = parse_media_name(&cleaned_name, rel_folder);
                        let explicit_ep = json_u32(&bind["episode"]).or_else(|| {
                            let grp = bind["episode"].as_str()?.strip_prefix('$')?.parse().ok()?;
                            caps.get(grp)?.as_str().parse().ok()
                        });
                        if let Some(ep) = explicit_ep {
                            parsed.episode = Some(ep);
                        }
                        if let Some(s) = json_u32(&bind["season"]) {
                            parsed.season = Some(s);
                        } else if bind_media == "tv" && parsed.season.is_none() && parsed.episode.is_some() {
                            parsed.season = Some(1);
                        }
                        if let Some(tid) = json_u32(&bind["tmdb"]) {
                            let mt = if !bind_media.is_empty() {
                                bind_media.to_string()
                            } else {
                                parsed.inferred_type.to_string()
                            };
                            return (parsed, Some((tid, mt, format!("{} · mr 强绑", layer.label))));
                        }
                    }
                } else {
                    let rep = if rule["action"].as_str() == Some("remove") {
                        Some("")
                    } else {
                        rule["replace"].as_str()
                    };
                    if let Some(r) = rep {
                        cleaned_name = rx.replace_all(&cleaned_name, r).into_owned();
                    }
                }
            }
        }

        let parsed = parse_media_name(&cleaned_name, rel_folder);
        let eff_type = if preferred_type == "movie" || preferred_type == "tv" {
            preferred_type
        } else {
            parsed.inferred_type
        };
        let tkey = norm_title_key(&parsed.title);
        if !tkey.is_empty() {
            for layer in &self.layers {
                if let Some(hit) = layer.tmdb["tmdb_override"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|r| {
                        r["media_type"].as_str() == Some(eff_type)
                            && norm_title_key(r["title"].as_str().unwrap_or_default()) == tkey
                    })
                {
                    if let Some(tid) = json_u32(&hit["tmdb_id"]) {
                        return (
                            parsed,
                            Some((tid, eff_type.to_string(), format!("{} · TMDB 覆盖", layer.label))),
                        );
                    }
                }
            }
        }
        (parsed, None)
    }

    pub fn apply_mp_mapping(&self, mut tmdb_id: u32, season: u32, ep: u32) -> (u32, u32, u32, bool) {
        for layer in &self.layers {
            for rule in layer.matching["mp"].as_array().into_iter().flatten() {
                if json_u32(&rule["tmdb"]) != Some(tmdb_id)
                    || json_u32(&rule["from"]["season"]) != Some(season)
                {
                    continue;
                }
                let from_ep = &rule["from"]["ep"];
                let to_ep = &rule["to"]["ep"];
                if let (Some(lo), Some(hi), Some(dst_lo), Some(dst_hi), Some(dst_s)) = (
                    json_u32(&from_ep[0]),
                    json_u32(&from_ep[1]),
                    json_u32(&to_ep[0]),
                    json_u32(&to_ep[1]),
                    json_u32(&rule["to"]["season"]),
                ) {
                    if (lo..=hi).contains(&ep) && dst_hi >= dst_lo && (ep - lo) <= (dst_hi - dst_lo) {
                        tmdb_id = json_u32(&rule["to_tmdb"]).unwrap_or(tmdb_id);
                        return (tmdb_id, dst_s, dst_lo + (ep - lo), true);
                    }
                }
            }
        }
        (tmdb_id, season, ep, false)
    }

    pub fn is_blacklisted(&self, tmdb_id: u32, media_type: &str) -> bool {
        self.layers.iter().any(|l| {
            l.tmdb["tmdb_blacklist"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| {
                    json_u32(&r["tmdb_id"]) == Some(tmdb_id)
                        && r["media_type"]
                            .as_str()
                            .is_none_or(|mt| mt == media_type)
                })
        })
    }

    pub fn resolve(
        &self,
        host: &HostClient,
        filename: &str,
        rel_folder: &str,
        preferred_type: &str,
    ) -> Result<ResolveOutcome> {
        let (parsed, bound) = self.preprocess(filename, rel_folder, preferred_type);
        let mut media_type = if let Some((_, ref mt, _)) = bound {
            mt.clone()
        } else if preferred_type == "movie" || preferred_type == "tv" {
            preferred_type.to_string()
        } else {
            parsed.inferred_type.to_string()
        };

        if media_type == "tv" && parsed.episode.is_none() {
            return Ok(ResolveOutcome::Unresolved(
                "剧集文件缺少可识别的季集号，请核对文件名或手工指定季集".to_string(),
            ));
        }

        let (mut tmdb_num, mut matched_by) = if let Some((tid, _, source_desc)) = bound {
            (tid, source_desc)
        } else {
            if parsed.title.is_empty() {
                return Ok(ResolveOutcome::Unresolved(
                    "无法从文件名或父目录提取片名，请人工指定 TMDB".to_string(),
                ));
            }
            match host.resolve_tmdb(&parsed.title, parsed.year, &media_type)? {
                Some(raw_id) => {
                    let Ok(tid) = raw_id.parse::<u32>() else {
                        return Ok(ResolveOutcome::Unresolved(
                            "宿主返回的 TMDB ID 格式异常".to_string(),
                        ));
                    };
                    (tid, "Mediary TMDB 识别".to_string())
                }
                None if preferred_type == "auto" && media_type == "movie" => {
                    // Fallback: maybe a movie-like filename without S01E01 is actually a special or movie without year
                    if parsed.year.is_some() {
                        if let Some(raw_id) = host.resolve_tmdb(&parsed.title, None, "movie")? {
                            if let Ok(tid) = raw_id.parse::<u32>() {
                                (tid, "Mediary TMDB 识别（忽略年份兜底）".to_string())
                            } else {
                                return Ok(ResolveOutcome::Unresolved("Mediary 未匹配到 TMDB 条目".to_string()));
                            }
                        } else {
                            return Ok(ResolveOutcome::Unresolved(format!(
                                "Mediary 未匹配到《{}》的 TMDB 条目，可点击「指定 TMDB」绑定",
                                parsed.title
                            )));
                        }
                    } else {
                        return Ok(ResolveOutcome::Unresolved(format!(
                            "Mediary 未匹配到《{}》的 TMDB 条目，可点击「指定 TMDB」绑定",
                            parsed.title
                        )));
                    }
                }
                None if preferred_type == "auto" && media_type == "tv" => {
                    // Fallback: if a filename like `电影名 (1).mkv` was a duplicate copy suffix rather than Episode 1, try movie!
                    if let Some(raw_id) = host.resolve_tmdb(&parsed.title, parsed.year, "movie")? {
                        if let Ok(tid) = raw_id.parse::<u32>() {
                            media_type = "movie".to_string();
                            (tid, "Mediary TMDB 识别（电影重名副本兜底）".to_string())
                        } else {
                            return Ok(ResolveOutcome::Unresolved("Mediary 未匹配到 TMDB 条目".to_string()));
                        }
                    } else {
                        return Ok(ResolveOutcome::Unresolved(format!(
                            "Mediary 未匹配到《{}》的 TMDB 条目，可点击「指定 TMDB」绑定",
                            parsed.title
                        )));
                    }
                }
                None => {
                    return Ok(ResolveOutcome::Unresolved(format!(
                        "Mediary 未匹配到《{}》的 TMDB 条目，可点击「指定 TMDB」绑定",
                        parsed.title
                    )));
                }
            }
        };

        let mut episode_pair = parsed.episode.map(|ep| (parsed.season.unwrap_or(1), ep));
        if media_type == "movie" {
            episode_pair = None;
        }
        if let Some((s, e)) = episode_pair {
            let (new_tmdb, new_s, new_e, mapped) = self.apply_mp_mapping(tmdb_num, s, e);
            tmdb_num = new_tmdb;
            episode_pair = Some((new_s, new_e));
            if mapped {
                matched_by = format!("{matched_by} + mp 季集映射(S{new_s:02}E{new_e:02})");
                media_type = "tv".to_string();
            }
        }

        if self.is_blacklisted(tmdb_num, &media_type) {
            return Ok(ResolveOutcome::Blacklisted {
                tmdb_id: tmdb_num.to_string(),
                reason: format!("命中 TMDB 黑名单 ({media_type}:{tmdb_num})，已拦截待人工复核"),
            });
        }

        Ok(ResolveOutcome::Matched(RecognizedMedia {
            media_type,
            tmdb_id: tmdb_num.to_string(),
            title: if parsed.title.is_empty() {
                format!("TMDB:{tmdb_num}")
            } else {
                parsed.title
            },
            year: parsed.year,
            episode: episode_pair,
            episode_end: parsed.episode_end,
            matched_by,
        }))
    }
}

fn json_u32(v: &Value) -> Option<u32> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_titles_folder_context_mp_mapping_and_tmdb_override_work() {
        let cfg = crate::db::default_app_config().unwrap();
        let eng = RecognitionEngine::new(&cfg);

        // 1917 numeric title replacement
        let (p1917, _) = eng.preprocess("1917.2019.2160p.UHD.BluRay.mkv", "", "auto");
        assert_eq!(p1917.title, "一九一七");
        assert_eq!(p1917.year, Some(2019));

        // 2046 numeric bind
        let (_, b2046) = eng.preprocess("2046.2004.2160p.BluRay.mkv", "", "auto");
        assert_eq!(b2046.unwrap().0, 844);

        // Poor Things (可怜的东西) no longer conflicts!
        let (_, bpoor) = eng.preprocess("可怜的东西.2023.2160p.WEB-DL.mkv", "", "movie");
        assert_eq!(bpoor.unwrap().0, 792307);

        // One Piece fixed TMDB 37854 + folder context bare episode `1089.mkv` + mp mapping
        let (pop, bop) = eng.preprocess("1089.mkv", "海贼王 (1999)/Season 01", "auto");
        assert_eq!(bop.unwrap().0, 37854);
        assert_eq!(pop.episode, Some(1089));
        let (mapped_tmdb, mapped_s, mapped_e, did_map) = eng.apply_mp_mapping(37854, 1, 1089);
        assert_eq!(mapped_tmdb, 37854);
        assert!(did_map || (mapped_s == 1 && mapped_e == 1089));

        // Blacklist check (`318729` 凡人修仙传 false entry)
        assert!(eng.is_blacklisted(318729, "tv"));

        // Classic HK/Asia parenthesized episode `爱情全保 (17).mkv` + full-width `（20）`
        let p_hk = parse_media_name("爱情全保 (17).mkv", "");
        assert_eq!(p_hk.title, "爱情全保");
        assert_eq!(p_hk.season, Some(1));
        assert_eq!(p_hk.episode, Some(17));
        assert_eq!(p_hk.inferred_type, "tv");

        // Duplicate movie copy with year `星际穿越 (2014) (1).mkv` must NOT be misparsed as S01E01!
        let p_dup = parse_media_name("星际穿越 (2014) (1).mkv", "");
        assert_eq!(p_dup.title, "星际穿越");
        assert_eq!(p_dup.year, Some(2014));
        assert_eq!(p_dup.episode, None);

        // Invisible Unicode bidi mark `\u{200e}` stripped + `{tmdb-154420}` extracted
        let p_bidi = parse_media_name("满城尽带黄金甲\u{200e} (2006) - 1080p.BluRay.mkv", "");
        assert_eq!(p_bidi.title, "满城尽带黄金甲");
        let p_tag = parse_media_name("半熟恋人.2021 - S05E20 - {tmdb-154420}.mp4", "");
        assert_eq!(p_tag.explicit_tmdb, Some(154420));
        assert_eq!(p_tag.season, Some(5));
        assert_eq!(p_tag.episode, Some(20));

        // Mediary PT upload folder category context (`link/电影/...` vs `link/电视剧/...`)
        let p_mov_dir = parse_media_name("英雄本色 (2).mkv", "link/电影/华语电影");
        assert_eq!(p_mov_dir.inferred_type, "movie");
        assert_eq!(p_mov_dir.episode, None);

        let p_tv_dir = parse_media_name("幕后特辑.mp4", "link/电视剧/国产剧/骨鉴卿心 (2026) [tmdbid-326202]/Season 01");
        assert_eq!(p_tv_dir.inferred_type, "tv");
        assert_eq!(p_tv_dir.explicit_tmdb, Some(326202));
    }
}

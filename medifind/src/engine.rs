use crate::db::Db;
use crate::host::{HostClient, Subscription};
use crate::models::{
    normalize_tg_level, AppConfig, MovieAttributes, PluginSettings, ProbeCacheEntry, ResourceItem,
    ReviewingItem, ShareFileNode, ShieldIndexedFile, ShieldShareRecord,
};
use crate::providers;
use crate::rules::{self, EpisodePair};
use anyhow::{anyhow, ensure, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;
use url::Url;

pub const IN_FLIGHT_CHASE_TTL_SECS: i64 = 6 * 3600;
pub const IN_FLIGHT_WASH_TTL_SECS: i64 = 24 * 3600;

#[derive(Debug, Clone, Default, Serialize)]
pub struct LibrarySnapshot {
    pub tmdb_id: String,
    pub media_type: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub poster_url: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub origin_country: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub original_language: String,
    pub in_library: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub movie_sizes_gb: Vec<f64>,
    #[serde(skip_serializing)]
    pub local_episodes: BTreeSet<EpisodePair>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub local_ep_labels: Vec<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub season_totals: HashMap<u32, u32>,
    #[serde(skip_serializing)]
    pub missing_episodes: Vec<EpisodePair>,
    #[serde(rename = "missing_episodes", skip_serializing_if = "Vec::is_empty")]
    pub missing_ep_labels: Vec<String>,
}

pub fn effective_proxy(mc: Option<&HostClient>, cfg: &AppConfig) -> String {
    if !cfg.custom_proxy.trim().is_empty() {
        return cfg.custom_proxy.trim().to_string();
    }
    if let Some(host) = mc {
        return host.get_host_status().proxy_url;
    }
    String::new()
}

pub fn build_library_snapshot(
    mc: Option<&HostClient>,
    cfg: &AppConfig,
    tmdb_id: &str,
    media_type: &str,
    fallback_title: &str,
) -> LibrarySnapshot {
    let mtype = if matches!(media_type.trim().to_ascii_lowercase().as_str(), "tv" | "series") {
        "tv"
    } else {
        "movie"
    };
    let mut snap = LibrarySnapshot {
        tmdb_id: tmdb_id.trim().to_string(),
        media_type: mtype.to_string(),
        title: fallback_title.trim().to_string(),
        ..Default::default()
    };
    let Some(host) = mc else {
        return snap;
    };
    if snap.tmdb_id.is_empty() {
        return snap;
    };

    let mut ignored_seasons = HashSet::new();
    for ig in &cfg.ignored_episodes {
        if ig.tmdb_id == snap.tmdb_id {
            if ig.episode == 0 {
                ignored_seasons.insert(ig.season);
            } else {
                snap.local_episodes.insert(EpisodePair {
                    season: ig.season,
                    episode: ig.episode,
                });
            }
        }
    }

    let today = Local::now().format("%Y-%m-%d").to_string();
    if let Ok(det) = host.get_tmdb_details(&snap.tmdb_id, mtype) {
        if snap.title.is_empty() {
            snap.title = if !det.title.is_empty() { det.title } else { det.name };
        }
        if let Some(p) = crate::host::tmdb_poster_https(&det.poster_path) {
            snap.poster_url = p;
        }
        snap.origin_country = det.origin_country;
        snap.original_language = det.original_language;
        for s in det.seasons {
            if s.season_number == 0 || s.episode_count == 0 {
                continue;
            }
            if s.air_date.len() >= 10 && s.air_date[..10] > *today {
                continue;
            }
            snap.season_totals.insert(s.season_number, s.episode_count);
        }
    }

    let status_key = format!("{}:{}", mtype, snap.tmdb_id);
    let st_map = host.get_media_statuses(&[status_key.clone()], mtype == "tv");
    if let Some(st) = st_map.get(&status_key) {
        if st.is_fully_collected || st.is_partially_collected || st.inventory_count > 0 {
            snap.in_library = true;
        }
        if mtype == "tv" {
            for (s_str, eps) in &st.collected_episodes_by_season {
                if let Ok(s_num) = s_str.trim().parse::<u32>() {
                    if s_num > 0 {
                        for &ep in eps {
                            if ep > 0 {
                                snap.local_episodes.insert(EpisodePair {
                                    season: s_num,
                                    episode: ep,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    if mtype == "movie" && snap.in_library {
        if let Ok(items) = host.list_library_items_by_tmdb("movie", &snap.tmdb_id) {
            for it in items {
                for src in it.effective_sources() {
                    if src.file_size > 0.0 {
                        snap.movie_sizes_gb.push(src.file_size / 1073741824.0);
                    }
                }
            }
        }
    }

    if mtype == "tv" {
        snap.local_ep_labels = snap.local_episodes.iter().map(|p| p.key()).collect();
        let mut seasons: Vec<u32> = snap
            .season_totals
            .keys()
            .copied()
            .filter(|s| !ignored_seasons.contains(s))
            .collect();
        seasons.sort_unstable();
        for s in seasons {
            let total = snap.season_totals[&s];
            for e in 1..=total {
                let pair = EpisodePair { season: s, episode: e };
                if !snap.local_episodes.contains(&pair) {
                    snap.missing_ep_labels.push(pair.key());
                    snap.missing_episodes.push(pair);
                }
            }
        }
    }
    snap
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AggregateRequest {
    pub query: String,
    #[serde(deserialize_with = "crate::models::flex_id")]
    pub tmdb_id: String,
    pub media_type: String,
    pub title: String,
    pub season: u32,
    pub episode: u32,
    pub force_refresh: bool,
    pub live_tg: bool,
    pub include_hidden: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AggregateResponse {
    pub items: Vec<ResourceItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library: Option<LibrarySnapshot>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

pub fn aggregate_resources(
    mc: Option<&HostClient>,
    db: &Db,
    cfg: &mut AppConfig,
    req: &AggregateRequest,
) -> AggregateResponse {
    let proxy_url = effective_proxy(mc, cfg);
    let mut mtype = if matches!(req.media_type.trim().to_ascii_lowercase().as_str(), "tv" | "series")
        || req.season > 0
        || req.episode > 0
    {
        "tv".to_string()
    } else {
        "movie".to_string()
    };

    let raw_q = req.query.trim();
    if is_direct_resource_input(raw_q) {
        let item = build_direct_input_resource(raw_q, &req.tmdb_id, &mtype, &req.title);
        let ranked = rules::rank_and_dedupe(vec![item], &cfg.rules, &req.title, "");
        return AggregateResponse {
            items: ranked,
            library: None,
            errors: Vec::new(),
        };
    }

    let mut tmdb_id = req.tmdb_id.trim().to_string();
    let mut title = if !req.title.trim().is_empty() {
        req.title.trim().to_string()
    } else {
        raw_q.to_string()
    };

    if tmdb_id.is_empty() && !title.is_empty() {
        if let Some(host) = mc {
            if let Ok(resolved) = host.resolve_tmdb(&title, &req.media_type, 0) {
                if !resolved.effective_id().is_empty() {
                    tmdb_id = resolved.effective_id().to_string();
                    if req.media_type.is_empty() || req.media_type == "all" {
                        mtype = resolved.effective_type().to_string();
                    }
                    if req.title.trim().is_empty() && !resolved.effective_title().is_empty() {
                        title = resolved.effective_title().to_string();
                    }
                }
            }
        }
    }

    let snap = build_library_snapshot(mc, cfg, &tmdb_id, &mtype, &title);
    if title.is_empty() && !snap.title.is_empty() {
        title = snap.title.clone();
    }

    // Ensure OAuth token before concurrent fan-out
    let _ = providers::ensure_hdhive_token(cfg, &proxy_url);

    let mut combined = search_shield_index(db, &title, &tmdb_id, &mtype);
    let mut errs = Vec::new();

    // 1. Telegram
    let tg_items = providers::search_telegram_scoped(
        db,
        cfg,
        &title,
        &tmdb_id,
        &mtype,
        req.live_tg,
        req.include_hidden,
        &proxy_url,
    );
    combined.extend(tg_items);

    // 2. HDHive & AYClub
    if !tmdb_id.is_empty() {
        if cfg.hdhive.enabled && !cfg.hdhive.api_key.trim().is_empty() {
            match providers::search_hdhive(
                db,
                cfg,
                &tmdb_id,
                &mtype,
                &title,
                &snap.season_totals,
                req.force_refresh,
                &proxy_url,
            ) {
                Ok(items) => combined.extend(items),
                Err(e) => errs.push(format!("HDHive/中转: {}", e)),
            }
        }
        if cfg.ayclub.enabled && !cfg.ayclub.nf_token.trim().is_empty() {
            match providers::search_ayclub(
                db,
                cfg,
                &tmdb_id,
                &mtype,
                &snap.season_totals,
                req.force_refresh,
                &proxy_url,
            ) {
                Ok(items) => combined.extend(items),
                Err(e) => errs.push(format!("AYClub: {}", e)),
            }
        }
    }

    let reviewing_set: HashSet<String> = db
        .list_reviewing()
        .into_iter()
        .map(|r| r.clean_slug)
        .collect();
    let missing_set: HashSet<EpisodePair> = snap.missing_episodes.iter().copied().collect();

    let mut filtered = Vec::with_capacity(combined.len());
    for mut it in combined {
        if it.clean_slug.is_empty() {
            it.clean_slug = rules::clean_slug(&it.slug);
        }
        if reviewing_set.contains(&it.clean_slug) {
            it.is_reviewing = true;
        }
        if (!tmdb_id.is_empty() || !req.media_type.trim().is_empty())
            && !is_candidate_compatible_with_target(
                &it,
                &mtype,
                &title,
                &tmdb_id,
                &snap.season_totals,
                0,
                false,
            )
        {
            continue;
        }
        enrich_shield_and_library_flags(&mut it, &snap, &missing_set);

        if mtype == "tv" && (req.season > 0 || req.episode > 0) && !it.parsed_episodes.is_empty() {
            let pairs = rules::extract_episode_pairs(&it.parsed_episodes, req.season.max(1));
            if !pairs.is_empty() {
                let matched = pairs.iter().any(|p| {
                    (req.season == 0 || p.season == req.season)
                        && (req.episode == 0 || p.episode == req.episode)
                });
                if !matched {
                    continue;
                }
            }
        }
        filtered.push(it);
    }

    let ranked = rules::rank_and_dedupe(filtered, &cfg.rules, &title, "");
    AggregateResponse {
        items: ranked,
        library: Some(snap),
        errors: errs,
    }
}

fn enrich_shield_and_library_flags(
    it: &mut ResourceItem,
    snap: &LibrarySnapshot,
    missing_set: &HashSet<EpisodePair>,
) {
    let mut size_gb = rules::parse_size_to_gb(&it.share_size);
    if size_gb <= 0.0 && it.size_bytes > 0 {
        size_gb = (it.size_bytes as f64) / 1073741824.0;
    }
    if snap.media_type == "movie" {
        if snap.in_library {
            it.local_in_library = true;
        }
        let mut best_diff = f64::INFINITY;
        for &local_gb in &snap.movie_sizes_gb {
            if let Some(diff) = rules::shield_size_match(size_gb, local_gb) {
                if diff < best_diff {
                    it.shield_matched = true;
                    best_diff = diff;
                }
            }
        }
        if best_diff.is_finite() {
            it.shield_diff_gb = best_diff;
        }
        return;
    }

    let pairs = rules::extract_episode_pairs(
        &format!("{} {} {}", it.parsed_episodes, it.title, it.remark),
        1,
    );
    if pairs.is_empty() {
        return;
    }
    let mut all_local = true;
    let mut hits = Vec::new();
    for p in pairs {
        if !snap.local_episodes.contains(&p) {
            all_local = false;
        }
        if (missing_set.is_empty() && !snap.local_episodes.contains(&p)) || missing_set.contains(&p)
        {
            hits.push(p.key());
        }
    }
    if all_local && !snap.local_episodes.is_empty() {
        it.local_in_library = true;
    }
    if hits.len() > 8 {
        let last = hits.last().cloned().unwrap_or_default();
        it.missing_eps_hit = vec![
            hits[0].clone(),
            hits[1].clone(),
            hits[2].clone(),
            hits[3].clone(),
            "...".into(),
            last,
        ];
    } else {
        it.missing_eps_hit = hits;
    }
}

fn is_direct_resource_input(q: &str) -> bool {
    let low = q.trim().to_lowercase();
    if low.starts_with("hdhive://")
        || low.starts_with("movie:hdhive://")
        || low.starts_with("tv:hdhive://")
        || low.starts_with("relay://")
        || low.starts_with("radar://")
        || low.starts_with("nextfind://")
        || low.starts_with("ed2k://")
        || low.starts_with("magnet:?")
    {
        return true;
    }
    let (sc, _) = rules::parse_115_share(q, "");
    !sc.is_empty() && low.contains("115")
}

fn build_direct_input_resource(raw: &str, tmdb_id: &str, mtype: &str, title: &str) -> ResourceItem {
    let low = raw.trim().to_lowercase();
    let mut pan_type = "115".to_string();
    let mut sc = String::new();
    let mut rc = String::new();
    let mut slug = raw.trim().to_string();
    let mut size_bytes = 0i64;
    let mut display_title = title.trim().to_string();

    if low.starts_with("ed2k://") {
        pan_type = "ed2k".into();
        for lk in rules::extract_all_resource_links(raw) {
            if lk.pan_type == "ed2k" {
                size_bytes += lk.size_bytes;
                if display_title.is_empty() && !lk.file_name.is_empty() {
                    display_title = lk.file_name;
                }
            }
        }
    } else if low.starts_with("magnet:?") {
        pan_type = "magnet".into();
        if let Some(first) = rules::extract_all_resource_links(raw).into_iter().next() {
            if display_title.is_empty() && !first.file_name.is_empty() {
                display_title = first.file_name;
            }
        }
    } else if low.starts_with("hdhive://")
        || low.starts_with("movie:hdhive://")
        || low.starts_with("tv:hdhive://")
        || low.starts_with("relay://")
        || low.starts_with("radar://")
    {
        pan_type = "radar".into();
    } else if low.starts_with("nextfind://") {
        pan_type = "shield".into();
    } else {
        let (s, r) = rules::parse_115_share(raw, "");
        sc = s;
        rc = r;
        slug = rules::format_115_share_url(&sc, &rc);
    }

    let eps = rules::parse_shallow_episodes(&format!("{} {}", display_title, raw), "tv", None);
    let final_mtype = if !eps.is_empty() && (mtype.is_empty() || mtype == "movie") {
        "tv"
    } else if mtype.is_empty() {
        "movie"
    } else {
        mtype
    };
    let attrs = rules::parse_movie_attributes(&format!("{} {}", display_title, raw));
    let is_unlocked = pan_type != "radar";

    ResourceItem {
        id: "manual-direct".into(),
        clean_slug: rules::clean_slug(&slug),
        slug,
        title: if display_title.is_empty() {
            "直链资源预览".into()
        } else {
            display_title
        },
        tmdb_id: tmdb_id.to_string(),
        media_type: final_mtype.into(),
        source_type: "manual".into(),
        channel_name: "手动直链".into(),
        pan_type,
        share_code: sc,
        receive_code: rc,
        share_size: rules::format_bytes(size_bytes),
        size_bytes,
        parsed_episodes: eps,
        movie_attributes: attrs,
        is_unlocked,
        ..Default::default()
    }
}

/* ==================== 115 Tree Inspect & Transfer ==================== */

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct InspectRequest {
    pub slug: String,
    pub title: String,
    #[serde(deserialize_with = "crate::models::flex_id")]
    pub tmdb_id: String,
    pub media_type: String,
    pub source_type: String,
    pub unlock_points: i32,
    pub allow_unlock: bool,
    pub force_refresh: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct InspectResponse {
    pub slug: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub share_code: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub receive_code: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub share_url: String,
    pub pan_type: String,
    pub precise_title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub parsed_episodes: String,
    pub movie_attributes: MovieAttributes,
    pub files: Vec<ShareFileNode>,
    #[serde(default)]
    pub is_reviewing: bool,
    pub missing_count: usize,
}

pub fn inspect_resource(
    mc: Option<&HostClient>,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
    req: &InspectRequest,
) -> Result<InspectResponse> {
    let mut slug = req.slug.trim().to_string();
    ensure!(!slug.is_empty(), "资源标识 slug 不能为空");
    let mtype = if matches!(req.media_type.trim().to_ascii_lowercase().as_str(), "tv" | "series") {
        "tv"
    } else {
        "movie"
    };
    let snap = build_library_snapshot(mc, cfg, &req.tmdb_id, mtype, &req.title);
    let low = slug.to_lowercase();

    // 1. ED2K bundle
    if low.contains("ed2k://") {
        let links = rules::extract_all_resource_links(&slug);
        let mut nodes = Vec::new();
        let mut all_pairs = Vec::new();
        let mut names = Vec::new();
        let mut missing_count = 0usize;
        for (idx, lk) in links.into_iter().enumerate() {
            if lk.pan_type != "ed2k" {
                continue;
            }
            let pairs = rules::extract_episode_pairs(&lk.file_name, 1);
            let ep_label = rules::format_episode_pairs(&pairs);
            let is_missing = is_node_missing_in_library(mtype, &pairs, &snap);
            if is_missing {
                missing_count += 1;
            }
            all_pairs.extend(pairs);
            names.push(lk.file_name.clone());
            nodes.push(ShareFileNode {
                id: idx.to_string(),
                name: lk.file_name.clone(),
                size: lk.size_bytes,
                is_video: rules::is_video_file(&lk.file_name),
                is_subtitle: rules::is_subtitle_file(&lk.file_name),
                episode_label: ep_label,
                is_missing_in_db: is_missing,
                direct_link: lk.url,
                ..Default::default()
            });
        }
        return Ok(InspectResponse {
            slug,
            pan_type: "ed2k".into(),
            precise_title: build_precise_title(&req.title, &req.tmdb_id),
            parsed_episodes: rules::format_episode_pairs(&all_pairs),
            movie_attributes: rules::parse_movie_attributes(&names.join(" ")),
            files: nodes,
            missing_count,
            ..Default::default()
        });
    }

    // 2. Magnet
    if low.starts_with("magnet:?") {
        let mut dn = req.title.clone();
        if let Ok(u) = Url::parse(&slug) {
            for (k, v) in u.query_pairs() {
                if k == "dn" && !v.is_empty() {
                    dn = v.into_owned();
                    break;
                }
            }
        }
        return Ok(InspectResponse {
            slug: slug.clone(),
            pan_type: "magnet".into(),
            precise_title: build_precise_title(&dn, &req.tmdb_id),
            movie_attributes: rules::parse_movie_attributes(&dn),
            files: vec![ShareFileNode {
                id: "0".into(),
                name: if dn.is_empty() { "Magnet 离线任务".into() } else { dn },
                is_video: true,
                is_missing_in_db: !snap.in_library,
                direct_link: slug,
                ..Default::default()
            }],
            missing_count: 1,
            ..Default::default()
        });
    }

    // 3. Private Shield index
    if low.starts_with("nextfind://") {
        let (sc, mut rc, parent_id) = {
            let rest = &slug["nextfind://".len()..];
            let parts: Vec<&str> = rest.split('/').collect();
            let s = parts.first().copied().unwrap_or("").to_string();
            let (r, p) = if parts.len() >= 3 {
                (parts[1].to_string(), parts[2].to_string())
            } else if parts.len() == 2 {
                (String::new(), parts[1].to_string())
            } else {
                (String::new(), "0".to_string())
            };
            (s, r, p)
        };
        let indexed = db.query_shield_folder(&sc, &parent_id);
        if !indexed.is_empty() {
            let mut nodes = Vec::new();
            let mut all_pairs = Vec::new();
            let mut names = Vec::new();
            let mut missing_count = 0usize;
            for f in indexed {
                if rc.is_empty() && !f.receive_code.is_empty() {
                    rc = f.receive_code.clone();
                }
                let is_vid = rules::is_video_file(&f.file_name);
                let pairs = extract_file_node_episodes(&f.rel_path, &f.file_name);
                if is_vid {
                    all_pairs.extend(pairs.iter().copied());
                    names.push(f.file_name.clone());
                }
                let is_missing = is_vid && is_node_missing_in_library(mtype, &pairs, &snap);
                if is_missing {
                    missing_count += 1;
                }
                nodes.push(ShareFileNode {
                    id: f.file_id,
                    parent_id: f.parent_id,
                    name: f.file_name.clone(),
                    path: f.rel_path,
                    size: f.file_size,
                    sha1: f.sha1,
                    is_video: is_vid,
                    is_subtitle: rules::is_subtitle_file(&f.file_name),
                    episode_label: rules::format_episode_pairs(&pairs),
                    is_missing_in_db: is_missing,
                    ..Default::default()
                });
            }
            let share_url = rules::format_115_share_url(&sc, &rc);
            return Ok(InspectResponse {
                slug,
                share_code: sc,
                receive_code: rc,
                share_url,
                pan_type: "shield".into(),
                precise_title: build_precise_title(&req.title, &req.tmdb_id),
                parsed_episodes: rules::format_episode_pairs(&all_pairs),
                movie_attributes: rules::parse_movie_attributes(&names.join(" ")),
                files: nodes,
                missing_count,
                ..Default::default()
            });
        }
        slug = rules::format_115_share_url(&sc, &rc);
    }

    // 4. HDHive / Relay unlock or 115 share snap
    let proxy_url = effective_proxy(mc, cfg);
    let (share_code, receive_code) = if low.starts_with("hdhive://")
        || low.starts_with("movie:hdhive://")
        || low.starts_with("tv:hdhive://")
        || low.starts_with("relay://")
        || low.starts_with("radar://")
        || (req.source_type.eq_ignore_ascii_case("hdhive") && !low.contains("115"))
    {
        let allow_spend = req.allow_unlock || settings.auto_unlock_hdhive;
        let (sc, rc, _) = providers::unlock_hdhive(
            db,
            cfg,
            &slug,
            mtype,
            req.unlock_points,
            allow_spend,
            &proxy_url,
        )?;
        (sc, rc)
    } else {
        rules::parse_115_share(&slug, "")
    };
    ensure!(!share_code.is_empty(), "无法解析 115 分享码: {}", slug);

    let share_url = rules::format_115_share_url(&share_code, &receive_code);
    let cache_key = format!("115:{}", share_code.to_lowercase());
    let now_ts = chrono::Utc::now().timestamp();

    if !req.force_refresh {
        if let Some(cached) = db.get_probe_cache(&cache_key) {
            if !cached.files.is_empty() && now_ts - cached.updated_ts < 24 * 3600 {
                let (files, missing_count) = annotate_missing_on_nodes(&cached.files, mtype, &snap);
                let rc_eff = if !cached.receive_code.is_empty() {
                    cached.receive_code
                } else {
                    receive_code
                };
                let t_eff = if !req.title.trim().is_empty() {
                    &req.title
                } else {
                    &cached.title
                };
                return Ok(InspectResponse {
                    slug,
                    share_code,
                    receive_code: rc_eff,
                    share_url,
                    pan_type: "115".into(),
                    precise_title: build_precise_title(t_eff, &req.tmdb_id),
                    parsed_episodes: cached.parsed_episodes,
                    movie_attributes: cached.movie_attributes,
                    files,
                    missing_count,
                    ..Default::default()
                });
            }
        }
    }

    let host = mc.ok_or_else(|| anyhow!("缺少 Mediary 宿主 API 运行环境，无法预览 115 分享树"))?;
    let root_raw = match host.open115_share_snap(&share_url, &receive_code, "0", 0, 500) {
        Ok(v) => v,
        Err(e) => {
            if is_115_reviewing_text(&e.to_string()) {
                return Ok(InspectResponse {
                    slug,
                    share_code,
                    receive_code,
                    share_url,
                    pan_type: "115".into(),
                    is_reviewing: true,
                    ..Default::default()
                });
            }
            return Err(e);
        }
    };

    let (share_title, root_nodes, reviewing, err_msg) = parse_snap_response(&root_raw, "0", "", 0);
    if reviewing {
        return Ok(InspectResponse {
            slug,
            share_code,
            receive_code,
            share_url,
            pan_type: "115".into(),
            is_reviewing: true,
            ..Default::default()
        });
    }
    ensure!(err_msg.is_empty(), "115 分享预览失败: {}", err_msg);

    // Prioritize matching archive folder when multi-country/language folders exist
    let dir_names: Vec<String> = root_nodes
        .iter()
        .filter(|n| n.is_dir)
        .map(|n| n.name.clone())
        .collect();
    let preferred_dir = evaluate_archive_tree(&dir_names, &snap.origin_country, &snap.original_language);

    let mut ordered_root = root_nodes;
    if !preferred_dir.is_empty() {
        ordered_root.sort_by_key(|n| if n.name == preferred_dir { 0 } else { 1 });
    }

    let mut all_nodes = Vec::new();
    let mut sub_expanded = 0usize;
    for n in ordered_root {
        let is_dir = n.is_dir;
        let nid = n.id.clone();
        let nname = n.name.clone();
        all_nodes.push(n);
        if is_dir && !nid.is_empty() && sub_expanded < 8 {
            sub_expanded += 1;
            std::thread::sleep(Duration::from_millis(380));
            if let Ok(sub_raw) = host.open115_share_snap(&share_url, &receive_code, &nid, 0, 500) {
                let (_, children, _, _) = parse_snap_response(&sub_raw, &nid, &nname, 1);
                for ch in children {
                    let ch_dir = ch.is_dir;
                    let ch_id = ch.id.clone();
                    let ch_path = ch.path.clone();
                    all_nodes.push(ch);
                    if ch_dir && !ch_id.is_empty() && sub_expanded < 8 {
                        sub_expanded += 1;
                        std::thread::sleep(Duration::from_millis(380));
                        if let Ok(sub2) =
                            host.open115_share_snap(&share_url, &receive_code, &ch_id, 0, 500)
                        {
                            let (_, grand, _, _) = parse_snap_response(&sub2, &ch_id, &ch_path, 2);
                            all_nodes.extend(grand);
                        }
                    }
                }
            }
        }
    }

    let mut video_names = Vec::new();
    let mut all_pairs = Vec::new();
    for n in &all_nodes {
        if n.is_video {
            video_names.push(n.name.clone());
            all_pairs.extend(extract_file_node_episodes(&n.path, &n.name));
        }
    }
    let eps_str = rules::format_episode_pairs(&all_pairs);
    let attrs = rules::parse_movie_attributes(&format!("{} {}", share_title, video_names.join(" ")));
    let eff_title = if !share_title.is_empty() {
        share_title
    } else {
        req.title.clone()
    };

    let entry = ProbeCacheEntry {
        clean_slug: cache_key.clone(),
        share_code: share_code.clone(),
        receive_code: receive_code.clone(),
        share_url: share_url.clone(),
        title: eff_title.clone(),
        parsed_episodes: eps_str.clone(),
        movie_attributes: attrs.clone(),
        files: all_nodes.clone(),
        is_unlocked: true,
        is_valid: true,
        updated_ts: now_ts,
        ..Default::default()
    };
    let _ = db.put_probe_cache(&entry);
    let orig_key = rules::clean_slug(&slug);
    if !orig_key.is_empty() && orig_key != cache_key {
        let mut orig_entry = entry.clone();
        orig_entry.clean_slug = orig_key;
        let _ = db.put_probe_cache(&orig_entry);
    }

    let (annotated, missing_count) = annotate_missing_on_nodes(&all_nodes, mtype, &snap);
    let disp_title = if !req.title.trim().is_empty() {
        &req.title
    } else {
        &eff_title
    };
    Ok(InspectResponse {
        slug,
        share_code,
        receive_code,
        share_url,
        pan_type: "115".into(),
        precise_title: build_precise_title(disp_title, &req.tmdb_id),
        parsed_episodes: eps_str,
        movie_attributes: attrs,
        files: annotated,
        missing_count,
        ..Default::default()
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TransferRequest {
    pub slug: String,
    pub title: String,
    #[serde(deserialize_with = "crate::models::flex_id")]
    pub tmdb_id: String,
    pub media_type: String,
    pub season: u32,
    pub source_type: String,
    pub channel_name: String,
    pub unlock_points: i32,
    pub allow_unlock: bool,
    #[serde(deserialize_with = "crate::models::null_default")]
    pub file_ids: Vec<String>,
    pub only_missing_episodes: bool,
    pub target_cid: String,
    pub trigger_reason: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TransferResult {
    pub status: String,
    pub notice: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub share_url: String,
    pub submitted_count: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub episodes: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn resolve_effective_target_cid(
    mc: &HostClient,
    cfg: &AppConfig,
    explicit_cid: &str,
    for_offline: bool,
) -> String {
    if crate::host::is_valid_115_target_cid(explicit_cid) {
        return explicit_cid.trim().to_string();
    }
    if crate::host::is_valid_115_target_cid(&cfg.rules.default_target_cid) {
        return cfg.rules.default_target_cid.trim().to_string();
    }
    if !cfg.rules.prefer_flowlink {
        let hs = mc.get_host_status();
        let candidates = if for_offline {
            [
                hs.transfer_source_cid.as_str(),
                hs.offline_folder_cid.as_str(),
                hs.target_cid.as_str(),
            ]
        } else {
            [
                hs.transfer_source_cid.as_str(),
                hs.target_cid.as_str(),
                hs.offline_folder_cid.as_str(),
            ]
        };
        for c in candidates {
            if crate::host::is_valid_115_target_cid(c) {
                return c.trim().to_string();
            }
        }
    }
    String::new()
}

fn format_target_dir_label(mc: &HostClient, cfg: &AppConfig, cid: &str) -> String {
    let c = cid.trim();
    if c.is_empty() {
        return String::new();
    }
    if cfg.rules.default_target_cid.trim() == c && !cfg.rules.default_target_name.trim().is_empty()
    {
        return format!("「{}」(CID:{})", cfg.rules.default_target_name.trim(), c);
    }
    let hs = mc.get_host_status();
    for (known_cid, known_name) in [
        (hs.transfer_source_cid.as_str(), hs.transfer_source_folder.as_str()),
        (hs.target_cid.as_str(), hs.target_folder.as_str()),
        (hs.offline_folder_cid.as_str(), hs.offline_folder.as_str()),
    ] {
        if known_cid.trim() == c && !known_name.trim().is_empty() {
            return format!("「{}」(CID:{})", known_name.trim(), c);
        }
    }
    format!("(CID:{})", c)
}

pub fn transfer_resource(
    mc: &HostClient,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
    req: &TransferRequest,
) -> Result<TransferResult> {
    let slug = req.slug.trim();
    ensure!(!slug.is_empty(), "转存链接不能为空");
    let reason = if req.trigger_reason.trim().is_empty() {
        "手动转存"
    } else {
        req.trigger_reason.trim()
    };
    let explicit_target_override = crate::host::is_valid_115_target_cid(&req.target_cid);
    let target_cid = resolve_effective_target_cid(mc, cfg, &req.target_cid, false);

    let low = slug.to_lowercase();

    // 1. Magnet (`POST /api/plugin/open115/offline` in staging/explicit CID mode, else FlowLink `/api/link/submit`)
    if low.starts_with("magnet:?") {
        let offline_cid = resolve_effective_target_cid(mc, cfg, &req.target_cid, true);
        let msg = if (!cfg.rules.prefer_flowlink || explicit_target_override)
            && !offline_cid.is_empty()
        {
            let (_, warns) = mc.open115_offline(&[slug.to_string()], &offline_cid)?;
            if cfg.rules.prefer_flowlink {
                mc.trigger_flowlink_move_all();
            }
            let dir_lbl = format_target_dir_label(mc, cfg, &offline_cid);
            if warns.is_empty() {
                format!("已通过 115 离线下载提交至目录 {}", dir_lbl)
            } else {
                format!(
                    "已提交至 115 离线目录 {}（提示: {}）",
                    dir_lbl,
                    warns.join("；")
                )
            }
        } else {
            mc.submit_link(slug)?
        };
        let magnet_dn = rules::extract_all_resource_links(slug)
            .into_iter()
            .find(|l| !l.file_name.trim().is_empty())
            .map(|l| l.file_name)
            .unwrap_or_default();
        let raw_t = if !req.title.trim().is_empty() && !rules::is_suspect_banner_title(&req.title) {
            req.title.as_str()
        } else if !magnet_dn.is_empty() {
            magnet_dn.as_str()
        } else if !req.title.trim().is_empty() {
            req.title.as_str()
        } else {
            "磁力任务"
        };
        let clean_t = rules::clean_display_title(raw_t);
        let mut eff_req = req.clone();
        if eff_req.tmdb_id.trim().is_empty() {
            let tid = rules::extract_tmdb_id(raw_t);
            if !tid.is_empty() {
                eff_req.tmdb_id = tid;
            }
        }
        let target_short = if (!cfg.rules.prefer_flowlink || explicit_target_override)
            && !offline_cid.is_empty()
        {
            format!("离线至 {}", format_target_dir_label(mc, cfg, &offline_cid))
        } else {
            "FlowLink 离线整理".to_string()
        };
        let mag_eps = rules::parse_shallow_episodes(raw_t, &eff_req.media_type, None);
        mark_transferred(db, cfg, &eff_req, slug, &clean_t, "magnet", &mag_eps, reason, &msg);
        notify_transfer_templated(
            mc,
            settings,
            cfg,
            &TransferNotifyInput {
                req: &eff_req,
                raw_title: raw_t,
                resource_title: if !magnet_dn.is_empty() { &magnet_dn } else { &clean_t },
                source_type: if req.source_type.is_empty() { "magnet" } else { &req.source_type },
                season_episode: &mag_eps,
                download_action: "离线下载",
                size_str: "",
                target_hint: &target_short,
            },
        );
        return Ok(TransferResult {
            status: "success".into(),
            notice: msg,
            submitted_count: 1,
            ..Default::default()
        });
    }

    // 2. ED2K (`POST /api/plugin/open115/offline` batch ≤50 in staging/explicit CID mode, else FlowLink `/api/link/submit`)
    if low.contains("ed2k://") {
        let inspected = inspect_resource(
            Some(mc),
            db,
            settings,
            cfg,
            &InspectRequest {
                slug: slug.to_string(),
                title: req.title.clone(),
                tmdb_id: req.tmdb_id.clone(),
                media_type: req.media_type.clone(),
                ..Default::default()
            },
        )?;
        let want_ids: HashSet<String> = req
            .file_ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut to_submit = Vec::new();
        let mut to_submit_ids = Vec::new();
        for f in &inspected.files {
            if f.direct_link.is_empty() {
                continue;
            }
            if !want_ids.is_empty() && !want_ids.contains(&f.id) {
                continue;
            }
            if want_ids.is_empty() && req.only_missing_episodes {
                if !f.is_missing_in_db {
                    continue;
                }
                if req.season > 0 && !f.episode_label.is_empty() {
                    let pairs = rules::extract_episode_pairs(&f.episode_label, 1);
                    if !pairs.is_empty() && !pairs.iter().any(|p| p.season == req.season) {
                        continue;
                    }
                }
            }
            to_submit.push(f.direct_link.clone());
            if !f.id.is_empty() {
                to_submit_ids.push(f.id.clone());
            }
        }
        if to_submit.is_empty() && req.only_missing_episodes {
            return Ok(TransferResult {
                status: "skipped".into(),
                notice: "本地媒体库已包含全部剧集，无需重复提交 ED2K".into(),
                ..Default::default()
            });
        }
        if to_submit.is_empty() {
            for f in &inspected.files {
                if !f.direct_link.is_empty() {
                    to_submit.push(f.direct_link.clone());
                    if !f.id.is_empty() {
                        to_submit_ids.push(f.id.clone());
                    }
                }
            }
        }

        let offline_cid = resolve_effective_target_cid(mc, cfg, &req.target_cid, true);
        let (submitted, warnings, notice) = if (!cfg.rules.prefer_flowlink || explicit_target_override)
            && !offline_cid.is_empty()
        {
            let (ok_cnt, warns) = mc.open115_offline(&to_submit, &offline_cid)?;
            if cfg.rules.prefer_flowlink {
                mc.trigger_flowlink_move_all();
            }
            let dir_lbl = format_target_dir_label(mc, cfg, &offline_cid);
            let msg = if cfg.rules.prefer_flowlink {
                format!(
                    "已批量提交 {} 个 ED2K 离线任务至 115 目录 {} 并触发 FlowLink 整理",
                    ok_cnt, dir_lbl
                )
            } else {
                format!(
                    "已批量提交 {} 个 ED2K 离线任务至 115 待整理中转目录 {}，等待下游守门整理",
                    ok_cnt, dir_lbl
                )
            };
            (ok_cnt, warns, msg)
        } else {
            let mut sub_cnt = 0usize;
            let mut warns = Vec::new();
            let mut last_msg = String::new();
            for ed in &to_submit {
                match mc.submit_link(ed) {
                    Ok(m) => {
                        last_msg = m;
                        sub_cnt += 1;
                    }
                    Err(e) => warns.push(e.to_string()),
                }
            }
            ensure!(sub_cnt > 0, "提交 ED2K 失败: {}", warns.join("；"));
            let msg = format!(
                "已提交 {} 个 ED2K 文件至 115 转存目录 ({})",
                sub_cnt, last_msg
            );
            (sub_cnt, warns, msg)
        };

        let (best_ed2k_title, best_ed2k_tid) =
            infer_share_identity(&req.title, &req.tmdb_id, &inspected);
        let mut eff_req = req.clone();
        if !best_ed2k_tid.is_empty() {
            eff_req.tmdb_id = best_ed2k_tid;
        }
        let clean_t = if !best_ed2k_title.is_empty() {
            best_ed2k_title
        } else if !req.title.is_empty() {
            rules::clean_display_title(&req.title)
        } else {
            "ED2K 资源".to_string()
        };
        let ed2k_bytes: i64 = inspected
            .files
            .iter()
            .filter(|f| to_submit.contains(&f.direct_link))
            .map(|f| f.size.max(0))
            .sum();
        let ed2k_size = if ed2k_bytes > 0 {
            rules::format_bytes(ed2k_bytes)
        } else {
            String::new()
        };
        let ed2k_sample = inspected
            .files
            .iter()
            .find(|f| to_submit.contains(&f.direct_link) && !f.name.trim().is_empty())
            .map(|f| {
                if submitted > 1 {
                    format!("{} 等{}个文件", f.name.trim(), submitted)
                } else {
                    f.name.trim().to_string()
                }
            })
            .unwrap_or_else(|| clean_t.clone());
        let ed2k_target_hint = if (!cfg.rules.prefer_flowlink || explicit_target_override)
            && !offline_cid.is_empty()
        {
            format!(
                "{}个文件 → {}",
                submitted,
                short_target_dir_name(mc, cfg, &offline_cid)
            )
        } else {
            format!("{}个文件 → FlowLink", submitted)
        };
        let (ed2k_episodes, _, _) = summarize_transferred_files(
            &inspected.files,
            &to_submit_ids,
            &inspected.parsed_episodes,
            &eff_req.media_type,
        );
        mark_transferred(
            db,
            cfg,
            &eff_req,
            slug,
            &clean_t,
            "ed2k",
            &ed2k_episodes,
            reason,
            &notice,
        );
        notify_transfer_templated(
            mc,
            settings,
            cfg,
            &TransferNotifyInput {
                req: &eff_req,
                raw_title: &clean_t,
                resource_title: &ed2k_sample,
                source_type: if req.source_type.is_empty() { "ed2k" } else { &req.source_type },
                season_episode: &ed2k_episodes,
                download_action: "离线下载",
                size_str: &ed2k_size,
                target_hint: &ed2k_target_hint,
            },
        );
        return Ok(TransferResult {
            status: "success".into(),
            notice,
            submitted_count: submitted,
            episodes: ed2k_episodes,
            warnings,
            ..Default::default()
        });
    }

    // 3. 115 Share / HDHive / Shield
    let inspected = inspect_resource(
        Some(mc),
        db,
        settings,
        cfg,
        &InspectRequest {
            slug: slug.to_string(),
            title: req.title.clone(),
            tmdb_id: req.tmdb_id.clone(),
            media_type: req.media_type.clone(),
            source_type: req.source_type.clone(),
            unlock_points: req.unlock_points,
            allow_unlock: req.allow_unlock,
            ..Default::default()
        },
    )?;

    if inspected.is_reviewing {
        enqueue_reviewing(db, req, &inspected.share_code, &inspected.receive_code, &target_cid);
        let notice = "该 115 分享当前处于官方「审核中」，已自动加入待审队列，后续巡检将自动重试转存".to_string();
        let _ = db.append_activity(crate::models::ActivityRecord {
            record_type: "transfer".into(),
            title: if req.title.is_empty() {
                inspected.share_code.clone()
            } else {
                req.title.clone()
            },
            source: if req.source_type.is_empty() {
                "115".into()
            } else {
                req.source_type.clone()
            },
            channel_name: req.channel_name.clone(),
            slug: if !inspected.share_url.is_empty() {
                inspected.share_url.clone()
            } else {
                slug.to_string()
            },
            clean_slug: format!("115:{}", inspected.share_code.to_lowercase()),
            tmdb_id: req.tmdb_id.clone(),
            media_type: req.media_type.clone(),
            action: "暂存待审".into(),
            status: "reviewing".into(),
            detail: notice.clone(),
            ..Default::default()
        });
        return Ok(TransferResult {
            status: "reviewing".into(),
            notice,
            share_url: inspected.share_url,
            ..Default::default()
        });
    }

    let mut selected_ids: Vec<String> = req
        .file_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if selected_ids.is_empty() && req.only_missing_episodes && req.media_type.eq_ignore_ascii_case("tv")
    {
        let tid_trim = req.tmdb_id.trim();
        let mut missing_ep_pairs: HashSet<EpisodePair> = HashSet::new();
        let mut selected_video_stems = Vec::new();
        for f in &inspected.files {
            if f.is_video && f.is_missing_in_db && !f.id.is_empty() {
                let pairs = if !f.episode_label.is_empty() {
                    rules::extract_episode_pairs(&f.episode_label, 1)
                } else {
                    Vec::new()
                };
                if req.season > 0 && !pairs.is_empty() && !pairs.iter().any(|p| p.season == req.season)
                {
                    continue;
                }
                if !tid_trim.is_empty() && !pairs.is_empty() {
                    if pairs.iter().all(|p| {
                        db.is_in_flight(
                            &format!("tv:{}:{}", tid_trim, p.key()),
                            IN_FLIGHT_CHASE_TTL_SECS,
                        )
                    }) {
                        continue;
                    }
                }
                selected_ids.push(f.id.clone());
                for p in pairs {
                    missing_ep_pairs.insert(p);
                }
                let stem = file_base_stem_lower(&f.name);
                if !stem.is_empty() {
                    selected_video_stems.push((f.parent_id.clone(), stem));
                }
            }
        }
        if !selected_ids.is_empty() {
            for f in &inspected.files {
                if f.is_subtitle && !f.id.is_empty() {
                    let sub_pairs = if !f.episode_label.is_empty() {
                        rules::extract_episode_pairs(&f.episode_label, 1)
                    } else {
                        Vec::new()
                    };
                    let matched_by_ep = !sub_pairs.is_empty()
                        && sub_pairs.iter().any(|p| missing_ep_pairs.contains(p));
                    let matched_by_stem = sub_pairs.is_empty()
                        && subtitle_matches_video_stems(f, &selected_video_stems);
                    if matched_by_ep || matched_by_stem {
                        selected_ids.push(f.id.clone());
                    }
                }
            }
        } else {
            let skip_msg = if inspected.files.is_empty() {
                "分享目录为空或未解析到视频文件，已跳过转存"
            } else {
                "对比媒体库及在途队列后未发现新缺集，已跳过重复转存"
            };
            return Ok(TransferResult {
                status: "skipped".into(),
                notice: skip_msg.into(),
                ..Default::default()
            });
        }
    }

    let (inferred_title, inferred_tid) =
        infer_share_identity(&req.title, &req.tmdb_id, &inspected);
    let mut eff_req = req.clone();
    if !inferred_tid.is_empty() {
        eff_req.tmdb_id = inferred_tid;
    }
    let clean_disp_title = if !inferred_title.is_empty() {
        inferred_title
    } else if !req.title.is_empty() {
        rules::clean_display_title(&req.title)
    } else {
        rules::clean_display_title(&inspected.precise_title)
    };
    if !clean_disp_title.is_empty() {
        eff_req.title = clean_disp_title.clone();
    }
    let disp_title = clean_disp_title.as_str();
    let src_label = if req.source_type.is_empty() {
        "115"
    } else {
        &req.source_type
    };
    let (eff_episodes, total_share_bytes, sample_video_name) = summarize_transferred_files(
        &inspected.files,
        &selected_ids,
        &inspected.parsed_episodes,
        &eff_req.media_type,
    );
    let share_size_str = if total_share_bytes > 0 {
        rules::format_bytes(total_share_bytes)
    } else {
        String::new()
    };
    let res_share_desc = if !sample_video_name.is_empty() {
        sample_video_name
    } else {
        rules::clean_display_title(if !inspected.precise_title.is_empty() {
            &inspected.precise_title
        } else {
            disp_title
        })
    };

    // Path A: Selective receive OR staging-directory mode (PreferFlowLink == false -> pure receive into target_cid for Mediwash gatekeeping!)
    if !target_cid.is_empty() && (!selected_ids.is_empty() || !cfg.rules.prefer_flowlink) {
        if let Err(e) = mc.open115_share_receive(
            &inspected.share_url,
            &inspected.receive_code,
            &target_cid,
            &selected_ids,
        ) {
            if is_115_reviewing_text(&e.to_string()) {
                enqueue_reviewing(db, &eff_req, &inspected.share_code, &inspected.receive_code, &target_cid);
                let notice = "115 分享处于审核中，已加入待审重试队列".to_string();
                let _ = db.append_activity(crate::models::ActivityRecord {
                    record_type: "transfer".into(),
                    title: disp_title.to_string(),
                    source: src_label.to_string(),
                    channel_name: eff_req.channel_name.clone(),
                    slug: if !inspected.share_url.is_empty() {
                        inspected.share_url.clone()
                    } else {
                        slug.to_string()
                    },
                    clean_slug: rules::clean_slug(slug),
                    tmdb_id: eff_req.tmdb_id.clone(),
                    media_type: eff_req.media_type.clone(),
                    episodes: eff_episodes.clone(),
                    action: "暂存待审".into(),
                    status: "reviewing".into(),
                    detail: notice.clone(),
                    ..Default::default()
                });
                return Ok(TransferResult {
                    status: "reviewing".into(),
                    notice,
                    share_url: inspected.share_url,
                    ..Default::default()
                });
            }
            return Err(e);
        }

        let dir_lbl = format_target_dir_label(mc, cfg, &target_cid);
        let short_dir = short_target_dir_name(mc, cfg, &target_cid);
        let notice = if cfg.rules.prefer_flowlink {
            mc.trigger_flowlink_move_all();
            format!(
                "已精准选集转存 {} 个文件至 115 目录 {} 并触发 FlowLink 整理",
                selected_ids.len().max(1),
                dir_lbl
            )
        } else if !selected_ids.is_empty() {
            format!(
                "已精准选集接收 {} 个文件至 115 待整理中转目录 {}，等待下游守门整理",
                selected_ids.len(),
                dir_lbl
            )
        } else {
            format!(
                "已整包接收至 115 待整理中转目录 {}，等待下游守门整理",
                dir_lbl
            )
        };

        let eff_slug = if !inspected.share_url.is_empty() {
            inspected.share_url.as_str()
        } else {
            slug
        };
        mark_transferred(
            db,
            cfg,
            &eff_req,
            eff_slug,
            disp_title,
            src_label,
            &eff_episodes,
            reason,
            &notice,
        );
        let dl_act = if !selected_ids.is_empty() { "选集转存" } else { "开始转存" };
        let target_hint = if !selected_ids.is_empty() {
            format!("选集{}项 → {}", selected_ids.len(), short_dir)
        } else {
            format!("接收至 {}", short_dir)
        };
        notify_transfer_templated(
            mc,
            settings,
            cfg,
            &TransferNotifyInput {
                req: &eff_req,
                raw_title: disp_title,
                resource_title: &res_share_desc,
                source_type: src_label,
                season_episode: &eff_episodes,
                download_action: dl_act,
                size_str: &share_size_str,
                target_hint: &target_hint,
            },
        );
        return Ok(TransferResult {
            status: "success".into(),
            notice,
            share_url: inspected.share_url,
            submitted_count: selected_ids.len().max(1),
            episodes: eff_episodes,
            warnings: Vec::new(),
        });
    }

    // Path B: Full share link submission via Mediary /link/submit (with automatic fallback to Open115ShareReceive if target_cid is set)
    let submit_text = rules::format_115_share_url(&inspected.share_code, &inspected.receive_code);
    match mc.submit_link(&submit_text) {
        Ok(msg) => {
            mark_transferred(
                db,
                cfg,
                &eff_req,
                &submit_text,
                disp_title,
                src_label,
                &eff_episodes,
                reason,
                &msg,
            );
            notify_transfer_templated(
                mc,
                settings,
                cfg,
                &TransferNotifyInput {
                    req: &eff_req,
                    raw_title: disp_title,
                    resource_title: &res_share_desc,
                    source_type: src_label,
                    season_episode: &eff_episodes,
                    download_action: "开始转存",
                    size_str: &share_size_str,
                    target_hint: "FlowLink 自动整理",
                },
            );
            Ok(TransferResult {
                status: "success".into(),
                notice: msg,
                share_url: inspected.share_url,
                submitted_count: selected_ids.len().max(1),
                episodes: eff_episodes,
                warnings: Vec::new(),
            })
        }
        Err(e) => {
            if !target_cid.is_empty() {
                if mc
                    .open115_share_receive(
                        &inspected.share_url,
                        &inspected.receive_code,
                        &target_cid,
                        &selected_ids,
                    )
                    .is_ok()
                {
                    if cfg.rules.prefer_flowlink {
                        mc.trigger_flowlink_move_all();
                    }
                    let dir_lbl = format_target_dir_label(mc, cfg, &target_cid);
                    let short_dir = short_target_dir_name(mc, cfg, &target_cid);
                    let notice = format!("已接收至 115 目录 {}", dir_lbl);
                    mark_transferred(
                        db,
                        cfg,
                        &eff_req,
                        &submit_text,
                        disp_title,
                        src_label,
                        &eff_episodes,
                        reason,
                        &notice,
                    );
                    notify_transfer_templated(
                        mc,
                        settings,
                        cfg,
                        &TransferNotifyInput {
                            req: &eff_req,
                            raw_title: disp_title,
                            resource_title: &res_share_desc,
                            source_type: src_label,
                            season_episode: &eff_episodes,
                            download_action: "开始转存",
                            size_str: &share_size_str,
                            target_hint: &format!("接收至 {}", short_dir),
                        },
                    );
                    return Ok(TransferResult {
                        status: "success".into(),
                        notice,
                        share_url: inspected.share_url,
                        submitted_count: selected_ids.len().max(1),
                        episodes: eff_episodes,
                        warnings: Vec::new(),
                    });
                }
            }
            if is_115_reviewing_text(&e.to_string()) {
                enqueue_reviewing(db, &eff_req, &inspected.share_code, &inspected.receive_code, &target_cid);
                let notice = "115 分享处于审核中，已加入待审重试队列".to_string();
                let _ = db.append_activity(crate::models::ActivityRecord {
                    record_type: "transfer".into(),
                    title: disp_title.to_string(),
                    source: src_label.to_string(),
                    channel_name: eff_req.channel_name.clone(),
                    slug: if !inspected.share_url.is_empty() {
                        inspected.share_url.clone()
                    } else {
                        slug.to_string()
                    },
                    clean_slug: rules::clean_slug(slug),
                    tmdb_id: eff_req.tmdb_id.clone(),
                    media_type: eff_req.media_type.clone(),
                    episodes: eff_episodes.clone(),
                    action: "暂存待审".into(),
                    status: "reviewing".into(),
                    detail: notice.clone(),
                    ..Default::default()
                });
                return Ok(TransferResult {
                    status: "reviewing".into(),
                    notice,
                    share_url: inspected.share_url,
                    ..Default::default()
                });
            }
            Err(e)
        }
    }
}

pub fn parse_snap_response(
    raw: &Value,
    parent_cid: &str,
    prefix: &str,
    depth: usize,
) -> (String, Vec<ShareFileNode>, bool, String) {
    if raw.get("state").and_then(|x| x.as_bool()) == Some(false) {
        let msg = val_str(raw, "error")
            .or_else(|| val_str(raw, "message"))
            .unwrap_or_else(|| "115 返回失败".into());
        let errno = val_str(raw, "errno").unwrap_or_default();
        let reviewing = errno == "4100029" || is_115_reviewing_text(&msg);
        return (String::new(), Vec::new(), reviewing, msg);
    }

    let data_obj = raw.get("data").filter(|x| x.is_object()).unwrap_or(raw);
    let mut share_title = data_obj
        .get("share_info")
        .and_then(|si| {
            val_str(si, "share_title")
                .or_else(|| val_str(si, "title"))
                .or_else(|| val_str(si, "name"))
        })
        .or_else(|| val_str(data_obj, "share_title"))
        .or_else(|| val_str(raw, "share_title"))
        .unwrap_or_default();
    share_title = share_title.trim().to_string();

    let mut list_opt = None;
    for k in ["list", "items", "files", "children", "data"] {
        if let Some(arr) = data_obj.get(k).and_then(|x| x.as_array()) {
            list_opt = Some(arr);
            break;
        }
        if let Some(arr) = raw.get(k).and_then(|x| x.as_array()) {
            list_opt = Some(arr);
            break;
        }
    }
    let Some(list) = list_opt else {
        return (share_title, Vec::new(), false, String::new());
    };

    let mut nodes = Vec::new();
    for m in list {
        let name = val_str(m, "name")
            .or_else(|| val_str(m, "n"))
            .or_else(|| val_str(m, "fn"))
            .or_else(|| val_str(m, "file_name"))
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let is_dir = if let Some(b) = m.get("is_dir").and_then(|x| x.as_bool()) {
            b
        } else if val_str(m, "fc").as_deref() == Some("0") {
            true
        } else {
            val_str(m, "fid").is_none()
                && val_str(m, "cid").is_some()
                && val_str(m, "s").is_none()
                && val_str(m, "size").is_none()
        };

        let id = if is_dir {
            val_str(m, "file_id")
                .or_else(|| val_str(m, "cid"))
                .or_else(|| val_str(m, "id"))
                .or_else(|| val_str(m, "fid"))
        } else {
            val_str(m, "file_id")
                .or_else(|| val_str(m, "fid"))
                .or_else(|| val_str(m, "id"))
                .or_else(|| val_str(m, "cid"))
        }
        .unwrap_or_default();

        let size = val_i64(m, "size")
            .or_else(|| val_i64(m, "s"))
            .or_else(|| val_i64(m, "file_size"))
            .unwrap_or(0);
        let sha1 = val_str(m, "sha1")
            .or_else(|| val_str(m, "sha"))
            .unwrap_or_default()
            .to_ascii_uppercase();
        let pick_code = val_str(m, "pick_code")
            .or_else(|| val_str(m, "pc"))
            .unwrap_or_default();
        let full_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", prefix, name)
        };
        let is_video = !is_dir && rules::is_video_file(&name);
        let is_subtitle = !is_dir && rules::is_subtitle_file(&name);
        let pairs = extract_file_node_episodes(prefix, &name);
        let (season, episode, episode_end) = if let (Some(first), Some(last)) = (pairs.first(), pairs.last())
        {
            (first.season, first.episode, last.episode)
        } else {
            (0, 0, 0)
        };

        nodes.push(ShareFileNode {
            id,
            parent_id: parent_cid.to_string(),
            name,
            path: full_path,
            size,
            sha1,
            pick_code,
            is_dir,
            is_video,
            is_subtitle,
            depth,
            season,
            episode,
            episode_end,
            episode_label: rules::format_episode_pairs(&pairs),
            ..Default::default()
        });
    }
    (share_title, nodes, false, String::new())
}

pub fn evaluate_archive_tree(
    folders: &[String],
    origin_countries: &[String],
    original_lang: &str,
) -> String {
    let mut targets = HashSet::new();
    for c in origin_countries {
        let cl = c.trim().to_ascii_lowercase();
        if !cl.is_empty() {
            targets.insert(cl);
        }
    }
    let ol = original_lang.trim().to_ascii_lowercase();
    if !ol.is_empty() {
        targets.insert(ol);
    }
    if targets.is_empty() {
        return String::new();
    }

    let aliases: &[(&str, &[&str])] = &[
        ("cn", &["国产", "华语", "大陆", "内地", "国剧"]),
        ("zh", &["国产", "华语", "国语", "中文"]),
        ("hk", &["港剧", "香港", "粤语"]),
        ("tw", &["台剧", "台湾"]),
        ("jp", &["日剧", "日本", "日漫", "番剧"]),
        ("ja", &["日剧", "日本", "日语"]),
        ("kr", &["韩剧", "韩国"]),
        ("ko", &["韩剧", "韩国", "韩语"]),
        ("us", &["美剧", "欧美", "美国"]),
        ("gb", &["英剧", "欧美", "英国"]),
    ];

    let mut best_folder = String::new();
    let mut best_score = 0;
    for f in folders {
        let low = f.to_lowercase();
        let mut score = 0;
        for t in &targets {
            if low.contains(t) {
                score += 100;
            }
            for &(k, words) in aliases {
                if k == t {
                    for w in words {
                        if low.contains(w) {
                            score += 100;
                        }
                    }
                }
            }
        }
        if score > best_score {
            best_score = score;
            best_folder = f.clone();
        }
    }
    best_folder
}

/* ==================== Private Shield Indexer (SQLite Backed) ==================== */

pub fn add_shield_share(
    db: &Db,
    raw_share_url: &str,
    receive_code: &str,
    custom_title: &str,
) -> Result<ShieldShareRecord> {
    let (sc, mut rc) = rules::parse_115_share(raw_share_url, receive_code);
    if rc.is_empty() {
        rc = receive_code.trim().to_string();
    }
    ensure!(!sc.is_empty(), "未识别到有效的 115 分享链接或分享码");
    let full_url = rules::format_115_share_url(&sc, &rc);
    let title = if custom_title.trim().is_empty() {
        format!("私有分享 {}", sc)
    } else {
        custom_title.trim().to_string()
    };

    let now_ts = chrono::Utc::now().timestamp();
    let mut existing = db
        .list_shield_shares()
        .into_iter()
        .find(|s| s.share_code.eq_ignore_ascii_case(&sc));

    let rec = if let Some(mut r) = existing.take() {
        r.receive_code = rc;
        r.share_url = full_url;
        if !custom_title.trim().is_empty() {
            r.title = title;
        }
        if r.pending_cids.is_empty() && r.status == "completed" {
            r.pending_cids = vec!["0".into()];
            r.processed_cids.clear();
        }
        r.status = "pending".into();
        r.last_error.clear();
        r.updated_ts = now_ts;
        r
    } else {
        let mut dir_paths = HashMap::new();
        dir_paths.insert("0".into(), String::new());
        ShieldShareRecord {
            id: format!("sh-{}", sc.to_lowercase()),
            share_code: sc,
            receive_code: rc,
            share_url: full_url,
            title,
            status: "pending".into(),
            pending_cids: vec!["0".into()],
            dir_paths,
            updated_ts: now_ts,
            ..Default::default()
        }
    };
    db.upsert_shield_share(&rec)?;
    Ok(rec)
}

pub fn step_shield_share(
    mc: &HostClient,
    db: &Db,
    share_id: &str,
    max_dirs: usize,
) -> Result<usize> {
    let limit = if max_dirs == 0 { 12 } else { max_dirs };
    let mut rec = db
        .list_shield_shares()
        .into_iter()
        .find(|s| s.id == share_id || s.share_code.eq_ignore_ascii_case(share_id))
        .ok_or_else(|| anyhow!("未找到神盾分享记录: {}", share_id))?;

    if rec.pending_cids.is_empty() {
        rec.status = "completed".into();
        db.upsert_shield_share(&rec)?;
        return Ok(0);
    }

    let mut processed_set: HashSet<String> = rec.processed_cids.iter().cloned().collect();
    rec.status = "parsing".into();
    let mut dirs_visited = 0usize;
    let mut new_files_total = 0usize;

    while !rec.pending_cids.is_empty() && dirs_visited < limit {
        let cid = rec.pending_cids.remove(0);
        if processed_set.contains(&cid) {
            continue;
        }
        if dirs_visited > 0 {
            std::thread::sleep(Duration::from_millis(800));
        }
        dirs_visited += 1;

        let snap_raw = match mc.open115_share_snap(&rec.share_url, &rec.receive_code, &cid, 0, 500)
        {
            Ok(v) => v,
            Err(e) => {
                rec.last_error = e.to_string();
                rec.status = "error".into();
                rec.pending_cids.insert(0, cid);
                rec.updated_ts = chrono::Utc::now().timestamp();
                db.upsert_shield_share(&rec)?;
                return Err(e);
            }
        };

        let parent_rel = rec.dir_paths.get(&cid).cloned().unwrap_or_default();
        let (share_title, nodes, reviewing, err_msg) =
            parse_snap_response(&snap_raw, &cid, &parent_rel, 0);
        if !err_msg.is_empty() {
            rec.last_error = err_msg.clone();
            rec.status = if reviewing { "paused" } else { "error" }.into();
            rec.pending_cids.insert(0, cid);
            rec.updated_ts = chrono::Utc::now().timestamp();
            db.upsert_shield_share(&rec)?;
            return Err(anyhow!("{}", err_msg));
        }
        if cid == "0" && !share_title.is_empty() && rec.title.starts_with("私有分享 ") {
            rec.title = share_title;
        }

        let mut batch_files = Vec::new();
        for n in nodes {
            if n.is_dir {
                if !processed_set.contains(&n.id) && !n.id.is_empty() && n.id != cid {
                    let sub_path = if parent_rel.is_empty() {
                        n.name
                    } else {
                        format!("{}/{}", parent_rel, n.name)
                    };
                    rec.dir_paths.insert(n.id.clone(), sub_path);
                    rec.pending_cids.push(n.id);
                    rec.parsed_folders += 1;
                }
                continue;
            }
            if !n.is_video && !n.is_subtitle {
                continue;
            }
            let ctx_text = format!("{} {}", parent_rel, n.name);
            let mut pure = rules::extract_pure_title(&n.name);
            if pure.is_empty() && !parent_rel.is_empty() {
                if let Some(last_seg) = parent_rel.rsplit('/').next() {
                    pure = rules::extract_pure_title(last_seg);
                }
            }
            let tmdb_id = rules::extract_tmdb_id(&ctx_text);
            let eps = rules::parse_shallow_episodes(&ctx_text, "tv", None);
            let mtype = if !eps.is_empty() { "tv" } else { "movie" };
            batch_files.push(ShieldIndexedFile {
                share_code: rec.share_code.clone(),
                receive_code: rec.receive_code.clone(),
                file_id: n.id,
                parent_id: cid.clone(),
                rel_path: parent_rel.clone(),
                file_name: n.name,
                file_size: n.size,
                sha1: n.sha1.to_ascii_uppercase(),
                pure_title: pure,
                tmdb_id,
                media_type: mtype.into(),
                parsed_episodes: eps,
            });
        }
        let inserted = db.insert_shield_files(&batch_files)?;
        rec.parsed_files += inserted;
        new_files_total += inserted;

        processed_set.insert(cid.clone());
        rec.processed_cids.push(cid);
    }

    rec.last_error.clear();
    rec.status = if rec.pending_cids.is_empty() {
        "completed"
    } else {
        "pending"
    }
    .into();
    rec.updated_ts = chrono::Utc::now().timestamp();
    db.upsert_shield_share(&rec)?;
    Ok(new_files_total)
}

pub fn search_shield_index(
    db: &Db,
    keyword: &str,
    tmdb_id: &str,
    media_type: &str,
) -> Vec<ResourceItem> {
    let files = db.query_shield_files(keyword, tmdb_id);
    if files.is_empty() {
        return Vec::new();
    }

    struct Group {
        share_code: String,
        receive_code: String,
        parent_id: String,
        rel_path: String,
        files: Vec<ShieldIndexedFile>,
    }
    let mut groups: HashMap<String, Group> = HashMap::new();
    let mut order = Vec::new();

    for f in files {
        let gkey = format!("{}:{}", f.share_code.to_lowercase(), f.parent_id);
        if !groups.contains_key(&gkey) {
            order.push(gkey.clone());
            groups.insert(
                gkey.clone(),
                Group {
                    share_code: f.share_code.clone(),
                    receive_code: f.receive_code.clone(),
                    parent_id: f.parent_id.clone(),
                    rel_path: f.rel_path.clone(),
                    files: Vec::new(),
                },
            );
        }
        groups.get_mut(&gkey).unwrap().files.push(f);
    }

    let mut out = Vec::new();
    for gkey in order {
        let g = groups.remove(&gkey).unwrap();
        let mut total_size = 0i64;
        let mut all_names = Vec::new();
        let mut all_pairs = Vec::new();
        let first_pure = g.files[0].pure_title.clone();
        let first_tid = g.files[0].tmdb_id.clone();
        let first_mtype = g.files[0].media_type.clone();

        for f in &g.files {
            if rules::is_video_file(&f.file_name) {
                total_size += f.file_size;
                all_names.push(f.file_name.clone());
                all_pairs.extend(rules::extract_episode_pairs(
                    &format!("{} {}", f.rel_path, f.file_name),
                    1,
                ));
            }
        }
        if all_names.is_empty() {
            continue;
        }
        let slug = format!(
            "nextfind://{}/{}/{}",
            g.share_code, g.receive_code, g.parent_id
        );
        let mut disp_title = first_pure;
        if disp_title.is_empty() && !g.rel_path.is_empty() {
            disp_title = g.rel_path.rsplit('/').next().unwrap_or("").to_string();
        }
        if disp_title.is_empty() {
            disp_title = all_names[0].clone();
        }
        let attrs = rules::parse_movie_attributes(&format!("{} {}", g.rel_path, all_names.join(" ")));
        let eps_str = rules::format_episode_pairs(&all_pairs);
        let mtype = if !media_type.is_empty() && media_type != "all" {
            media_type.to_string()
        } else if !eps_str.is_empty() {
            "tv".into()
        } else {
            first_mtype
        };

        out.push(ResourceItem {
            id: format!("shield-{}", gkey),
            slug,
            clean_slug: format!("shield:{}", gkey),
            title: disp_title,
            raw_title: format!("{}/{}", g.rel_path, all_names[0]),
            remark: format!(
                "神盾索引 · {} ({} 个视频文件)",
                if g.rel_path.is_empty() { "根目录" } else { &g.rel_path },
                all_names.len()
            ),
            tmdb_id: if !first_tid.is_empty() {
                first_tid
            } else {
                tmdb_id.trim().to_string()
            },
            media_type: mtype,
            source_type: "shield".into(),
            channel_name: "私有神盾库".into(),
            pan_type: "shield".into(),
            share_code: g.share_code,
            receive_code: g.receive_code,
            share_size: rules::format_bytes(total_size),
            size_bytes: total_size,
            parsed_episodes: eps_str,
            movie_attributes: attrs,
            is_unlocked: true,
            shield_matched: true,
            ..Default::default()
        });
    }
    out
}

/* ==================== Scheduled Tick, Checkin, RSS & Chase ==================== */

#[derive(Debug, Clone, Default, Serialize)]
pub struct TickSummary {
    pub tg_added: usize,
    pub shield_added: usize,
    pub review_retried: usize,
    pub review_succeeded: usize,
    pub rss_processed: usize,
    pub subs_checked: usize,
    pub subs_transferred: usize,
    pub nextbot_radar: usize,
    pub nextbot_transfers: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    pub message: String,
}

pub fn run_daily_checkin(
    mc: Option<&HostClient>,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
    force_hdhive: bool,
) -> Result<String> {
    let proxy_url = effective_proxy(mc, cfg);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    if cfg.hdhive.last_checkin_date.trim() != today {
        cfg.hdhive.checked_in_today = false;
    }

    let mut parts = Vec::new();
    let mut channels_run = Vec::new();
    let mut executed = 0usize;
    let mut err_count = 0usize;

    if cfg.hdhive.enabled
        && (force_hdhive || cfg.hdhive.auto_checkin)
        && !cfg.hdhive.api_key.trim().is_empty()
    {
        executed += 1;
        channels_run.push("影巢 / HDHive");
        match providers::hdhive_checkin(cfg, &proxy_url) {
            Ok(msg) => {
                let detail = format!("{} (当前 {} 积分)", msg, cfg.hdhive.points);
                parts.push(format!("影巢: {}", detail));
                let _ = db.append_record("HDHive 每日签到", "hdhive", "", "每日签到", "success", &detail);
            }
            Err(e) => {
                err_count += 1;
                parts.push(format!("影巢签到失败: {}", e));
                let _ = db.append_record(
                    "HDHive 每日签到",
                    "hdhive",
                    "",
                    "每日签到",
                    "error",
                    &e.to_string(),
                );
            }
        }
    }

    if cfg.ayclub.enabled && !cfg.ayclub.nf_token.trim().is_empty() {
        executed += 1;
        channels_run.push("AYClub");
        match providers::refresh_ayclub_quota(cfg, &proxy_url) {
            Ok(msg) => parts.push(format!("AYClub: {}", msg)),
            Err(e) => {
                err_count += 1;
                parts.push(format!("AYClub配额刷新失败: {}", e));
            }
        }
    }

    if parts.is_empty() {
        if cfg.hdhive.enabled && !cfg.hdhive.auto_checkin {
            parts.push("影巢已关闭每日自动签到（如需手动签到可在资源渠道页点击按钮）".into());
        } else {
            parts.push("未启用需要签到/刷额度的渠道（可在工作台「资源渠道」配置 HDHive 或 AYClub）".into());
        }
    }
    let summary = parts.join("；");
    cfg.last_result = summary.clone();
    if executed > 0 && settings.notify && cfg.notifications.enabled && cfg.notifications.on_checkin
    {
        if let Some(host) = mc {
            let status_badge = if err_count == 0 {
                "✅签到完成"
            } else if err_count < executed {
                "⚠️部分完成"
            } else {
                "❌签到失败"
            };
            let title = format!("📅 每日签到 · 额度报告 {}", status_badge);
            let mut lines = Vec::new();
            lines.push(format!("渠道: {}", channels_run.join(" / ")));
            for p in &parts {
                lines.push(format!("结果: {}", p));
            }
            lines.push(format!(
                "时间: {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ));
            lines.push("来源: MediFind 每日签到".into());
            let _ = host.notify_with_image(
                &title,
                &lines.join("\n"),
                Some(crate::host::MEDIARY_DEFAULT_NOTIFY_IMAGE),
            );
        }
    }
    Ok(summary)
}

pub fn run_tick(
    mc: Option<&HostClient>,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
) -> TickSummary {
    let mut sum = TickSummary::default();
    let proxy_url = effective_proxy(mc, cfg);

    // 1. Telegram sync + NextFind level 64 (入新) / 99 (全量) + FindBOT 推送雷达
    let (tg_added, tg_by_ch, tg_errs) = providers::sync_all_tg_channels(db, cfg, false, &proxy_url);
    sum.tg_added = tg_added;
    sum.errors.extend(tg_errs);
    if !tg_by_ch.is_empty() {
        if let Some(host) = mc {
            if settings.auto_chase {
                sum.subs_transferred += process_tg_auto_levels(host, db, settings, cfg, &tg_by_ch);
            }
        }
        if settings.notify {
            let new_tg_flat: Vec<ResourceItem> = tg_by_ch.values().flatten().cloned().collect();
            sum.nextbot_radar =
                crate::nextbot::notify_radar_bots(db, mc, cfg, &new_tg_flat, &proxy_url);
        }
    }

    // 1.5 FindBOT BOT 转存队列拉取
    if let Some(host) = mc {
        if crate::nextbot::has_active_transfer_bots(cfg) {
            let (nb_cnt, nb_errs) =
                crate::nextbot::poll_bot_transfers(host, db, settings, cfg, &proxy_url);
            sum.nextbot_transfers += nb_cnt;
            sum.errors.extend(nb_errs);
        }
    }

    // 2. Private Shield background batch
    if let Some(host) = mc {
        let mut remaining = 8usize;
        for s in db.list_shield_shares() {
            if remaining == 0 {
                break;
            }
            if matches!(s.status.as_str(), "pending" | "parsing") && !s.pending_cids.is_empty() {
                if let Ok(n) = step_shield_share(host, db, &s.id, remaining) {
                    sum.shield_added += n;
                    remaining = remaining.saturating_sub(4);
                }
            }
        }
    }

    // 3. Retry 115 reviewing queue
    if let Some(host) = mc {
        let (retried, ok_cnt) = retry_reviewing_queue(host, db, settings, cfg, None, 5);
        sum.review_retried = retried;
        sum.review_succeeded = ok_cnt;
    }

    // 4. Process due RSS feeds
    let (rss_cnt, rss_errs) = process_due_rss_feeds(mc, db, settings, cfg, false);
    sum.rss_processed = rss_cnt;
    sum.errors.extend(rss_errs);

    // 5. Subscription auto-chase
    if let Some(host) = mc {
        if settings.auto_chase {
            let (checked, hit, chase_errs) = chase_mediary_subscriptions(host, db, settings, cfg, 6);
            sum.subs_checked = checked;
            sum.subs_transferred += hit;
            sum.errors.extend(chase_errs);
        }
    }

    sum.message = format!(
        "巡检完成：TG新增 {}，神盾索引 +{}，待审成功 {}/{}，RSS处理 {}，追更命中 {}/{}",
        sum.tg_added,
        sum.shield_added,
        sum.review_succeeded,
        sum.review_retried,
        sum.rss_processed,
        sum.subs_transferred,
        sum.subs_checked
    );
    if sum.nextbot_transfers > 0 || sum.nextbot_radar > 0 {
        sum.message.push_str(&format!(
            "，FindBOT(转存{}/雷达{})",
            sum.nextbot_transfers, sum.nextbot_radar
        ));
    }
    if !sum.errors.is_empty() {
        sum.message
            .push_str(&format!("（警告 {} 项）", sum.errors.len()));
    }
    if sum.tg_added > 0
        || sum.shield_added > 0
        || sum.review_retried > 0
        || sum.rss_processed > 0
        || sum.subs_transferred > 0
        || sum.nextbot_transfers > 0
        || sum.nextbot_radar > 0
        || !sum.errors.is_empty()
    {
        let _ = db.append_activity(crate::models::ActivityRecord {
            record_type: "task".into(),
            title: "定时追更与待审巡检".into(),
            source: "system".into(),
            action: "巡检任务".into(),
            status: if sum.errors.is_empty() { "success" } else { "warn" }.into(),
            detail: sum.message.clone(),
            ..Default::default()
        });
    }
    cfg.last_result = sum.message.clone();
    sum
}

pub fn retry_reviewing_queue(
    mc: &HostClient,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
    only_slug: Option<&str>,
    max_batch: usize,
) -> (usize, usize) {
    let mut queue = db.list_reviewing();
    if queue.is_empty() {
        return (0, 0);
    }
    if let Some(target) = only_slug.filter(|s| !s.trim().is_empty()) {
        queue.retain(|x| x.clean_slug == target.trim());
    }
    let mut retried = 0usize;
    let mut succeeded = 0usize;

    for (idx, mut item) in queue.into_iter().take(max_batch).enumerate() {
        if idx > 0 {
            std::thread::sleep(Duration::from_secs(3));
        }
        retried += 1;
        let slug = if !item.share_code.is_empty() {
            rules::format_115_share_url(&item.share_code, &item.receive_code)
        } else {
            item.slug.clone()
        };
        let res = transfer_resource(
            mc,
            db,
            settings,
            cfg,
            &TransferRequest {
                slug,
                title: item.title.clone(),
                tmdb_id: item.tmdb_id.clone(),
                media_type: item.media_type.clone(),
                source_type: item.source_type.clone(),
                target_cid: item.target_cid.clone(),
                only_missing_episodes: item.media_type.eq_ignore_ascii_case("tv"),
                trigger_reason: "待审队列自动重试".into(),
                ..Default::default()
            },
        );
        match res {
            Ok(r) if r.status == "success" || r.status == "skipped" => {
                let _ = db.remove_reviewing(&item.clean_slug);
                succeeded += 1;
            }
            Ok(r) => {
                item.retry_count += 1;
                item.last_tried_ts = chrono::Utc::now().timestamp();
                item.last_error = r.notice;
                if item.retry_count < 72 {
                    let _ = db.upsert_reviewing(&item);
                } else {
                    let _ = db.remove_reviewing(&item.clean_slug);
                }
            }
            Err(e) => {
                item.retry_count += 1;
                item.last_tried_ts = chrono::Utc::now().timestamp();
                item.last_error = e.to_string();
                if item.retry_count < 72 && is_115_reviewing_text(&item.last_error) {
                    let _ = db.upsert_reviewing(&item);
                } else {
                    let _ = db.remove_reviewing(&item.clean_slug);
                    let _ = db.append_activity(crate::models::ActivityRecord {
                        record_type: "transfer".into(),
                        title: item.title.clone(),
                        source: item.source_type.clone(),
                        slug: if !item.share_code.is_empty() {
                            rules::format_115_share_url(&item.share_code, &item.receive_code)
                        } else {
                            item.slug.clone()
                        },
                        clean_slug: item.clean_slug.clone(),
                        tmdb_id: item.tmdb_id.clone(),
                        media_type: item.media_type.clone(),
                        action: "移出待审".into(),
                        status: "error".into(),
                        detail: item.last_error.clone(),
                        ..Default::default()
                    });
                }
            }
        }
    }
    (retried, succeeded)
}

pub fn process_due_rss_feeds(
    mc: Option<&HostClient>,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
    force_all: bool,
) -> (usize, Vec<String>) {
    let proxy_url = effective_proxy(mc, cfg);
    let now = chrono::Utc::now().timestamp();
    let mut processed = 0usize;
    let mut errs = Vec::new();

    let mut existing_sub_tmdb = HashSet::new();
    if let Some(host) = mc {
        if let Ok(subs) = host.list_subscriptions() {
            for s in subs {
                existing_sub_tmdb.insert(s.tmdb_id);
            }
        }
    }

    let feed_count = cfg.rss_feeds.len();
    for i in 0..feed_count {
        if !cfg.rss_feeds[i].enabled && !force_all {
            continue;
        }
        let interval = providers::rss_interval_seconds(&cfg.rss_feeds[i].refresh_interval);
        if !force_all
            && cfg.rss_feeds[i].last_refresh_at > 0
            && now - cfg.rss_feeds[i].last_refresh_at < interval
        {
            continue;
        }

        let feed_url = cfg.rss_feeds[i].url.clone();
        let feed_name = cfg.rss_feeds[i].name.clone();
        let auto_trans = cfg.rss_feeds[i].auto_transfer;
        let auto_sub = cfg.rss_feeds[i].auto_subscribe;
        cfg.rss_feeds[i].last_refresh_at = now;

        let items = match providers::fetch_rss_feed(&feed_url, &proxy_url) {
            Ok(list) => list,
            Err(e) => {
                cfg.rss_feeds[i].last_result = format!("失败: {}", e);
                errs.push(format!("RSS[{}]: {}", feed_name, e));
                continue;
            }
        };

        let mut seen: HashSet<String> = cfg.rss_feeds[i].seen_guids.iter().cloned().collect();
        let mut new_guids = Vec::new();
        let mut feed_hits = 0usize;

        for item in &items {
            if !seen.insert(item.guid.clone()) {
                continue;
            }
            new_guids.push(item.guid.clone());

            if auto_trans {
                if let (Some(host), Some(lk)) = (mc, item.direct_links.first()) {
                    if !db.is_slug_transferred(&lk.clean_key) {
                        let t = if !item.pure_title.is_empty() {
                            &item.pure_title
                        } else {
                            &item.title
                        };
                        if transfer_resource(
                            host,
                            db,
                            settings,
                            cfg,
                            &TransferRequest {
                                slug: lk.url.clone(),
                                title: t.to_string(),
                                media_type: item.media_type.clone(),
                                source_type: "rss".into(),
                                trigger_reason: "RSS 自动转存".into(),
                                ..Default::default()
                            },
                        )
                        .is_ok()
                        {
                            feed_hits += 1;
                            processed += 1;
                        }
                    }
                }
            }

            if auto_sub && !item.pure_title.is_empty() {
                if let Some(host) = mc {
                    if let Ok(resolved) =
                        host.resolve_tmdb(&item.pure_title, &item.media_type, item.year)
                    {
                        let tid = resolved.effective_id().to_string();
                        if let Ok(tid_num) = tid.parse::<i64>() {
                            if tid_num > 0 && !existing_sub_tmdb.contains(&tid) {
                                let snap = build_library_snapshot(
                                    Some(host),
                                    cfg,
                                    &tid,
                                    resolved.effective_type(),
                                    resolved.effective_title(),
                                );
                                if !(resolved.effective_type() == "movie" && snap.in_library)
                                    && host
                                        .create_subscription(
                                            tid_num,
                                            resolved.effective_title(),
                                            resolved.effective_type(),
                                            0,
                                            item.year,
                                        )
                                        .is_ok()
                                {
                                    existing_sub_tmdb.insert(tid);
                                    feed_hits += 1;
                                    processed += 1;
                                    let _ = db.append_record(
                                        resolved.effective_title(),
                                        &format!("rss:{}", feed_name),
                                        "",
                                        "自动订阅",
                                        "success",
                                        &format!("TMDB:{} 已创建 Mediary 订阅", tid_num),
                                    );
                                    if settings.notify
                                        && cfg.notifications.enabled
                                        && cfg.notifications.on_rss_subscribe
                                    {
                                        let tpls = host.get_notification_templates();
                                         // Mediary POST /api/subscriptions natively fires `subscription_added` when enabled;
                                         // only send a fallback templated notice if host's native subscription_added is disabled.
                                         if !tpls.subscription_added.enabled {
                                             let eff_type = resolved.effective_type();
                                             let det_opt = host
                                                 .get_tmdb_details(
                                                     &tid_num.to_string(),
                                                     eff_type,
                                                 )
                                                 .ok();
                                             let img = cfg
                                                 .notifications
                                                 .include_poster
                                                 .then(|| {
                                                     det_opt
                                                         .as_ref()
                                                         .and_then(|d| {
                                                             crate::host::tmdb_card_image_https(
                                                                 &d.backdrop_path,
                                                                 &d.poster_path,
                                                             )
                                                         })
                                                         .or_else(|| {
                                                             crate::host::tmdb_card_image_https(
                                                                 &resolved.backdrop_path,
                                                                 &resolved.poster_path,
                                                             )
                                                         })
                                                         .or_else(|| {
                                                             (!snap.poster_url.is_empty())
                                                                 .then(|| snap.poster_url.clone())
                                                         })
                                                 })
                                                 .flatten();
                                             let mut vars = HashMap::new();
                                             vars.insert(
                                                 "media_icon",
                                                 if eff_type == "tv" { "📺" } else { "🎬" }
                                                     .to_string(),
                                             );
                                             vars.insert("name", resolved.effective_title().to_string());
                                             if item.year > 0 {
                                                 vars.insert("year", item.year.to_string());
                                             }
                                             if eff_type == "tv" {
                                                 vars.insert("season_label", "S01".to_string());
                                             }
                                             let overview = det_opt
                                                 .as_ref()
                                                 .map(|d| d.overview.trim())
                                                 .filter(|s| !s.is_empty())
                                                 .unwrap_or_else(|| resolved.overview.trim());
                                             vars.insert("overview", overview.to_string());
                                             let nt_title = crate::host::render_mediary_template(
                                                 &tpls.subscription_added.title,
                                                 &vars,
                                             );
                                             let nt_body = crate::host::render_mediary_template(
                                                 &tpls.subscription_added.content,
                                                 &vars,
                                             );
                                             let final_body = if nt_body.is_empty() {
                                                 format!("🌐 来源: RSS · {}", feed_name)
                                             } else {
                                                 nt_body
                                             };
                                             let _ = host.notify_with_image(
                                                 &nt_title,
                                                 &final_body,
                                                 img.as_deref(),
                                             );
                                         }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        cfg.rss_feeds[i].seen_guids.extend(new_guids);
        if cfg.rss_feeds[i].seen_guids.len() > 500 {
            let drain_n = cfg.rss_feeds[i].seen_guids.len() - 500;
            cfg.rss_feeds[i].seen_guids.drain(..drain_n);
        }
        cfg.rss_feeds[i].last_result = format!("同步 {} 条，触发 {} 项", items.len(), feed_hits);
    }
    (processed, errs)
}

pub fn chase_single_subscription(
    mc: &HostClient,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
    sub: &Subscription,
) -> Result<(bool, String)> {
    let tid = sub.tmdb_id.trim();
    if tid.is_empty() || tid == "0" {
        return Ok((false, "无有效 TMDB ID".into()));
    }
    let mtype = sub.effective_type();
    let title = sub.effective_title();

    if mtype == "movie"
        && db.is_in_flight(&format!("movie:{}", tid), IN_FLIGHT_CHASE_TTL_SECS)
    {
        return Ok((false, "该影片近期已投递转存，等待下游归档入库中".into()));
    }
    if mtype == "tv" && db.is_in_flight(&format!("tv:{}:ALL", tid), IN_FLIGHT_CHASE_TTL_SECS) {
        return Ok((false, "该剧集整包近期已投递转存，等待下游归档入库中".into()));
    }

    let snap = build_library_snapshot(Some(mc), cfg, tid, mtype, title);
    if mtype == "movie" && snap.in_library {
        return Ok((false, "电影已在媒体库中".into()));
    }
    if mtype == "tv" && !snap.season_totals.is_empty() && snap.missing_episodes.is_empty() {
        return Ok((false, "当前已播剧集齐全".into()));
    }

    let uninflight_missing: HashSet<EpisodePair> = if mtype == "tv" {
        snap.missing_episodes
            .iter()
            .copied()
            .filter(|p| {
                !db.is_in_flight(
                    &format!("tv:{}:{}", tid, p.key()),
                    IN_FLIGHT_CHASE_TTL_SECS,
                )
            })
            .collect()
    } else {
        HashSet::new()
    };
    if mtype == "tv" && !snap.missing_episodes.is_empty() && uninflight_missing.is_empty() {
        return Ok((false, "缺失剧集近期已投递转存，等待下游归档入库中".into()));
    }

    let agg = aggregate_resources(
        Some(mc),
        db,
        cfg,
        &AggregateRequest {
            query: title.to_string(),
            tmdb_id: tid.to_string(),
            media_type: mtype.to_string(),
            title: title.to_string(),
            season: sub.season,
            live_tg: true,
            include_hidden: true,
            ..Default::default()
        },
    );
    if agg.items.is_empty() {
        return Ok((false, "暂未检索到可用网盘资源".into()));
    }

    for cand in agg.items {
        if !cfg.rules.is_item_auto_sub_enabled(&cand) || !cfg.is_tg_channel_chase_allowed(&cand) {
            continue;
        }
        if db.is_slug_transferred(&cand.clean_slug) || cand.local_in_library {
            continue;
        }
        if !is_candidate_compatible_with_target(
            &cand,
            mtype,
            title,
            tid,
            &snap.season_totals,
            sub.expected_episodes,
            true,
        ) {
            continue;
        }
        if mtype == "tv" && !snap.missing_episodes.is_empty() && !cand.parsed_episodes.is_empty() {
            if cand.missing_eps_hit.is_empty() {
                continue;
            }
            let cand_pairs = rules::extract_episode_pairs(
                &format!("{} {} {}", cand.parsed_episodes, cand.title, cand.remark),
                1,
            );
            if !cand_pairs.is_empty()
                && !cand_pairs.iter().any(|p| uninflight_missing.contains(p))
            {
                continue;
            }
        }
        if !settings.auto_unlock_hdhive && !cand.is_unlocked && cand.unlock_points > 0 {
            continue;
        }
        if cand.unlock_points > 0
            && cfg.hdhive.max_unlock_points > 0
            && cand.unlock_points > cfg.hdhive.max_unlock_points
        {
            continue;
        }

        if let Ok(res) = transfer_resource(
            mc,
            db,
            settings,
            cfg,
            &TransferRequest {
                slug: cand.slug,
                title: title.to_string(),
                tmdb_id: tid.to_string(),
                media_type: mtype.to_string(),
                season: sub.season,
                source_type: cand.source_type,
                channel_name: cand.channel_name,
                unlock_points: cand.unlock_points,
                allow_unlock: settings.auto_unlock_hdhive,
                only_missing_episodes: mtype == "tv",
                trigger_reason: "订阅自动追更".into(),
                ..Default::default()
            },
        ) {
            if res.status == "success" || res.status == "reviewing" {
                return Ok((true, res.notice));
            }
        }
    }
    Ok((false, "未找到满足初筛规则或包含新缺集的候选资源".into()))
}

fn chase_mediary_subscriptions(
    mc: &HostClient,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
    budget: usize,
) -> (usize, usize, Vec<String>) {
    let subs = match mc.list_subscriptions() {
        Ok(s) => s,
        Err(e) => return (0, 0, vec![format!("读取 Mediary 订阅失败: {}", e)]),
    };
    let active: Vec<Subscription> = subs
        .into_iter()
        .filter(|s| {
            !matches!(
                s.status.trim().to_ascii_lowercase().as_str(),
                "completed" | "paused" | "disabled"
            )
        })
        .collect();
    if active.is_empty() {
        return (0, 0, Vec::new());
    }
    let count = budget.min(active.len()).max(1);
    if cfg.subscription_cursor >= active.len() {
        cfg.subscription_cursor = 0;
    }

    let mut checked = 0usize;
    let mut hit = 0usize;
    let mut errs = Vec::new();
    for i in 0..count {
        let idx = (cfg.subscription_cursor + i) % active.len();
        let sub = &active[idx];
        checked += 1;
        match chase_single_subscription(mc, db, settings, cfg, sub) {
            Ok((true, _)) => hit += 1,
            Ok((false, _)) => {}
            Err(e) => errs.push(format!("{}: {}", sub.effective_title(), e)),
        }
    }
    cfg.subscription_cursor = (cfg.subscription_cursor + checked) % active.len();
    (checked, hit, errs)
}

fn process_tg_auto_levels(
    mc: &HostClient,
    db: &Db,
    settings: &PluginSettings,
    cfg: &mut AppConfig,
    tg_by_ch: &HashMap<String, Vec<ResourceItem>>,
) -> usize {
    let mut triggered = 0usize;
    let ch_snapshots: Vec<(bool, i32, String)> = cfg
        .telegram
        .channels
        .iter()
        .map(|c| {
            (
                c.enabled,
                normalize_tg_level(c.level),
                providers::normalize_tg_channel(&c.identifier).to_lowercase(),
            )
        })
        .collect();

    for (enabled, lvl, norm) in ch_snapshots {
        if !enabled || triggered >= 5 || (lvl != 99 && (lvl & (64 | 32)) == 0) {
            continue;
        }
        let Some(items) = tg_by_ch.get(&norm) else {
            continue;
        };
        for orig in items {
            if triggered >= 5 {
                break;
            }
            if db.is_slug_transferred(&orig.clean_slug) {
                continue;
            }
            let mut it = orig.clone();
            let orig_title = orig.title.clone();
            let score_res = rules::score_resource(&mut it, &cfg.rules, &orig_title, "");
            it.rule_score = score_res.score;
            if !score_res.accepted {
                continue;
            }
            if lvl == 99 {
                let mut tid = if !it.tmdb_id.trim().is_empty() {
                    it.tmdb_id.trim().to_string()
                } else {
                    rules::extract_tmdb_id(&it.raw_title)
                };
                let mut mtype = if it.media_type.is_empty() {
                    if !it.parsed_episodes.is_empty() {
                        "tv".to_string()
                    } else {
                        "movie".to_string()
                    }
                } else {
                    it.media_type.clone()
                };
                let mut title = if !rules::is_suspect_banner_title(&it.title) {
                    it.title.clone()
                } else {
                    let from_raw = rules::extract_pure_title(&it.raw_title);
                    if !from_raw.is_empty() {
                        from_raw
                    } else {
                        it.title.clone()
                    }
                };
                if tid.is_empty() && !title.is_empty() && !rules::is_suspect_banner_title(&title) {
                    let pure = rules::extract_pure_title(&title);
                    let q_title = if pure.is_empty() { &title } else { &pure };
                    if let Ok(resolved) = mc.resolve_tmdb(q_title, &mtype, 0) {
                        if !resolved.effective_id().is_empty() {
                            tid = resolved.effective_id().to_string();
                            mtype = resolved.effective_type().to_string();
                            if title.trim().is_empty() && !resolved.effective_title().is_empty() {
                                title = resolved.effective_title().to_string();
                            }
                        }
                    }
                }
                if let Ok(res) = transfer_resource(
                    mc,
                    db,
                    settings,
                    cfg,
                    &TransferRequest {
                        slug: it.slug,
                        title,
                        tmdb_id: tid,
                        media_type: mtype,
                        source_type: "telegram".into(),
                        channel_name: it.channel_name,
                        trigger_reason: "TG全量频道自动接收".into(),
                        ..Default::default()
                    },
                ) {
                    if res.status == "success" || res.status == "reviewing" {
                        triggered += 1;
                    }
                }
                continue;
            }

            // Bit 64 (入新) & Bit 32 (洗版)
            let mut tid = if !it.tmdb_id.trim().is_empty() {
                it.tmdb_id.trim().to_string()
            } else {
                rules::extract_tmdb_id(&it.raw_title)
            };
            let mut mtype = if it.media_type.is_empty() {
                if !it.parsed_episodes.is_empty() {
                    "tv".to_string()
                } else {
                    "movie".to_string()
                }
            } else {
                it.media_type.clone()
            };
            let mut title = if !rules::is_suspect_banner_title(&it.title) {
                it.title.clone()
            } else {
                let from_raw = rules::extract_pure_title(&it.raw_title);
                if !from_raw.is_empty() {
                    from_raw
                } else {
                    it.title.clone()
                }
            };
            if tid.is_empty() && !title.is_empty() && !rules::is_suspect_banner_title(&title) {
                let pure = rules::extract_pure_title(&title);
                let q_title = if pure.is_empty() { &title } else { &pure };
                if let Ok(resolved) = mc.resolve_tmdb(q_title, &mtype, 0) {
                    tid = resolved.effective_id().to_string();
                    mtype = resolved.effective_type().to_string();
                    if !resolved.effective_title().is_empty() {
                        title = resolved.effective_title().to_string();
                    }
                }
            }
            if tid.is_empty() {
                continue;
            }
            let snap = build_library_snapshot(Some(mc), cfg, &tid, &mtype, &title);
            if !is_candidate_compatible_with_target(
                &it,
                &mtype,
                &title,
                &tid,
                &snap.season_totals,
                0,
                true,
            ) {
                continue;
            }
            let missing_set: HashSet<EpisodePair> = snap.missing_episodes.iter().copied().collect();
            enrich_shield_and_library_flags(&mut it, &snap, &missing_set);

            let fully_in_lib = snap.in_library && (mtype == "movie" || snap.missing_episodes.is_empty());
            if fully_in_lib {
                // Bit 32: 洗版 upgrade for already-collected items when score meets cutoff_score
                if (lvl & 32) == 0 {
                    continue;
                }
                let cutoff = cfg.rules.cutoff_score.max(80.0);
                let wash_key = format!("wash:{}:{}", mtype, tid);
                if it.rule_score < cutoff
                    || it.shield_matched
                    || db.is_in_flight(&wash_key, IN_FLIGHT_WASH_TTL_SECS)
                {
                    continue;
                }
                if let Ok(res) = transfer_resource(
                    mc,
                    db,
                    settings,
                    cfg,
                    &TransferRequest {
                        slug: it.slug,
                        title,
                        tmdb_id: tid,
                        media_type: mtype,
                        source_type: "telegram".into(),
                        channel_name: it.channel_name,
                        only_missing_episodes: false,
                        trigger_reason: "TG洗版频道高分升级".into(),
                        ..Default::default()
                    },
                ) {
                    if res.status == "success" || res.status == "reviewing" {
                        db.mark_in_flight(&wash_key);
                        triggered += 1;
                    }
                }
                continue;
            }

            // Bit 64: 入新 for uncollected / missing-episode items
            if (lvl & 64) == 0 {
                continue;
            }
            if mtype == "movie"
                && db.is_in_flight(&format!("movie:{}", tid), IN_FLIGHT_CHASE_TTL_SECS)
            {
                continue;
            }
            if let Ok(res) = transfer_resource(
                mc,
                db,
                settings,
                cfg,
                &TransferRequest {
                    slug: it.slug,
                    title,
                    tmdb_id: tid,
                    media_type: mtype.clone(),
                    source_type: "telegram".into(),
                    channel_name: it.channel_name,
                    only_missing_episodes: mtype == "tv",
                    trigger_reason: "TG入新频道自动进货".into(),
                    ..Default::default()
                },
            ) {
                if res.status == "success" || res.status == "reviewing" {
                    triggered += 1;
                }
            }
        }
    }
    triggered
}

/* ==================== Helpers ==================== */

pub fn is_candidate_compatible_with_target(
    cand: &ResourceItem,
    target_mtype: &str,
    target_title: &str,
    target_tmdb_id: &str,
    season_totals: &HashMap<u32, u32>,
    sub_expected_eps: u32,
    strict_title: bool,
) -> bool {
    let tid = target_tmdb_id.trim();
    let cand_tid = cand.tmdb_id.trim();
    if !tid.is_empty() && !cand_tid.is_empty() && cand_tid != tid {
        return false;
    }

    let ep_blob = format!("{} {} {}", cand.parsed_episodes, cand.title, cand.raw_title);
    let ep_pairs = rules::extract_episode_pairs(&ep_blob, 1);
    let mtype = target_mtype.trim().to_ascii_lowercase();

    if mtype == "movie" {
        if !ep_pairs.is_empty() || cand.media_type.trim().eq_ignore_ascii_case("tv") {
            return false;
        }
        let raw_head = cand.raw_title.lines().next().unwrap_or("");
        if raw_head.contains("电视剧") || raw_head.contains("短剧") || raw_head.contains("连续剧") {
            return false;
        }
    } else if mtype == "tv" {
        if cand.media_type.trim().eq_ignore_ascii_case("movie") && ep_pairs.is_empty() {
            return false;
        }
        // Guard against 60–120 ep vertical short dramas colliding with standard TV dramas of the same name
        for p in &ep_pairs {
            let known_max = season_totals
                .get(&p.season)
                .copied()
                .unwrap_or(if p.season <= 1 { sub_expected_eps } else { 0 });
            if known_max >= 4 && known_max <= 55 && p.episode > known_max + 4 {
                return false;
            }
        }
    }

    if strict_title && cand_tid.is_empty() && !target_title.trim().is_empty() {
        let want_norm = rules::normalize_title_for_match(target_title);
        let cand_pure = rules::extract_pure_title(&cand.title);
        let cand_norm = rules::normalize_title_for_match(if cand_pure.is_empty() {
            &cand.title
        } else {
            &cand_pure
        });
        if !want_norm.is_empty() && !cand_norm.is_empty() && cand_norm != want_norm {
            return false;
        }
    }

    true
}

fn file_base_stem_lower(name: &str) -> String {
    let trimmed = name.trim();
    match trimmed.rsplit_once('.') {
        Some((stem, _)) if !stem.trim().is_empty() => stem.trim().to_lowercase(),
        _ => trimmed.to_lowercase(),
    }
}

fn subtitle_matches_video_stems(sub_node: &ShareFileNode, video_stems: &[(String, String)]) -> bool {
    if video_stems.is_empty() {
        return false;
    }
    let sub_low = sub_node.name.trim().to_lowercase();
    video_stems.iter().any(|(vid_parent, vid_stem)| {
        vid_parent == &sub_node.parent_id
            && !vid_stem.is_empty()
            && (sub_low == *vid_stem
                || sub_low.starts_with(&format!("{}.", vid_stem))
                || sub_low.starts_with(&format!("{}_", vid_stem))
                || sub_low.starts_with(&format!("{}-", vid_stem))
                || sub_low.starts_with(&format!("{} ", vid_stem)))
    })
}

fn annotate_missing_on_nodes(
    nodes: &[ShareFileNode],
    mtype: &str,
    snap: &LibrarySnapshot,
) -> (Vec<ShareFileNode>, usize) {
    let mut out = Vec::with_capacity(nodes.len());
    let mut missing = 0usize;
    let mut missing_ep_pairs: HashSet<EpisodePair> = HashSet::new();
    let mut missing_video_stems = Vec::new();

    for n in nodes {
        let mut c = n.clone();
        if c.is_video || c.is_subtitle {
            let pairs = extract_file_node_episodes(&c.path, &c.name);
            c.episode_label = rules::format_episode_pairs(&pairs);
            if c.is_video {
                c.is_missing_in_db = is_node_missing_in_library(mtype, &pairs, snap);
                if c.is_missing_in_db {
                    missing += 1;
                    for p in pairs {
                        missing_ep_pairs.insert(p);
                    }
                    let stem = file_base_stem_lower(&c.name);
                    if !stem.is_empty() {
                        missing_video_stems.push((c.parent_id.clone(), stem));
                    }
                }
            }
        }
        out.push(c);
    }

    for c in &mut out {
        if c.is_subtitle {
            let sub_pairs = if !c.episode_label.is_empty() {
                rules::extract_episode_pairs(&c.episode_label, 1)
            } else {
                Vec::new()
            };
            c.is_missing_in_db = (!sub_pairs.is_empty()
                && sub_pairs.iter().any(|p| missing_ep_pairs.contains(p)))
                || (sub_pairs.is_empty()
                    && subtitle_matches_video_stems(c, &missing_video_stems));
        }
    }

    (out, missing)
}

fn is_node_missing_in_library(mtype: &str, pairs: &[EpisodePair], snap: &LibrarySnapshot) -> bool {
    if mtype == "movie" {
        return !snap.in_library;
    }
    if pairs.is_empty() {
        return true;
    }
    pairs.iter().any(|p| !snap.local_episodes.contains(p))
}

fn enqueue_reviewing(
    db: &Db,
    req: &TransferRequest,
    share_code: &str,
    receive_code: &str,
    target_cid: &str,
) {
    let clean_key = if !share_code.is_empty() {
        format!("115:{}", share_code.to_lowercase())
    } else {
        rules::clean_slug(&req.slug)
    };
    let now_ts = chrono::Utc::now().timestamp();
    let existing = db
        .list_reviewing()
        .into_iter()
        .find(|x| x.clean_slug == clean_key);
    let item = if let Some(mut prev) = existing {
        prev.retry_count += 1;
        prev.last_tried_ts = now_ts;
        prev
    } else {
        ReviewingItem {
            slug: req.slug.clone(),
            clean_slug: clean_key,
            share_code: share_code.to_string(),
            receive_code: receive_code.to_string(),
            title: if req.title.is_empty() {
                share_code.to_string()
            } else {
                req.title.clone()
            },
            tmdb_id: req.tmdb_id.clone(),
            media_type: req.media_type.clone(),
            source_type: if req.source_type.is_empty() {
                "115".into()
            } else {
                req.source_type.clone()
            },
            target_cid: target_cid.to_string(),
            retry_count: 0,
            last_error: String::new(),
            created_ts: now_ts,
            last_tried_ts: now_ts,
        }
    };
    let _ = db.upsert_reviewing(&item);
}

fn mark_transferred(
    db: &Db,
    cfg: &mut AppConfig,
    req: &TransferRequest,
    eff_slug: &str,
    title: &str,
    source: &str,
    eps: &str,
    action: &str,
    detail: &str,
) {
    let orig_ck = rules::clean_slug(&req.slug);
    let eff_ck = rules::clean_slug(eff_slug);
    let now_ts = chrono::Utc::now().timestamp();
    if !orig_ck.is_empty() {
        let _ = db.mark_slug_transferred(&orig_ck, now_ts);
    }
    if !eff_ck.is_empty() && eff_ck != orig_ck {
        let _ = db.mark_slug_transferred(&eff_ck, now_ts);
    }
    let tid = req.tmdb_id.trim();
    if !tid.is_empty() {
        let is_movie = req.media_type.trim().eq_ignore_ascii_case("movie")
            || (req.media_type.trim().is_empty() && eps.trim().is_empty());
        if is_movie {
            db.mark_in_flight(&format!("movie:{}", tid));
        } else {
            let pairs = rules::extract_episode_pairs(eps, 1);
            if pairs.is_empty() {
                db.mark_in_flight(&format!("tv:{}:ALL", tid));
            } else {
                for p in pairs {
                    db.mark_in_flight(&format!("tv:{}:{}", tid, p.key()));
                }
            }
        }
    }
    cfg.transferred_count += 1;

    let channel_name = &req.channel_name;
    let mut ch_norm = String::new();
    for ck_cand in [&orig_ck, &eff_ck] {
        if !ck_cand.is_empty() {
            if let Some(found) = db.find_tg_channel_by_slug(ck_cand) {
                ch_norm = found;
                break;
            }
        }
    }
    if ch_norm.is_empty() {
        ch_norm = providers::normalize_tg_channel(channel_name).to_lowercase();
    }
    let clean_disp = channel_name
        .trim()
        .trim_start_matches("TG · ")
        .trim_start_matches('@')
        .trim();
    if !ch_norm.is_empty() || !clean_disp.is_empty() {
        for ch in &mut cfg.telegram.channels {
            let id_match = !ch_norm.is_empty()
                && providers::normalize_tg_channel(&ch.identifier).to_lowercase() == ch_norm;
            let name_match = !clean_disp.is_empty() && ch.name.trim().eq_ignore_ascii_case(clean_disp);
            if id_match || name_match {
                ch.transferred_count += 1;
                break;
            }
        }
    }
    let _ = db.append_activity(crate::models::ActivityRecord {
        record_type: "transfer".into(),
        title: title.trim().to_string(),
        source: source.to_string(),
        channel_name: channel_name.trim().to_string(),
        slug: eff_slug.trim().to_string(),
        clean_slug: if !eff_ck.is_empty() { eff_ck } else { orig_ck },
        tmdb_id: req.tmdb_id.trim().to_string(),
        media_type: req.media_type.trim().to_string(),
        episodes: eps.to_string(),
        action: action.to_string(),
        status: "success".into(),
        detail: detail.to_string(),
        ..Default::default()
    });
}

fn short_target_dir_name(mc: &HostClient, cfg: &AppConfig, cid: &str) -> String {
    let c = cid.trim();
    if c.is_empty() {
        return "FlowLink".to_string();
    }
    if cfg.rules.default_target_cid.trim() == c && !cfg.rules.default_target_name.trim().is_empty()
    {
        return cfg.rules.default_target_name.trim().to_string();
    }
    let hs = mc.get_host_status();
    for (known_cid, known_name) in [
        (hs.transfer_source_cid.as_str(), hs.transfer_source_folder.as_str()),
        (hs.target_cid.as_str(), hs.target_folder.as_str()),
        (hs.offline_folder_cid.as_str(), hs.offline_folder.as_str()),
    ] {
        if known_cid.trim() == c && !known_name.trim().is_empty() {
            return known_name.trim().to_string();
        }
    }
    format!("CID:{}", c)
}

fn extract_file_node_episodes(path_ctx: &str, file_name: &str) -> Vec<EpisodePair> {
    let def_s = rules::infer_season_num(path_ctx, 1);
    let from_name = rules::extract_episode_pairs(file_name, def_s);
    if !from_name.is_empty() {
        return from_name;
    }
    if path_ctx.trim().is_empty() {
        return Vec::new();
    }
    // Only fall back to folder path if folder encodes a single episode (not `全60集` pack header!)
    let from_path = rules::extract_episode_pairs(path_ctx, def_s);
    if from_path.len() <= 2 {
        from_path
    } else {
        Vec::new()
    }
}

fn is_generic_folder_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() || rules::is_suspect_banner_title(trimmed) {
        return true;
    }
    let low = trimmed.to_lowercase();
    matches!(
        low.as_str(),
        "video" | "videos" | "movie" | "movies" | "tv" | "bdmv" | "stream" | "subs" | "subtitles"
            | "4k" | "1080p" | "2160p" | "国语" | "粤语" | "原盘" | "正片" | "花絮"
    ) || low.starts_with("season ")
        || (low.starts_with('s') && low[1..].chars().all(|c| c.is_ascii_digit()))
        || (trimmed.starts_with('第') && trimmed.ends_with('季'))
}

fn infer_share_identity(
    req_title: &str,
    req_tmdb_id: &str,
    inspected: &InspectResponse,
) -> (String, String) {
    let mut best_tid = req_tmdb_id.trim().to_string();
    if best_tid.is_empty() {
        best_tid = rules::extract_tmdb_id(req_title);
    }
    if best_tid.is_empty() {
        best_tid = rules::extract_tmdb_id(&inspected.precise_title);
    }
    if best_tid.is_empty() {
        for f in &inspected.files {
            let tid = rules::extract_tmdb_id(&f.name);
            if !tid.is_empty() {
                best_tid = tid;
                break;
            }
            let ptid = rules::extract_tmdb_id(&f.path);
            if !ptid.is_empty() {
                best_tid = ptid;
                break;
            }
        }
    }

    let mut best_title = String::new();
    if !req_title.trim().is_empty() && !rules::is_suspect_banner_title(req_title) {
        let p = rules::extract_pure_title(req_title);
        best_title = if !p.is_empty() && !rules::is_suspect_banner_title(&p) {
            p
        } else {
            rules::clean_display_title(req_title)
        };
    }
    if best_title.is_empty() || rules::is_suspect_banner_title(&best_title) {
        let prec_clean = rules::clean_display_title(&inspected.precise_title);
        if !prec_clean.is_empty() && !rules::is_suspect_banner_title(&prec_clean) {
            let p = rules::extract_pure_title(&prec_clean);
            best_title = if !p.is_empty() { p } else { prec_clean };
        }
    }
    if best_title.is_empty() || rules::is_suspect_banner_title(&best_title) {
        for f in &inspected.files {
            if f.is_dir && !is_generic_folder_name(&f.name) {
                let p = rules::extract_pure_title(&f.name);
                if !p.is_empty() && !rules::is_suspect_banner_title(&p) {
                    best_title = p;
                    break;
                }
            }
        }
    }
    if best_title.is_empty() || rules::is_suspect_banner_title(&best_title) {
        for f in &inspected.files {
            if f.is_video && !f.name.trim().is_empty() {
                let p = rules::extract_pure_title(&f.name);
                if !p.is_empty() && !rules::is_suspect_banner_title(&p) {
                    best_title = p;
                    break;
                }
            }
        }
    }

    (best_title, best_tid)
}

fn summarize_transferred_files(
    files: &[ShareFileNode],
    selected_ids: &[String],
    fallback_episodes: &str,
    media_type: &str,
) -> (String, i64, String) {
    let sel_set: HashSet<&str> = selected_ids.iter().map(String::as_str).collect();
    let mut total_bytes = 0i64;
    let mut ep_pairs = Vec::new();
    let mut sample_video_name = String::new();
    let mut video_count = 0usize;
    for f in files {
        if f.is_dir {
            continue;
        }
        if !sel_set.is_empty() && !sel_set.contains(f.id.as_str()) {
            continue;
        }
        if sel_set.is_empty() && !f.is_video && !f.is_subtitle {
            continue;
        }
        total_bytes += f.size.max(0);
        if f.is_video {
            video_count += 1;
            if sample_video_name.is_empty() && !f.name.trim().is_empty() {
                sample_video_name = f.name.trim().to_string();
            }
            ep_pairs.extend(extract_file_node_episodes(&f.path, &f.name));
        }
    }
    if video_count > 1 && !sample_video_name.is_empty() {
        sample_video_name = format!("{} 等{}个视频", sample_video_name, video_count);
    }
    let ep_str = if media_type.eq_ignore_ascii_case("movie") && sel_set.is_empty() {
        String::new()
    } else if !ep_pairs.is_empty() {
        rules::format_episode_pairs(&ep_pairs)
    } else {
        fallback_episodes.trim().to_string()
    };
    (ep_str, total_bytes, sample_video_name)
}

fn format_source_site_label(source_type: &str, channel_name: &str) -> String {
    let ch = channel_name.trim();
    if !ch.is_empty() && ch != "手动直链" {
        if source_type.eq_ignore_ascii_case("telegram") && !ch.starts_with("TG") {
            return format!("TG · {}", ch);
        }
        return ch.to_string();
    }
    match source_type.trim().to_ascii_lowercase().as_str() {
        "telegram" => "Telegram 频道".to_string(),
        "hdhive" => "影巢 HDHive".to_string(),
        "ayclub" => "AYClub".to_string(),
        "shield" => "私有神盾".to_string(),
        "rss" => "RSS 订阅".to_string(),
        "115" => "115 网盘".to_string(),
        "ed2k" => "ED2K 离线".to_string(),
        "magnet" => "磁力离线".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => "MediFind".to_string(),
    }
}

struct TransferNotifyInput<'a> {
    req: &'a TransferRequest,
    raw_title: &'a str,
    resource_title: &'a str,
    source_type: &'a str,
    season_episode: &'a str,
    download_action: &'a str,
    size_str: &'a str,
    target_hint: &'a str,
}

fn notify_transfer_templated(
    mc: &HostClient,
    settings: &PluginSettings,
    cfg: &AppConfig,
    inp: &TransferNotifyInput<'_>,
) {
    if !settings.notify {
        return;
    }
    let host_notify_on = cfg.notifications.enabled && cfg.notifications.on_transfer;
    let has_nextbot_notify = cfg.notifications.telegram_bots.iter().any(|b| {
        !b.is_paused
            && b.library_notification_enabled
            && !b.bot_token.trim().is_empty()
            && !b.chat_id.trim().is_empty()
    });
    if !host_notify_on && !has_nextbot_notify {
        return;
    }
    let tpls = if host_notify_on {
        Some(mc.get_notification_templates())
    } else {
        None
    };
    let should_send_host = host_notify_on
        && tpls
            .as_ref()
            .map(|t| t.download_started.enabled)
            .unwrap_or(false);
    if !should_send_host && !has_nextbot_notify {
        return;
    }

    let mut clean_name = rules::clean_display_title(inp.raw_title);
    if !rules::is_suspect_banner_title(&clean_name) {
        let pure_cand = rules::extract_pure_title(&clean_name);
        if !pure_cand.is_empty() && !rules::is_suspect_banner_title(&pure_cand) {
            clean_name = pure_cand;
        }
    } else {
        let res_pure = rules::extract_pure_title(inp.resource_title);
        if !res_pure.is_empty() && !rules::is_suspect_banner_title(&res_pure) {
            clean_name = res_pure;
        }
    }

    let mut tid = inp.req.tmdb_id.trim().to_string();
    if tid.is_empty() {
        tid = rules::extract_tmdb_id(inp.raw_title);
    }
    if tid.is_empty() {
        tid = rules::extract_tmdb_id(inp.resource_title);
    }

    let mut year = rules::extract_year_str(inp.raw_title);
    if year.is_empty() {
        year = rules::extract_year_str(inp.resource_title);
    }
    let mut mtype = if !inp.req.media_type.trim().is_empty() {
        inp.req.media_type.trim().to_ascii_lowercase()
    } else if !inp.season_episode.trim().is_empty() {
        "tv".to_string()
    } else {
        "movie".to_string()
    };
    let mut poster_url: Option<String> = None;
    let mut nb_posters: Vec<String> = Vec::new();
    let mut nb_vote = String::new();
    let mut nb_theme = String::new();
    let mut nb_region = String::new();
    let mut nb_overview = String::new();

    if !tid.is_empty() {
        let alt_mtype = if mtype == "tv" { "movie" } else { "tv" };
        let mut det_res = mc.get_tmdb_details(&tid, &mtype).ok();
        if det_res
            .as_ref()
            .map(|d| d.title.trim().is_empty() && d.name.trim().is_empty())
            .unwrap_or(true)
            && inp.season_episode.trim().is_empty()
        {
            if let Ok(alt_det) = mc.get_tmdb_details(&tid, alt_mtype) {
                if !alt_det.title.trim().is_empty() || !alt_det.name.trim().is_empty() {
                    mtype = alt_mtype.to_string();
                    det_res = Some(alt_det);
                }
            }
        }
        if let Some(det) = det_res {
            let det_date = if !det.release_date.trim().is_empty() {
                det.release_date.trim()
            } else {
                det.first_air_date.trim()
            };
            if det_date.len() >= 4 && det_date[..4].chars().all(|c| c.is_ascii_digit()) {
                year = det_date[..4].to_string();
            }
            let det_title = if !det.title.trim().is_empty() {
                det.title.trim()
            } else {
                det.name.trim()
            };
            if !det_title.is_empty()
                && (rules::is_suspect_banner_title(&clean_name)
                    || clean_name.chars().all(|c| c.is_ascii()))
            {
                clean_name = det_title.to_string();
            }
            if det.vote_average > 0.0 {
                nb_vote = format!("{:.1}", det.vote_average);
            }
            let gnames: Vec<String> = det.genres.iter().map(|g| g.name.clone()).collect();
            nb_theme = crate::nextbot::map_tmdb_genres_to_theme(&det.genre_ids, &gnames);
            nb_region = crate::nextbot::map_tmdb_country_to_region(
                &det.origin_country,
                &det.original_language,
            )
            .to_string();
            nb_overview = det.overview.clone();
            if let Some(p) =
                crate::host::tmdb_card_image_https(&det.backdrop_path, &det.poster_path)
            {
                if cfg.notifications.include_poster {
                    poster_url = Some(p.clone());
                }
                nb_posters.push(p);
            }
            if let Some(p2) = crate::host::tmdb_poster_https(&det.poster_path) {
                if !nb_posters.contains(&p2) {
                    nb_posters.push(p2);
                }
            }
        }
    }

    if poster_url.is_none() || year.is_empty() || rules::is_suspect_banner_title(&clean_name) {
        let mut cand_queries = Vec::new();
        if !clean_name.is_empty() && !rules::is_suspect_banner_title(&clean_name) {
            cand_queries.push(clean_name.clone());
        }
        let res_pure = rules::extract_pure_title(inp.resource_title);
        if !res_pure.is_empty()
            && !rules::is_suspect_banner_title(&res_pure)
            && !cand_queries.contains(&res_pure)
        {
            cand_queries.push(res_pure);
        }

        let yr_num = year.parse::<i32>().unwrap_or(0);
        for q in &cand_queries {
            let mut hit_item: Option<crate::host::TMDBMediaItem> =
                mc.resolve_tmdb(q, &mtype, yr_num).ok();
            if hit_item.is_none() && yr_num > 0 {
                hit_item = mc.resolve_tmdb(q, &mtype, 0).ok();
            }
            if hit_item.is_none() && inp.season_episode.trim().is_empty() {
                let alt_mtype = if mtype == "tv" { "movie" } else { "tv" };
                hit_item = mc.resolve_tmdb(q, alt_mtype, yr_num).ok();
            }
            if hit_item.is_none() {
                if let Ok(hits) = mc.search_tmdb(q, "all", 1) {
                    hit_item = hits.into_iter().next();
                }
            }

            if let Some(resolved) = hit_item {
                let eff_t = resolved.effective_type().to_string();
                if !eff_t.is_empty() {
                    mtype = eff_t;
                }
                if tid.is_empty() && !resolved.effective_id().is_empty() {
                    tid = resolved.effective_id().to_string();
                }
                let r_year = resolved.effective_year();
                if year.is_empty() && !r_year.is_empty() {
                    year = r_year;
                }
                let r_title = resolved.effective_title();
                if !r_title.is_empty()
                    && (rules::is_suspect_banner_title(&clean_name)
                        || clean_name.chars().all(|c| c.is_ascii()))
                {
                    clean_name = r_title.to_string();
                }
                if nb_vote.is_empty() && resolved.vote_average > 0.0 {
                    nb_vote = format!("{:.1}", resolved.vote_average);
                }
                if nb_overview.is_empty() && !resolved.overview.trim().is_empty() {
                    nb_overview = resolved.overview.clone();
                }
                if nb_theme.is_empty() && !resolved.genre_ids.is_empty() {
                    nb_theme = crate::nextbot::map_tmdb_genres_to_theme(&resolved.genre_ids, &[]);
                }
                if nb_region.is_empty()
                    && (!resolved.origin_country.is_empty()
                        || !resolved.original_language.trim().is_empty())
                {
                    nb_region = crate::nextbot::map_tmdb_country_to_region(
                        &resolved.origin_country,
                        &resolved.original_language,
                    )
                    .to_string();
                }
                if (cfg.notifications.include_poster || has_nextbot_notify)
                    && (poster_url.is_none() || nb_posters.is_empty())
                {
                    // Upgrade to full TMDB details if search/resolve lacked `backdrop_path` so WeCom/NextBOT gets `w780` 16:9 card!
                    if resolved.backdrop_path.trim().is_empty()
                        && !resolved.effective_id().is_empty()
                    {
                        if let Ok(det) =
                            mc.get_tmdb_details(resolved.effective_id(), resolved.effective_type())
                        {
                            if let Some(p) = crate::host::tmdb_card_image_https(
                                &det.backdrop_path,
                                &det.poster_path,
                            ) {
                                if cfg.notifications.include_poster && poster_url.is_none() {
                                    poster_url = Some(p.clone());
                                }
                                if !nb_posters.contains(&p) {
                                    nb_posters.push(p);
                                }
                            }
                        }
                    }
                    if let Some(p) = crate::host::tmdb_card_image_https(
                        &resolved.backdrop_path,
                        &resolved.poster_path,
                    ) {
                        if cfg.notifications.include_poster && poster_url.is_none() {
                            poster_url = Some(p.clone());
                        }
                        if !nb_posters.contains(&p) {
                            nb_posters.push(p);
                        }
                    }
                }
                if poster_url.is_some() && !year.is_empty() {
                    break;
                }
            }
        }
    }

    if clean_name.is_empty() || rules::is_suspect_banner_title(&clean_name) {
        clean_name = "未命名资源".to_string();
    }

    let is_tv = mtype == "tv" || !inp.season_episode.trim().is_empty();
    let media_icon = if is_tv { "📺" } else { "🎬" };
    let display_title = if !year.is_empty() && !clean_name.contains(&format!("({})", year)) {
        format!("{} ({})", clean_name, year)
    } else {
        clean_name.clone()
    };

    let res_base = if !inp.resource_title.trim().is_empty() {
        inp.resource_title.trim()
    } else {
        clean_name.as_str()
    };
    let torrent_title = if !inp.target_hint.trim().is_empty() {
        format!("{} · {}", res_base, inp.target_hint.trim())
    } else {
        res_base.to_string()
    };
    let site = format_source_site_label(inp.source_type, &inp.req.channel_name);

    if should_send_host {
        if let Some(tpls) = tpls {
            let mut vars = HashMap::new();
            vars.insert("media_icon", media_icon.to_string());
            vars.insert("name", clean_name.clone());
            vars.insert("year", year.clone());
            vars.insert("display_title", display_title.clone());
            vars.insert("season_episode", inp.season_episode.trim().to_string());
            vars.insert("download_action", inp.download_action.trim().to_string());
            vars.insert("torrent_title", torrent_title.clone());
            vars.insert("site", site.clone());
            vars.insert("size", inp.size_str.trim().to_string());

            let title = crate::host::render_mediary_template(&tpls.download_started.title, &vars);
            let content =
                crate::host::render_mediary_template(&tpls.download_started.content, &vars);
            let img = if cfg.notifications.include_poster {
                poster_url
                    .as_deref()
                    .or(Some(crate::host::MEDIARY_DEFAULT_NOTIFY_IMAGE))
            } else {
                None
            };
            let _ = mc.notify_with_image(&title, &content, img);
        }
    }

    if has_nextbot_notify {
        let parsed_attrs =
            rules::parse_movie_attributes(&format!("{} {}", inp.raw_title, inp.resource_title));
        let trigger_lbl = if !inp.req.trigger_reason.trim().is_empty() {
            if !inp.target_hint.trim().is_empty() {
                format!(
                    "{} · {}",
                    inp.req.trigger_reason.trim(),
                    inp.target_hint.trim()
                )
            } else {
                inp.req.trigger_reason.trim().to_string()
            }
        } else if !inp.target_hint.trim().is_empty() {
            format!("{} · {}", inp.download_action.trim(), inp.target_hint.trim())
        } else {
            inp.download_action.trim().to_string()
        };
        let nb_data = crate::nextbot::NextBotMessageData {
            notification_type: "入库通知".into(),
            media_type: if is_tv { "tv".into() } else { "movie".into() },
            base_title: display_title,
            ep_part: inp.season_episode.trim().to_string(),
            tmdb_id: tid,
            vote_average: nb_vote,
            theme: nb_theme,
            region: nb_region,
            overview: nb_overview,
            media_tags: crate::nextbot::format_movie_attrs_tags(&parsed_attrs),
            size_str: inp.size_str.trim().to_string(),
            total_size_str: inp.size_str.trim().to_string(),
            trigger_reason_folder: trigger_lbl,
            source: site,
            slug: inp.req.slug.trim().to_string(),
            poster_urls: nb_posters,
            was_encrypted: false,
        };
        let proxy_url = effective_proxy(Some(mc), cfg);
        crate::nextbot::notify_transfer_bots(cfg, &nb_data, &proxy_url);
    }
}

pub fn is_115_reviewing_text(s: &str) -> bool {
    s.contains("审核")
        || s.contains("4100029")
        || s.to_ascii_uppercase().contains("WAITING:REVIEW")
}

fn build_precise_title(title: &str, tmdb_id: &str) -> String {
    let t = if title.trim().is_empty() {
        "未命名资源"
    } else {
        title.trim()
    };
    if !tmdb_id.trim().is_empty() && !t.to_ascii_lowercase().contains("tmdbid") {
        format!("{} [tmdbid-{}]", t, tmdb_id.trim())
    } else {
        t.to_string()
    }
}

fn val_str(v: &Value, k: &str) -> Option<String> {
    match v.get(k)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn val_i64(v: &Value, k: &str) -> Option<i64> {
    match v.get(k)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_archive_tree_and_snap_parse() {
        let best = evaluate_archive_tree(
            &["欧美剧集".into(), "国产剧".into(), "日韩剧".into()],
            &["CN".into()],
            "zh",
        );
        assert_eq!(best, "国产剧");

        let raw = json!({
            "state": true,
            "data": {
                "share_info": { "share_title": "庆余年第二季 4K" },
                "list": [
                    { "fid": "101", "n": "Joy.of.Life.S02E01.2160p.mkv", "s": 4294967296i64 },
                    { "fid": "102", "n": "Joy.of.Life.S02E02.2160p.mkv", "s": 4294967296i64 }
                ]
            }
        });
        let (title, nodes, reviewing, err) = parse_snap_response(&raw, "0", "", 0);
        assert!(!reviewing && err.is_empty());
        assert_eq!(title, "庆余年第二季 4K");
        assert_eq!(nodes.len(), 2);

        let mut snap = LibrarySnapshot {
            media_type: "tv".into(),
            ..Default::default()
        };
        snap.local_episodes.insert(EpisodePair { season: 2, episode: 1 });
        let mixed_nodes = vec![
            ShareFileNode {
                id: "101".into(),
                parent_id: "d1".into(),
                name: "Joy.of.Life.S02E01.2160p.mkv".into(),
                is_video: true,
                ..Default::default()
            },
            ShareFileNode {
                id: "102".into(),
                parent_id: "d1".into(),
                name: "Joy.of.Life.S02E01.zh.srt".into(),
                is_subtitle: true,
                ..Default::default()
            },
            ShareFileNode {
                id: "103".into(),
                parent_id: "d1".into(),
                name: "Joy.of.Life.S02E02.2160p.mkv".into(),
                is_video: true,
                ..Default::default()
            },
            ShareFileNode {
                id: "104".into(),
                parent_id: "d1".into(),
                name: "Joy.of.Life.S02E02.zh.srt".into(),
                is_subtitle: true,
                ..Default::default()
            },
        ];
        let (annotated, missing) = annotate_missing_on_nodes(&mixed_nodes, "tv", &snap);
        assert_eq!(missing, 1);
        assert!(!annotated[0].is_missing_in_db); // S02E01 video in db
        assert!(!annotated[1].is_missing_in_db); // S02E01 subtitle NOT selected!
        assert!(annotated[2].is_missing_in_db);  // S02E02 video missing
        assert!(annotated[3].is_missing_in_db);  // S02E02 subtitle selected!

        // P0 #1: Manual TG channel (level=0) blocked from auto-chase while level=16/112/99 allowed
        let mut app_cfg = AppConfig::default();
        app_cfg.telegram.enabled = true;
        app_cfg.telegram.channels = vec![
            crate::models::TGChannelEntry {
                id: "c1".into(),
                identifier: "manual_ch".into(),
                name: "星河115 影视资源免费频道".into(),
                level: 0,
                enabled: true,
                ..Default::default()
            },
            crate::models::TGChannelEntry {
                id: "c2".into(),
                identifier: "chase_ch".into(),
                name: "115影视资源分享频道".into(),
                level: 112,
                enabled: true,
                ..Default::default()
            },
        ];
        let manual_cand = ResourceItem {
            source_type: "telegram".into(),
            channel_name: "TG · 星河115 影视资源免费频道".into(),
            ..Default::default()
        };
        let chase_cand = ResourceItem {
            source_type: "telegram".into(),
            channel_name: "TG · 115影视资源分享频道".into(),
            ..Default::default()
        };
        assert!(!app_cfg.is_tg_channel_chase_allowed(&manual_cand));
        assert!(app_cfg.is_tg_channel_chase_allowed(&chase_cand));

        // P0 #3: Movie vs short-drama cross-type & 29-ep TV vs 60-ep short-drama guard
        let short_drama_cand = ResourceItem {
            title: "我没错！".into(),
            parsed_episodes: "S01E01-E06".into(),
            media_type: "tv".into(),
            ..Default::default()
        };
        assert!(!is_candidate_compatible_with_target(
            &short_drama_cand,
            "movie",
            "我没错！",
            "1821748",
            &HashMap::new(),
            0,
            true,
        ));

        let mut season_totals = HashMap::new();
        season_totals.insert(1, 29);
        let overflow_short = ResourceItem {
            title: "骨鉴卿心".into(),
            parsed_episodes: "S01E01-E60".into(),
            media_type: "tv".into(),
            ..Default::default()
        };
        assert!(!is_candidate_compatible_with_target(
            &overflow_short,
            "tv",
            "骨鉴卿心",
            "999999",
            &season_totals,
            29,
            true,
        ));

        // P0 #4: Share identity recovery when TG post title was a banner (`✅新片送达–请您检阅`) + selective `S01E11 - 2160p` episode accuracy
        let banner_inspected = InspectResponse {
            precise_title: "✅新片送达–请您检阅".into(),
            files: vec![
                ShareFileNode {
                    id: "d1".into(),
                    name: "欢迎来龙餐馆".into(),
                    is_dir: true,
                    ..Default::default()
                },
                ShareFileNode {
                    id: "f1".into(),
                    name: "Once.Upon.a.Time.in.the.Middle.East.2026.2160p.YOUKU.WEB-DL.mkv".into(),
                    path: "欢迎来龙餐馆/Once.Upon.a.Time.in.the.Middle.East.2026.2160p.YOUKU.WEB-DL.mkv".into(),
                    size: 58_000_000_000,
                    is_video: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (rec_title, _) =
            infer_share_identity("✅新片送达–请您检阅", "", &banner_inspected);
        assert_eq!(rec_title, "欢迎来龙餐馆");

        let tmdbid_inspected = InspectResponse {
            precise_title: "✅新片送达–请您检阅".into(),
            files: vec![ShareFileNode {
                id: "f2".into(),
                name: "欢迎来龙餐馆(2026)(tmdbid=1391021).2160p.WEB-DL.mp4".into(),
                size: 36_000_000_000,
                is_video: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let (rec_title2, rec_tid2) =
            infer_share_identity("✅新片送达–请您检阅", "", &tmdbid_inspected);
        assert_eq!(rec_title2, "欢迎来龙餐馆");
        assert_eq!(rec_tid2, "1391021");

        let shunu_files = vec![ShareFileNode {
            id: "ep11".into(),
            name: "庶女 (2026) - S01E11 - 2160p.WEB-DL.H265.AAC.mp4".into(),
            path: "庶女 (2026) 全60集/庶女 (2026) - S01E11 - 2160p.WEB-DL.H265.AAC.mp4".into(),
            size: 369_623_040,
            is_video: true,
            ..Default::default()
        }];
        let (ep_sum, sz_sum, _) = summarize_transferred_files(
            &shunu_files,
            &["ep11".to_string()],
            "S01E01-E60",
            "tv",
        );
        assert_eq!(ep_sum, "S01E11");
        assert_eq!(sz_sum, 369_623_040);

        // Verify double-episode video matches single-episode subtitle via EpisodePair intersection
        let multi_ep_nodes = vec![
            ShareFileNode {
                id: "v1".into(),
                name: "Show.S01E01-E02.2160p.WEB-DL.mkv".into(),
                is_video: true,
                ..Default::default()
            },
            ShareFileNode {
                id: "s1".into(),
                name: "Show.S01E01.chs.ass".into(),
                is_subtitle: true,
                ..Default::default()
            },
            ShareFileNode {
                id: "s3".into(),
                name: "Show.S01E03.chs.ass".into(),
                is_subtitle: true,
                ..Default::default()
            },
        ];
        let snap_empty = LibrarySnapshot {
            media_type: "tv".into(),
            ..Default::default()
        };
        let (annotated, miss_cnt) = annotate_missing_on_nodes(&multi_ep_nodes, "tv", &snap_empty);
        assert_eq!(miss_cnt, 1);
        assert!(annotated[0].is_missing_in_db);
        assert!(annotated[1].is_missing_in_db, "S01E01 sub must match S01E01-E02 missing video");
        assert!(!annotated[2].is_missing_in_db, "S01E03 sub must not match S01E01-E02");

        // Verify ActivityRecord source_group & action_group classification
        let manual_rec = crate::models::ActivityRecord {
            source: "115".into(),
            channel_name: "手动直链".into(),
            action: "手动转存".into(),
            ..Default::default()
        };
        assert_eq!(manual_rec.source_group(), "manual");
        assert_eq!(manual_rec.action_group(), "manual");

        let wash_rec = crate::models::ActivityRecord {
            source: "telegram".into(),
            channel_name: "4K洗版频道".into(),
            action: "TG洗版频道高分升级".into(),
            ..Default::default()
        };
        assert_eq!(wash_rec.source_group(), "telegram");
        assert_eq!(wash_rec.action_group(), "tg_auto");
    }
}

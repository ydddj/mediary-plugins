use crate::db::Db;
use crate::host::{HostClient, TMDBDetails, TMDBMediaItem};
use crate::providers;
use crate::rules;
use anyhow::Result;
use chrono::Local;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DiscoverRequest {
    pub query: String,
    #[serde(alias = "type")]
    pub media_type: String,
    pub sort: String,
    #[serde(alias = "language")]
    pub region: String,
    pub year: String,
    pub genre: String,
    pub status: String,
    pub troubleshoot: String,
    pub channel: String,
    pub page: usize,
    pub page_size: usize,
}

pub fn normalize_type_filter(raw: &str) -> &'static str {
    match raw.trim().to_lowercase().as_str() {
        "movie" | "电影" => "movie",
        "tv" | "series" | "show" | "电视剧" | "剧集" | "动漫" => "tv",
        _ => "all",
    }
}

pub fn matches_media_type(item: &TMDBMediaItem, eff_type: &str) -> bool {
    match eff_type {
        "movie" => item.effective_type() == "movie",
        "tv" => item.effective_type() == "tv",
        _ => true,
    }
}

pub fn matches_year(item: &TMDBMediaItem, year_filter: &str) -> bool {
    let yf = year_filter.trim();
    if yf.is_empty() || yf == "全部" || yf.eq_ignore_ascii_case("all") {
        return true;
    }
    let y_str = item.effective_year();
    let Ok(y) = y_str.parse::<i32>() else {
        return false;
    };
    match yf {
        "更早" | "80年代及以前" | "80年代前" => y <= 1989,
        "90年代" => (1990..=1999).contains(&y),
        "2000年代" => (2000..=2009).contains(&y),
        "2010年代" => (2010..=2019).contains(&y),
        exact => y_str == exact,
    }
}

pub fn year_date_bounds(year_filter: &str) -> Option<(Option<String>, Option<String>)> {
    let yf = year_filter.trim();
    if yf.is_empty() || yf == "全部" || yf.eq_ignore_ascii_case("all") {
        return None;
    }
    match yf {
        "更早" | "80年代及以前" | "80年代前" => Some((None, Some("1989-12-31".into()))),
        "90年代" => Some((Some("1990-01-01".into()), Some("1999-12-31".into()))),
        "2000年代" => Some((Some("2000-01-01".into()), Some("2009-12-31".into()))),
        "2010年代" => Some((Some("2010-01-01".into()), Some("2019-12-31".into()))),
        exact if exact.len() == 4 && exact.chars().all(|c| c.is_ascii_digit()) => Some((
            Some(format!("{}-01-01", exact)),
            Some(format!("{}-12-31", exact)),
        )),
        _ => None,
    }
}

pub fn region_countries_and_langs(
    region_filter: &str,
) -> Option<(&'static [&'static str], &'static [&'static str])> {
    match region_filter.trim() {
        "" | "全部" | "all" | "ALL" => None,
        "欧美" => Some((
            &[
                "US", "GB", "CA", "FR", "DE", "IT", "ES", "AU", "NL", "SE", "NO", "DK", "BE", "IE",
                "CH", "AT", "PL", "RU",
            ],
            &["en", "fr", "de", "es", "it", "ru", "pt"],
        )),
        "大陆" => Some((&["CN"], &["zh"])),
        "港台" => Some((&["HK", "TW", "MO"], &["cn", "yue"])),
        "韩国" => Some((&["KR"], &["ko"])),
        "日本" => Some((&["JP"], &["ja"])),
        "亚太" => Some((
            &["IN", "TH", "SG", "MY", "ID", "VN", "PH", "NZ"],
            &["th", "hi", "id", "vi", "ms", "tl", "ta"],
        )),
        _ => None,
    }
}

pub fn matches_region(item: &TMDBMediaItem, region_filter: &str) -> bool {
    let Some((countries, langs)) = region_countries_and_langs(region_filter) else {
        return true;
    };
    if !item.origin_country.is_empty() {
        if item
            .origin_country
            .iter()
            .any(|c| countries.iter().any(|want| c.trim().eq_ignore_ascii_case(want)))
        {
            return true;
        }
        if matches!(region_filter.trim(), "大陆" | "港台") {
            return false;
        }
    }
    let ol = item.original_language.trim().to_ascii_lowercase();
    !ol.is_empty() && langs.iter().any(|l| *l == ol)
}

pub fn genre_ids_for_filter(genre_filter: &str, mtype: &str) -> &'static [i32] {
    let is_tv = mtype == "tv";
    match genre_filter.trim() {
        "剧情" => &[18],
        "动作冒险" => {
            if is_tv {
                &[10759, 28, 12]
            } else {
                &[28, 12]
            }
        }
        "喜剧" => &[35],
        "科幻奇幻" => {
            if is_tv {
                &[10765, 878, 14]
            } else {
                &[878, 14]
            }
        }
        "爱情家庭" => &[10749, 10751],
        "惊悚犯罪" => {
            if is_tv {
                &[80, 9648]
            } else {
                &[53, 80]
            }
        }
        "恐怖悬疑" => {
            if is_tv {
                &[9648]
            } else {
                &[27, 9648]
            }
        }
        "动漫" => &[16],
        "少儿" => {
            if is_tv {
                &[10762, 10751]
            } else {
                &[10751]
            }
        }
        "真人秀" => {
            if is_tv {
                &[10764, 10767]
            } else {
                &[99]
            }
        }
        "纪录片" => &[99],
        "战争历史" => {
            if is_tv {
                &[10768, 36, 10752]
            } else {
                &[10752, 36]
            }
        }
        "音乐会" => &[10402],
        _ => &[],
    }
}

pub fn matches_genre(item: &TMDBMediaItem, genre_filter: &str) -> bool {
    let gf = genre_filter.trim();
    if gf.is_empty() || gf == "全部" || gf.eq_ignore_ascii_case("all") {
        return true;
    }
    let want = genre_ids_for_filter(gf, item.effective_type());
    if want.is_empty() {
        return true;
    }
    if item.genre_ids.is_empty() {
        return false;
    }
    item.genre_ids.iter().any(|g| want.contains(g))
}

fn matches_all_metadata_filters(
    item: &TMDBMediaItem,
    eff_type: &str,
    region: &str,
    year: &str,
    genre: &str,
    sort: &str,
    query: &str,
) -> bool {
    if !matches_media_type(item, eff_type) {
        return false;
    }
    if !matches_year(item, year) {
        return false;
    }
    if !matches_region(item, region) {
        return false;
    }
    if !matches_genre(item, genre) {
        return false;
    }
    if item.effective_type() == "tv" {
        let st_low = item.status.trim().to_ascii_lowercase();
        if sort == "连载" && matches!(st_low.as_str(), "ended" | "canceled" | "已完结" | "完结") {
            return false;
        }
        if sort == "完结"
            && !st_low.is_empty()
            && !matches!(st_low.as_str(), "ended" | "canceled" | "已完结" | "完结")
        {
            return false;
        }
    }
    let q = query.trim().to_lowercase();
    if !q.is_empty() {
        let hay = format!(
            "{} {} {} {}",
            item.effective_title(),
            item.original_title,
            item.effective_id(),
            item.overview
        )
        .to_lowercase();
        if !hay.contains(&q) {
            return false;
        }
    }
    true
}

fn release_date_key(item: &TMDBMediaItem) -> String {
    if item.effective_type() == "tv" {
        if !item.first_air_date.trim().is_empty() {
            return item.first_air_date.trim().to_string();
        }
    } else if !item.release_date.trim().is_empty() {
        return item.release_date.trim().to_string();
    }
    item.effective_year()
}

pub fn sort_media_items(
    items: &mut [TMDBMediaItem],
    sort: &str,
    tg_seq_map: &HashMap<String, i64>,
) {
    match sort.trim() {
        "评分" => items.sort_by(|a, b| {
            b.vote_average
                .partial_cmp(&a.vote_average)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    b.popularity
                        .partial_cmp(&a.popularity)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        }),
        "上映" => items.sort_by(|a, b| {
            release_date_key(b)
                .cmp(&release_date_key(a))
                .then_with(|| {
                    b.popularity
                        .partial_cmp(&a.popularity)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        }),
        "热度" | "连载" | "完结" => items.sort_by(|a, b| {
            b.popularity
                .partial_cmp(&a.popularity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    b.vote_average
                        .partial_cmp(&a.vote_average)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        }),
        _ => {
            items.sort_by(|a, b| {
                let sa = tg_seq_map.get(a.effective_id()).copied().unwrap_or(0);
                let sb = tg_seq_map.get(b.effective_id()).copied().unwrap_or(0);
                sb.cmp(&sa)
                    .then_with(|| {
                        b.popularity
                            .partial_cmp(&a.popularity)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .then_with(|| release_date_key(b).cmp(&release_date_key(a)))
            });
        }
    }
}

pub fn normalize_objective_troubleshoot(raw: &str) -> &'static str {
    match raw.trim() {
        "多版本" => "多版本",
        "单DV" => "单DV",
        "ISO" => "ISO",
        "STRM" => "STRM",
        _ => "全部",
    }
}

pub fn discover_or_search_media(
    host: &HostClient,
    db: &Db,
    cfg: &crate::models::AppConfig,
    req: &DiscoverRequest,
) -> Result<Value> {
    let mut eff_type = normalize_type_filter(&req.media_type);
    let sort = if req.sort.trim().is_empty() {
        "更新".to_string()
    } else {
        req.sort.trim().to_string()
    };
    let region = if req.region.trim().is_empty() {
        "全部".to_string()
    } else {
        req.region.trim().to_string()
    };
    let year = if req.year.trim().is_empty() {
        "全部".to_string()
    } else {
        req.year.trim().to_string()
    };
    let genre = if req.genre.trim().is_empty() {
        "全部".to_string()
    } else {
        req.genre.trim().to_string()
    };
    let mut status = if req.status.trim().is_empty() {
        "全部".to_string()
    } else {
        req.status.trim().to_string()
    };
    let troubleshoot = normalize_objective_troubleshoot(&req.troubleshoot);
    let channel = if req.channel.trim().is_empty() {
        "全部".to_string()
    } else {
        req.channel.trim().to_string()
    };
    let query = req.query.trim().to_string();
    let page = req.page.max(1);
    let page_size = if req.page_size == 0 {
        24
    } else {
        req.page_size.clamp(12, 60)
    };

    if (sort == "连载" || sort == "完结") && eff_type == "movie" {
        eff_type = "tv";
    }
    if troubleshoot != "全部" {
        status = "已入库".into();
    }

    let tg_seq_map = db.latest_tg_tmdb_seq_map();

    // --- Branch 1: Explicit TG Channel Filter (`channel != "全部"`) ---
    if channel != "全部" && !channel.eq_ignore_ascii_case("all") {
        return Ok(discover_by_tg_channel(
            host,
            db,
            cfg,
            &channel,
            eff_type,
            &sort,
            &region,
            &year,
            &genre,
            &status,
            &query,
            page,
            page_size,
            &tg_seq_map,
        ));
    }

    // --- Branch 2: Deterministic Index-Backed Diagnostics (`#12b`: 多版本 / 单DV / ISO / STRM) ---
    if troubleshoot != "全部" {
        return Ok(discover_objective_troubleshoot(
            host,
            db,
            troubleshoot,
            eff_type,
            &sort,
            &region,
            &year,
            &genre,
            &query,
            page,
            page_size,
            &tg_seq_map,
        ));
    }

    // --- Branch 3: Local Status Collections (`订阅中` / `已忽略` / `审核中`) ---
    if matches!(status.as_str(), "订阅中" | "已忽略" | "审核中") {
        let local_items = collect_local_status_items(
            host,
            db,
            cfg,
            &status,
            eff_type,
            &sort,
            &region,
            &year,
            &genre,
            &query,
            &tg_seq_map,
        );
        return Ok(paginate_and_format(
            host,
            local_items,
            page,
            page_size,
            "discover",
            &status,
        ));
    }

    // --- Branch 4: Keyword / TMDB ID Search (`!query.is_empty()`) ---
    if !query.is_empty() {
        let mut raw_items = Vec::new();
        let clean_num = query
            .trim_start_matches("tmdb:")
            .trim_start_matches("tmdbid-")
            .trim_start_matches('#')
            .trim();
        if clean_num.len() >= 2 && clean_num.chars().all(|c| c.is_ascii_digit()) {
            for mt in ["movie", "tv"] {
                if eff_type != "all" && eff_type != mt {
                    continue;
                }
                if let Some(det) = cached_tmdb_details(host, db, clean_num, mt) {
                    if !det.id.is_empty() {
                        raw_items.push(det.to_media_item(mt));
                    }
                }
            }
        }
        if let Ok(searched) = host.search_tmdb(&query, eff_type, page) {
            let mut seen: HashSet<String> = raw_items
                .iter()
                .map(|x| format!("{}:{}", x.effective_type(), x.effective_id()))
                .collect();
            for it in searched {
                let key = format!("{}:{}", it.effective_type(), it.effective_id());
                if !it.effective_id().is_empty() && seen.insert(key) {
                    raw_items.push(it);
                }
            }
        }
        raw_items.retain(|it| {
            matches_media_type(it, eff_type)
                && matches_year(it, &year)
                && matches_region(it, &region)
                && matches_genre(it, &genre)
        });
        if sort != "热度" {
            sort_media_items(&mut raw_items, &sort, &tg_seq_map);
        }
        let filtered = filter_by_library_status(host, raw_items, &status);
        let total_results = filtered.len();
        return Ok(json!({
            "items": normalize_tmdb_cards(host, &filtered),
            "mode": "search",
            "category": "search",
            "page": page,
            "total_pages": if filtered.len() >= 18 { page + 1 } else { page },
            "total_results": total_results,
        }));
    }

    // --- Branch 5: Mediary 3.4.6 Parameterized `GET /api/plugin/tmdb/discover` (`#10a`) ---
    let needs_status_post_filter = status == "已入库" || status == "未入库";
    let (mut items, mut total_pages, mut total_results) = fetch_host_tmdb_discover(
        host,
        eff_type,
        &sort,
        &region,
        &year,
        &genre,
        page,
        needs_status_post_filter,
    )?;

    if sort == "更新" {
        sort_media_items(&mut items, "更新", &tg_seq_map);
    }
    if needs_status_post_filter {
        items = filter_by_library_status(host, items, &status);
        if status == "已入库" && items.len() < page_size {
            let extra = collect_in_library_from_subs_and_movies(
                host, db, eff_type, &region, &year, &genre, page,
            );
            let mut seen: HashSet<String> = items
                .iter()
                .map(|x| format!("{}:{}", x.effective_type(), x.effective_id()))
                .collect();
            for ex in extra {
                let k = format!("{}:{}", ex.effective_type(), ex.effective_id());
                if !ex.effective_id().is_empty() && seen.insert(k) {
                    items.push(ex);
                }
            }
        }
        items.truncate(page_size);
        total_results = items.len();
        total_pages = total_pages.min(500);
    } else {
        items.truncate(page_size.max(20));
    }

    Ok(json!({
        "items": normalize_tmdb_cards(host, &items),
        "mode": "discover",
        "category": "discover",
        "page": page,
        "total_pages": total_pages.max(1),
        "total_results": total_results,
    }))
}

fn fetch_host_tmdb_discover(
    host: &HostClient,
    eff_type: &str,
    sort: &str,
    region: &str,
    year: &str,
    genre: &str,
    page: usize,
    fetch_extra_pages: bool,
) -> Result<(Vec<TMDBMediaItem>, usize, usize)> {
    let types_to_fetch: &[&str] = match eff_type {
        "movie" => &["movie"],
        "tv" => &["tv"],
        _ => &["movie", "tv"],
    };
    let pages_per_type = if fetch_extra_pages { 2usize } else { 1usize };
    let today = Local::now().format("%Y-%m-%d").to_string();

    let mut combined = Vec::new();
    let mut max_pages = 1usize;
    let mut sum_results = 0usize;
    let mut last_err = None;
    let mut any_ok = false;

    for &mt in types_to_fetch {
        let mut owned_params: Vec<(&str, String)> = Vec::new();
        let sort_by = match sort.trim() {
            "评分" => {
                owned_params.push(("vote_count.gte", "50".into()));
                "vote_average.desc"
            }
            "上映" => {
                if mt == "tv" {
                    owned_params.push(("first_air_date.lte", today.clone()));
                    "first_air_date.desc"
                } else {
                    owned_params.push(("primary_release_date.lte", today.clone()));
                    "primary_release_date.desc"
                }
            }
            "连载" if mt == "tv" => {
                owned_params.push(("with_status", "0".into()));
                "popularity.desc"
            }
            "完结" if mt == "tv" => {
                owned_params.push(("with_status", "3|4".into()));
                "popularity.desc"
            }
            _ => "popularity.desc",
        };
        owned_params.push(("sort_by", sort_by.into()));

        if let Some((countries, _)) = region_countries_and_langs(region) {
            if !countries.is_empty() {
                owned_params.push(("with_origin_country", countries.join("|")));
            }
        }

        let gids = genre_ids_for_filter(genre, mt);
        if !gids.is_empty() {
            let g_str: Vec<String> = gids.iter().map(|g| g.to_string()).collect();
            owned_params.push(("with_genres", g_str.join("|")));
        }

        if let Some((gte, lte)) = year_date_bounds(year) {
            if let Some(g) = gte {
                owned_params.push((
                    if mt == "tv" {
                        "first_air_date.gte"
                    } else {
                        "primary_release_date.gte"
                    },
                    g,
                ));
            }
            if let Some(l) = lte {
                owned_params.push((
                    if mt == "tv" {
                        "first_air_date.lte"
                    } else {
                        "primary_release_date.lte"
                    },
                    l,
                ));
            }
        }

        let borrowed: Vec<(&str, &str)> =
            owned_params.iter().map(|(k, v)| (*k, v.as_str())).collect();

        for p_off in 0..pages_per_type {
            let target_page = (page - 1) * pages_per_type + p_off + 1;
            match host.discover_tmdb_page(mt, &borrowed, target_page) {
                Ok(disc_page) => {
                    any_ok = true;
                    max_pages = max_pages.max(disc_page.total_pages.min(500));
                    if p_off == 0 {
                        sum_results += disc_page.total_results;
                    }
                    combined.extend(disc_page.items);
                }
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }
    }

    if !any_ok {
        if let Some(e) = last_err {
            return Err(e);
        }
    }

    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(combined.len());
    for it in combined {
        let key = format!("{}:{}", it.effective_type(), it.effective_id());
        if !it.effective_id().is_empty()
            && seen.insert(key)
            && matches_year(&it, year)
            && matches_region(&it, region)
        {
            deduped.push(it);
        }
    }

    if types_to_fetch.len() > 1 {
        let empty_map = HashMap::new();
        sort_media_items(&mut deduped, sort, &empty_map);
    }

    Ok((deduped, max_pages.max(1), sum_results))
}

fn discover_objective_troubleshoot(
    host: &HostClient,
    db: &Db,
    troubleshoot: &str,
    eff_type: &str,
    sort: &str,
    region: &str,
    year: &str,
    genre: &str,
    query: &str,
    page: usize,
    page_size: usize,
    tg_seq_map: &HashMap<String, i64>,
) -> Value {
    let filter_kv: (&str, &str) = match troubleshoot {
        "多版本" => ("has_multiple_versions", "true"),
        "单DV" => ("hdr_format", "dv_p5"),
        "ISO" => ("source_kind", "iso"),
        "STRM" => ("source_kind", "strm"),
        _ => return paginate_and_format(host, Vec::new(), page, page_size, "discover", troubleshoot),
    };

    let has_post_filters = region != "全部" || year != "全部" || genre != "全部" || !query.is_empty();
    let types: &[&str] = match eff_type {
        "movie" => &["movie"],
        "tv" => &["tv"],
        _ => &["movie", "tv"],
    };

    let fetch_limit = if has_post_filters { 120usize } else { 60usize };
    let start_offset = if has_post_filters {
        0usize
    } else {
        (page.saturating_sub(1)).saturating_mul(page_size)
    };
    let start_s = start_offset.to_string();
    let limit_s = fetch_limit.to_string();

    let mut seeds: Vec<(String, String, String)> = Vec::new();
    let mut seen = HashSet::new();
    let mut raw_total = 0usize;

    for &mt in types {
        let q = [
            ("media_type", mt),
            (filter_kv.0, filter_kv.1),
            ("start", start_s.as_str()),
            ("limit", limit_s.as_str()),
        ];
        if let Ok(lib_page) = host.list_library_items_filtered(&q) {
            raw_total += lib_page.total;
            for li in lib_page.items {
                let tid = if mt == "tv" && !li.series_tmdb_id.trim().is_empty() && li.series_tmdb_id != "0" {
                    li.series_tmdb_id.trim().to_string()
                } else {
                    li.tmdb_id.trim().to_string()
                };
                if tid.is_empty() || tid == "0" {
                    continue;
                }
                if seen.insert(format!("{}:{}", mt, tid)) {
                    seeds.push((tid, mt.to_string(), li.name));
                }
            }
        }
    }

    let mut items = Vec::new();
    let mut detail_budget = 28usize;
    for (tid, mt, fallback_name) in seeds {
        if !has_post_filters && items.len() >= page_size {
            break;
        }
        if detail_budget > 0 {
            if let Some(det) = cached_tmdb_details(host, db, &tid, &mt) {
                detail_budget = detail_budget.saturating_sub(1);
                if !det.id.is_empty() {
                    let media_item = det.to_media_item(&mt);
                    if matches_all_metadata_filters(
                        &media_item,
                        eff_type,
                        region,
                        year,
                        genre,
                        sort,
                        query,
                    ) {
                        items.push(media_item);
                    }
                    continue;
                }
            }
        }
        let fallback_item = TMDBMediaItem {
            id: tid.clone(),
            tmdb_id: tid,
            title: fallback_name,
            media_type: mt,
            ..Default::default()
        };
        if matches_all_metadata_filters(&fallback_item, eff_type, region, year, genre, sort, query) {
            items.push(fallback_item);
        }
    }

    sort_media_items(&mut items, sort, tg_seq_map);

    if has_post_filters {
        return paginate_and_format(host, items, page, page_size, "discover", troubleshoot);
    }

    items.truncate(page_size);
    let est_total = raw_total.max(items.len());
    let total_pages = ((est_total + page_size - 1) / page_size).clamp(1, 500);
    json!({
        "items": normalize_tmdb_cards(host, &items),
        "mode": "discover",
        "category": troubleshoot,
        "page": page,
        "total_pages": total_pages,
        "total_results": est_total,
    })
}

fn filter_by_library_status(
    host: &HostClient,
    items: Vec<TMDBMediaItem>,
    status: &str,
) -> Vec<TMDBMediaItem> {
    if status.is_empty() || status == "全部" {
        return items;
    }
    let keys: Vec<String> = items
        .iter()
        .map(|it| format!("{}:{}", it.effective_type(), it.effective_id()))
        .collect();
    let st_map = host.get_media_statuses(&keys, false);
    items
        .into_iter()
        .filter(|it| {
            let k = format!("{}:{}", it.effective_type(), it.effective_id());
            let st = st_map.get(&k).cloned().unwrap_or_default();
            let in_lib = st.is_fully_collected || st.is_partially_collected || st.inventory_count > 0;
            match status {
                "已入库" => in_lib,
                "未入库" => !in_lib,
                "订阅中" => st.is_subscribed,
                _ => true,
            }
        })
        .collect()
}

fn collect_in_library_from_subs_and_movies(
    host: &HostClient,
    db: &Db,
    eff_type: &str,
    region: &str,
    year: &str,
    genre: &str,
    page: usize,
) -> Vec<TMDBMediaItem> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    if let Ok(subs) = host.list_subscriptions() {
        for s in subs.into_iter().filter(|s| s.collected_episodes > 0).take(20) {
            let mt = s.effective_type();
            if eff_type != "all" && eff_type != mt {
                continue;
            }
            if !s.tmdb_id.is_empty() && seen.insert(format!("{}:{}", mt, s.tmdb_id)) {
                if let Some(det) = cached_tmdb_details(host, db, &s.tmdb_id, mt) {
                    let it = det.to_media_item(mt);
                    if matches_year(&it, year)
                        && matches_region(&it, region)
                        && matches_genre(&it, genre)
                    {
                        out.push(it);
                    }
                }
            }
        }
    }

    if eff_type == "all" || eff_type == "movie" {
        let start = page.saturating_sub(1) * 24;
        if let Ok(movies) = host.list_library_movies_page(start, 24) {
            for m in movies.into_iter().take(16) {
                if !m.tmdb_id.is_empty() && seen.insert(format!("movie:{}", m.tmdb_id)) {
                    if let Some(det) = cached_tmdb_details(host, db, &m.tmdb_id, "movie") {
                        let it = det.to_media_item("movie");
                        if matches_year(&it, year)
                            && matches_region(&it, region)
                            && matches_genre(&it, genre)
                        {
                            out.push(it);
                        }
                    }
                }
            }
        }
    }
    out
}

fn discover_by_tg_channel(
    host: &HostClient,
    db: &Db,
    cfg: &crate::models::AppConfig,
    channel_query: &str,
    eff_type: &str,
    sort: &str,
    region: &str,
    year: &str,
    genre: &str,
    status: &str,
    query: &str,
    page: usize,
    page_size: usize,
    tg_seq_map: &HashMap<String, i64>,
) -> Value {
    let q_norm = providers::normalize_tg_channel(channel_query).to_lowercase();
    let matched_ch = cfg.telegram.channels.iter().find(|c| {
        c.name.trim().eq_ignore_ascii_case(channel_query.trim())
            || providers::normalize_tg_channel(&c.identifier).to_lowercase() == q_norm
    });
    let norm_key = matched_ch
        .map(|c| {
            let k = if !c.identifier.trim().is_empty() {
                &c.identifier
            } else {
                &c.name
            };
            providers::normalize_tg_channel(k).to_lowercase()
        })
        .unwrap_or(q_norm);

    let tg_rows = db.list_tg_items_by_channel(&norm_key, 240);
    let mut resolved_items: Vec<TMDBMediaItem> = Vec::new();
    let mut direct_cards: Vec<Value> = Vec::new();
    let mut seen_tmdb = HashSet::new();
    let mut seen_titles = HashSet::new();
    let mut network_resolves = 0usize;

    for r in tg_rows {
        let mt = if r.media_type.eq_ignore_ascii_case("tv") {
            "tv"
        } else {
            "movie"
        };
        if !query.is_empty() {
            let blob = format!("{} {} {}", r.title, r.raw_title, r.tmdb_id).to_lowercase();
            if !blob.contains(&query.to_lowercase()) {
                continue;
            }
        }
        if !r.tmdb_id.trim().is_empty() {
            let key = format!("{}:{}", mt, r.tmdb_id.trim());
            if seen_tmdb.insert(key) {
                if let Some(det) = cached_tmdb_details(host, db, r.tmdb_id.trim(), mt) {
                    if !det.id.is_empty() {
                        resolved_items.push(det.to_media_item(mt));
                        continue;
                    }
                }
                resolved_items.push(TMDBMediaItem {
                    id: r.tmdb_id.trim().to_string(),
                    tmdb_id: r.tmdb_id.trim().to_string(),
                    title: r.title.clone(),
                    media_type: mt.into(),
                    year: r.year.clone(),
                    ..Default::default()
                });
            }
            continue;
        }

        let pure = rules::extract_pure_title(&r.title);
        let lookup_title = if !pure.is_empty() {
            pure
        } else {
            r.title.trim().to_string()
        };
        if lookup_title.is_empty() || !seen_titles.insert(lookup_title.to_lowercase()) {
            continue;
        }
        let yr_num = r.year.parse::<i32>().unwrap_or(0);
        if let Some((item, used_net)) =
            cached_resolve_tmdb(host, db, &lookup_title, mt, yr_num, network_resolves < 14)
        {
            if used_net {
                network_resolves += 1;
            }
            let tid = item.effective_id().to_string();
            let eff_mt = item.effective_type();
            if !tid.is_empty() {
                if seen_tmdb.insert(format!("{}:{}", eff_mt, tid)) {
                    resolved_items.push(item);
                }
                continue;
            }
        }
        if (eff_type == "all" || eff_type == mt)
            && region == "全部"
            && genre == "全部"
            && status == "全部"
        {
            direct_cards.push(json!({
                "tmdb_id": "",
                "title": lookup_title,
                "media_type": mt,
                "year": r.year,
                "poster_path": "",
                "status_label": "频道直出",
                "status_tone": "info",
                "overview": r.remark,
                "vote_average": 0.0,
                "is_subscribed": false,
                "in_library": false,
            }));
        }
    }

    resolved_items.retain(|it| {
        matches_all_metadata_filters(it, eff_type, region, year, genre, sort, query)
    });
    if sort != "更新" {
        sort_media_items(&mut resolved_items, sort, tg_seq_map);
    }
    if status != "全部" {
        resolved_items = filter_by_library_status(host, resolved_items, status);
    }

    let mut all_cards = normalize_tmdb_cards(host, &resolved_items);
    all_cards.extend(direct_cards);
    let total_results = all_cards.len();
    let total_pages = ((total_results + page_size - 1) / page_size).max(1);
    let start = (page - 1).saturating_mul(page_size).min(total_results);
    let end = (start + page_size).min(total_results);

    json!({
        "items": all_cards[start..end],
        "mode": "channel",
        "category": channel_query,
        "page": page,
        "total_pages": total_pages,
        "total_results": total_results,
    })
}

fn collect_local_status_items(
    host: &HostClient,
    db: &Db,
    cfg: &crate::models::AppConfig,
    status: &str,
    eff_type: &str,
    sort: &str,
    region: &str,
    year: &str,
    genre: &str,
    query: &str,
    tg_seq_map: &HashMap<String, i64>,
) -> Vec<TMDBMediaItem> {
    let mut seeds: Vec<(String, String, String, String)> = Vec::new();
    let mut seen = HashSet::new();

    match status {
        "订阅中" => {
            if let Ok(subs) = host.list_subscriptions() {
                for s in subs {
                    if s.status.trim().eq_ignore_ascii_case("completed") {
                        continue;
                    }
                    let mt = s.effective_type().to_string();
                    if !s.tmdb_id.is_empty() && seen.insert(format!("{}:{}", mt, s.tmdb_id)) {
                        seeds.push((
                            s.tmdb_id.clone(),
                            mt,
                            s.effective_title().to_string(),
                            s.year.clone(),
                        ));
                    }
                }
            }
        }
        "已忽略" => {
            for ig in &cfg.ignored_episodes {
                let mt = if ig.media_type.eq_ignore_ascii_case("movie") {
                    "movie"
                } else {
                    "tv"
                };
                if !ig.tmdb_id.is_empty() && seen.insert(format!("{}:{}", mt, ig.tmdb_id)) {
                    seeds.push((ig.tmdb_id.clone(), mt.into(), ig.title.clone(), String::new()));
                }
            }
        }
        "审核中" => {
            for rv in db.list_reviewing() {
                let mt = if rv.media_type.eq_ignore_ascii_case("tv") {
                    "tv"
                } else {
                    "movie"
                };
                let key = if !rv.tmdb_id.is_empty() {
                    format!("{}:{}", mt, rv.tmdb_id)
                } else {
                    rv.clean_slug.clone()
                };
                if seen.insert(key) {
                    seeds.push((rv.tmdb_id, mt.into(), rv.title, String::new()));
                }
            }
        }
        _ => {}
    }

    let mut out = Vec::new();
    let mut detail_budget = 32usize;
    for (tid, mt, fallback_title, fallback_year) in seeds {
        if eff_type != "all" && eff_type != mt {
            continue;
        }
        if !tid.is_empty() && detail_budget > 0 {
            if let Some(det) = cached_tmdb_details(host, db, &tid, &mt) {
                detail_budget = detail_budget.saturating_sub(1);
                if !det.id.is_empty() {
                    let it = det.to_media_item(&mt);
                    if matches_all_metadata_filters(&it, eff_type, region, year, genre, sort, query)
                    {
                        out.push(it);
                    }
                    continue;
                }
            }
        }
        let fallback_item = TMDBMediaItem {
            id: if tid.is_empty() {
                fallback_title.clone()
            } else {
                tid.clone()
            },
            tmdb_id: tid,
            title: fallback_title,
            media_type: mt,
            year: fallback_year,
            ..Default::default()
        };
        if matches_all_metadata_filters(&fallback_item, eff_type, region, year, genre, sort, query)
        {
            out.push(fallback_item);
        }
    }
    sort_media_items(&mut out, sort, tg_seq_map);
    out
}

fn cached_tmdb_details(
    host: &HostClient,
    db: &Db,
    tmdb_id: &str,
    mtype: &str,
) -> Option<TMDBDetails> {
    let tid = tmdb_id.trim();
    if tid.is_empty() {
        return None;
    }
    let key = format!("tmdb_det:{}:{}", mtype, tid);
    if let Some(val) = db.get_json_cache(&key, 7 * 86400) {
        if let Ok(det) = serde_json::from_value::<TMDBDetails>(val) {
            return Some(det);
        }
    }
    let det = host.get_tmdb_details(tid, mtype).ok()?;
    if let Ok(val) = serde_json::to_value(&det) {
        let _ = db.put_json_cache(&key, &val);
    }
    Some(det)
}

fn cached_resolve_tmdb(
    host: &HostClient,
    db: &Db,
    title: &str,
    mtype: &str,
    year: i32,
    allow_network: bool,
) -> Option<(TMDBMediaItem, bool)> {
    let key = format!("tmdb_res:{}:{}:{}", mtype, title.trim().to_lowercase(), year);
    if let Some(val) = db.get_json_cache(&key, 7 * 86400) {
        if let Ok(item) = serde_json::from_value::<TMDBMediaItem>(val) {
            return Some((item, false));
        }
    }
    if !allow_network {
        return None;
    }
    let resolved = host.resolve_tmdb(title, mtype, year).ok()?;
    if let Ok(val) = serde_json::to_value(&resolved) {
        let _ = db.put_json_cache(&key, &val);
    }
    Some((resolved, true))
}

fn paginate_and_format(
    host: &HostClient,
    items: Vec<TMDBMediaItem>,
    page: usize,
    page_size: usize,
    mode: &str,
    category: &str,
) -> Value {
    let total_results = items.len();
    let total_pages = ((total_results + page_size - 1) / page_size).max(1);
    let start = (page - 1).saturating_mul(page_size).min(total_results);
    let end = (start + page_size).min(total_results);
    let slice = &items[start..end];

    json!({
        "items": normalize_tmdb_cards(host, slice),
        "mode": mode,
        "category": category,
        "page": page,
        "total_pages": total_pages,
        "total_results": total_results,
    })
}

pub fn normalize_tmdb_cards(host: &HostClient, items: &[TMDBMediaItem]) -> Vec<Value> {
    let status_keys: Vec<String> = items
        .iter()
        .filter_map(|it| {
            let tid = it.effective_id();
            (!tid.is_empty() && tid.chars().all(|c| c.is_ascii_digit()))
                .then(|| format!("{}:{}", it.effective_type(), tid))
        })
        .collect();
    let statuses = host.get_media_statuses(&status_keys, false);

    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let tid = it.effective_id();
        if tid.is_empty() {
            continue;
        }
        let is_numeric_tmdb = tid.chars().all(|c| c.is_ascii_digit());
        let mtype = it.effective_type();
        let poster_path = providers::normalize_poster_path(&it.poster_path);

        let mut status_label = "";
        let mut status_tone = "";
        let mut is_subscribed = false;
        let mut in_library = false;
        if is_numeric_tmdb {
            if let Some(st) = statuses.get(&format!("{}:{}", mtype, tid)) {
                is_subscribed = st.is_subscribed;
                in_library =
                    st.is_fully_collected || st.is_partially_collected || st.inventory_count > 0;
                if st.is_fully_collected {
                    status_label = "已入库";
                    status_tone = "success";
                } else if st.is_subscribed {
                    status_label = "订阅中";
                    status_tone = "info";
                } else if st.is_partially_collected || st.inventory_count > 0 {
                    status_label = "部分入库";
                    status_tone = "warn";
                }
            }
        }

        out.push(json!({
            "tmdb_id": if is_numeric_tmdb { tid } else { "" },
            "title": it.effective_title(),
            "media_type": mtype,
            "year": it.effective_year(),
            "poster_path": poster_path,
            "status_label": status_label,
            "status_tone": status_tone,
            "overview": it.overview,
            "vote_average": it.vote_average,
            "is_subscribed": is_subscribed,
            "in_library": in_library,
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_year_region_genre_and_objective_troubleshoot() {
        let cn_tv = TMDBMediaItem {
            id: "100".into(),
            name: "庆余年".into(),
            media_type: "tv".into(),
            first_air_date: "2019-11-26".into(),
            origin_country: vec!["CN".into()],
            original_language: "zh".into(),
            genre_ids: vec![18, 9648],
            ..Default::default()
        };
        assert!(matches_media_type(&cn_tv, "tv"));
        assert!(!matches_media_type(&cn_tv, "movie"));
        assert!(matches_year(&cn_tv, "2010年代"));
        assert!(!matches_year(&cn_tv, "2024"));
        assert!(matches_region(&cn_tv, "大陆"));
        assert!(!matches_region(&cn_tv, "港台"));
        assert!(!matches_region(&cn_tv, "欧美"));
        assert!(matches_genre(&cn_tv, "剧情"));
        assert!(matches_genre(&cn_tv, "恐怖悬疑"));
        assert!(!matches_genre(&cn_tv, "动漫"));

        let hk_movie = TMDBMediaItem {
            id: "200".into(),
            title: "无间道".into(),
            media_type: "movie".into(),
            release_date: "2002-12-12".into(),
            origin_country: vec!["HK".into()],
            original_language: "cn".into(),
            genre_ids: vec![80, 53],
            ..Default::default()
        };
        assert!(matches_year(&hk_movie, "2000年代"));
        assert!(matches_region(&hk_movie, "港台"));
        assert!(!matches_region(&hk_movie, "大陆"));
        assert!(matches_genre(&hk_movie, "惊悚犯罪"));

        assert_eq!(normalize_objective_troubleshoot("多版本"), "多版本");
        assert_eq!(normalize_objective_troubleshoot("单DV"), "单DV");
        assert_eq!(normalize_objective_troubleshoot("ISO"), "ISO");
        assert_eq!(normalize_objective_troubleshoot("STRM"), "STRM");
        assert_eq!(normalize_objective_troubleshoot("缺失集"), "全部");
        assert_eq!(normalize_objective_troubleshoot("散落季"), "全部");
    }
}

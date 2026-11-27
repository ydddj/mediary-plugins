use crate::models::{MovieAttributes, ResourceItem, RuleChipItem, RuleConfig};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;
use url::Url;

static RE_115_SHARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:115://|https?://(?:[a-z0-9-]+\.)?(?:115|115cdn|115v|anxia)\.com/s/)([a-zA-Z0-9]{6,24})").unwrap()
});
static RE_ACCESS_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:password|pwd|pass|code|访问码|验证码|提取码|访问密码|密码)[=：:\s#]*([A-Za-z0-9]{4,12})").unwrap()
});
static RE_HASH_FRAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#([A-Za-z0-9]{4,12})$").unwrap());
static RE_ED2K: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)ed2k://\|file\|([^|]+)\|(\d+)\|([a-fA-F0-9]{32})\|/?").unwrap());
static RE_MAGNET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)magnet:\?xt=urn:btih:[a-zA-Z0-9]{32,40}[^\s"'<>]*"#).unwrap());
static RE_TELEGRAPH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)https?://(?:[a-z0-9-]+\.)?telegra\.ph/[^\s"'<>]+"#).unwrap());
static RE_TMDB_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)tmdb(?:\s*id)?[-_:：\s=]*(\d{2,10})").unwrap());
static RE_YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(19\d{2}|20\d{2})\b").unwrap());
static RE_SIZE_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*(TB|TiB|T|GB|GiB|G|MB|MiB|M)\b").unwrap());
static RE_SXXEXX_RANGE_EXPLICIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[Ss](\d{1,2})[.\s_-]*[Ee][Pp]?(\d{1,4})\s*(?:-|~|至)\s*(?:[Ss](\d{1,2})[.\s_-]*)?[Ee][Pp]?(\d{1,4})\b").unwrap()
});
static RE_SXXEXX_RANGE_TIGHT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[Ss](\d{1,2})[.\s_-]*[Ee][Pp]?(\d{1,4})(?:-|~|至)(\d{1,4})\b").unwrap()
});
static RE_SXXEXX_SINGLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b[Ss](\d{1,2})[.\s_-]*[Ee][Pp]?(\d{1,4})\b").unwrap());
static RE_EP_ONLY_RANGE_EXPLICIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[Ee][Pp]?(\d{1,4})\s*(?:-|~|至)\s*[Ee][Pp]?(\d{1,4})\b").unwrap()
});
static RE_EP_ONLY_RANGE_TIGHT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b[Ee][Pp]?(\d{1,4})(?:-|~|至)(\d{1,4})\b").unwrap());
static RE_SEASON_RANGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:[Ss]|Season\s*)(\d{1,2})\s*(?:-|~|至)\s*(?:[Ss]|Season\s*)?(\d{1,2})\b").unwrap()
});
static RE_SEASON_SINGLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:[Ss]|Season\s*)(\d{1,2})\b").unwrap());
static RE_CN_SEASON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"第\s*([0-9零〇一二两三四五六七八九十百]+)\s*季").unwrap());
static RE_CN_EP_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:第\s*)?(\d{1,4})\s*(?:-|~|至)\s*(\d{1,4})\s*[集话期]").unwrap());
static RE_CN_UPDATE_TO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"更新(?:至|到)?\s*(?:第\s*)?(\d{1,4})\s*[集话期]?").unwrap());
static RE_CN_FULL_COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"全\s*(\d{1,4})\s*[集话期]").unwrap());
static RE_CN_EP_SINGLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"第\s*(\d{1,4})\s*[集话期]").unwrap());
static RE_FULL_MARKERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"全集|完结|合集|全剧终").unwrap());
static RE_BRACKET_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:【([^】]+)】|\[([^\]]+)\]|（([^）]+)）)\s*").unwrap());
static RE_LEADING_DECOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\s📺🎬🎞️🎥📽️🔥✨📌🔔💿📀🆕✅☑️🎉💥📣📢💎⭐🌟]+\s*").unwrap()
});
static RE_CAT_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?:名称|片名|标题|剧名|译名|又名|影视|电影|剧集|电视剧|连续剧|动漫|动画|番剧|综艺|纪录片|短剧|国产剧|美剧|日剧|韩剧|港剧|台剧|泰剧|英剧)[：:\s]+)+").unwrap()
});
static RE_LABELED_TITLE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[\s📺🎬🎞️🎥📽️🔥✨📌🔔💿📀🆕✅☑️]*)(?:\[(?:电影|剧集|电视剧|动漫|动画|番剧|综艺|纪录片|短剧)\]\s*[：:]\s*|(?:名称|片名|标题|剧名|译名|影视|电影|剧集|电视剧|连续剧|动漫|动画|番剧|综艺|纪录片|短剧|◎\s*片\s*名|◎\s*译\s*名)\s*[：:]+\s*)(.+)$").unwrap()
});
static RE_RELEASE_SPEC_CUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[\s._\[\(【（-])(?:2160p|1080p|1080i|720p|576p|480p|4k|uhd|web-dl|webdl|webrip|bluray|blu-ray|bdrip|remux|hdr10\+?|dovi|dv|h\.?265|h\.?264|x265|x264|hevc|avc|ddp?\d|dts|truehd|atmos|hq|hfr|60fps|120fps)\b").unwrap()
});
static RE_HASHTAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#([^\s#，,。！!？?|｜/\\]+)").unwrap());
static RE_TMDB_BRACKET_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*[\[\(【（]?\s*tmdb(?:\s*id)?[-_:：\s=]*\d+\s*[\]\)】）]?").unwrap()
});
static RE_GROUP_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-([A-Za-z0-9@._]{2,20})(?:\.[a-zA-Z0-9]{2,5})?$").unwrap());
static RE_CONTAINER_EXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(mkv|mp4|ts|m2ts|iso|avi|mov|wmv|flv|rmvb|m4v|webm|mpg|mpeg|vob|3gp|asf)(?:\s|$)").unwrap()
});

pub fn strip_ignored_texts(text: &str, ignored: &[String]) -> String {
    if text.is_empty() || ignored.is_empty() {
        return text.to_string();
    }
    let mut cur = text.to_string();
    for phrase in ignored {
        let p = phrase.trim();
        if p.is_empty() {
            continue;
        }
        if let Ok(re) = RegexBuilder::new(&regex::escape(p))
            .case_insensitive(true)
            .build()
        {
            cur = re.replace_all(&cur, " ").into_owned();
        }
    }
    cur
}

pub fn is_video_file(name: &str) -> bool {
    let Some(ext) = Path::new(name.trim()).extension().and_then(|x| x.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp4"
            | "mkv"
            | "ts"
            | "m2ts"
            | "mov"
            | "avi"
            | "wmv"
            | "flv"
            | "webm"
            | "iso"
            | "rmvb"
            | "m4v"
            | "mpg"
            | "mpeg"
    )
}

pub fn is_subtitle_file(name: &str) -> bool {
    let Some(ext) = Path::new(name.trim()).extension().and_then(|x| x.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "srt" | "ass" | "ssa" | "sup" | "vtt" | "sub"
    )
}

pub fn parse_115_share(value: &str, extra_text: &str) -> (String, String) {
    let raw = value.trim();
    let combined = if extra_text.trim().is_empty() {
        raw.to_string()
    } else {
        format!("{}\n{}", raw, extra_text)
    };

    let mut receive_code = String::new();
    if let Some(first_tok) = raw.split_whitespace().next() {
        if let Ok(u) = Url::parse(first_tok) {
            for (k, v) in u.query_pairs() {
                if matches!(k.to_ascii_lowercase().as_str(), "password" | "pwd" | "code")
                    && v.trim().len() >= 4
                {
                    receive_code = v.trim().to_string();
                    break;
                }
            }
        }
    }
    if receive_code.is_empty() {
        if let Some(cap) = RE_ACCESS_CODE.captures(&combined) {
            receive_code = cap[1].to_string();
        }
    }
    if receive_code.is_empty() {
        if let Some(cap) = RE_HASH_FRAG.captures(raw) {
            receive_code = cap[1].to_string();
        }
    }

    let mut share_code = String::new();
    if let Some(cap) = RE_115_SHARE.captures(&combined) {
        share_code = cap[1].to_string();
    } else if (8..=24).contains(&raw.len()) && raw.chars().all(|c| c.is_ascii_alphanumeric()) {
        share_code = raw.to_string();
    }
    (share_code, receive_code)
}

pub fn format_115_share_url(share_code: &str, receive_code: &str) -> String {
    let sc = share_code.trim();
    if sc.is_empty() {
        return String::new();
    }
    let rc = receive_code.trim();
    if !rc.is_empty() {
        format!("https://115.com/s/{}?password={}", sc, rc)
    } else {
        format!("https://115.com/s/{}", sc)
    }
}

pub fn clean_slug(slug: &str) -> String {
    let mut raw = slug.trim();
    if raw.is_empty() {
        return String::new();
    }
    for wrapper in ["relay://", "movie:", "tv:"] {
        if raw.len() > wrapper.len() && raw[..wrapper.len()].eq_ignore_ascii_case(wrapper) {
            raw = raw[wrapper.len()..].trim();
        }
    }
    let low = raw.to_lowercase();
    if low.starts_with("ed2k://") || low.starts_with("magnet:?") {
        return low;
    }
    for prefix in ["hdhive://", "radar://", "nextfind://"] {
        if low.starts_with(prefix) {
            return raw[prefix.len()..].trim_matches('/').to_string();
        }
    }
    let (sc, _) = parse_115_share(raw, "");
    if !sc.is_empty() {
        return format!("115:{}", sc.to_lowercase());
    }
    low
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractedLink {
    pub pan_type: String,
    pub url: String,
    pub clean_key: String,
    pub share_code: String,
    pub receive_code: String,
    pub file_name: String,
    pub size_bytes: i64,
}

pub fn extract_all_resource_links(text: &str) -> Vec<ExtractedLink> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    // 1. 115 share links with nearby access code pairing
    let caps: Vec<_> = RE_115_SHARE.captures_iter(text).collect();
    for (idx, cap) in caps.iter().enumerate() {
        let m0 = cap.get(0).unwrap();
        let share_code = cap[1].to_string();
        let mut window_end = text.len();
        if let Some(next_cap) = caps.get(idx + 1) {
            window_end = window_end.min(next_cap.get(0).unwrap().start());
        }
        let max_forward = m0.end().saturating_add(160).min(window_end);
        // Align to valid UTF-8 char boundary
        let mut safe_end = max_forward;
        while safe_end < window_end && !text.is_char_boundary(safe_end) {
            safe_end += 1;
        }
        let snippet = &text[m0.start()..safe_end.min(text.len())];
        let (_, mut rc) = parse_115_share(snippet, "");
        if rc.is_empty() {
            let (_, global_rc) = parse_115_share("", text);
            rc = global_rc;
        }
        let key = format!("115:{}", share_code.to_lowercase());
        if seen.insert(key.clone()) {
            out.push(ExtractedLink {
                pan_type: "115".into(),
                url: format_115_share_url(&share_code, &rc),
                clean_key: key,
                share_code,
                receive_code: rc,
                ..Default::default()
            });
        }
    }

    // 2. ED2K links
    for cap in RE_ED2K.captures_iter(text) {
        let mut full = cap[0].to_string();
        if !full.ends_with('/') {
            full.push('/');
        }
        let file_name = percent_decode(&cap[1]);
        let size_bytes = cap[2].parse::<i64>().unwrap_or(0);
        let md4 = cap[3].to_lowercase();
        let key = format!("ed2k:{}", md4);
        if seen.insert(key.clone()) {
            out.push(ExtractedLink {
                pan_type: "ed2k".into(),
                url: full,
                clean_key: key,
                file_name,
                size_bytes,
                ..Default::default()
            });
        }
    }

    // 3. Magnet links
    for m in RE_MAGNET.find_iter(text) {
        let clean = m
            .as_str()
            .trim_end_matches(&['.', ',', ';', ')', ']', '}', '>', '"', '\''][..])
            .to_string();
        let low = clean.to_lowercase();
        let key = low.split('&').next().unwrap_or(&low).to_string();
        if seen.insert(key.clone()) {
            let mut dn = String::new();
            if let Ok(u) = Url::parse(&clean) {
                for (k, v) in u.query_pairs() {
                    if k == "dn" {
                        dn = v.to_string();
                        break;
                    }
                }
            }
            out.push(ExtractedLink {
                pan_type: "magnet".into(),
                url: clean,
                clean_key: key,
                file_name: dn,
                ..Default::default()
            });
        }
    }

    out
}

fn percent_decode(s: &str) -> String {
    if let Ok(u) = Url::parse(&format!("http://x/?q={}", s)) {
        for (k, v) in u.query_pairs() {
            if k == "q" {
                return v.into_owned();
            }
        }
    }
    s.to_string()
}

pub fn extract_telegraph_urls(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in RE_TELEGRAPH.find_iter(text) {
        let u = m
            .as_str()
            .trim_end_matches(&['"', '\'', '<', '>', '.', ',', ';', ')'][..])
            .to_string();
        if seen.insert(u.clone()) {
            out.push(u);
        }
    }
    out
}

pub fn extract_tmdb_id(text: &str) -> String {
    RE_TMDB_TAG
        .captures(text)
        .map(|c| c[1].to_string())
        .unwrap_or_default()
}

pub fn extract_year(text: &str) -> String {
    RE_YEAR
        .captures(text)
        .map(|c| c[1].to_string())
        .unwrap_or_default()
}

pub fn is_suspect_banner_title(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return true;
    }
    let cleaned = RE_LEADING_DECOR.replace(t, "");
    let c = cleaned.trim();
    if c.is_empty() {
        return true;
    }
    let low = c.to_lowercase();
    for kw in [
        "新片送达",
        "请您检阅",
        "全球顶级封装",
        "首发",
        "置顶公告",
        "频道公告",
        "防失联",
        "分享者",
        "关注公众号",
        "扫码领",
        "热更频道",
        "搬搬群组",
        "转存频道",
        "资源分享频道",
    ] {
        if low.contains(kw) {
            return true;
        }
    }
    matches!(
        c,
        "磁力任务" | "ED2K 资源" | "未命名资源" | "115分享" | "115网盘" | "影视资源"
    )
}

fn is_bracket_tag_noise(inner: &str) -> bool {
    let low = inner.trim().to_lowercase();
    if low.is_empty() {
        return true;
    }
    for kw in [
        "首发", "封装", "4k", "2160", "1080", "720", "remux", "bluray", "blu-ray", "bd",
        "web", "dv", "hdr", "sdr", "dovi", "x265", "h265", "hevc", "x264", "h264", "aac",
        "eac3", "ddp", "truehd", "atmos", "dts", "国语", "粤语", "英语", "双语", "中字", "简繁",
        "内封", "外挂", "特效", "高码", "完结", "全集", "合集", "集全", "多版本", "电影", "剧集",
        "电视剧", "动漫", "动画", "番剧", "综艺", "纪录片", "短剧", "韩剧", "美剧", "日剧", "国产",
        "tmdbid", "tmdb", "关注", "公众号", "福利",
    ] {
        if low.contains(kw) {
            return true;
        }
    }
    false
}

fn is_metadata_or_banner_line(line: &str) -> bool {
    let l = line.trim();
    if l.is_empty() {
        return true;
    }
    let low = l.to_lowercase();
    if low.contains("http://")
        || low.contains("https://")
        || low.contains("ed2k://")
        || low.contains("magnet:?")
        || l.starts_with('#')
        || l.starts_with('@')
        || l.starts_with('◎')
    {
        return true;
    }
    if is_suspect_banner_title(l) {
        return true;
    }
    let stripped = RE_LEADING_DECOR
        .replace(l, "")
        .trim_start_matches(&['👤', '📦', '💾', '📡', '📅', '🧲', '🎭', '📂', '🗂', '🌍', '🗣', '👥', '📖', '📝', '📶', '🎯', '📥', '🔖', '🏷', '🤖', '🍿', '📼', '⭐', '🌟', '\u{fe0f}', ' '][..])
        .trim()
        .to_string();
    for prefix in [
        "链接", "标签", "体积", "大小", "分享者", "质量", "帧率", "平台", "年份", "年代",
        "产地", "地区", "语言", "类别", "类型", "题材", "分类", "主演", "导演", "编剧",
        "评分", "豆瓣", "imdb", "IMDb", "tmdb", "TMDB", "文件", "备注", "描述", "简介",
        "计划", "播出进度", "转存入库", "触发", "已发版本", "提取码", "访问码", "密码", "投稿",
    ] {
        if stripped.starts_with(prefix) {
            let rest = stripped[prefix.len()..].trim_start();
            if rest.is_empty() || rest.starts_with('：') || rest.starts_with(':') || rest.starts_with(' ') {
                return true;
            }
        }
    }
    false
}

fn normalize_title_candidate(raw_cand: &str) -> String {
    let mut candidate = RE_TMDB_BRACKET_TAG.replace_all(raw_cand.trim(), "").into_owned();
    // Strip trailing media file extension (e.g. `.mkv`, `.mp4`, `.strm`)
    if let Some(dot_pos) = candidate.rfind('.') {
        let ext = candidate[dot_pos + 1..].trim().to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "mp4" | "mkv" | "ts" | "m2ts" | "mov" | "avi" | "wmv" | "flv" | "webm"
                | "iso" | "rmvb" | "m4v" | "mpg" | "mpeg" | "strm" | "srt" | "ass" | "ssa"
        ) {
            candidate.truncate(dot_pos);
        }
    }

    if let Some(start) = candidate.find('《') {
        let rest = &candidate[start + '《'.len_utf8()..];
        if let Some(end) = rest.find('》') {
            let book = &rest[..end];
            let cleaned_book = RE_CN_SEASON.replace_all(book, "").trim().to_string();
            if !cleaned_book.is_empty() && !is_suspect_banner_title(&cleaned_book) {
                return cleaned_book;
            }
        }
    }

    let mut preserved_bracket_title = String::new();
    loop {
        let before = candidate.clone();
        candidate = RE_LEADING_DECOR.replace(&candidate, "").into_owned();
        while let Some(cap) = RE_BRACKET_PREFIX.captures(&candidate) {
            let inner = cap
                .get(1)
                .or_else(|| cap.get(2))
                .or_else(|| cap.get(3))
                .map(|m| m.as_str().trim())
                .unwrap_or_default()
                .to_string();
            let after = candidate[cap.get(0).unwrap().end()..].trim().to_string();
            if is_bracket_tag_noise(&inner) {
                candidate = after;
            } else if !after.is_empty() && !after.starts_with('[') && !after.starts_with('【') {
                // E.g. `[新番] 葬送的芙莉莲` -> keep `葬送的芙莉莲`
                candidate = after;
            } else {
                // E.g. `[七龙珠][BD-REMUX]` -> unwrap `七龙珠` as the real title and strip trailing bracket tags!
                if preserved_bracket_title.is_empty() {
                    preserved_bracket_title = inner;
                }
                candidate = after;
            }
        }
        candidate = RE_CAT_PREFIX.replace(&candidate, "").into_owned();
        if candidate == before {
            break;
        }
    }
    if candidate.trim().is_empty() && !preserved_bracket_title.is_empty() {
        candidate = preserved_bracket_title;
    }

    for re in [
        &*RE_YEAR,
        &*RE_SXXEXX_SINGLE,
        &*RE_SEASON_SINGLE,
        &*RE_CN_SEASON,
        &*RE_RELEASE_SPEC_CUT,
    ] {
        if let Some(m) = re.find(&candidate) {
            if m.start() > 1 {
                candidate.truncate(m.start());
            }
        }
    }
    for sep in ["|", "｜", " / ", "【", "["] {
        if let Some(idx) = candidate.find(sep) {
            if idx > 0 {
                candidate.truncate(idx);
            }
        }
    }
    // Split CJK dual-title `育种师/繁殖者` -> `育种师` (without breaking `1/2` or `9/11`)
    if let Some(slash_idx) = candidate.find('/') {
        let left = candidate[..slash_idx].trim();
        let right = candidate[slash_idx + 1..].trim();
        if left.chars().filter(|c| !c.is_ascii()).count() >= 2
            && right.chars().filter(|c| !c.is_ascii()).count() >= 2
        {
            candidate = left.to_string();
        }
    }
    // Convert dot-separated Latin scene names (`Once.Upon.a.Time.in.the.Middle.East`) to spaces
    if candidate.contains('.') && !candidate.contains(' ') {
        let dot_parts: Vec<&str> = candidate.split('.').filter(|s| !s.is_empty()).collect();
        if dot_parts.len() >= 2
            && dot_parts
                .iter()
                .all(|p| p.chars().all(|c| c.is_ascii_alphanumeric() || c == '\'' || c == '-'))
        {
            candidate = dot_parts.join(" ");
        }
    }

    candidate
        .trim_matches(
            &[
                ' ', '.', '_', '-', '—', '–', ':', '：', '(', ')', '（', '）', '[', ']', '【',
                '】', '\t',
            ][..],
        )
        .trim()
        .to_string()
}

pub fn extract_pure_title(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");

    // Pass 1: Explicit labeled title line anywhere in the post (e.g. `🎬 影视：欢迎来龙餐馆`, `🎬 名称：胭脂扣`, `[剧集]：我的偶像总裁`)
    for line in normalized.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(cap) = RE_LABELED_TITLE_LINE.captures(l) {
            let val = normalize_title_candidate(&cap[1]);
            if !val.is_empty() && !is_suspect_banner_title(&val) {
                return val;
            }
        }
    }

    // Pass 2: First non-banner, non-metadata content line
    for line in normalized.lines() {
        let l = line.trim();
        if is_metadata_or_banner_line(l) {
            continue;
        }
        let val = normalize_title_candidate(l);
        if !val.is_empty() && !is_suspect_banner_title(&val) {
            return val;
        }
    }

    // Pass 3: Hashtag fallback (`#欢迎来龙餐馆 #电影 #2160P`)
    for cap in RE_HASHTAG.captures_iter(&normalized) {
        let tag = cap[1].trim();
        let low = tag.to_lowercase();
        if tag.chars().count() >= 2
            && !matches!(
                low.as_str(),
                "电影"
                    | "剧集"
                    | "电视剧"
                    | "动漫"
                    | "动画"
                    | "番剧"
                    | "综艺"
                    | "纪录片"
                    | "短剧"
                    | "国产剧"
                    | "美剧"
                    | "韩剧"
                    | "日剧"
                    | "热更"
                    | "完结"
                    | "合集"
                    | "4k"
                    | "2160p"
                    | "1080p"
                    | "hdr"
                    | "dv"
                    | "remux"
                    | "bluray"
                    | "原盘"
            )
            && !is_suspect_banner_title(tag)
        {
            return tag.to_string();
        }
    }

    String::new()
}

pub fn clean_display_title(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains('\n') {
        let pure = extract_pure_title(trimmed);
        if !pure.is_empty() {
            return pure;
        }
    }
    let mut s = RE_TMDB_BRACKET_TAG.replace_all(trimmed, "").into_owned();
    let mut preserved = String::new();
    loop {
        let before = s.clone();
        s = RE_LEADING_DECOR.replace(&s, "").into_owned();
        while let Some(cap) = RE_BRACKET_PREFIX.captures(&s) {
            let inner = cap
                .get(1)
                .or_else(|| cap.get(2))
                .or_else(|| cap.get(3))
                .map(|m| m.as_str().trim())
                .unwrap_or_default()
                .to_string();
            let after = s[cap.get(0).unwrap().end()..].trim().to_string();
            if is_bracket_tag_noise(&inner) || (!after.is_empty() && !after.starts_with('[') && !after.starts_with('【')) {
                s = after;
            } else {
                if preserved.is_empty() {
                    preserved = inner;
                }
                s = after;
            }
        }
        s = RE_CAT_PREFIX.replace(&s, "").into_owned();
        if s == before {
            break;
        }
    }
    if s.trim().is_empty() && !preserved.is_empty() {
        s = preserved;
    }
    let cleaned = s
        .trim_matches(&[' ', ':', '：', '-', '—', '–', '\t'][..])
        .trim();
    if cleaned.is_empty() {
        trimmed.to_string()
    } else {
        cleaned.to_string()
    }
}

pub fn extract_year_str(raw: &str) -> String {
    RE_YEAR
        .captures(raw)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

pub fn normalize_title_for_match(raw: &str) -> String {
    clean_display_title(raw)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

pub fn parse_size_to_gb(s: &str) -> f64 {
    let Some(cap) = RE_SIZE_TOKEN.captures(s) else {
        return 0.0;
    };
    let Ok(val) = cap[1].parse::<f64>() else {
        return 0.0;
    };
    if val <= 0.0 {
        return 0.0;
    }
    match cap[2].to_ascii_uppercase().as_str() {
        "TB" | "TIB" | "T" => val * 1024.0,
        "GB" | "GIB" | "G" => val,
        "MB" | "MIB" | "M" => val / 1024.0,
        _ => 0.0,
    }
}

pub fn format_bytes(b: i64) -> String {
    if b <= 0 {
        return String::new();
    }
    let gb = (b as f64) / (1024.0 * 1024.0 * 1024.0);
    if gb >= 1024.0 {
        format!("{:.2} TB", gb / 1024.0)
    } else if gb >= 1.0 {
        format!("{:.2} GB", gb)
    } else {
        format!("{:.1} MB", (b as f64) / (1024.0 * 1024.0))
    }
}

pub fn parse_movie_attributes(text: &str) -> MovieAttributes {
    let low = format!(" {} ", text.to_lowercase());
    let mut a = MovieAttributes::default();

    if low.contains("2160p") || low.contains("4k") || low.contains("uhd") {
        a.resolution = "4K".into();
    } else if low.contains("1080p") || low.contains("1080i") {
        a.resolution = "1080P".into();
    } else if low.contains("720p") || low.contains("576p") || low.contains("480p") {
        a.resolution = "720P".into();
    }

    if low.contains("remux") {
        a.remux = "REMUX".into();
    } else if low.contains("bluray") || low.contains("blu-ray") {
        a.remux = "BluRay".into();
    } else if low.contains("web-dl") || low.contains("webdl") || low.contains("webrip") {
        a.remux = "WEB-DL".into();
    }

    let has_dv = low.contains("dolby vision")
        || low.contains("杜比视界")
        || contains_token(&low, "dv")
        || low.contains("dovi");
    let has_hdr10 = low.contains("hdr10+") || low.contains("hdr10");
    let has_hdr = has_hdr10 || contains_token(&low, "hdr") || low.contains("高动态");
    let has_hlg = contains_token(&low, "hlg");

    if has_dv && has_hdr {
        a.color = "DV&HDR".into();
    } else if has_dv {
        a.color = "DV".into();
    } else if has_hdr10 {
        a.color = "HDR10".into();
    } else if has_hdr {
        a.color = "HDR".into();
    } else if has_hlg {
        a.color = "HLG".into();
    } else if contains_token(&low, "sdr") {
        a.color = "SDR".into();
    }

    if low.contains("truehd") || low.contains("atmos") || low.contains("杜比全景声") {
        a.audio = "TRUEHD".into();
    } else if low.contains("dts-hd.ma") || low.contains("dts-hd ma") || low.contains("dtshd") {
        a.audio = "DTS-HD MA".into();
    } else if low.contains("dts-x") || low.contains("dtsx") {
        a.audio = "DTS-X".into();
    } else if low.contains("dts-hd") {
        a.audio = "DTS-HD".into();
    } else if contains_token(&low, "dts") {
        a.audio = "DTS".into();
    } else if low.contains("ddp") || low.contains("eac3") || low.contains("dd+") {
        a.audio = "EAC3".into();
    } else if contains_token(&low, "ac3") || contains_token(&low, "dd") {
        a.audio = "AC3".into();
    } else if low.contains("flac") {
        a.audio = "FLAC".into();
    } else if low.contains("aac") {
        a.audio = "AAC".into();
    }

    if let Some(cap) = RE_CONTAINER_EXT.captures(&low) {
        a.ext = cap[1].to_ascii_uppercase();
    } else if contains_token(&low, "iso") || low.contains("原盘iso") {
        a.ext = "ISO".into();
    }

    if low.contains("特效") {
        a.subtitle = "特效字幕".into();
    } else if low.contains("中英")
        || low.contains("双语")
        || low.contains("简繁")
        || low.contains("中字")
        || low.contains("chs")
    {
        a.subtitle = "中字".into();
    }

    if let Some(first_line) = text.lines().next() {
        if let Some(cap) = RE_GROUP_SUFFIX.captures(first_line.trim()) {
            let grp = &cap[1];
            if !matches!(
                grp.to_ascii_lowercase().as_str(),
                "10bit" | "x265" | "h265" | "dl"
            ) {
                a.group = grp.to_string();
            }
        }
    }
    a
}

pub fn merge_movie_attributes(outer: &MovieAttributes, inner: &MovieAttributes) -> MovieAttributes {
    let mut m = inner.clone();
    if m.resolution.is_empty() {
        m.resolution = outer.resolution.clone();
    }
    if m.remux.is_empty() {
        m.remux = outer.remux.clone();
    }
    if m.audio.is_empty() {
        m.audio = outer.audio.clone();
    }
    if m.group.is_empty() {
        m.group = outer.group.clone();
    }
    if m.ext.is_empty() {
        m.ext = outer.ext.clone();
    }
    if m.subtitle.is_empty() {
        m.subtitle = outer.subtitle.clone();
    }
    if m.color == "DV&HDR"
        || outer.color == "DV&HDR"
        || (m.color == "DV" && outer.color.starts_with("HDR"))
        || (outer.color == "DV" && m.color.starts_with("HDR"))
    {
        m.color = "DV&HDR".into();
    } else if m.color.is_empty() {
        m.color = outer.color.clone();
    }
    m
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpisodePair {
    pub season: u32,
    pub episode: u32,
}

impl EpisodePair {
    pub fn key(&self) -> String {
        format!("S{:02}E{:02}", self.season, self.episode)
    }
}

fn push_ep(seen: &mut BTreeSet<EpisodePair>, s: u32, e: u32) {
    if s <= 99 && (1..=2500).contains(&e) {
        seen.insert(EpisodePair { season: s, episode: e });
    }
}

fn is_plausible_tight_range(start: u32, end: u32) -> bool {
    if end < start || end - start > 500 {
        return false;
    }
    if matches!(end, 480 | 576 | 720 | 1080 | 2160 | 4320) && end.saturating_sub(start) > 50 {
        return false;
    }
    if (1900..=2099).contains(&end) && start < 1800 {
        return false;
    }
    true
}

fn strip_schedule_preview_lines(text: &str) -> String {
    if !text.contains('\n') {
        return text.to_string();
    }
    let mut kept = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.contains("计划：")
            || l.contains("计划:")
            || l.contains("下集")
            || l.contains("预告")
            || l.contains("播出时间")
        {
            continue;
        }
        kept.push(line);
    }
    kept.join("\n")
}

pub fn infer_season_num(text: &str, fallback: u32) -> u32 {
    infer_season(text, if fallback == 0 { 1 } else { fallback })
}

pub fn extract_episode_pairs(text: &str, default_season: u32) -> Vec<EpisodePair> {
    let def_s = if default_season == 0 { 1 } else { default_season };
    let cleaned = strip_schedule_preview_lines(text);
    let src = cleaned.as_str();
    let mut seen = BTreeSet::new();

    for cap in RE_SXXEXX_RANGE_EXPLICIT.captures_iter(src) {
        let s1 = cap[1].parse::<u32>().unwrap_or(0);
        let start = cap[2].parse::<u32>().unwrap_or(0);
        let s2 = cap
            .get(3)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(s1);
        let end = cap[4].parse::<u32>().unwrap_or(0);
        if s1 == s2 && end >= start && end - start <= 500 {
            for ep in start..=end {
                push_ep(&mut seen, s1, ep);
            }
        }
    }
    for cap in RE_SXXEXX_RANGE_TIGHT.captures_iter(src) {
        let s = cap[1].parse::<u32>().unwrap_or(0);
        let start = cap[2].parse::<u32>().unwrap_or(0);
        let end = cap[3].parse::<u32>().unwrap_or(0);
        if is_plausible_tight_range(start, end) {
            for ep in start..=end {
                push_ep(&mut seen, s, ep);
            }
        }
    }
    for cap in RE_SXXEXX_SINGLE.captures_iter(src) {
        let s = cap[1].parse::<u32>().unwrap_or(0);
        let ep = cap[2].parse::<u32>().unwrap_or(0);
        push_ep(&mut seen, s, ep);
    }

    if seen.is_empty() {
        let season = infer_season(src, def_s);
        for cap in RE_EP_ONLY_RANGE_EXPLICIT.captures_iter(src) {
            let start = cap[1].parse::<u32>().unwrap_or(0);
            let end = cap[2].parse::<u32>().unwrap_or(0);
            if end >= start && end - start <= 500 {
                for ep in start..=end {
                    push_ep(&mut seen, season, ep);
                }
            }
        }
        for cap in RE_EP_ONLY_RANGE_TIGHT.captures_iter(src) {
            let start = cap[1].parse::<u32>().unwrap_or(0);
            let end = cap[2].parse::<u32>().unwrap_or(0);
            if is_plausible_tight_range(start, end) {
                for ep in start..=end {
                    push_ep(&mut seen, season, ep);
                }
            }
        }
        for cap in RE_CN_EP_RANGE.captures_iter(src) {
            let start = cap[1].parse::<u32>().unwrap_or(0);
            let end = cap[2].parse::<u32>().unwrap_or(0);
            if end >= start && end - start <= 500 {
                for ep in start..=end {
                    push_ep(&mut seen, season, ep);
                }
            }
        }
        if seen.is_empty() {
            if let Some(cap) = RE_CN_UPDATE_TO.captures(src) {
                let end = cap[1].parse::<u32>().unwrap_or(0).min(500);
                for ep in 1..=end {
                    push_ep(&mut seen, season, ep);
                }
            } else if let Some(cap) = RE_CN_FULL_COUNT.captures(src) {
                let end = cap[1].parse::<u32>().unwrap_or(0).min(500);
                for ep in 1..=end {
                    push_ep(&mut seen, season, ep);
                }
            } else if let Some(cap) = RE_CN_EP_SINGLE.captures(src) {
                let ep = cap[1].parse::<u32>().unwrap_or(0);
                push_ep(&mut seen, season, ep);
            }
        }
    }

    seen.into_iter().collect()
}

pub fn parse_shallow_episodes(
    text: &str,
    media_type: &str,
    season_totals: Option<&HashMap<u32, u32>>,
) -> String {
    if media_type.eq_ignore_ascii_case("movie") {
        return String::new();
    }
    let pairs = extract_episode_pairs(text, 1);
    if !pairs.is_empty() {
        return format_episode_pairs(&pairs);
    }
    let mut seasons = extract_seasons(text);
    let full_pack = RE_FULL_MARKERS.is_match(text);
    if full_pack && seasons.is_empty() {
        seasons.push(1);
    }
    if !seasons.is_empty() {
        let parts: Vec<String> = seasons
            .into_iter()
            .map(|s| {
                if let Some(&total) = season_totals.and_then(|m| m.get(&s)) {
                    if total > 0 {
                        return format!("S{:02}E01-E{:02}", s, total);
                    }
                }
                format!("S{:02}", s)
            })
            .collect();
        return parts.join(" ");
    }
    String::new()
}

pub fn format_episode_pairs(pairs: &[EpisodePair]) -> String {
    if pairs.is_empty() {
        return String::new();
    }
    let mut by_season: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for p in pairs {
        by_season.entry(p.season).or_default().insert(p.episode);
    }
    let mut out = Vec::new();
    for (s, eps_set) in by_season {
        let eps: Vec<u32> = eps_set.into_iter().collect();
        if eps.is_empty() {
            continue;
        }
        let mut start = eps[0];
        let mut prev = eps[0];
        let mut emit = |a: u32, b: u32| {
            if a == b {
                out.push(format!("S{:02}E{:02}", s, a));
            } else {
                out.push(format!("S{:02}E{:02}-E{:02}", s, a, b));
            }
        };
        for &ep in &eps[1..] {
            if ep == prev + 1 {
                prev = ep;
            } else {
                emit(start, prev);
                start = ep;
                prev = ep;
            }
        }
        emit(start, prev);
    }
    out.join(" ")
}

fn infer_season(text: &str, fallback: u32) -> u32 {
    extract_seasons(text).last().copied().unwrap_or(fallback)
}

fn extract_seasons(text: &str) -> Vec<u32> {
    let mut list = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |s: u32| {
        if (1..=50).contains(&s) && seen.insert(s) {
            list.push(s);
        }
    };
    for cap in RE_SEASON_RANGE.captures_iter(text) {
        let a = cap[1].parse::<u32>().unwrap_or(0);
        let b = cap[2].parse::<u32>().unwrap_or(0);
        if b >= a && b - a <= 30 {
            for s in a..=b {
                add(s);
            }
        }
    }
    for cap in RE_SEASON_SINGLE.captures_iter(text) {
        if let Ok(s) = cap[1].parse::<u32>() {
            add(s);
        }
    }
    for cap in RE_CN_SEASON.captures_iter(text) {
        let n = cn_num_to_u32(&cap[1]);
        if n > 0 {
            add(n);
        }
    }
    list
}

fn cn_num_to_u32(s: &str) -> u32 {
    let trimmed = s.trim();
    if let Ok(n) = trimmed.parse::<u32>() {
        return n;
    }
    let mut total = 0u32;
    let mut cur = 0u32;
    for ch in trimmed.chars() {
        match ch {
            '零' | '〇' => cur = 0,
            '一' => cur = 1,
            '二' | '两' => cur = 2,
            '三' => cur = 3,
            '四' => cur = 4,
            '五' => cur = 5,
            '六' => cur = 6,
            '七' => cur = 7,
            '八' => cur = 8,
            '九' => cur = 9,
            '十' => {
                total += if cur == 0 { 1 } else { cur } * 10;
                cur = 0;
            }
            '百' => {
                total += if cur == 0 { 1 } else { cur } * 100;
                cur = 0;
            }
            _ => return 0,
        }
    }
    total + cur
}

pub fn contains_token(haystack_lower: &str, token_lower: &str) -> bool {
    let t_bytes = token_lower.as_bytes();
    if t_bytes.is_empty() || haystack_lower.len() < t_bytes.len() {
        return false;
    }
    for (idx, _) in haystack_lower.match_indices(token_lower) {
        let before_ok = idx == 0
            || !haystack_lower[..idx]
                .chars()
                .next_back()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
        let after_pos = idx + token_lower.len();
        let after_ok = after_pos >= haystack_lower.len()
            || !haystack_lower[after_pos..]
                .chars()
                .next()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// NextFind v3.2.8 one-sided 0.1% (0.001) size tolerance check.
pub fn shield_size_match(item_size_gb: f64, local_size_gb: f64) -> Option<f64> {
    if item_size_gb <= 0.0 || local_size_gb <= 0.0 {
        return None;
    }
    let diff = item_size_gb - local_size_gb;
    if diff < 0.0 || diff / local_size_gb > 0.001 {
        return None;
    }
    Some(diff)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoreResult {
    pub accepted: bool,
    pub score: f64,
    pub reasons: Vec<String>,
}

pub fn score_resource(
    item: &mut ResourceItem,
    cfg: &RuleConfig,
    target_title: &str,
    keyword: &str,
) -> ScoreResult {
    let eff = cfg;

    let clean_title = strip_ignored_texts(&item.title, &eff.ignored_texts);
    let clean_raw = strip_ignored_texts(&item.raw_title, &eff.ignored_texts);
    let clean_remark = strip_ignored_texts(&item.remark, &eff.ignored_texts);

    if !eff.ignored_texts.is_empty() {
        let reparsed = parse_movie_attributes(&format!("{} {} {}", clean_title, clean_raw, clean_remark));
        if item.movie_attributes.resolution.is_empty() {
            item.movie_attributes.resolution = reparsed.resolution;
        }
        if item.movie_attributes.color.is_empty() {
            item.movie_attributes.color = reparsed.color;
        }
        if item.movie_attributes.audio.is_empty() {
            item.movie_attributes.audio = reparsed.audio;
        }
        if item.movie_attributes.ext.is_empty() {
            item.movie_attributes.ext = reparsed.ext;
        }
    }

    let blob = format!(
        " {} {} {} {} {} {} {} {} {} {} {} ",
        clean_title,
        clean_raw,
        clean_remark,
        item.parsed_episodes,
        item.movie_attributes.resolution,
        item.movie_attributes.remux,
        item.movie_attributes.color,
        item.movie_attributes.audio,
        item.movie_attributes.subtitle,
        item.movie_attributes.group,
        item.movie_attributes.ext
    )
    .to_lowercase();

    // 1. Resource Type filter & preference (115 / ED2K / MAGNET)
    let pan_label = match item.pan_type.trim().to_ascii_lowercase().as_str() {
        "ed2k" => "ED2K",
        "magnet" => "MAGNET",
        _ => "115",
    };
    let mut res_type_bonus = 0.0f64;
    if let Some(pos) = eff
        .resource_types
        .iter()
        .position(|rt| rt.name.trim().eq_ignore_ascii_case(pan_label))
    {
        if !eff.resource_types[pos].enabled {
            return ScoreResult {
                accepted: false,
                score: -90.0,
                reasons: vec![format!("屏蔽类型:{}", pan_label)],
            };
        }
        let enabled_pos = eff.resource_types[..pos].iter().filter(|x| x.enabled).count();
        res_type_bonus = (4.5 - (enabled_pos as f64) * 1.5).max(0.5);
    }

    // 2. Block Filters (single keyword OR multi-tag `且` AND group)
    for grp in &eff.block_filters {
        let active_tags: Vec<&str> = grp
            .tags
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if active_tags.is_empty() {
            continue;
        }
        if active_tags
            .iter()
            .all(|t| matches_filter_tag(t, pan_label, item, &blob))
        {
            return ScoreResult {
                accepted: false,
                score: -100.0,
                reasons: vec![format!("屏蔽过滤:{}", active_tags.join(" 且 "))],
            };
        }
    }

    // 3. Parse size in GB & check custom MB floor + disabled size ranges
    let mut size_gb = parse_size_to_gb(&item.share_size);
    if size_gb <= 0.0 && item.size_bytes > 0 {
        size_gb = (item.size_bytes as f64) / (1024.0 * 1024.0 * 1024.0);
    }
    if size_gb > 0.0 {
        for sc in &eff.size_items {
            if sc.is_custom_size && sc.value > 0 {
                let size_mb = size_gb * 1024.0;
                if size_mb < (sc.value as f64) {
                    return ScoreResult {
                        accepted: false,
                        score: -60.0,
                        reasons: vec![format!("体积过小:<{}MB", sc.value)],
                    };
                }
            }
        }
        if let Some(sc) = find_matching_size_range(&eff.size_items, size_gb) {
            if !sc.enabled {
                return ScoreResult {
                    accepted: false,
                    score: -60.0,
                    reasons: vec![format!("屏蔽体积:{}", sc.name)],
                };
            }
        }
    }

    // 4. Disabled chips in Resolution / Color / Audio / Extension / Group
    if !item.movie_attributes.resolution.is_empty() {
        if let Some(rc) = find_resolution_chip(&eff.resolution_items, &item.movie_attributes.resolution) {
            if !rc.enabled {
                return ScoreResult {
                    accepted: false,
                    score: -50.0,
                    reasons: vec![format!("屏蔽分辨率:{}", rc.name)],
                };
            }
        }
    }
    if !item.movie_attributes.color.is_empty() {
        if let Some(cc) = eff
            .color_items
            .iter()
            .find(|c| c.name.trim().eq_ignore_ascii_case(&item.movie_attributes.color))
        {
            if !cc.enabled {
                return ScoreResult {
                    accepted: false,
                    score: -50.0,
                    reasons: vec![format!("屏蔽色彩:{}", cc.name)],
                };
            }
        }
    }
    if !item.movie_attributes.audio.is_empty() {
        if let Some(ac) = eff
            .audio_items
            .iter()
            .find(|a| a.name.trim().eq_ignore_ascii_case(&item.movie_attributes.audio))
        {
            if !ac.enabled {
                return ScoreResult {
                    accepted: false,
                    score: -50.0,
                    reasons: vec![format!("屏蔽音频:{}", ac.name)],
                };
            }
        }
    }
    if !item.movie_attributes.ext.is_empty() {
        if let Some(ec) = find_extension_chip(&eff.extension_items, &item.movie_attributes.ext) {
            if !ec.enabled {
                return ScoreResult {
                    accepted: false,
                    score: -80.0,
                    reasons: vec![format!("屏蔽格式:{}", item.movie_attributes.ext)],
                };
            }
        }
    }
    for gc in &eff.group_items {
        let w = gc.name.trim();
        if !w.is_empty() && !gc.enabled && matches_keyword_in_blob(&blob, w) {
            return ScoreResult {
                accepted: false,
                score: -80.0,
                reasons: vec![format!("屏蔽关键词:{}", w)],
            };
        }
    }

    // 5. Dynamic NextFind Category Weights Scoring
    const CAT_MAX_SCORES: [f64; 6] = [34.0, 20.0, 15.0, 10.0, 9.0, 10.0];
    let mut score = res_type_bonus;
    let mut reasons = Vec::new();
    let mut kw_hits = Vec::new();

    for (cat_rank, cat_key) in eff.weights.iter().enumerate() {
        let cat_max = CAT_MAX_SCORES.get(cat_rank).copied().unwrap_or(9.0);
        match cat_key.as_str() {
            "resolutions" => {
                let enabled: Vec<&RuleChipItem> =
                    eff.resolution_items.iter().filter(|x| x.enabled).collect();
                if let Some(pos) = enabled
                    .iter()
                    .position(|rc| resolution_chip_matches(&rc.name, &item.movie_attributes.resolution))
                {
                    score += weighted_chip_points(pos, enabled.len(), cat_max);
                    reasons.push(item.movie_attributes.resolution.clone());
                }
            }
            "colors" => {
                let enabled: Vec<&RuleChipItem> =
                    eff.color_items.iter().filter(|x| x.enabled).collect();
                if !item.movie_attributes.color.is_empty() {
                    if let Some(pos) = enabled
                        .iter()
                        .position(|c| c.name.trim().eq_ignore_ascii_case(&item.movie_attributes.color))
                    {
                        score += weighted_chip_points(pos, enabled.len(), cat_max);
                        reasons.push(item.movie_attributes.color.clone());
                    }
                }
            }
            "audios" => {
                let enabled: Vec<&RuleChipItem> =
                    eff.audio_items.iter().filter(|x| x.enabled).collect();
                if !item.movie_attributes.audio.is_empty() {
                    if let Some(pos) = enabled
                        .iter()
                        .position(|a| a.name.trim().eq_ignore_ascii_case(&item.movie_attributes.audio))
                    {
                        score += weighted_chip_points(pos, enabled.len(), cat_max);
                        reasons.push(item.movie_attributes.audio.clone());
                    }
                }
            }
            "extensions" => {
                if !item.movie_attributes.ext.is_empty() {
                    let enabled: Vec<&RuleChipItem> =
                        eff.extension_items.iter().filter(|x| x.enabled).collect();
                    if let Some(matched) =
                        find_extension_chip(&eff.extension_items, &item.movie_attributes.ext)
                    {
                        if let Some(pos) = enabled.iter().position(|e| e.name == matched.name) {
                            score += weighted_chip_points(pos, enabled.len(), cat_max);
                            reasons.push(item.movie_attributes.ext.clone());
                        }
                    }
                }
            }
            "sizes" => {
                if size_gb > 0.0 {
                    let prefer_larger = eff
                        .size_items
                        .iter()
                        .find(|x| x.name == "由大到小")
                        .map(|x| x.enabled)
                        .unwrap_or(true);
                    let enabled_ranges: Vec<&RuleChipItem> = eff
                        .size_items
                        .iter()
                        .filter(|x| x.enabled && !x.is_custom_size && x.name != "由大到小")
                        .collect();
                    if let Some(pos) = enabled_ranges
                        .iter()
                        .position(|sc| size_range_matches(sc, size_gb))
                    {
                        score += weighted_chip_points(pos, enabled_ranges.len(), cat_max);
                    }
                    if prefer_larger {
                        score += (size_gb * 0.12).min(5.0);
                    }
                }
            }
            "groups" => {
                let enabled: Vec<&RuleChipItem> = eff
                    .group_items
                    .iter()
                    .filter(|x| x.enabled && !x.name.trim().is_empty())
                    .collect();
                let mut best_pts = 0.0f64;
                for (pos, gc) in enabled.iter().enumerate() {
                    let w = gc.name.trim();
                    if matches_keyword_in_blob(&blob, w) {
                        kw_hits.push(w.to_string());
                        let pts = weighted_chip_points(pos, enabled.len(), cat_max);
                        if pts > best_pts {
                            best_pts = pts;
                        }
                    }
                }
                if !kw_hits.is_empty() {
                    let extra = ((kw_hits.len().saturating_sub(1)) as f64 * 2.0).min(6.0);
                    score += (best_pts * 0.75 + extra).min(cat_max + 4.0);
                    reasons.push(format!("关键词:{}", kw_hits.join("/")));
                }
            }
            _ => {}
        }
    }
    item.custom_keywords = kw_hits;

    // 6. Remux & Subtitle quality bonus
    match item.movie_attributes.remux.as_str() {
        "REMUX" => {
            score += 14.0;
            reasons.push("REMUX".into());
        }
        "BluRay" => {
            score += 10.0;
            reasons.push("BluRay".into());
        }
        "WEB-DL" => {
            score += 6.0;
            reasons.push("WEB-DL".into());
        }
        _ => {}
    }
    if item.movie_attributes.subtitle == "特效字幕" {
        score += 6.0;
        reasons.push("特效字幕".into());
    } else if !item.movie_attributes.subtitle.is_empty() {
        score += 4.0;
        reasons.push(item.movie_attributes.subtitle.clone());
    }

    // 7. Subscription source priority (including per-TG-channel priority) & unlock points bonus
    if let Some((idx, sub_src)) = eff.find_sub_source_for_item(item) {
        if sub_src.enabled {
            let total = eff.subscription_sources.len().max(1);
            let pts = if total <= 1 {
                6.0
            } else {
                6.0 - (idx as f64 / (total - 1) as f64) * 5.0
            };
            score += pts.max(0.5);
        }
    }
    if item.is_unlocked || item.unlock_points == 0 {
        score += 5.0;
        reasons.push("免积分".into());
    } else if item.unlock_points > 0 {
        score -= (item.unlock_points as f64) * 0.3;
        reasons.push(format!("需{}分", item.unlock_points));
    }

    // 8. Relevance
    if !target_title.trim().is_empty() && blob.contains(&target_title.trim().to_lowercase()) {
        score += 6.0;
    }
    if !keyword.trim().is_empty() && !blob.contains(&keyword.trim().to_lowercase()) {
        return ScoreResult {
            accepted: false,
            score: -1.0,
            reasons: vec!["关键词未命中".into()],
        };
    }

    let rounded = (score * 100.0).round() / 100.0;
    let accepted = rounded >= eff.min_score;
    ScoreResult {
        accepted,
        score: rounded,
        reasons,
    }
}

fn weighted_chip_points(pos: usize, total: usize, cat_max: f64) -> f64 {
    if total <= 1 {
        return cat_max;
    }
    let ratio = (pos as f64) / ((total - 1) as f64);
    (cat_max * (1.0 - ratio * 0.65)).max(cat_max * 0.25)
}

fn resolution_chip_matches(chip_name: &str, item_res: &str) -> bool {
    let cn = chip_name.trim();
    let ir = item_res.trim();
    if cn.is_empty() || ir.is_empty() {
        return false;
    }
    if cn == "720P及以下" {
        ir.eq_ignore_ascii_case("720P")
            || ir.eq_ignore_ascii_case("576P")
            || ir.eq_ignore_ascii_case("480P")
    } else {
        cn.eq_ignore_ascii_case(ir)
    }
}

fn find_resolution_chip<'a>(
    items: &'a [RuleChipItem],
    item_res: &str,
) -> Option<&'a RuleChipItem> {
    items.iter().find(|rc| resolution_chip_matches(&rc.name, item_res))
}

fn find_extension_chip<'a>(
    items: &'a [RuleChipItem],
    item_ext: &str,
) -> Option<&'a RuleChipItem> {
    let ext = item_ext.trim();
    if ext.is_empty() {
        return None;
    }
    if let Some(exact) = items
        .iter()
        .find(|e| e.name != "小众格式" && e.name.trim().eq_ignore_ascii_case(ext))
    {
        return Some(exact);
    }
    items.iter().find(|e| e.name == "小众格式")
}

fn size_range_matches(sc: &RuleChipItem, size_gb: f64) -> bool {
    if sc.is_custom_size || sc.name == "由大到小" || size_gb <= 0.0 {
        return false;
    }
    match sc.name.trim() {
        "115G以上" => size_gb >= 115.0,
        "40G以上" => (40.0..115.0).contains(&size_gb),
        "20-40G" => (20.0..40.0).contains(&size_gb),
        "10-20G" => (10.0..20.0).contains(&size_gb),
        "5-10G" => (5.0..10.0).contains(&size_gb),
        "0-5G" => size_gb > 0.0 && size_gb < 5.0,
        _ => sc.max > sc.min && size_gb >= sc.min && size_gb < sc.max,
    }
}

fn find_matching_size_range(items: &[RuleChipItem], size_gb: f64) -> Option<&RuleChipItem> {
    items.iter().find(|sc| size_range_matches(sc, size_gb))
}

fn matches_keyword_in_blob(blob: &str, word: &str) -> bool {
    let wl = word.trim().to_lowercase();
    if wl.is_empty() {
        return false;
    }
    if wl.len() <= 4 && wl.chars().all(|c| c.is_ascii_alphanumeric()) {
        contains_token(blob, &wl)
    } else {
        blob.contains(&wl)
    }
}

fn matches_filter_tag(tag: &str, pan_label: &str, item: &ResourceItem, blob: &str) -> bool {
    let t = tag.trim();
    if t.is_empty() {
        return false;
    }
    let up = t.to_ascii_uppercase();
    match up.as_str() {
        "115" => return pan_label == "115",
        "ED2K" => return pan_label == "ED2K",
        "MAGNET" | "磁力" => return pan_label == "MAGNET",
        "4K" | "2160P" => return item.movie_attributes.resolution.eq_ignore_ascii_case("4K"),
        "1080P" => return item.movie_attributes.resolution.eq_ignore_ascii_case("1080P"),
        "720P" | "720P及以下" => {
            return resolution_chip_matches("720P及以下", &item.movie_attributes.resolution)
        }
        "DV&HDR" | "DV" | "HDR10" | "HDR" | "HLG" | "SDR" => {
            if item.movie_attributes.color.eq_ignore_ascii_case(&up) {
                return true;
            }
            if up == "DV" && item.movie_attributes.color == "DV&HDR" {
                return true;
            }
            if up == "HDR" && item.movie_attributes.color.contains("HDR") {
                return true;
            }
            return false;
        }
        "MKV" | "MP4" | "M2TS" | "TS" | "ISO" | "RMVB" | "AVI" | "MOV" | "WMV" => {
            if item.movie_attributes.ext.eq_ignore_ascii_case(&up) {
                return true;
            }
        }
        _ => {}
    }
    if item.movie_attributes.audio.eq_ignore_ascii_case(t)
        || item.movie_attributes.remux.eq_ignore_ascii_case(t)
    {
        return true;
    }
    matches_keyword_in_blob(blob, t)
}

pub fn rank_and_dedupe(
    items: Vec<ResourceItem>,
    cfg: &RuleConfig,
    target_title: &str,
    keyword: &str,
) -> Vec<ResourceItem> {
    let mut by_slug: HashMap<String, ResourceItem> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut no_slug: Vec<ResourceItem> = Vec::new();

    for mut it in items {
        if it.clean_slug.is_empty() {
            it.clean_slug = clean_slug(&it.slug);
        }
        if it.movie_attributes.resolution.is_empty() && it.movie_attributes.color.is_empty() {
            let outer = parse_movie_attributes(&format!("{} {}", it.title, it.remark));
            it.movie_attributes = merge_movie_attributes(&outer, &it.movie_attributes);
        }
        let res = score_resource(&mut it, cfg, target_title, keyword);
        it.rule_score = res.score;
        it.rule_reasons = res.reasons;
        if !res.accepted {
            continue;
        }
        if it.clean_slug.is_empty() {
            no_slug.push(it);
            continue;
        }
        if let Some(prev) = by_slug.remove(&it.clean_slug) {
            let merged = if it.rule_score > prev.rule_score || (it.is_unlocked && !prev.is_unlocked) {
                merge_resource_item(prev, it)
            } else {
                merge_resource_item(it, prev)
            };
            by_slug.insert(merged.clean_slug.clone(), merged);
        } else {
            order.push(it.clean_slug.clone());
            by_slug.insert(it.clean_slug.clone(), it);
        }
    }

    let mut out = Vec::with_capacity(order.len() + no_slug.len());
    for k in order {
        if let Some(v) = by_slug.remove(&k) {
            out.push(v);
        }
    }
    out.extend(no_slug);

    out.sort_by(|a, b| {
        let a_hit = !a.missing_eps_hit.is_empty();
        let b_hit = !b.missing_eps_hit.is_empty();
        if a_hit != b_hit {
            return b_hit.cmp(&a_hit);
        }
        b.rule_score
            .partial_cmp(&a.rule_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    out
}

fn merge_resource_item(secondary: ResourceItem, mut primary: ResourceItem) -> ResourceItem {
    if primary.share_code.is_empty() {
        primary.share_code = secondary.share_code;
    }
    if primary.receive_code.is_empty() {
        primary.receive_code = secondary.receive_code;
    }
    if primary.share_size.is_empty() {
        primary.share_size = secondary.share_size;
    }
    if primary.size_bytes == 0 {
        primary.size_bytes = secondary.size_bytes;
    }
    if primary.parsed_episodes.is_empty() {
        primary.parsed_episodes = secondary.parsed_episodes;
    }
    if primary.remark.is_empty() {
        primary.remark = secondary.remark;
    }
    primary.movie_attributes =
        merge_movie_attributes(&secondary.movie_attributes, &primary.movie_attributes);
    primary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_115_and_links() {
        let (sc, rc) = parse_115_share("https://115cdn.com/s/sw33aabbcc?password=x9Y8", "");
        assert_eq!(sc, "sw33aabbcc");
        assert_eq!(rc, "x9Y8");

        let msg = "剧集《庆余年 第二季》S02E01-E36 2160p DV&HDR\n链接：https://115.com/s/sw11223344 访问码：ab12\ned2k://|file|Joy.of.Life.S02E01.2160p.mkv|4294967296|0123456789abcdef0123456789abcdef|/";
        let links = extract_all_resource_links(msg);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].share_code, "sw11223344");
        assert_eq!(links[0].receive_code, "ab12");
        assert_eq!(links[1].pan_type, "ed2k");
    }

    #[test]
    fn test_episodes_and_scoring() {
        assert_eq!(
            parse_shallow_episodes("藏海传 第2季 第1-12集 4K", "tv", None),
            "S02E01-E12"
        );
        assert!(shield_size_match(42.03, 42.00).is_some());
        assert!(shield_size_match(41.90, 42.00).is_none());

        let cfg = RuleConfig::default();
        let mut iso_item = ResourceItem {
            title: "沙丘2 4K 原盘.iso".into(),
            movie_attributes: parse_movie_attributes("沙丘2 2160p BluRay.iso"),
            ..Default::default()
        };
        assert!(!score_resource(&mut iso_item, &cfg, "沙丘2", "").accepted);

        let mut good_item = ResourceItem {
            title: "沙丘2 2160p REMUX DV&HDR TrueHD 特效中字.mkv".into(),
            share_size: "62.5GB".into(),
            source_type: "telegram".into(),
            is_unlocked: true,
            movie_attributes: parse_movie_attributes(
                "沙丘2 2160p REMUX DV&HDR TrueHD 特效中字.mkv",
            ),
            ..Default::default()
        };
        let res = score_resource(&mut good_item, &cfg, "沙丘2", "");
        assert!(res.accepted && res.score >= 85.0);

        // NextFind multi-tag AND block filter (`4K 且 AAC`), custom min MB, and ignored_texts
        let mut nf_cfg = RuleConfig::default();
        nf_cfg.ensure_weights_initialized();
        nf_cfg.block_filters.push(crate::models::BlockFilterGroup {
            id: "and-1".into(),
            tags: vec!["4K".into(), "AAC".into()],
        });
        nf_cfg.ignored_texts = vec!["关注公众号领福利".into()];
        if let Some(custom_sz) = nf_cfg.size_items.iter_mut().find(|x| x.is_custom_size) {
            custom_sz.value = 300;
        }

        let mut combo_item = ResourceItem {
            title: "关注公众号领福利 沙丘2 2160p AAC.mp4".into(),
            share_size: "8.5GB".into(),
            movie_attributes: parse_movie_attributes("沙丘2 2160p AAC.mp4"),
            ..Default::default()
        };
        let combo_res = score_resource(&mut combo_item, &nf_cfg, "沙丘2", "");
        assert!(!combo_res.accepted);
        assert!(combo_res.reasons[0].contains("4K 且 AAC"));

        let mut ok_1080_aac = ResourceItem {
            title: "沙丘2 1080p AAC.mp4".into(),
            share_size: "4.2GB".into(),
            movie_attributes: parse_movie_attributes("沙丘2 1080p AAC.mp4"),
            ..Default::default()
        };
        assert!(score_resource(&mut ok_1080_aac, &nf_cfg, "沙丘2", "").accepted);

        let mut tiny_item = ResourceItem {
            title: "沙丘2 1080p EAC3.mkv".into(),
            share_size: "120MB".into(),
            movie_attributes: parse_movie_attributes("沙丘2 1080p EAC3.mkv"),
            ..Default::default()
        };
        assert!(!score_resource(&mut tiny_item, &nf_cfg, "沙丘2", "").accepted);

        let stripped = strip_ignored_texts("【关注公众号领福利】《沙丘2》2160p", &nf_cfg.ignored_texts);
        assert_eq!(extract_pure_title(&stripped), "沙丘2");
    }

    #[test]
    fn test_tg_channel_normalize_and_sub_sources_sync() {
        assert_eq!(
            crate::providers::normalize_tg_channel("https://t.me/s/Lsp115?q=abc"),
            "Lsp115"
        );
        assert_eq!(
            crate::providers::normalize_tg_channel("https://t.me/c/2167886055/8899"),
            "-1002167886055"
        );
        assert_eq!(
            crate::providers::normalize_tg_channel("https://t.me/joinchat/AbCdEf_123"),
            "https://t.me/+AbCdEf_123"
        );
        assert_eq!(
            crate::providers::normalize_tg_channel(
                "懒狗集中营-115/阿里/百度/迅雷/夸克 影视分享"
            ),
            "懒狗集中营-115/阿里/百度/迅雷/夸克 影视分享"
        );

        let mut cfg = RuleConfig::default();
        cfg.ensure_weights_initialized();
        let channels = vec![
            crate::models::TGChannelEntry {
                id: "ch-1".into(),
                identifier: "-5155069736".into(),
                name: "115搬搬群组".into(),
                enabled: true,
                ..Default::default()
            },
            crate::models::TGChannelEntry {
                id: "ch-2".into(),
                identifier: "-1001344815419".into(),
                name: "懒狗集中营-115/阿里/百度".into(),
                enabled: true,
                ..Default::default()
            },
        ];
        cfg.sync_subscription_sources(&channels);
        assert!(!cfg.subscription_sources.iter().any(|s| s.id == "telegram"));
        assert!(cfg
            .subscription_sources
            .iter()
            .any(|s| s.id == "telegram:-5155069736" && s.name == "115搬搬群组"));
        assert!(cfg
            .subscription_sources
            .iter()
            .any(|s| s.id == "telegram:-1001344815419" && s.name == "懒狗集中营-115/阿里/百度"));

        // Disable one TG channel for auto-chase and rename the other
        if let Some(s) = cfg
            .subscription_sources
            .iter_mut()
            .find(|s| s.id == "telegram:-1001344815419")
        {
            s.enabled = false;
        }
        let mut updated_channels = channels.clone();
        updated_channels[0].name = "115搬搬Pro".into();
        cfg.sync_subscription_sources(&updated_channels);

        let item1 = ResourceItem {
            source_type: "telegram".into(),
            channel_id: "-5155069736".into(),
            channel_name: "TG · 115搬搬Pro".into(),
            ..Default::default()
        };
        let item2 = ResourceItem {
            source_type: "telegram".into(),
            channel_id: "-1001344815419".into(),
            channel_name: "TG · 懒狗集中营-115/阿里/百度".into(),
            ..Default::default()
        };
        assert!(cfg.is_item_auto_sub_enabled(&item1));
        assert!(!cfg.is_item_auto_sub_enabled(&item2));
        assert_eq!(
            cfg.find_sub_source_for_item(&item1).unwrap().1.name,
            "115搬搬Pro"
        );
        assert_eq!(clean_display_title("📺 电视剧：妈妈发疯了 [tmdbid-12345]"), "妈妈发疯了");
        assert_eq!(extract_pure_title("📺 电视剧：芝加哥烈焰 S15E06 2160p"), "芝加哥烈焰");
    }

    #[test]
    fn test_real_world_tg_banners_tmdb_spaces_and_resolution_episodes() {
        // 1. Multi-line TG post with channel banner header (`✅新片送达–请您检阅`)
        let tg_banner_post = "✅新片送达–请您检阅\n👤 分享者：亲爱的小孩\n\n🎬 影视：欢迎来龙餐馆\n📦 质量：2160p HQ DV H.265 DD\n💾 大小：54.4GB\n\n📡 平台：优酷\n📅 年份：2026\n🧲 类型：电影 ｜ 主演：沈腾\n#欢迎来龙餐馆 #电影 #2160P";
        assert_eq!(extract_pure_title(tg_banner_post), "欢迎来龙餐馆");
        assert!(is_suspect_banner_title("✅新片送达–请您检阅"));
        assert!(is_suspect_banner_title("【LGNB首发】"));

        // 2. LGNB multi-line posts & bracketed titles (`[七龙珠][BD-REMUX]`, `[剧集]：我的偶像总裁`)
        let lgnb_post = "【LGNB首发】\n🎬 名称：胭脂扣\n📝 描述：在香港报馆任职的袁永定...";
        assert_eq!(extract_pure_title(lgnb_post), "胭脂扣");
        assert_eq!(
            extract_pure_title("[剧集]：我的偶像总裁 (2026)\nS01E01-E12 · 完结"),
            "我的偶像总裁"
        );
        assert_eq!(extract_pure_title("[七龙珠][BD-REMUX]\n◎年 代: 1986"), "七龙珠");
        assert_eq!(
            extract_pure_title(
                "Once.Upon.a.Time.in.the.Middle.East.2026.2160p.YOUKU.WEB-DL.DTS5.1.HQ.DV.HFR.H.265-SalmonWeb.mkv"
            ),
            "Once Upon a Time in the Middle East"
        );
        assert_eq!(
            extract_pure_title("欢迎来龙餐馆(2026)(tmdbid=1391021).2160p.WEB-DL.mp4"),
            "欢迎来龙餐馆"
        );

        // 3. Space-separated `TMDB ID: ...` and `tmdbid=...`
        assert_eq!(extract_tmdb_id("🍿 TMDB ID: 1391021"), "1391021");
        assert_eq!(extract_tmdb_id("🍿 TMDB ID：296140"), "296140");
        assert_eq!(
            extract_tmdb_id("欢迎来龙餐馆(2026)(tmdbid=1391021).2160p.mp4"),
            "1391021"
        );

        // 4. Resolution (`2160p` / `1080p`) after hyphen must NEVER be parsed as `-E60` / `-E80`!
        assert_eq!(
            parse_shallow_episodes("庶女 (2026) - S01E11 - 2160p.WEB-DL.H265.AAC.mp4", "tv", None),
            "S01E11"
        );
        assert_eq!(
            parse_shallow_episodes("妈妈发疯了 - S01E43 - 1080p.WEB-DL.mkv", "tv", None),
            "S01E43"
        );
        assert_eq!(
            parse_shallow_episodes("花儿与少年 - S08E19 - 1080p.mp4", "tv", None),
            "S08E19"
        );
        assert_eq!(
            parse_shallow_episodes("Reading Bones 2026 S01E01-S01E08 1080p", "tv", None),
            "S01E01-E08"
        );
        assert_eq!(
            parse_shallow_episodes("骨鉴卿心 S01E01-08 2160p", "tv", None),
            "S01E01-E08"
        );
        // Schedule preview line (`下集 ... S06E12`) must not pollute current episode `S06E11`
        let jojo_post = "✅新片送达–请您检阅\n🎬 影视：JOJO的奇妙冒险\n📺 季集：S06E11\n📶 计划：连载中 · 下集 12-04（周五）S06E12";
        assert_eq!(extract_pure_title(jojo_post), "JOJO的奇妙冒险");
        assert_eq!(parse_shallow_episodes(jojo_post, "tv", None), "S06E11");
    }
}

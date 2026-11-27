use crate::models::CategoryRule;
use anyhow::{bail, ensure, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn clean_parts(path: &str) -> Vec<&str> {
    path.trim()
        .split('/')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

pub fn parse_category_yaml(raw: &str) -> Result<Vec<CategoryRule>> {
    let stripped = raw.trim_start_matches('\u{feff}');
    let data: BTreeMap<String, serde_yaml::Mapping> = serde_yaml::from_str(stripped)?;
    let mut set = BTreeSet::new();
    for (media_type, rules) in data {
        if !matches!(media_type.as_str(), "movie" | "tv") {
            continue;
        }
        for (key, cond) in rules {
            let Some(path) = key.as_str() else {
                bail!("分类路径必须为字符串");
            };
            let parts = clean_parts(path);
            ensure!(
                !parts.is_empty() && !parts.iter().any(|&p| p == "." || p == ".."),
                "无效分类路径 {path}"
            );
            let normalized = parts.join("/");
            let mut iso_zone = normalized.contains("蓝光原盘")
                || normalized.to_ascii_lowercase().contains("iso");
            if let Some(map) = cond.as_mapping() {
                for (ck, cv) in map {
                    if let Some(ck_str) = ck.as_str() {
                        if ck_str.trim_start_matches('?') == "file_extension" {
                            if let Some(ext_s) = cv.as_str() {
                                let low = ext_s.to_ascii_lowercase();
                                if low.contains("iso") && !low.contains("-iso") && !low.contains("mkv") {
                                    iso_zone = true;
                                }
                            }
                        }
                    }
                }
            }
            set.insert(CategoryRule {
                media_type: media_type.clone(),
                path: normalized,
                iso_zone,
            });
        }
    }
    ensure!(!set.is_empty(), "分类文件内没有找到 movie 或 tv 二级分类");
    Ok(set.into_iter().collect())
}

pub fn parse_categories_json(val: &Value) -> Option<Vec<CategoryRule>> {
    let mut set = BTreeSet::new();
    let root = val.get("data").unwrap_or(val);
    for media_type in ["movie", "tv"] {
        if let Some(arr) = root.get(media_type).and_then(Value::as_array) {
            for item in arr {
                let path_opt = item
                    .as_str()
                    .or_else(|| item.get("path").and_then(Value::as_str))
                    .or_else(|| item.get("name").and_then(Value::as_str))
                    .or_else(|| item.get("category").and_then(Value::as_str));
                if let Some(raw_path) = path_opt {
                    let parts = clean_parts(raw_path);
                    if !parts.is_empty() && !parts.iter().any(|&p| p == "." || p == "..") {
                        let normalized = parts.join("/");
                        let iso_zone = normalized.contains("蓝光原盘")
                            || normalized.to_ascii_lowercase().contains("iso");
                        set.insert(CategoryRule {
                            media_type: media_type.to_string(),
                            path: normalized,
                            iso_zone,
                        });
                    }
                }
            }
        } else if let Some(obj) = root.get(media_type).and_then(Value::as_object) {
            for raw_path in obj.keys() {
                let parts = clean_parts(raw_path);
                if !parts.is_empty() && !parts.iter().any(|&p| p == "." || p == "..") {
                    let normalized = parts.join("/");
                    let iso_zone = normalized.contains("蓝光原盘")
                        || normalized.to_ascii_lowercase().contains("iso");
                    set.insert(CategoryRule {
                        media_type: media_type.to_string(),
                        path: normalized,
                        iso_zone,
                    });
                }
            }
        }
    }
    (!set.is_empty()).then(|| set.into_iter().collect())
}

pub fn within(path: &str, prefix: &str) -> bool {
    let p = prefix.trim().trim_matches('/');
    p.is_empty()
        || path == p
        || path
            .strip_prefix(p)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Locates the secondary category of a media source from its directory segments.
/// 1) Prefers exact deepest leaf category match (e.g. `电影/华语电影/2020-2030`).
/// 2) Falls back to unambiguous 2-segment branch prefix (e.g. `电影/华语电影`) when
///    library items sit directly under a library root like `/strm/cloud/媒体库/电影/华语电影/片名/片名.strm`.
/// 3) Returns `None` if multiple paths on the same source disagree (e.g. STRM vs ISO in different zones).
pub fn locate(categories: &[CategoryRule], media_type: &str, paths: &[&str]) -> Option<String> {
    if categories.is_empty() {
        return None;
    }
    let mut resolved: Option<String> = None;

    for raw_path in paths.iter().copied().filter(|p| !p.trim().is_empty()) {
        let normalized_input = raw_path.trim();
        let mut dirs = clean_parts(normalized_input);
        // Strip filename if last segment looks like a file or path has >1 segment
        if dirs
            .last()
            .is_some_and(|last| last.contains('.') || normalized_input.starts_with('/'))
            && !dirs.is_empty()
        {
            dirs.pop();
        }
        if dirs.is_empty() {
            continue;
        }

        // Pass 1: exact category path match (deepest wins)
        let mut best: Option<(&str, usize)> = None;
        let mut ambiguous = false;
        for cat in categories.iter().filter(|c| c.media_type == media_type) {
            let cat_parts = clean_parts(&cat.path);
            let depth = cat_parts.len();
            if depth == 0 || !dirs.windows(depth).any(|w| w == cat_parts.as_slice()) {
                continue;
            }
            match best {
                None => best = Some((&cat.path, depth)),
                Some((_, prior_d)) if depth > prior_d => {
                    best = Some((&cat.path, depth));
                    ambiguous = false;
                }
                Some((prior_name, prior_d)) if depth == prior_d && prior_name != cat.path => {
                    ambiguous = true;
                }
                _ => {}
            }
        }
        if ambiguous {
            return None;
        }
        let path_match = if let Some((exact, _)) = best {
            Some(exact.to_string())
        } else {
            // Pass 2: unambiguous 2-segment branch prefix (e.g. `电影/华语电影`, `动漫/儿童`)
            let mut branch_hits = BTreeSet::new();
            for cat in categories.iter().filter(|c| c.media_type == media_type) {
                let cat_parts = clean_parts(&cat.path);
                if cat_parts.len() >= 2 {
                    let prefix_parts = &cat_parts[..2];
                    if dirs.windows(2).any(|w| w == prefix_parts) {
                        branch_hits.insert(format!("{}/{}", prefix_parts[0], prefix_parts[1]));
                    }
                }
            }
            if branch_hits.len() == 1 {
                branch_hits.into_iter().next()
            } else {
                None
            }
        };

        if let Some(hit) = path_match {
            if let Some(ref prev) = resolved {
                if prev != &hit && !within(prev, &hit) && !within(&hit, prev) {
                    return None;
                }
                // Keep the more specific one when one is a parent branch of the other
                if hit.len() > prev.len() {
                    resolved = Some(hit);
                }
            } else {
                resolved = Some(hit);
            }
        }
    }
    resolved
}

pub fn read_local_host_category_yaml() -> Option<String> {
    use std::{env, fs, path::Path};
    let config_path = env::var_os("MEDIARY_PLUGIN_CONFIG_PATH")?;
    let plugin_dir = Path::new(&config_path).parent()?;
    let plugins_dir = plugin_dir.parent()?;
    if plugins_dir.file_name().and_then(|n| n.to_str()) != Some("plugins") {
        return None;
    }
    let root = plugins_dir.parent()?;
    fs::read_to_string(root.join("category.yaml")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_zone_and_locates_leaf_and_branch_categories() {
        let yaml = r#"
movie:
  电影/技术专区/蓝光原盘:
    genre_ids: "-16"
    file_extension: ".iso,iso,.ISO,ISO"
  电影/华语电影/2020-2030:
    year: "2020-2030"
  电影/华语电影/2010-2019:
    year: "2010-2019"
tv:
  动漫/儿童:
    genre_ids: "16"
"#;
        let cats = parse_category_yaml(yaml).unwrap();
        let iso_cat = cats
            .iter()
            .find(|c| c.path == "电影/技术专区/蓝光原盘")
            .unwrap();
        assert!(iso_cat.iso_zone);

        // Exact leaf match
        assert_eq!(
            locate(
                &cats,
                "movie",
                &["/strm/cloud/媒体库/电影/华语电影/2020-2030/热辣滚烫 (2024)/热辣滚烫.strm"]
            )
            .as_deref(),
            Some("电影/华语电影/2020-2030")
        );

        // Branch fallback when item is directly under 电影/华语电影 without year subfolder
        assert_eq!(
            locate(
                &cats,
                "movie",
                &["/strm/cloud/媒体库/电影/华语电影/让子弹飞 (2010)/让子弹飞.strm"]
            )
            .as_deref(),
            Some("电影/华语电影")
        );

        // Conflicting zones across strm_path and file_path -> None (must review)
        assert_eq!(
            locate(
                &cats,
                "movie",
                &[
                    "/strm/cloud/媒体库/电影/华语电影/2020-2030/A.strm",
                    "/media/link/电影/技术专区/蓝光原盘/A.iso"
                ]
            ),
            None
        );
    }
}

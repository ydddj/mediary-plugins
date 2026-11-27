use crate::db;
use crate::models::{
    is_valid_sha256, ArchiveInspection, CatalogArtifact, CatalogDocument, CatalogPluginEntry,
    EngineSettings, GithubRepoBridge, ParsedPluginManifest, MAX_UNPACKED_ARCHIVE_BYTES,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::Engine as _;
use flate2::read::GzDecoder;
use regex::Regex;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::Path;
use std::time::Duration;
use url::Url;

const MAX_COMPRESSED_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CHECKSUM_TEXT_BYTES: usize = 512 * 1024;

pub fn github_api_base() -> String {
    env::var("MEDIPLUG_GITHUB_API_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .unwrap_or_else(|| "https://api.github.com".to_string())
}

pub fn github_raw_base() -> String {
    env::var("MEDIPLUG_GITHUB_RAW_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .unwrap_or_else(|| "https://raw.githubusercontent.com".to_string())
}

/// Parses `"owner/repo"`, `"https://github.com/owner/repo/..."`, or `"git@github.com:owner/repo.git"`
/// into `(owner, repo)`.
pub fn parse_github_repo_spec(raw: &str) -> Result<(String, String)> {
    let trimmed = raw.trim();
    ensure!(!trimmed.is_empty(), "请输入 GitHub 仓库地址（如 https://github.com/owner/repo 或 owner/repo）");

    let path_part = if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest.to_string()
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let u = Url::parse(trimmed).context("GitHub 仓库 URL 格式不合法")?;
        u.path().trim_start_matches('/').to_string()
    } else {
        trimmed.trim_start_matches('/').to_string()
    };

    let mut segs = path_part
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let owner = segs.next().unwrap_or_default();
    let repo_raw = segs.next().unwrap_or_default();
    let repo = repo_raw.strip_suffix(".git").unwrap_or(repo_raw);

    let valid_ident = |s: &str| -> bool {
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    ensure!(
        valid_ident(owner) && valid_ident(repo),
        "无法识别合法的 GitHub owner/repo: {trimmed}"
    );
    Ok((owner.to_string(), repo.to_string()))
}

/// Returns `(arch_key, plugin_stem)` for a `.tar.gz` / `.tgz` release asset.
/// Supports multi-plugin releases by extracting the stem before version/arch suffixes.
pub fn detect_asset_arch(filename: &str) -> Option<(&'static str, String)> {
    let name = filename.trim();
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("source code") {
        return None;
    }
    let without_ext = lower
        .strip_suffix(".tar.gz")
        .or_else(|| lower.strip_suffix(".tgz"))?;

    // Reject obvious non-Linux/macOS archives
    if without_ext.contains("windows") || without_ext.contains("win64") || without_ext.contains("win32") {
        return None;
    }

    let arch = if without_ext.contains("linux-amd64")
        || without_ext.contains("linux_amd64")
        || without_ext.contains("linux-x86_64")
        || without_ext.contains("linux_x86_64")
        || without_ext.contains("linux-x64")
        || without_ext.contains("x86_64-unknown-linux")
    {
        "linux-amd64"
    } else if without_ext.contains("linux-arm64")
        || without_ext.contains("linux_arm64")
        || without_ext.contains("linux-aarch64")
        || without_ext.contains("linux_aarch64")
        || without_ext.contains("aarch64-unknown-linux")
    {
        "linux-arm64"
    } else if without_ext.contains("darwin-arm64")
        || without_ext.contains("macos-arm64")
        || without_ext.contains("aarch64-apple-darwin")
    {
        "darwin-arm64"
    } else if without_ext.contains("darwin-amd64")
        || without_ext.contains("macos-amd64")
        || without_ext.contains("macos-x86_64")
        || without_ext.contains("x86_64-apple-darwin")
    {
        "darwin-amd64"
    } else if !without_ext.contains("darwin")
        && !without_ext.contains("macos")
        && !without_ext.contains("arm")
        && !without_ext.contains("aarch64")
    {
        // Minimalist release tarball e.g. `myplugin-1.0.0.tar.gz` -> treat as linux-amd64 candidate
        "linux-amd64"
    } else {
        return None;
    };

    let mut stem = without_ext.to_string();
    for pat in [
        "-linux-amd64",
        "_linux_amd64",
        "-linux-x86_64",
        "_linux_x86_64",
        "-linux-x64",
        "-x86_64-unknown-linux-gnu",
        "-x86_64-unknown-linux-musl",
        "-linux-arm64",
        "_linux_arm64",
        "-linux-aarch64",
        "_linux_aarch64",
        "-aarch64-unknown-linux-gnu",
        "-aarch64-unknown-linux-musl",
        "-darwin-arm64",
        "-macos-arm64",
        "-aarch64-apple-darwin",
        "-darwin-amd64",
        "-macos-amd64",
        "-x86_64-apple-darwin",
    ] {
        if let Some(idx) = stem.find(pat) {
            stem.truncate(idx);
            break;
        }
    }

    // Strip trailing `-v1.2.3` or `-1.2.3` version suffix from stem
    if let Ok(re) = Regex::new(r"[-_]v?\d+\.\d+(?:\.\d+)?(?:[-.][0-9a-z]+)?$") {
        stem = re.replace(&stem, "").to_string();
    }
    let stem = stem.trim_matches(['-', '_', '.']).to_string();
    let final_stem = if stem.is_empty() {
        "plugin".to_string()
    } else {
        stem
    };

    Some((arch, final_stem))
}

/// Parses `SHA256SUMS` / `.sha256` / Release body text into `filename -> lowercase_64_hex_sha256`,
/// plus an optional standalone 64-hex hash if the file only contains a single hash.
pub fn parse_sha256_text(text: &str) -> (HashMap<String, String>, Option<String>) {
    let mut by_file = HashMap::new();
    let mut single_hash = None;

    let Ok(line_re) = Regex::new(r"(?i)\b([0-9a-f]{64})\b(?:\s+[*]?([^\s`]+))?") else {
        return (by_file, None);
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // BSD style: SHA256 (foo.tar.gz) = 0123...
        if let Some(rest) = trimmed.strip_prefix("SHA256 (") {
            if let Some((fname, eq_hash)) = rest.split_once(") = ") {
                let h = eq_hash.trim().to_ascii_lowercase();
                if is_valid_sha256(&h) {
                    let base = fname.rsplit('/').next().unwrap_or(fname).trim();
                    by_file.insert(base.to_string(), h);
                    continue;
                }
            }
        }
        for cap in line_re.captures_iter(trimmed) {
            let hash = cap[1].to_ascii_lowercase();
            if let Some(m) = cap.get(2) {
                let raw_name = m
                    .as_str()
                    .trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '(' | ')' | ','));
                let base = raw_name.rsplit('/').next().unwrap_or(raw_name);
                if base.ends_with(".tar.gz") || base.ends_with(".tgz") {
                    by_file.insert(base.to_string(), hash.clone());
                }
            } else if single_hash.is_none() {
                single_hash = Some(hash);
            }
        }
    }

    (by_file, single_hash)
}

pub fn apply_github_mirror(url: &str, mirror_prefix: &str) -> String {
    let m = mirror_prefix.trim();
    if m.is_empty() || (!url.starts_with("https://github.com/") && !url.starts_with("https://objects.githubusercontent.com/")) {
        return url.to_string();
    }
    let clean_m = m.trim_end_matches('/');
    format!("{clean_m}/{url}")
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    bytes_read: u64,
    max_bytes: u64,
}

impl<R: Read> HashingReader<R> {
    fn new(inner: R, max_bytes: u64) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes_read: 0,
            max_bytes,
        }
    }

    fn finish(mut self) -> Result<(u64, String)> {
        let mut sink = [0u8; 16 * 1024];
        loop {
            let n = self.read(&mut sink)?;
            if n == 0 {
                break;
            }
        }
        let digest = self.hasher.finalize();
        let mut hex = String::with_capacity(64);
        for b in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{b:02x}");
        }
        Ok((self.bytes_read, hex))
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.bytes_read = self.bytes_read.saturating_add(n as u64);
            if self.bytes_read > self.max_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("压缩包体积超过安全上限 ({} MiB)", self.max_bytes / (1024 * 1024)),
                ));
            }
            self.hasher.update(&buf[..n]);
        }
        Ok(n)
    }
}

fn parse_octal(field: &[u8]) -> u64 {
    // Handle GNU base-256 extension (leading bit 0x80) as well as standard ASCII octal
    if field.first().copied() == Some(0x80) {
        let mut val = 0u64;
        for &b in &field[1..] {
            val = val.saturating_mul(256).saturating_add(b as u64);
        }
        return val;
    }
    let s = std::str::from_utf8(field).unwrap_or_default();
    let trimmed = s.trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace());
    u64::from_str_radix(trimmed, 8).unwrap_or(0)
}

fn parse_cstr(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).trim().to_string()
}

fn parse_pax_path(pax_bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(pax_bytes).ok()?;
    for line in text.lines() {
        if let Some((_, kv)) = line.split_once(' ') {
            if let Some(val) = kv.strip_prefix("path=") {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn normalize_tar_entry_path(raw: &str) -> (String, bool) {
    let mut unsafe_path = raw.starts_with('/');
    let mut parts = Vec::new();
    for seg in raw.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            unsafe_path = true;
            continue;
        }
        parts.push(seg);
    }
    (parts.join("/"), unsafe_path)
}

fn is_valid_action_ident(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.len() <= 64
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn is_safe_relative_path(raw: &str) -> bool {
    let t = raw.trim();
    if t.is_empty() || t.starts_with('/') || t.starts_with('\\') || t.contains('\\') || t.contains('\0') {
        return false;
    }
    let mut has_normal = false;
    for seg in t.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return false;
        }
        has_normal = true;
    }
    has_normal
}

fn is_loopback_http_endpoint(raw: &str) -> bool {
    let t = raw.trim();
    let Ok(u) = Url::parse(t) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    if !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    matches!(
        u.host_str(),
        Some("127.0.0.1" | "::1" | "[::1]" | "localhost")
    )
}

/// Validates Mediary v3.4.6 ~ v3.4.8+ manifest extension rules (`playback_hook`, `resource_search`, `ui`).
pub fn validate_manifest_extensions(
    m: &ParsedPluginManifest,
    symlink_entries: &[String],
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if let Some(ref hook) = m.playback_hook {
        let transport = hook.transport.trim().to_ascii_lowercase();
        match transport.as_str() {
            "unix" => {
                let sock = hook.socket.trim();
                if sock.is_empty() {
                    issues.push("playback_hook.transport=unix 时必须填写 socket 相对路径（如 data/playback-hook.sock）".to_string());
                } else if !is_safe_relative_path(sock) {
                    issues.push(format!(
                        "playback_hook.socket 必须为安全相对路径（禁止绝对路径或 .. 越界）: {sock}"
                    ));
                } else {
                    let (norm_sock, _) = normalize_tar_entry_path(sock);
                    if symlink_entries.iter().any(|s| {
                        s == &norm_sock || norm_sock.starts_with(&format!("{s}/"))
                    }) {
                        issues.push(format!(
                            "playback_hook.socket 路径不可指向符号链接: {sock}"
                        ));
                    }
                }
                if !hook.endpoint.trim().is_empty() {
                    warnings.push("playback_hook.transport=unix 时无需设置 endpoint 字段".to_string());
                }
                if m.runtime
                    .as_ref()
                    .map(|r| r.entrypoint.trim().is_empty())
                    .unwrap_or(true)
                {
                    warnings.push(
                        "playback_hook 使用 unix 传输但未声明 runtime，请确认常驻进程由宿主托管或插件自行拉起"
                            .to_string(),
                    );
                }
            }
            "http" => {
                let ep = hook.endpoint.trim();
                if ep.is_empty() {
                    issues.push(
                        "playback_hook.transport=http 时必须填写本机回环 endpoint（如 http://127.0.0.1:18120）"
                            .to_string(),
                    );
                } else if !is_loopback_http_endpoint(ep) {
                    issues.push(format!(
                        "playback_hook.endpoint 必须为不含凭据的本机回环地址（127.0.0.1 / ::1 / localhost）: {ep}"
                    ));
                }
            }
            other => {
                issues.push(format!(
                    "playback_hook.transport 必须为 \"unix\" 或 \"http\"（当前: \"{other}\"）"
                ));
            }
        }

        let path = hook.path.trim();
        if !path.is_empty() {
            if !path.starts_with('/') || path.contains("..") || path.chars().any(char::is_whitespace) {
                issues.push(format!(
                    "playback_hook.path 必须以 / 开头且不含空白或 ..（如 /resolve）: {path}"
                ));
            }
        }
        if let Some(ms) = hook.timeout_ms {
            if ms == 0 || ms > 30_000 {
                issues.push(format!(
                    "playback_hook.timeout_ms 超出合法范围（要求 1 ~ 30000 ms，当前: {ms}）"
                ));
            }
        }
    }

    if let Some(ref rs) = m.resource_search {
        let has_action_rt = m
            .action_runtime
            .as_ref()
            .map(|r| !r.entrypoint.trim().is_empty())
            .unwrap_or(false);
        if !has_action_rt {
            issues.push(
                "声明 resource_search 扩展时必须同时声明有效的 action_runtime（含 entrypoint）"
                    .to_string(),
            );
        }
        match rs.source_id {
            Some(id) if id <= -2 => {}
            Some(id) => {
                issues.push(format!(
                    "resource_search.source_id 必须 <= -2（当前: {id}，-1 及以上为宿主保留源 ID）"
                ));
            }
            None => {
                issues.push("resource_search 缺少必填整数 source_id（要求 <= -2）".to_string());
            }
        }
        if !is_valid_action_ident(&rs.action) {
            issues.push(format!(
                "resource_search.action 无效（须非空且仅含字母/数字/下划线/连字符）: \"{}\"",
                rs.action.trim()
            ));
        }
        match rs.transfer_action {
            Some(ref ta) => {
                if !is_valid_action_ident(ta) {
                    issues.push(format!(
                        "resource_search.transfer_action 无效（须非空且仅含字母/数字/下划线/连字符）: \"{}\"",
                        ta.trim()
                    ));
                }
            }
            None => {
                warnings.push(
                    "resource_search 未声明 transfer_action（仅支持资源搜索，不支持宿主一键转存）"
                        .to_string(),
                );
            }
        }
        if let Some(secs) = rs.timeout_seconds {
            if secs == 0 || secs > 300 {
                issues.push(format!(
                    "resource_search.timeout_seconds 超出合法范围（要求 1 ~ 300 秒，当前: {secs}）"
                ));
            }
        }
    }

    if let Some(ref ui) = m.ui {
        if !ui.dir.trim().is_empty() && !is_safe_relative_path(&ui.dir) {
            issues.push(format!("ui.dir 必须为安全相对路径: {}", ui.dir));
        }
        if !ui.entry.trim().is_empty() && !is_safe_relative_path(&ui.entry) {
            issues.push(format!("ui.entry 必须为安全相对路径: {}", ui.entry));
        }
    }
}

/// Streams a `.tar.gz` archive, simultaneously computing its exact compressed `size` & `sha256`
/// AND validating Mediary v3.4.6 ~ v3.4.8+'s strict archive & manifest extension contract:
/// - Root `plugin.json` present and valid
/// - Root `plugin` executable present (`mode & 0o111 != 0`)
/// - Forbidden runtime files (`config.json`, `data/`) absent
/// - Unpacked total size `<= 250 MB`
/// - `playback_hook` (`unix` relative non-symlink socket / `http` loopback endpoint, `path`, `timeout_ms`)
/// - `resource_search` (`action_runtime` required, `source_id <= -2`, `action`/`transfer_action`, `timeout_seconds`)
pub fn inspect_tar_gz_reader<R: Read>(
    reader: R,
    filename: &str,
    download_url_hint: &str,
) -> Result<ArchiveInspection> {
    let mut hashing = HashingReader::new(reader, MAX_COMPRESSED_DOWNLOAD_BYTES);
    let mut issues = Vec::new();
    let mut warnings = Vec::new();
    let mut forbidden_entries = Vec::new();
    let mut symlink_entries = Vec::new();
    let mut has_plugin_json = false;
    let mut has_plugin_bin = false;
    let mut plugin_bin_executable = false;
    let mut unpacked_size = 0u64;
    let mut manifest: Option<ParsedPluginManifest> = None;

    {
        let mut gz = GzDecoder::new(&mut hashing);
        let mut header = [0u8; 512];
        let mut pending_long_name: Option<String> = None;
        let mut pending_pax_path: Option<String> = None;

        loop {
            let mut read_hdr = 0usize;
            while read_hdr < 512 {
                let n = gz
                    .read(&mut header[read_hdr..])
                    .context("解压读取 .tar.gz 归档流失败")?;
                if n == 0 {
                    break;
                }
                read_hdr += n;
            }
            if read_hdr == 0 {
                break;
            }
            if read_hdr < 512 {
                issues.push("tar 归档头部不完整（文件可能已截断）".to_string());
                break;
            }
            if header.iter().all(|&b| b == 0) {
                break;
            }

            let name = parse_cstr(&header[0..100]);
            let mode = parse_octal(&header[100..108]);
            let size = parse_octal(&header[124..136]);
            let typeflag = header[156];
            let is_ustar = &header[257..262] == b"ustar";
            let prefix = if is_ustar {
                parse_cstr(&header[345..500])
            } else {
                String::new()
            };

            let padded_size = size.div_ceil(512) * 512;

            if typeflag == b'L' {
                let mut buf = vec![0u8; size.min(8192) as usize];
                gz.read_exact(&mut buf)?;
                let rem = padded_size.saturating_sub(buf.len() as u64);
                io::copy(&mut (&mut gz).take(rem), &mut io::sink())?;
                pending_long_name = Some(parse_cstr(&buf));
                continue;
            }
            if typeflag == b'x' || typeflag == b'g' {
                let mut buf = vec![0u8; size.min(32768) as usize];
                gz.read_exact(&mut buf)?;
                let rem = padded_size.saturating_sub(buf.len() as u64);
                io::copy(&mut (&mut gz).take(rem), &mut io::sink())?;
                if typeflag == b'x' {
                    pending_pax_path = parse_pax_path(&buf);
                }
                continue;
            }

            let raw_path = if let Some(p) = pending_pax_path.take() {
                pending_long_name = None;
                p
            } else if let Some(l) = pending_long_name.take() {
                l
            } else if !prefix.is_empty() {
                format!("{prefix}/{name}")
            } else {
                name
            };

            let (norm_path, unsafe_path) = normalize_tar_entry_path(&raw_path);
            if unsafe_path {
                issues.push(format!("归档包含非法越界路径: {raw_path}"));
            }
            if matches!(typeflag, b'2' | b'1') {
                symlink_entries.push(norm_path.clone());
                issues.push(format!("归档包含符号链接/硬链接条目: {raw_path}"));
            }

            unpacked_size = unpacked_size.saturating_add(size);
            if unpacked_size > MAX_UNPACKED_ARCHIVE_BYTES {
                bail!("解压后总大小超过宿主 250 MB 限制");
            }

            if norm_path == "config.json"
                || norm_path == "data"
                || norm_path.starts_with("data/")
            {
                forbidden_entries.push(norm_path.clone());
            }

            let is_regular = typeflag == 0 || typeflag == b'0';
            if is_regular && norm_path == "plugin.json" {
                has_plugin_json = true;
                let read_len = size.min(1024 * 1024) as usize;
                let mut json_buf = vec![0u8; read_len];
                gz.read_exact(&mut json_buf)?;
                let rem = padded_size.saturating_sub(read_len as u64);
                io::copy(&mut (&mut gz).take(rem), &mut io::sink())?;
                match serde_json::from_slice::<ParsedPluginManifest>(&json_buf) {
                    Ok(mut m) => {
                        if m.api_version == 0 {
                            m.api_version = 1;
                        }
                        manifest = Some(m);
                    }
                    Err(e) => {
                        issues.push(format!("解析包内 plugin.json 失败: {e}"));
                    }
                }
                continue;
            }

            if is_regular && norm_path == "plugin" {
                has_plugin_bin = true;
                plugin_bin_executable = (mode & 0o111) != 0;
            }

            if padded_size > 0 {
                io::copy(&mut (&mut gz).take(padded_size), &mut io::sink())?;
            }
        }
    }

    let (compressed_size, sha256) = hashing.finish()?;
    let arch = detect_asset_arch(filename)
        .map(|(a, _)| a.to_string())
        .unwrap_or_else(|| "linux-amd64".to_string());

    if !has_plugin_json {
        issues.push("压缩包根目录缺少 plugin.json（注意：不可多套一层外层子目录）".to_string());
    }
    if !has_plugin_bin {
        issues.push("压缩包根目录缺少可执行文件 plugin".to_string());
    } else if !plugin_bin_executable {
        issues.push("压缩包内 plugin 缺少可执行权限（要求 chmod 0755）".to_string());
    }
    if !forbidden_entries.is_empty() {
        issues.push(format!(
            "严禁包含运行态文件/目录: {}（否则宿主安装器会拒绝安装）",
            forbidden_entries.join(", ")
        ));
    }
    if let Some(ref m) = manifest {
        if m.id.trim().is_empty() {
            issues.push("plugin.json 缺少 id 字段".to_string());
        }
        if m.version.trim().is_empty() {
            issues.push("plugin.json 缺少 version 字段".to_string());
        }
        validate_manifest_extensions(m, &symlink_entries, &mut issues, &mut warnings);
    }

    let valid_for_mediary_348 = issues.is_empty() && manifest.is_some();
    let valid_for_mediary_346 = valid_for_mediary_348;
    let capabilities = manifest
        .as_ref()
        .map(ParsedPluginManifest::capabilities)
        .unwrap_or_default();
    let compat_note = manifest
        .as_ref()
        .map(ParsedPluginManifest::compat_note)
        .unwrap_or_else(|| "兼容 Mediary v3.4.6 ~ v3.4.8+".to_string());

    let catalog_entry = manifest.as_ref().map(|m| {
        let mut artifacts = BTreeMap::new();
        let art_url = if !download_url_hint.trim().is_empty() {
            download_url_hint.trim().to_string()
        } else {
            format!(
                "https://github.com/yourname/{}/releases/download/v{}/{}",
                m.id, m.version, filename
            )
        };
        artifacts.insert(
            arch.clone(),
            CatalogArtifact {
                url: art_url,
                sha256: sha256.clone(),
                size: compressed_size,
            },
        );
        manifest_to_catalog_entry(m, "", "", artifacts)
    });

    Ok(ArchiveInspection {
        filename: filename.to_string(),
        arch,
        compressed_size,
        unpacked_size,
        sha256,
        has_plugin_json,
        has_plugin_bin,
        plugin_bin_executable,
        forbidden_entries,
        valid_for_mediary_346,
        valid_for_mediary_348,
        compat_note,
        capabilities,
        issues,
        warnings,
        manifest,
        catalog_entry,
    })
}

pub fn inspect_tar_gz_file(path: &Path, download_url_hint: &str) -> Result<ArchiveInspection> {
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin-linux-amd64.tar.gz");
    let f = File::open(path).with_context(|| format!("无法打开归档文件: {}", path.display()))?;
    inspect_tar_gz_reader(f, fname, download_url_hint)
}

pub fn inspect_tar_gz_base64(
    filename: &str,
    raw_b64: &str,
    download_url_hint: &str,
) -> Result<ArchiveInspection> {
    let payload = raw_b64
        .trim()
        .split_once(";base64,")
        .map(|(_, b)| b)
        .unwrap_or_else(|| raw_b64.trim());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .context("解码上传压缩包 Base64 失败")?;
    inspect_tar_gz_reader(Cursor::new(bytes), filename, download_url_hint)
}

pub fn manifest_to_catalog_entry(
    m: &ParsedPluginManifest,
    fallback_owner: &str,
    fallback_repo: &str,
    artifacts: BTreeMap<String, CatalogArtifact>,
) -> CatalogPluginEntry {
    let repo_url = if !fallback_owner.is_empty() && !fallback_repo.is_empty() {
        format!("https://github.com/{fallback_owner}/{fallback_repo}")
    } else {
        String::new()
    };
    CatalogPluginEntry {
        id: m.id.trim().to_string(),
        name: if !m.name.trim().is_empty() {
            m.name.trim().to_string()
        } else {
            m.id.trim().to_string()
        },
        version: m
            .version
            .trim()
            .trim_start_matches(['v', 'V'])
            .to_string(),
        description: m.description.trim().to_string(),
        author: if !m.author.trim().is_empty() {
            m.author.trim().to_string()
        } else {
            fallback_owner.to_string()
        },
        homepage: if !m.homepage.trim().is_empty() {
            m.homepage.trim().to_string()
        } else {
            repo_url.clone()
        },
        source: if !m.source.trim().is_empty() {
            m.source.trim().to_string()
        } else {
            repo_url
        },
        license: if !m.license.trim().is_empty() {
            m.license.trim().to_string()
        } else {
            "MIT".to_string()
        },
        api_version: m.api_version.max(1),
        min_mediary_version: if !m.min_mediary_version.trim().is_empty() {
            m.min_mediary_version.trim().to_string()
        } else {
            "3.4.6".to_string()
        },
        permissions: m
            .requested_scopes
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        artifacts,
    }
}

pub struct GithubClient {
    agent: ureq::Agent,
    github_token: String,
    api_base: String,
    raw_base: String,
}

impl GithubClient {
    pub fn new(engine: &EngineSettings, fallback_host_proxy: &str) -> Self {
        let proxy_str = if !engine.proxy_url.trim().is_empty() {
            engine.proxy_url.trim()
        } else {
            fallback_host_proxy.trim()
        };

        let mut builder = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .user_agent(concat!("Mediary-MediPlug/", env!("CARGO_PKG_VERSION")));

        // Do NOT route local 127.0.0.1 mock URLs through external proxy
        let api_base = github_api_base();
        let is_local_mock = api_base.contains("127.0.0.1") || api_base.contains("localhost");
        if !proxy_str.is_empty() && !is_local_mock {
            if let Ok(p) = ureq::Proxy::new(proxy_str) {
                builder = builder.proxy(p);
            }
        }

        Self {
            agent: builder.build(),
            github_token: engine.github_token.trim().to_string(),
            api_base,
            raw_base: github_raw_base(),
        }
    }

    fn get_request(&self, url: &str, accept_api: bool) -> ureq::Request {
        let mut req = self.agent.get(url);
        if accept_api {
            req = req
                .set("Accept", "application/vnd.github+json")
                .set("X-GitHub-Api-Version", "2022-11-28");
        }
        if !self.github_token.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", self.github_token));
        }
        req
    }

    fn fetch_json(&self, url: &str, accept_api: bool) -> Result<Value> {
        match self.get_request(url, accept_api).call() {
            Ok(resp) => {
                let raw = resp.into_string().unwrap_or_default();
                Ok(serde_json::from_str(&raw)?)
            }
            Err(ureq::Error::Status(code, resp)) => {
                let raw = resp.into_string().unwrap_or_default();
                let snippet = raw.chars().take(200).collect::<String>();
                if code == 403 && snippet.to_ascii_lowercase().contains("rate limit") {
                    bail!("GitHub API 触发匿名访问频率限制（HTTP 403），请在 MediPlug「全局设置」填写 GitHub Token 或代理");
                }
                bail!("GitHub HTTP {code} ({url}): {snippet}");
            }
            Err(e) => bail!("请求 GitHub 失败 ({url}): {e}"),
        }
    }

    fn fetch_small_text(&self, url: &str) -> Result<String> {
        let resp = self
            .get_request(url, false)
            .call()
            .map_err(|e| anyhow!("拉取文本失败 ({url}): {e}"))?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(MAX_CHECKSUM_TEXT_BYTES as u64)
            .read_to_end(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    fn stream_inspect_asset(&self, url: &str, filename: &str) -> Result<ArchiveInspection> {
        let resp = self
            .get_request(url, false)
            .call()
            .map_err(|e| anyhow!("下载校验 Release 资产失败 ({filename}): {e}"))?;
        inspect_tar_gz_reader(resp.into_reader(), filename, url)
    }

    /// Publishes or updates a `CatalogDocument` on GitHub Gist (`public: true`) so hosts that
    /// enforce public HTTPS source URLs (`插件源地址必须使用不含凭据的公网 HTTPS 地址`) can
    /// fetch the converted catalog natively without requiring a local HTTPS domain or tunnel.
    pub fn publish_gist_catalog(
        &self,
        conn: &Connection,
        kv_gist_key: &str,
        filename: &str,
        description: &str,
        doc: &CatalogDocument,
    ) -> Result<String> {
        ensure!(
            !self.github_token.is_empty(),
            "宿主仅允许公网 HTTPS 插件源地址：请在 MediPlug「全局设置」填写勾选了 gist 权限的 GitHub Token（或配置外部 HTTPS 反代地址）"
        );
        let content = serde_json::to_string_pretty(doc)?;
        let payload = serde_json::json!({
            "description": description,
            "public": true,
            "files": {
                filename: {
                    "content": content
                }
            }
        });

        let existing_id = db::get_kv(conn, kv_gist_key).unwrap_or_default();
        let mut gist_resp: Option<Value> = None;

        if !existing_id.trim().is_empty() {
            let patch_url = format!("{}/gists/{}", self.api_base, existing_id.trim());
            let req = self
                .agent
                .request("PATCH", &patch_url)
                .set("Accept", "application/vnd.github+json")
                .set("X-GitHub-Api-Version", "2022-11-28")
                .set("Authorization", &format!("Bearer {}", self.github_token));
            if let Ok(r) = req.send_json(payload.clone()) {
                if let Ok(raw) = r.into_string() {
                    gist_resp = serde_json::from_str(&raw).ok();
                }
            }
        }

        let v = if let Some(ok_v) = gist_resp {
            ok_v
        } else {
            let post_url = format!("{}/gists", self.api_base);
            let req = self
                .agent
                .post(&post_url)
                .set("Accept", "application/vnd.github+json")
                .set("X-GitHub-Api-Version", "2022-11-28")
                .set("Authorization", &format!("Bearer {}", self.github_token));
            match req.send_json(payload) {
                Ok(r) => {
                    let raw = r.into_string().unwrap_or_default();
                    serde_json::from_str(&raw).context("解析 GitHub Gist 响应失败")?
                }
                Err(ureq::Error::Status(code, r)) => {
                    let raw = r.into_string().unwrap_or_default();
                    bail!("发布公网 HTTPS Gist 索引失败 (HTTP {code}，请确认 GitHub Token 勾选了 gist 权限): {}", raw.chars().take(180).collect::<String>());
                }
                Err(e) => bail!("连接 GitHub Gist API 失败: {e}"),
            }
        };

        if let Some(gid) = v["id"].as_str().filter(|s| !s.is_empty()) {
            let _ = db::set_kv(conn, kv_gist_key, gid);
        }
        let raw_url = v["files"][filename]["raw_url"]
            .as_str()
            .filter(|s| s.starts_with("https://"))
            .context("GitHub Gist 未返回合法的 HTTPS raw_url")?;
        Ok(raw_url.to_string())
    }

    /// Synchronizes a GitHub repository into exact Mediary v3.4.6 `CatalogPluginEntry` items.
    /// Supports:
    /// 1. Standard single-plugin GitHub Releases (`*-linux-amd64.tar.gz` + optional `linux-arm64` + `SHA256SUMS` or auto-computed SHA256)
    /// 2. Multi-plugin monorepo GitHub Releases (multiple plugin `.tar.gz` packages in one Release)
    /// 3. Fallback if a repo already hosts `catalog.json` as a Release asset or in its default branch
    pub fn sync_repo(
        &self,
        conn: &Connection,
        bridge: &GithubRepoBridge,
        engine: &EngineSettings,
    ) -> Result<(String, String, Vec<CatalogPluginEntry>)> {
        let (owner, repo) = parse_github_repo_spec(&bridge.repo)?;
        let repo_meta = self
            .fetch_json(&format!("{}/repos/{owner}/{repo}", self.api_base), true)
            .unwrap_or_else(|_| Value::Null);
        let default_branch = repo_meta["default_branch"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("main");
        let repo_desc = repo_meta["description"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        let repo_license = repo_meta["license"]["spdx_id"]
            .as_str()
            .filter(|s| !s.is_empty() && *s != "NOASSERTION")
            .unwrap_or("MIT")
            .to_string();

        // Locate target Release
        let release_val = if !bridge.pinned_tag.trim().is_empty() {
            let t = bridge.pinned_tag.trim();
            self.fetch_json(
                &format!("{}/repos/{owner}/{repo}/releases/tags/{t}", self.api_base),
                true,
            )
            .ok()
        } else if bridge.include_prerelease {
            self.fetch_json(
                &format!("{}/repos/{owner}/{repo}/releases?per_page=10", self.api_base),
                true,
            )
            .ok()
            .and_then(|arr| {
                arr.as_array()?
                    .iter()
                    .find(|r| r["draft"].as_bool() != Some(true))
                    .cloned()
            })
        } else {
            self.fetch_json(
                &format!("{}/repos/{owner}/{repo}/releases/latest", self.api_base),
                true,
            )
            .ok()
            .or_else(|| {
                self.fetch_json(
                    &format!("{}/repos/{owner}/{repo}/releases?per_page=10", self.api_base),
                    true,
                )
                .ok()
                .and_then(|arr| {
                    arr.as_array()?
                        .iter()
                        .find(|r| r["draft"].as_bool() != Some(true))
                        .cloned()
                })
            })
        };

        // Fallback if repository has no Releases at all, check if it has a root `catalog.json`
        let Some(rel) = release_val else {
            let raw_cat_url = format!(
                "{}/{owner}/{repo}/{default_branch}/catalog.json",
                self.raw_base
            );
            if let Ok(cat_val) = self.fetch_json(&raw_cat_url, false) {
                let mut doc: CatalogDocument =
                    serde_json::from_value(cat_val).context("仓库 catalog.json 格式无效")?;
                ensure!(!doc.plugins.is_empty(), "仓库 catalog.json 中未包含任何插件条目");
                for p in &mut doc.plugins {
                    for art in p.artifacts.values_mut() {
                        art.url = apply_github_mirror(&art.url, &engine.github_mirror);
                    }
                }
                return Ok((
                    default_branch.to_string(),
                    format!("https://github.com/{owner}/{repo}"),
                    doc.plugins,
                ));
            }
            bail!("仓库 {owner}/{repo} 暂无已发布的 GitHub Release，且分支根目录未找到 catalog.json");
        };

        let tag_name = rel["tag_name"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        let html_url = rel["html_url"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://github.com/{owner}/{repo}/releases/tag/{tag_name}"));
        let release_body = rel["body"].as_str().unwrap_or_default();

        let assets = rel["assets"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        // Check if release explicitly publishes a `catalog.json` asset
        if let Some(cat_asset) = assets
            .iter()
            .find(|a| a["name"].as_str() == Some("catalog.json"))
        {
            if let Some(dl_url) = cat_asset["browser_download_url"].as_str() {
                if let Ok(cat_val) = self.fetch_json(dl_url, false) {
                    if let Ok(mut doc) = serde_json::from_value::<CatalogDocument>(cat_val) {
                        if !doc.plugins.is_empty() {
                            for p in &mut doc.plugins {
                                for art in p.artifacts.values_mut() {
                                    art.url = apply_github_mirror(&art.url, &engine.github_mirror);
                                }
                            }
                            return Ok((tag_name, html_url, doc.plugins));
                        }
                    }
                }
            }
        }

        // Gather SHA-256 checksums from Release body and any `SHA256SUMS` / `*.sha256` assets
        let (mut sha_by_file, body_single_sha) = parse_sha256_text(release_body);
        for asset in &assets {
            let aname = asset["name"].as_str().unwrap_or_default();
            let alower = aname.to_ascii_lowercase();
            let is_sum_file = alower == "sha256sums"
                || alower == "sha256sums.txt"
                || alower == "checksums.txt"
                || alower.ends_with(".sha256")
                || alower.ends_with(".sha256sum");
            if !is_sum_file {
                continue;
            }
            let Some(dl_url) = asset["browser_download_url"].as_str() else {
                continue;
            };
            if let Ok(txt) = self.fetch_small_text(dl_url) {
                let (parsed_map, single) = parse_sha256_text(&txt);
                sha_by_file.extend(parsed_map);
                if let Some(h) = single {
                    if let Some(target_tar) = aname
                        .strip_suffix(".sha256")
                        .or_else(|| aname.strip_suffix(".sha256sum"))
                    {
                        sha_by_file.insert(target_tar.to_string(), h);
                    }
                }
            }
        }

        // Group `.tar.gz` assets by plugin stem
        #[derive(Clone)]
        struct CandidateAsset {
            name: String,
            arch: &'static str,
            url: String,
            size: u64,
            updated_at: String,
            known_sha256: Option<String>,
        }

        let kw_filter = bridge.asset_keyword.trim().to_ascii_lowercase();
        let mut grouped: BTreeMap<String, Vec<CandidateAsset>> = BTreeMap::new();
        let mut total_tarballs = 0usize;

        for asset in &assets {
            let aname = asset["name"].as_str().unwrap_or_default().trim();
            if !kw_filter.is_empty() && !aname.to_ascii_lowercase().contains(&kw_filter) {
                continue;
            }
            let Some((arch, stem)) = detect_asset_arch(aname) else {
                continue;
            };
            let Some(dl_url) = asset["browser_download_url"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let size = asset["size"].as_u64().unwrap_or(0);
            let updated_at = asset["updated_at"]
                .as_str()
                .unwrap_or(&tag_name)
                .to_string();

            // 1. GitHub native `asset.digest` ("sha256:...")
            let mut known_sha = asset["digest"]
                .as_str()
                .and_then(|d| d.strip_prefix("sha256:"))
                .map(str::to_ascii_lowercase)
                .filter(|h| is_valid_sha256(h));

            // 2. SHA256SUMS / `.sha256` lookup
            if known_sha.is_none() {
                known_sha = sha_by_file.get(aname).cloned();
            }

            total_tarballs += 1;
            grouped.entry(stem).or_default().push(CandidateAsset {
                name: aname.to_string(),
                arch,
                url: dl_url.to_string(),
                size,
                updated_at,
                known_sha256: known_sha,
            });
        }

        ensure!(
            !grouped.is_empty(),
            "Release {tag_name} 中未找到符合规范的插件 .tar.gz 资产包"
        );

        // If there is only 1 tarball in the whole release and release body had a single standalone sha256
        if total_tarballs == 1 {
            if let Some(ref single_h) = body_single_sha {
                for list in grouped.values_mut() {
                    if let Some(first) = list.first_mut() {
                        if first.known_sha256.is_none() {
                            first.known_sha256 = Some(single_h.clone());
                        }
                    }
                }
            }
        }

        // Optional repo-root `plugin.json` fallback for single-plugin repos when `deep_inspect_archive == false`
        let raw_manifest_fallback: Option<ParsedPluginManifest> = if grouped.len() == 1 {
            let ref_tag = if !tag_name.is_empty() {
                tag_name.as_str()
            } else {
                default_branch
            };
            self.fetch_json(
                &format!("{}/{owner}/{repo}/{ref_tag}/plugin.json", self.raw_base),
                false,
            )
            .or_else(|_| {
                self.fetch_json(
                    &format!(
                        "{}/{owner}/{repo}/{default_branch}/plugin.json",
                        self.raw_base
                    ),
                    false,
                )
            })
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
        } else {
            None
        };

        let mut converted_plugins = Vec::new();

        for (stem, cand_list) in grouped {
            let mut artifacts = BTreeMap::new();
            let mut resolved_manifest: Option<ParsedPluginManifest> = None;

            for cand in cand_list {
                let cache_key = format!("{}|{}|{}", cand.url, cand.size, cand.updated_at);
                if let Some((cached_sha, cached_manifest)) = db::get_cached_asset(conn, &cache_key) {
                    if resolved_manifest.is_none() && cached_manifest.is_some() {
                        resolved_manifest = cached_manifest;
                    }
                    artifacts.insert(
                        cand.arch.to_string(),
                        CatalogArtifact {
                            url: apply_github_mirror(&cand.url, &engine.github_mirror),
                            sha256: cached_sha,
                            size: cand.size,
                        },
                    );
                    continue;
                }

                let need_stream = engine.deep_inspect_archive
                    || cand.known_sha256.is_none()
                    || cand.size == 0
                    || (resolved_manifest.is_none() && raw_manifest_fallback.is_none());

                if need_stream {
                    let inspected = self.stream_inspect_asset(&cand.url, &cand.name)?;
                    ensure!(
                        inspected.forbidden_entries.is_empty(),
                        "Release 资产 {} 包含宿主禁止的运行态文件 ({})，无法接入插件中心",
                        cand.name,
                        inspected.forbidden_entries.join(", ")
                    );
                    ensure!(
                        inspected.has_plugin_json && inspected.has_plugin_bin,
                        "Release 资产 {} 根目录缺少 plugin.json 或可执行文件 plugin",
                        cand.name
                    );
                    if let Some(ref known) = cand.known_sha256 {
                        ensure!(
                            known.eq_ignore_ascii_case(&inspected.sha256),
                            "资产 {} 的 SHA256 校验不一致（预期 {known}，实际 {}）",
                            cand.name,
                            inspected.sha256
                        );
                    }
                    db::put_cached_asset(
                        conn,
                        &cache_key,
                        &format!("{owner}/{repo}"),
                        &tag_name,
                        &cand.name,
                        &cand.url,
                        inspected.compressed_size,
                        &inspected.sha256,
                        inspected.manifest.as_ref(),
                    );
                    if resolved_manifest.is_none() {
                        resolved_manifest = inspected.manifest;
                    }
                    artifacts.insert(
                        cand.arch.to_string(),
                        CatalogArtifact {
                            url: apply_github_mirror(&cand.url, &engine.github_mirror),
                            sha256: inspected.sha256,
                            size: inspected.compressed_size,
                        },
                    );
                } else {
                    let sha = cand.known_sha256.clone().unwrap();
                    db::put_cached_asset(
                        conn,
                        &cache_key,
                        &format!("{owner}/{repo}"),
                        &tag_name,
                        &cand.name,
                        &cand.url,
                        cand.size,
                        &sha,
                        raw_manifest_fallback.as_ref(),
                    );
                    if resolved_manifest.is_none() {
                        resolved_manifest = raw_manifest_fallback.clone();
                    }
                    artifacts.insert(
                        cand.arch.to_string(),
                        CatalogArtifact {
                            url: apply_github_mirror(&cand.url, &engine.github_mirror),
                            sha256: sha,
                            size: cand.size,
                        },
                    );
                }
            }

            if artifacts.is_empty() {
                continue;
            }

            let manifest = resolved_manifest
                .or_else(|| raw_manifest_fallback.clone())
                .unwrap_or_else(|| ParsedPluginManifest {
                    id: stem.clone(),
                    name: if !bridge.name.trim().is_empty() {
                        bridge.name.trim().to_string()
                    } else {
                        repo.clone()
                    },
                    version: tag_name.trim_start_matches(['v', 'V']).to_string(),
                    description: repo_desc.clone(),
                    author: owner.clone(),
                    license: repo_license.clone(),
                    api_version: 1,
                    min_mediary_version: "3.4.6".to_string(),
                    ..Default::default()
                });

            let mut entry = manifest_to_catalog_entry(&manifest, &owner, &repo, artifacts);
            if entry.version.is_empty() {
                entry.version = tag_name.trim_start_matches(['v', 'V']).to_string();
            }
            if entry.description.is_empty() && !repo_desc.is_empty() {
                entry.description = repo_desc.clone();
            }
            if entry.license.is_empty() {
                entry.license = repo_license.clone();
            }
            converted_plugins.push(entry);
        }

        ensure!(
            !converted_plugins.is_empty(),
            "未能从 {owner}/{repo} ({tag_name}) 解析出任何有效插件"
        );
        Ok((tag_name, html_url, converted_plugins))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn write_tar_entry(out: &mut Vec<u8>, path: &str, mode: u32, content: &[u8]) {
        let mut hdr = [0u8; 512];
        let path_bytes = path.as_bytes();
        hdr[..path_bytes.len()].copy_from_slice(path_bytes);
        let mode_s = format!("{mode:07o}\0");
        hdr[100..108].copy_from_slice(mode_s.as_bytes());
        hdr[108..116].copy_from_slice(b"0000000\0");
        hdr[116..124].copy_from_slice(b"0000000\0");
        let size_s = format!("{:011o}\0", content.len());
        hdr[124..136].copy_from_slice(size_s.as_bytes());
        hdr[136..148].copy_from_slice(b"14700000000\0");
        hdr[148..156].fill(b' ');
        hdr[156] = b'0';
        hdr[257..263].copy_from_slice(b"ustar\0");
        hdr[263..265].copy_from_slice(b"00");
        let cksum: u32 = hdr.iter().map(|&b| b as u32).sum();
        let ck_s = format!("{cksum:06o}\0 ");
        hdr[148..156].copy_from_slice(ck_s.as_bytes());
        out.extend_from_slice(&hdr);
        out.extend_from_slice(content);
        let rem = (512 - (content.len() % 512)) % 512;
        out.extend(std::iter::repeat(0u8).take(rem));
    }

    fn build_tgz(entries: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (p, m, c) in entries {
            write_tar_entry(&mut tar, p, *m, c);
        }
        tar.extend_from_slice(&[0u8; 1024]);
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn test_parse_github_repo_spec() {
        assert_eq!(
            parse_github_repo_spec("https://github.com/ydddj/mediwash/releases/tag/v3.1.3").unwrap(),
            ("ydddj".to_string(), "mediwash".to_string())
        );
        assert_eq!(
            parse_github_repo_spec("git@github.com:KyleYu2024/Mediary-Plugins.git").unwrap(),
            ("KyleYu2024".to_string(), "Mediary-Plugins".to_string())
        );
        assert_eq!(
            parse_github_repo_spec("owner/my-plugin").unwrap(),
            ("owner".to_string(), "my-plugin".to_string())
        );
        assert!(parse_github_repo_spec("invalid-single-segment").is_err());
    }

    #[test]
    fn test_detect_asset_arch_and_stem() {
        assert_eq!(
            detect_asset_arch("mediwash-3.1.3-linux-amd64.tar.gz"),
            Some(("linux-amd64", "mediwash".to_string()))
        );
        assert_eq!(
            detect_asset_arch("medifind-v3.2.0-linux-aarch64.tar.gz"),
            Some(("linux-arm64", "medifind".to_string()))
        );
        assert_eq!(detect_asset_arch("SHA256SUMS"), None);
    }

    #[test]
    fn test_parse_sha256_text() {
        let raw = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  mediwash-3.1.3-linux-amd64.tar.gz\n";
        let (map, _) = parse_sha256_text(raw);
        assert_eq!(
            map.get("mediwash-3.1.3-linux-amd64.tar.gz").map(String::as_str),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }

    #[test]
    fn test_inspect_valid_and_invalid_packages() {
        let pjson = br#"{"api_version":1,"id":"demoplug","name":"Demo","version":"1.2.3","min_mediary_version":"3.4.6","requested_scopes":["plugins:read","settings:read"]}"#;
        let good = build_tgz(&[("plugin.json", 0o644, pjson), ("plugin", 0o755, b"ELF_BIN")]);
        let res = inspect_tar_gz_reader(
            Cursor::new(&good),
            "demoplug-1.2.3-linux-amd64.tar.gz",
            "https://github.com/u/demoplug/releases/download/v1.2.3/demoplug-1.2.3-linux-amd64.tar.gz",
        )
        .unwrap();
        assert!(res.valid_for_mediary_346, "{:?}", res.issues);
        assert_eq!(res.sha256.len(), 64);
        let entry = res.catalog_entry.unwrap();
        assert_eq!(entry.id, "demoplug");
        assert_eq!(entry.permissions, vec!["plugins:read", "settings:read"]);
        assert!(entry.artifacts.contains_key("linux-amd64"));

        // Archive containing forbidden `config.json` or `data/` must fail validation
        let bad = build_tgz(&[
            ("plugin.json", 0o644, pjson),
            ("plugin", 0o755, b"ELF_BIN"),
            ("config.json", 0o644, b"{}"),
            ("data/test.db", 0o644, b"123"),
        ]);
        let bad_res = inspect_tar_gz_reader(
            Cursor::new(&bad),
            "demoplug-1.2.3-linux-amd64.tar.gz",
            "",
        )
        .unwrap();
        assert!(!bad_res.valid_for_mediary_346);
        assert!(!bad_res.valid_for_mediary_348);
        assert_eq!(bad_res.forbidden_entries.len(), 2);
    }

    #[test]
    fn test_validate_v348_playback_hook_and_resource_search() {
        // 1. Valid unix playback_hook + ui
        let hook_ok = br#"{
            "api_version": 1,
            "id": "medinext",
            "name": "MediNext",
            "version": "2.1.2",
            "runtime": {"entrypoint": "./plugin", "args": ["daemon"]},
            "action_runtime": {"entrypoint": "./plugin"},
            "playback_hook": {"transport": "unix", "socket": "data/playback-hook.sock", "path": "/resolve", "timeout_ms": 1500},
            "ui": {"dir": "./ui", "entry": "index.html"},
            "requested_scopes": ["media-service:read"]
        }"#;
        let tgz_hook_ok = build_tgz(&[("plugin.json", 0o644, hook_ok), ("plugin", 0o755, b"BIN")]);
        let r1 = inspect_tar_gz_reader(Cursor::new(&tgz_hook_ok), "medinext-2.1.2-linux-amd64.tar.gz", "").unwrap();
        assert!(r1.valid_for_mediary_348, "{:?}", r1.issues);
        assert!(r1.capabilities.iter().any(|c| c.label == "playback_hook (+/prefetch)"));
        assert!(r1.capabilities.iter().any(|c| c.id == "ui"));
        assert!(r1.compat_note.contains("POST /prefetch"));

        // 2. Valid http loopback playback_hook
        let hook_http_ok = br#"{
            "api_version": 1,
            "id": "http-hook",
            "version": "1.0.0",
            "playback_hook": {"transport": "http", "endpoint": "http://127.0.0.1:18120", "path": "/resolve", "timeout_ms": 2000}
        }"#;
        let r2 = inspect_tar_gz_reader(
            Cursor::new(build_tgz(&[("plugin.json", 0o644, hook_http_ok), ("plugin", 0o755, b"BIN")])),
            "http-hook-1.0.0-linux-amd64.tar.gz",
            "",
        )
        .unwrap();
        assert!(r2.valid_for_mediary_348, "{:?}", r2.issues);

        // 3. Invalid playback_hook: non-loopback endpoint + absolute unix socket
        let hook_http_bad = br#"{
            "api_version": 1,
            "id": "bad-http-hook",
            "version": "1.0.0",
            "playback_hook": {"transport": "http", "endpoint": "http://10.0.0.5:8080", "path": "resolve", "timeout_ms": 99999}
        }"#;
        let r3 = inspect_tar_gz_reader(
            Cursor::new(build_tgz(&[("plugin.json", 0o644, hook_http_bad), ("plugin", 0o755, b"BIN")])),
            "bad-http-hook-1.0.0-linux-amd64.tar.gz",
            "",
        )
        .unwrap();
        assert!(!r3.valid_for_mediary_348);
        assert!(r3.issues.iter().any(|i| i.contains("本机回环")));
        assert!(r3.issues.iter().any(|i| i.contains("playback_hook.path")));
        assert!(r3.issues.iter().any(|i| i.contains("timeout_ms")));

        let hook_unix_bad = br#"{
            "api_version": 1,
            "id": "bad-unix-hook",
            "version": "1.0.0",
            "playback_hook": {"transport": "unix", "socket": "../escape.sock", "path": "/resolve"}
        }"#;
        let r4 = inspect_tar_gz_reader(
            Cursor::new(build_tgz(&[("plugin.json", 0o644, hook_unix_bad), ("plugin", 0o755, b"BIN")])),
            "bad-unix-hook-1.0.0-linux-amd64.tar.gz",
            "",
        )
        .unwrap();
        assert!(!r4.valid_for_mediary_348);
        assert!(r4.issues.iter().any(|i| i.contains("playback_hook.socket")));

        // 4. Valid resource_search with action_runtime and source_id <= -2
        let rs_ok = br#"{
            "api_version": 1,
            "id": "medifind",
            "name": "MediFind",
            "version": "3.2.1",
            "action_runtime": {"entrypoint": "./plugin"},
            "resource_search": {"source_id": -10, "name": "MediFind", "action": "resource_search", "transfer_action": "resource_transfer", "timeout_seconds": 60},
            "ui": {"dir": "./ui", "entry": "index.html"}
        }"#;
        let r5 = inspect_tar_gz_reader(
            Cursor::new(build_tgz(&[("plugin.json", 0o644, rs_ok), ("plugin", 0o755, b"BIN")])),
            "medifind-3.2.1-linux-amd64.tar.gz",
            "",
        )
        .unwrap();
        assert!(r5.valid_for_mediary_348, "{:?}", r5.issues);
        assert!(r5.capabilities.iter().any(|c| c.id == "resource_search"));

        // 5. Invalid resource_search: missing action_runtime, source_id = -1, bad action & timeout
        let rs_bad = br#"{
            "api_version": 1,
            "id": "bad-rs",
            "version": "1.0.0",
            "resource_search": {"source_id": -1, "action": "bad action!", "transfer_action": "", "timeout_seconds": 600}
        }"#;
        let r6 = inspect_tar_gz_reader(
            Cursor::new(build_tgz(&[("plugin.json", 0o644, rs_bad), ("plugin", 0o755, b"BIN")])),
            "bad-rs-1.0.0-linux-amd64.tar.gz",
            "",
        )
        .unwrap();
        assert!(!r6.valid_for_mediary_348);
        assert!(r6.issues.iter().any(|i| i.contains("action_runtime")));
        assert!(r6.issues.iter().any(|i| i.contains("source_id")));
        assert!(r6.issues.iter().any(|i| i.contains("resource_search.action")));
        assert!(r6.issues.iter().any(|i| i.contains("transfer_action")));
        assert!(r6.issues.iter().any(|i| i.contains("timeout_seconds")));
    }
}

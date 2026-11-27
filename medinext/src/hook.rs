use crate::engine;
use crate::host::playback_socket_path;
use crate::models::{PluginSettings, ResolveRequest};
use anyhow::{bail, ensure, Context, Result};
use serde_json::json;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};
use tokio::signal::unix::{signal, SignalKind};
use url::Url;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 512 * 1024;

pub fn run_daemon(data_dir: PathBuf, settings: PluginSettings) -> Result<()> {
    fs::create_dir_all(&data_dir).context("创建插件数据目录失败")?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化 Tokio 异步运行时失败")?;

    rt.block_on(async move {
        let mut sigterm = signal(SignalKind::terminate()).context("监听 SIGTERM 失败")?;
        let mut sigint = signal(SignalKind::interrupt()).context("监听 SIGINT 失败")?;

        let shared_dir = Arc::new(data_dir.clone());
        let shared_settings = Arc::new(settings);

        // Prefer Unix Domain Socket (`MEDIARY_PLUGIN_PLAYBACK_SOCKET` or `data/playback-hook.sock`).
        // Also support loopback HTTP if `MEDIARY_PLUGIN_PLAYBACK_ENDPOINT` is explicitly configured without socket.
        if env::var_os("MEDIARY_PLUGIN_PLAYBACK_SOCKET").is_none() {
            if let Ok(ep) = env::var("MEDIARY_PLUGIN_PLAYBACK_ENDPOINT") {
                if !ep.trim().is_empty() {
                    return serve_loopback_http(&ep, shared_dir, shared_settings, &mut sigterm, &mut sigint).await;
                }
            }
        }

        let sock_path = playback_socket_path(&data_dir);
        prepare_socket_file(&sock_path)?;
        let listener = UnixListener::bind(&sock_path)
            .with_context(|| format!("绑定 playback_hook Unix Socket 失败: {}", sock_path.display()))?;
        let _ = fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600));
        eprintln!(
            "[medinext] v{} 原生 playback_hook 已就绪: {}",
            env!("CARGO_PKG_VERSION"),
            sock_path.display()
        );

        loop {
            tokio::select! {
                _ = sigterm.recv() => {
                    eprintln!("[medinext] 收到 SIGTERM，正在优雅关闭 playback_hook...");
                    break;
                }
                _ = sigint.recv() => {
                    eprintln!("[medinext] 收到 SIGINT，正在优雅关闭 playback_hook...");
                    break;
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            let dir = Arc::clone(&shared_dir);
                            let st = Arc::clone(&shared_settings);
                            tokio::spawn(async move {
                                let _ = handle_hook_stream(stream, &dir, &st).await;
                            });
                        }
                        Err(err) => {
                            eprintln!("[medinext] Unix Socket accept 异常: {err}");
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }

        let _ = fs::remove_file(&sock_path);
        Ok(())
    })
}

async fn serve_loopback_http(
    endpoint: &str,
    shared_dir: Arc<PathBuf>,
    shared_settings: Arc<PluginSettings>,
    sigterm: &mut tokio::signal::unix::Signal,
    sigint: &mut tokio::signal::unix::Signal,
) -> Result<()> {
    let parsed = Url::parse(endpoint.trim()).context("解析 MEDIARY_PLUGIN_PLAYBACK_ENDPOINT 失败")?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    ensure!(
        matches!(host, "127.0.0.1" | "::1" | "localhost"),
        "playback_hook HTTP endpoint 仅允许本机回环地址"
    );
    let port = parsed.port().context("playback_hook HTTP endpoint 缺少端口")?;
    let bind_addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("监听 playback_hook 回环地址失败: {bind_addr}"))?;
    eprintln!(
        "[medinext] v{} 原生 playback_hook (HTTP) 已就绪: {bind_addr}",
        env!("CARGO_PKG_VERSION")
    );

    loop {
        tokio::select! {
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    let dir = Arc::clone(&shared_dir);
                    let st = Arc::clone(&shared_settings);
                    tokio::spawn(async move {
                        let _ = handle_hook_stream(stream, &dir, &st).await;
                    });
                }
            }
        }
    }
    Ok(())
}

fn prepare_socket_file(sock_path: &Path) -> Result<()> {
    if let Some(parent) = sock_path.parent() {
        fs::create_dir_all(parent).context("创建 socket 父目录失败")?;
    }
    if let Ok(meta) = fs::symlink_metadata(sock_path) {
        if meta.file_type().is_symlink() {
            bail!("拒绝移除符号链接 socket 路径: {}", sock_path.display());
        }
        let _ = fs::remove_file(sock_path);
    }
    Ok(())
}

struct ParsedHttpHead {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    header_end: usize,
}

fn parse_http_head(buf: &[u8]) -> Option<ParsedHttpHead> {
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head_str = std::str::from_utf8(&buf[..header_end - 4]).ok()?;
    let mut lines = head_str.split("\r\n");
    let req_line = lines.next()?;
    let mut parts = req_line.split_whitespace();
    let method = parts.next()?.to_ascii_uppercase();
    let target = parts.next()?.to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Some(ParsedHttpHead {
        method,
        target,
        headers,
        header_end,
    })
}

fn decode_chunked_body(raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < raw.len() {
        let rel_end = raw[pos..].windows(2).position(|w| w == b"\r\n")?;
        let len_line = std::str::from_utf8(&raw[pos..pos + rel_end]).ok()?;
        let hex_part = len_line.split(';').next()?.trim();
        let chunk_len = usize::from_str_radix(hex_part, 16).ok()?;
        pos += rel_end + 2;
        if chunk_len == 0 {
            return Some(out);
        }
        if pos + chunk_len + 2 > raw.len() {
            return None;
        }
        out.extend_from_slice(&raw[pos..pos + chunk_len]);
        pos += chunk_len + 2;
    }
    None
}

async fn write_json_response<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status_line: &str,
    body: &[u8],
) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    if !body.is_empty() {
        stream.write_all(body).await?;
    }
    stream.shutdown().await?;
    Ok(())
}

async fn handle_hook_stream<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    data_dir: &Path,
    settings: &PluginSettings,
) -> Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    let parsed = loop {
        let n = tokio::time::timeout(
            std::time::Duration::from_millis(1500),
            stream.read(&mut tmp),
        )
        .await??;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = parse_http_head(&buf) {
            break p;
        }
        if buf.len() >= MAX_HEADER_BYTES {
            let err = serde_json::to_vec(&json!({ "status": "pass", "reason": "请求头过大" }))?;
            return write_json_response(&mut stream, "200 OK", &err).await;
        }
    };

    let dummy_url = if parsed.target.starts_with("http://") || parsed.target.starts_with("https://") {
        Url::parse(&parsed.target).unwrap_or_else(|_| Url::parse("http://localhost/").unwrap())
    } else {
        Url::parse(&format!("http://localhost{}", parsed.target))
            .unwrap_or_else(|_| Url::parse("http://localhost/").unwrap())
    };
    let path = dummy_url.path();

    if parsed.method == "GET" && (path == "/health" || path == "/__medinext/health") {
        let body = serde_json::to_vec(&json!({
            "ok": true,
            "service": "medinext",
            "version": env!("CARGO_PKG_VERSION"),
        }))?;
        return write_json_response(&mut stream, "200 OK", &body).await;
    }

    if parsed.method != "POST" {
        let body = serde_json::to_vec(&json!({
            "status": "pass",
            "status_code": 200,
            "reason": "仅处理 POST 播放决策请求",
        }))?;
        return write_json_response(&mut stream, "200 OK", &body).await;
    }

    let is_chunked = parsed
        .headers
        .get("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));

    let body_bytes = if is_chunked {
        let mut raw_chunked = buf[parsed.header_end..].to_vec();
        loop {
            if let Some(decoded) = decode_chunked_body(&raw_chunked) {
                break decoded;
            }
            if raw_chunked.len() >= MAX_BODY_BYTES {
                bail!("chunked body 超过大小限制");
            }
            let n = tokio::time::timeout(
                std::time::Duration::from_millis(1200),
                stream.read(&mut tmp),
            )
            .await??;
            if n == 0 {
                break Vec::new();
            }
            raw_chunked.extend_from_slice(&tmp[..n]);
        }
    } else if let Some(cl_str) = parsed.headers.get("content-length") {
        let content_len: usize = cl_str.parse().unwrap_or(0).min(MAX_BODY_BYTES);
        let mut raw_body = buf[parsed.header_end..].to_vec();
        while raw_body.len() < content_len {
            let n = tokio::time::timeout(
                std::time::Duration::from_millis(1200),
                stream.read(&mut tmp),
            )
            .await??;
            if n == 0 {
                break;
            }
            raw_body.extend_from_slice(&tmp[..n]);
        }
        raw_body.truncate(content_len);
        raw_body
    } else {
        let mut raw_body = buf[parsed.header_end..].to_vec();
        while raw_body.len() < MAX_BODY_BYTES {
            let n = tokio::time::timeout(
                std::time::Duration::from_millis(1200),
                stream.read(&mut tmp),
            )
            .await??;
            if n == 0 {
                break;
            }
            raw_body.extend_from_slice(&tmp[..n]);
        }
        raw_body
    };

    let mut req_obj: ResolveRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(err) => {
            eprintln!(
                "[medinext] 解析 playback_hook 请求失败: {err} | body={}",
                String::from_utf8_lossy(&body_bytes[..body_bytes.len().min(256)])
            );
            let fallback = serde_json::to_vec(&json!({
                "handled": false,
                "status": "pass",
                "status_code": 200,
                "reason": format!("解析请求载荷失败: {err}"),
            }))?;
            return write_json_response(&mut stream, "200 OK", &fallback).await;
        }
    };
    if path == "/prefetch" || path == "/__medinext/prefetch" {
        req_obj.prefetch = true;
    }

    let dir_owned = data_dir.to_path_buf();
    let st_owned = settings.clone();
    let decision = match tokio::task::spawn_blocking(move || {
        engine::resolve_playback(&dir_owned, &st_owned, req_obj)
    })
    .await
    {
        Ok(Ok(d)) => serde_json::to_vec(&d)?,
        Ok(Err(err)) => serde_json::to_vec(&json!({
            "handled": false,
            "status": "pass",
            "status_code": 200,
            "reason": format!("内部调度异常回退宿主: {err}"),
        }))?,
        Err(err) => serde_json::to_vec(&json!({
            "handled": false,
            "status": "pass",
            "status_code": 200,
            "reason": format!("任务执行异常: {err}"),
        }))?,
    };

    write_json_response(&mut stream, "200 OK", &decision).await
}

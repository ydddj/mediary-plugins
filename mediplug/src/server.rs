use crate::db;
use crate::models::{now_ts, CatalogDocument, DaemonRuntimeState, DEFAULT_LISTEN_PORT};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Notify;
use url::Url;

const MAX_REQ_HEADER_BYTES: usize = 32 * 1024;

pub fn run_daemon(data_dir: PathBuf) -> Result<()> {
    fs::create_dir_all(&data_dir).context("创建插件数据目录失败")?;
    if let Ok(conn) = db::open_db(&data_dir) {
        if let Ok(cfg) = db::load_config(&conn) {
            db::write_catalog_snapshot_file(&data_dir, &cfg);
        }
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化 Tokio 运行时失败")?;

    rt.block_on(async move {
        let mut sigterm = signal(SignalKind::terminate()).context("监听 SIGTERM 失败")?;
        let mut sigint = signal(SignalKind::interrupt()).context("监听 SIGINT 失败")?;
        let shared_dir = Arc::new(data_dir.clone());
        let my_pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let shutdown_token = Arc::new(format!("{my_pid}-{}-{nanos}", now_ts()));
        let shutdown_notify = Arc::new(Notify::new());

        loop {
            let desired = read_desired_bind(&shared_dir);
            let (listener, bound_host, bound_port) = bind_with_fallback(&desired.0, desired.1).await?;

            db::write_daemon_state(
                &shared_dir,
                &DaemonRuntimeState {
                    pid: my_pid,
                    listen_host: bound_host.clone(),
                    port: bound_port,
                    started_at: now_ts(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    shutdown_token: (*shutdown_token).clone(),
                },
            );
            eprintln!(
                "[mediplug] v{} 本地 catalog.json 服务已启动: http://{bound_host}:{bound_port}/catalog.json",
                env!("CARGO_PKG_VERSION")
            );

            let mut reload_tick = tokio::time::interval(std::time::Duration::from_secs(3));
            reload_tick.tick().await;

            loop {
                tokio::select! {
                    _ = sigterm.recv() => {
                        eprintln!("[mediplug] 收到 SIGTERM，正在停止本地 catalog.json 服务...");
                        db::remove_daemon_state_if_matches(&shared_dir, my_pid, &shutdown_token);
                        return Ok(());
                    }
                    _ = sigint.recv() => {
                        eprintln!("[mediplug] 收到 SIGINT，正在停止本地 catalog.json 服务...");
                        db::remove_daemon_state_if_matches(&shared_dir, my_pid, &shutdown_token);
                        return Ok(());
                    }
                    _ = shutdown_notify.notified() => {
                        eprintln!("[mediplug] 收到版本热升级平滑退出指令，正在释放端口 :{bound_port}...");
                        db::remove_daemon_state_if_matches(&shared_dir, my_pid, &shutdown_token);
                        return Ok(());
                    }
                    _ = reload_tick.tick() => {
                        let (plugin_enabled, _, _, _) = crate::host::inspect_local_plugin_state(&shared_dir);
                        if !plugin_enabled {
                            eprintln!("[mediplug] 检测到插件已停用，正在退出本地 catalog.json 服务...");
                            db::remove_daemon_state_if_matches(&shared_dir, my_pid, &shutdown_token);
                            return Ok(());
                        }
                        let next_bind = read_desired_bind(&shared_dir);
                        if next_bind.0 != bound_host || (next_bind.1 != bound_port && next_bind.1 >= 1024) {
                            // Only rebind if user explicitly changed desired host/port in DB
                            if next_bind != desired {
                                eprintln!(
                                    "[mediplug] 检测到监听配置变更 ({bound_host}:{bound_port} -> {}:{}), 正在平滑重绑...",
                                    next_bind.0, next_bind.1
                                );
                                break;
                            }
                        }
                    }
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, _)) => {
                                let dir = Arc::clone(&shared_dir);
                                let tok = Arc::clone(&shutdown_token);
                                let notify = Arc::clone(&shutdown_notify);
                                tokio::spawn(async move {
                                    let _ = handle_http_conn(stream, &dir, bound_port, &tok, &notify).await;
                                });
                            }
                            Err(err) => {
                                eprintln!("[mediplug] TCP accept 异常: {err}");
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            }
                        }
                    }
                }
            }

            drop(listener);
        }
    })
}

fn read_desired_bind(data_dir: &Path) -> (String, u16) {
    if let Ok(conn) = db::open_db(data_dir) {
        if let Ok(cfg) = db::load_config(&conn) {
            return (cfg.engine.listen_host, cfg.engine.listen_port);
        }
    }
    ("127.0.0.1".to_string(), DEFAULT_LISTEN_PORT)
}

async fn bind_with_fallback(host: &str, preferred_port: u16) -> Result<(TcpListener, String, u16)> {
    let clean_host = if matches!(host, "0.0.0.0" | "127.0.0.1" | "::1" | "localhost") {
        host
    } else {
        "127.0.0.1"
    };
    let base = if preferred_port >= 1024 {
        preferred_port
    } else {
        DEFAULT_LISTEN_PORT
    };

    let mut last_err = None;
    for offset in 0..=10u16 {
        let port = base.saturating_add(offset);
        let addr = format!("{clean_host}:{port}");
        match TcpListener::bind(&addr).await {
            Ok(l) => return Ok((l, clean_host.to_string(), port)),
            Err(e) => last_err = Some((addr, e)),
        }
    }
    let (addr, err) = last_err.unwrap();
    Err(err).with_context(|| format!("绑定本地 catalog HTTP 端口失败: {addr}"))
}

fn parse_request_line(buf: &[u8]) -> Option<(String, String, HashMap<String, String>)> {
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = std::str::from_utf8(&buf[..header_end - 4]).ok()?;
    let mut lines = head.split("\r\n");
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
    Some((method, target, headers))
}

async fn send_json(
    stream: &mut TcpStream,
    status: &str,
    body: &[u8],
    head_only: bool,
) -> Result<()> {
    let hdr = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(hdr.as_bytes()).await?;
    if !head_only && !body.is_empty() {
        stream.write_all(body).await?;
    }
    stream.shutdown().await?;
    Ok(())
}

async fn handle_http_conn(
    mut stream: TcpStream,
    data_dir: &Path,
    active_port: u16,
    shutdown_token: &str,
    shutdown_notify: &Notify,
) -> Result<()> {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 2048];
    let (method, target, headers) = loop {
        let n = tokio::time::timeout(
            std::time::Duration::from_millis(2000),
            stream.read(&mut tmp),
        )
        .await??;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(parsed) = parse_request_line(&buf) {
            break parsed;
        }
        if buf.len() >= MAX_REQ_HEADER_BYTES {
            let err = serde_json::to_vec(&json!({"error": "request header too large"}))?;
            return send_json(&mut stream, "400 Bad Request", &err, false).await;
        }
    };

    let parsed_url = if target.starts_with("http://") || target.starts_with("https://") {
        Url::parse(&target).unwrap_or_else(|_| Url::parse("http://localhost/").unwrap())
    } else {
        Url::parse(&format!("http://localhost{target}"))
            .unwrap_or_else(|_| Url::parse("http://localhost/").unwrap())
    };
    let path = parsed_url.path();

    if method == "POST" && path == "/__mediplug/shutdown" {
        let provided = headers
            .get("x-mediplug-token")
            .map(String::as_str)
            .unwrap_or_default();
        if !shutdown_token.is_empty() && provided == shutdown_token {
            let ok_body = serde_json::to_vec(&json!({"ok": true, "stopping": true}))?;
            let _ = send_json(&mut stream, "200 OK", &ok_body, false).await;
            shutdown_notify.notify_one();
            return Ok(());
        }
        let err = serde_json::to_vec(&json!({"error": "forbidden"}))?;
        return send_json(&mut stream, "403 Forbidden", &err, false).await;
    }

    let head_only = method == "HEAD";
    if method != "GET" && method != "HEAD" {
        let err = serde_json::to_vec(&json!({"error": "method not allowed"}))?;
        return send_json(&mut stream, "405 Method Not Allowed", &err, head_only).await;
    }

    if path == "/health" || path == "/__mediplug/health" {
        let body = serde_json::to_vec(&json!({
            "ok": true,
            "service": "mediplug",
            "version": env!("CARGO_PKG_VERSION"),
            "port": active_port,
        }))?;
        return send_json(&mut stream, "200 OK", &body, head_only).await;
    }

    let dir_owned = data_dir.to_path_buf();
    let path_owned = path.to_string();
    let resolved = tokio::task::spawn_blocking(move || -> Result<Option<Vec<u8>>> {
        let conn = db::open_db(&dir_owned)?;
        let cfg = db::load_config(&conn)?;
        if path_owned == "/" || path_owned == "/catalog.json" {
            let doc: CatalogDocument = db::build_unified_catalog(&cfg);
            return Ok(Some(serde_json::to_vec_pretty(&doc)?));
        }
        if let Some(rest) = path_owned.strip_prefix("/catalog/") {
            if let Some(doc) = db::build_repo_catalog(&cfg, rest) {
                return Ok(Some(serde_json::to_vec_pretty(&doc)?));
            }
        }
        Ok(None)
    })
    .await?;

    match resolved? {
        Some(body) => send_json(&mut stream, "200 OK", &body, head_only).await,
        None => {
            let err = serde_json::to_vec(&json!({"error": "catalog not found"}))?;
            send_json(&mut stream, "404 Not Found", &err, head_only).await
        }
    }
}

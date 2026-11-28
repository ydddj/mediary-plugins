use crate::db::Db;
use crate::models::{normalize_tg_level, AppConfig, ResourceItem};
use crate::providers::{
    convert_tg_posts_to_resources, detect_image_mime, normalize_tg_channel, RawTGPost,
    TGChannelMeta,
};
use anyhow::{anyhow, ensure, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::Local;
use grammers_client::types::{Chat, PasswordToken, User};
use grammers_client::{Client, Config, InitParams, SignInError};
use grammers_mtsender::InvocationError;
use grammers_session::Session;
use grammers_tl_types as tl;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

pub const DEFAULT_TG_API_ID: i32 = 3212490;
pub const DEFAULT_TG_API_HASH: &str = "f4a8dc1f1cae2f42dcc185235d7138f5";
const SESSION_FILENAME: &str = "tg.session";

const DC_ADDRESSES: [(Ipv4Addr, u16); 6] = [
    (Ipv4Addr::new(0, 0, 0, 0), 0),
    (Ipv4Addr::new(149, 154, 175, 53), 443),
    (Ipv4Addr::new(149, 154, 167, 51), 443),
    (Ipv4Addr::new(149, 154, 175, 100), 443),
    (Ipv4Addr::new(149, 154, 167, 92), 443),
    (Ipv4Addr::new(91, 108, 56, 190), 443),
];

pub fn has_session_file(data_dir: &Path) -> bool {
    let p = data_dir.join(SESSION_FILENAME);
    p.exists() && p.metadata().map(|m| m.len() > 16).unwrap_or(false)
}

fn effective_api_credentials(cfg: &AppConfig) -> (i32, String) {
    let id = cfg
        .telegram
        .api_id
        .trim()
        .parse::<i32>()
        .ok()
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_TG_API_ID);
    let hash = if cfg.telegram.api_hash.trim().is_empty() {
        DEFAULT_TG_API_HASH.to_string()
    } else {
        cfg.telegram.api_hash.trim().to_string()
    };
    (id, hash)
}

pub fn normalize_phone(raw: &str) -> String {
    let mut digits_and_plus: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    if digits_and_plus.is_empty() {
        return String::new();
    }
    if !digits_and_plus.starts_with('+') {
        if digits_and_plus.len() == 11 && digits_and_plus.starts_with('1') {
            digits_and_plus = format!("+86{}", digits_and_plus);
        } else {
            digits_and_plus = format!("+{}", digits_and_plus);
        }
    }
    digits_and_plus
}

fn mask_phone(phone: &str) -> String {
    let chars: Vec<char> = phone.chars().collect();
    if chars.len() <= 7 {
        return phone.to_string();
    }
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[chars.len() - 4..].iter().collect();
    format!("{}****{}", prefix, suffix)
}

fn format_user_label(user: &User) -> String {
    if let Some(uname) = user.username().filter(|s| !s.trim().is_empty()) {
        format!("@{}", uname.trim())
    } else {
        let full = user.full_name();
        if !full.trim().is_empty() {
            full.trim().to_string()
        } else {
            format!("UID:{}", user.id())
        }
    }
}

fn save_session_bytes(session: &Session, session_path: &Path) -> Result<()> {
    if let Some(parent) = session_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(session_path, session.save()).context("保存 Telegram 会话文件失败")
}

fn build_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化异步运行时失败")
}

fn pick_raw_proxy(proxy_url: &str) -> String {
    let mut raw = proxy_url.trim().to_string();
    if raw.is_empty() {
        for k in [
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ] {
            if let Ok(v) = env::var(k) {
                if !v.trim().is_empty() {
                    raw = v.trim().to_string();
                    break;
                }
            }
        }
    }
    raw
}

/// Resolves any proxy (`socks5://` or `http://`) into a `socks5://` URL accepted by `grammers-mtsender`.
/// When an HTTP CONNECT proxy (`http://host:port`) is configured, spawns an in-process loopback
/// SOCKS5 -> HTTP CONNECT bridge on `127.0.0.1:0` so Telegram MTProto works seamlessly.
async fn prepare_mtproto_proxy(proxy_url: &str) -> Result<Option<String>> {
    let mut raw = pick_raw_proxy(proxy_url);
    if raw.is_empty() {
        return Ok(None);
    }
    if !raw.contains("://") {
        raw = format!("http://{}", raw);
    }
    let low = raw.to_ascii_lowercase();
    if low.starts_with("socks5h://") {
        return Ok(Some(format!("socks5://{}", &raw["socks5h://".len()..])));
    }
    if low.starts_with("socks5://") {
        return Ok(Some(raw));
    }
    if low.starts_with("http://") || low.starts_with("https://") {
        let parsed = Url::parse(&raw).context("解析代理 URL 失败")?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("代理 URL 缺少主机名"))?
            .to_string();
        let port = parsed.port_or_known_default().unwrap_or(7890);
        let auth_header = if !parsed.username().is_empty() {
            let user = parsed.username();
            let pass = parsed.password().unwrap_or("");
            let token = B64.encode(format!("{}:{}", user, pass));
            Some(format!("Proxy-Authorization: Basic {}\r\n", token))
        } else {
            None
        };

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("启动本地 MTProto 代理桥失败")?;
        let local_port = listener.local_addr()?.port();

        tokio::spawn(async move {
            while let Ok((mut client_stream, _)) = listener.accept().await {
                let upstream_host = host.clone();
                let auth_hdr = auth_header.clone();
                tokio::spawn(async move {
                    let _ = handle_socks5_to_http_connect(
                        &mut client_stream,
                        &upstream_host,
                        port,
                        auth_hdr.as_deref(),
                    )
                    .await;
                });
            }
        });

        return Ok(Some(format!("socks5://127.0.0.1:{}", local_port)));
    }
    Ok(None)
}

async fn handle_socks5_to_http_connect(
    client: &mut TcpStream,
    proxy_host: &str,
    proxy_port: u16,
    auth_header: Option<&str>,
) -> Result<()> {
    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting).await?;
    ensure!(greeting[0] == 0x05, "invalid socks5 version");
    let mut methods = vec![0u8; greeting[1] as usize];
    client.read_exact(&mut methods).await?;
    client.write_all(&[0x05, 0x00]).await?;

    let mut req_head = [0u8; 4];
    client.read_exact(&mut req_head).await?;
    ensure!(req_head[0] == 0x05 && req_head[1] == 0x01, "unsupported socks5 cmd");

    let target_host = match req_head[3] {
        0x01 => {
            let mut ip = [0u8; 4];
            client.read_exact(&mut ip).await?;
            Ipv4Addr::from(ip).to_string()
        }
        0x03 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;
            let mut dom = vec![0u8; len[0] as usize];
            client.read_exact(&mut dom).await?;
            String::from_utf8_lossy(&dom).into_owned()
        }
        0x04 => {
            let mut ip = [0u8; 16];
            client.read_exact(&mut ip).await?;
            format!("[{}]", Ipv6Addr::from(ip))
        }
        _ => return Err(anyhow!("unsupported atyp")),
    };
    let mut port_buf = [0u8; 2];
    client.read_exact(&mut port_buf).await?;
    let target_port = u16::from_be_bytes(port_buf);
    let target = format!("{}:{}", target_host, target_port);

    let mut upstream = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect((proxy_host, proxy_port)),
    )
    .await
    .context("连接 HTTP 代理超时")?
    .context("连接 HTTP 代理失败")?;

    let mut connect_req = format!(
        "CONNECT {} HTTP/1.1\r\nHost: {}\r\nProxy-Connection: Keep-Alive\r\n",
        target, target
    );
    if let Some(h) = auth_header {
        connect_req.push_str(h);
    }
    connect_req.push_str("\r\n");
    upstream.write_all(connect_req.as_bytes()).await?;

    let mut resp_buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while resp_buf.len() < 4096 {
        let n = upstream.read(&mut byte).await?;
        if n == 0 {
            break;
        }
        resp_buf.push(byte[0]);
        if resp_buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let status_line = String::from_utf8_lossy(&resp_buf);
    let first_line = status_line.lines().next().unwrap_or("");
    if !first_line.contains(" 200") {
        let _ = client
            .write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await;
        return Err(anyhow!("HTTP CONNECT rejected: {}", first_line));
    }

    client
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    let _ = tokio::io::copy_bidirectional(client, &mut upstream).await;
    Ok(())
}

async fn connect_client(
    session: Session,
    api_id: i32,
    api_hash: &str,
    dc_override: Option<i32>,
    socks5_proxy: Option<String>,
) -> Result<Client> {
    let mut params = InitParams {
        device_model: "MediFind Rust".to_string(),
        system_version: "Linux".to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        system_lang_code: "zh-hans".to_string(),
        lang_code: "zh-hans".to_string(),
        catch_up: false,
        flood_sleep_threshold: 20,
        update_queue_limit: Some(0),
        proxy_url: socks5_proxy,
        ..Default::default()
    };
    if let Some(dc) = dc_override.filter(|d| (1..=5).contains(d)) {
        params.server_addr = Some(SocketAddr::from(DC_ADDRESSES[dc as usize]));
    }

    tokio::time::timeout(
        Duration::from_secs(22),
        Client::connect(Config {
            session,
            api_id,
            api_hash: api_hash.to_string(),
            params,
        }),
    )
    .await
    .map_err(|_| anyhow!("连接 Telegram MTProto 服务器超时（如在国内网络请确认系统代理可用）"))?
    .map_err(|e| anyhow!("建立 Telegram MTProto 加密连接失败: {}", e))
}

pub fn send_login_code(
    data_dir: &Path,
    cfg: &mut AppConfig,
    raw_phone: &str,
    proxy_url: &str,
) -> Result<String> {
    let phone = normalize_phone(raw_phone);
    ensure!(
        phone.len() >= 8,
        "请输入有效的手机号（如 +8613800138000 或直接输入 11 位国内手机号）"
    );
    let (api_id, api_hash) = effective_api_credentials(cfg);
    let session_path = data_dir.join(SESSION_FILENAME);

    let rt = build_runtime()?;
    rt.block_on(async {
        let socks5 = prepare_mtproto_proxy(proxy_url).await?;
        let mut active_dc = 2i32;
        let session = Session::new();
        session.set_user(0, active_dc, false);

        let mut client = connect_client(
            session,
            api_id,
            &api_hash,
            Some(active_dc),
            socks5.clone(),
        )
        .await?;

        let make_req = || tl::functions::auth::SendCode {
            phone_number: phone.clone(),
            api_id,
            api_hash: api_hash.clone(),
            settings: tl::types::CodeSettings {
                allow_flashcall: false,
                current_number: false,
                allow_app_hash: false,
                allow_missed_call: false,
                allow_firebase: false,
                logout_tokens: None,
                token: None,
                app_sandbox: None,
                unknown_number: false,
            }
            .into(),
        };

        let sent = match client.invoke(&make_req()).await {
            Ok(res) => res,
            Err(InvocationError::Rpc(err)) if err.code == 303 => {
                active_dc = (err.value.unwrap_or(5) as i32).clamp(1, 5);
                let migrated_session = Session::new();
                migrated_session.set_user(0, active_dc, false);
                client = connect_client(
                    migrated_session,
                    api_id,
                    &api_hash,
                    Some(active_dc),
                    socks5,
                )
                .await?;
                client
                    .invoke(&make_req())
                    .await
                    .map_err(|e| anyhow!("发送 Telegram 验证码失败: {}", format_tg_rpc_err(&e)))?
            }
            Err(e) => {
                return Err(anyhow!(
                    "发送 Telegram 验证码失败: {}",
                    format_tg_rpc_err(&e)
                ))
            }
        };

        match sent {
            tl::enums::auth::SentCode::Code(code) => {
                client.session().set_user(0, active_dc, false);
                save_session_bytes(client.session(), &session_path)?;
                cfg.telegram.is_logged_in = false;
                cfg.telegram.pending_phone = phone.clone();
                cfg.telegram.pending_code_hash = code.phone_code_hash;
                cfg.telegram.pending_dc_id = active_dc;
                cfg.telegram.password_required = false;
                cfg.telegram.password_hint.clear();
                Ok(format!(
                    "验证码已发送至 {} 的 Telegram 客户端或短信，请填入验证码后点击「确认登录」",
                    mask_phone(&phone)
                ))
            }
            tl::enums::auth::SentCode::Success(tl::types::auth::SentCodeSuccess {
                authorization,
            }) => {
                if let tl::enums::auth::Authorization::Authorization(auth) = authorization {
                    let user = User::from_raw(auth.user);
                    client.session().set_user(user.id(), active_dc, user.is_bot());
                    save_session_bytes(client.session(), &session_path)?;
                    let label = format_user_label(&user);
                    cfg.telegram.is_logged_in = true;
                    cfg.telegram.tg_username = label.clone();
                    cfg.telegram.tg_phone = mask_phone(&phone);
                    cfg.telegram.pending_phone.clear();
                    cfg.telegram.pending_code_hash.clear();
                    cfg.telegram.password_required = false;
                    cfg.telegram.password_hint.clear();
                    Ok(format!("Telegram 已直接完成登录：{}", label))
                } else {
                    Err(anyhow!("该手机号尚未注册 Telegram 账号"))
                }
            }
        }
    })
}

pub fn sign_in_with_code_or_password(
    data_dir: &Path,
    cfg: &mut AppConfig,
    secret_input: &str,
    proxy_url: &str,
) -> Result<String> {
    let trimmed = secret_input.trim();
    let session_path = data_dir.join(SESSION_FILENAME);
    ensure!(
        session_path.exists(),
        "未找到登录会话，请先输入手机号点击「获取验证码」"
    );
    let session = Session::load_file(&session_path).context("加载 Telegram 登录会话失败，请重新获取验证码")?;
    let (api_id, api_hash) = effective_api_credentials(cfg);
    let active_dc = if (1..=5).contains(&cfg.telegram.pending_dc_id) {
        cfg.telegram.pending_dc_id
    } else {
        session.get_user().map(|u| u.dc).unwrap_or(2)
    };

    let rt = build_runtime()?;
    rt.block_on(async {
        let socks5 = prepare_mtproto_proxy(proxy_url).await?;
        let client = connect_client(session, api_id, &api_hash, Some(active_dc), socks5).await?;

        if cfg.telegram.password_required {
            ensure!(!trimmed.is_empty(), "请输入 Telegram 两步验证密码");
            let pw_raw = client
                .invoke(&tl::functions::account::GetPassword {})
                .await
                .map_err(|e| anyhow!("读取两步验证参数失败: {}", format_tg_rpc_err(&e)))?;
            let pw_token = PasswordToken::new(pw_raw.into());
            let user = match client.check_password(pw_token, trimmed.as_bytes()).await {
                Ok(u) => u,
                Err(SignInError::InvalidPassword) => {
                    return Err(anyhow!("两步验证密码错误，请重新输入"));
                }
                Err(e) => return Err(anyhow!("两步验证失败: {}", e)),
            };
            client.session().set_user(user.id(), active_dc, user.is_bot());
            save_session_bytes(client.session(), &session_path)?;
            let label = format_user_label(&user);
            cfg.telegram.is_logged_in = true;
            cfg.telegram.tg_username = label.clone();
            if !cfg.telegram.pending_phone.is_empty() {
                cfg.telegram.tg_phone = mask_phone(&cfg.telegram.pending_phone);
            }
            cfg.telegram.pending_phone.clear();
            cfg.telegram.pending_code_hash.clear();
            cfg.telegram.password_required = false;
            cfg.telegram.password_hint.clear();
            return Ok(format!("Telegram 两步验证登录成功：{}", label));
        }

        ensure!(!trimmed.is_empty(), "请输入收到的 Telegram 验证码");
        ensure!(
            !cfg.telegram.pending_phone.is_empty() && !cfg.telegram.pending_code_hash.is_empty(),
            "验证码会话已过期，请重新点击「获取验证码」"
        );
        let clean_code: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();

        let sign_res = client
            .invoke(&tl::functions::auth::SignIn {
                phone_number: cfg.telegram.pending_phone.clone(),
                phone_code_hash: cfg.telegram.pending_code_hash.clone(),
                phone_code: Some(clean_code),
                email_verification: None,
            })
            .await;

        match sign_res {
            Ok(tl::enums::auth::Authorization::Authorization(auth)) => {
                let user = User::from_raw(auth.user);
                client.session().set_user(user.id(), active_dc, user.is_bot());
                save_session_bytes(client.session(), &session_path)?;
                let label = format_user_label(&user);
                cfg.telegram.is_logged_in = true;
                cfg.telegram.tg_username = label.clone();
                cfg.telegram.tg_phone = mask_phone(&cfg.telegram.pending_phone);
                cfg.telegram.pending_phone.clear();
                cfg.telegram.pending_code_hash.clear();
                cfg.telegram.password_required = false;
                cfg.telegram.password_hint.clear();
                Ok(format!("Telegram 登录成功：{}", label))
            }
            Ok(tl::enums::auth::Authorization::SignUpRequired(_)) => {
                Err(anyhow!("该手机号尚未注册 Telegram，请先在官方客户端注册"))
            }
            Err(err) if err.is("SESSION_PASSWORD_NEEDED") => {
                let mut hint = String::new();
                if let Ok(tl::enums::account::Password::Password(pw)) =
                    client.invoke(&tl::functions::account::GetPassword {}).await
                {
                    hint = pw.hint.unwrap_or_default();
                }
                save_session_bytes(client.session(), &session_path)?;
                cfg.telegram.password_required = true;
                cfg.telegram.password_hint = hint.clone();
                if hint.is_empty() {
                    Ok("验证码正确！该账号开启了两步验证，请输入两步验证密码后再点「两步验证」".into())
                } else {
                    Ok(format!(
                        "验证码正确！请输入 Telegram 两步验证密码（密码提示：{}）",
                        hint
                    ))
                }
            }
            Err(err) if err.is("PHONE_CODE_*") => {
                Err(anyhow!("验证码错误或已过期，请核对后重试或重新获取验证码"))
            }
            Err(err) => Err(anyhow!("Telegram 登录失败: {}", format_tg_rpc_err(&err))),
        }
    })
}

pub fn logout_tg(data_dir: &Path, cfg: &mut AppConfig, proxy_url: &str) -> Result<String> {
    let session_path = data_dir.join(SESSION_FILENAME);
    if has_session_file(data_dir) {
        if let Ok(session) = Session::load_file(&session_path) {
            let (api_id, api_hash) = effective_api_credentials(cfg);
            if let Ok(rt) = build_runtime() {
                let _ = rt.block_on(async {
                    if let Ok(socks5) = prepare_mtproto_proxy(proxy_url).await {
                        if let Ok(client) =
                            connect_client(session, api_id, &api_hash, None, socks5).await
                        {
                            let _ = tokio::time::timeout(Duration::from_secs(5), client.sign_out())
                                .await;
                        }
                    }
                });
            }
        }
    }
    let _ = fs::remove_file(&session_path);
    cfg.telegram.is_logged_in = false;
    cfg.telegram.tg_username.clear();
    cfg.telegram.tg_phone.clear();
    cfg.telegram.pending_phone.clear();
    cfg.telegram.pending_code_hash.clear();
    cfg.telegram.pending_dc_id = 0;
    cfg.telegram.password_required = false;
    cfg.telegram.password_hint.clear();
    Ok("已退出 Telegram 账号登录".into())
}

pub fn check_tg_connection(data_dir: &Path, cfg: &mut AppConfig, proxy_url: &str) -> Result<String> {
    ensure!(
        has_session_file(data_dir),
        "尚未登录 Telegram 账号，请先输入手机号登录"
    );
    let session_path = data_dir.join(SESSION_FILENAME);
    let session = Session::load_file(&session_path).context("读取 Telegram 会话失败")?;
    let (api_id, api_hash) = effective_api_credentials(cfg);

    let rt = build_runtime()?;
    rt.block_on(async {
        let start = Instant::now();
        let socks5 = prepare_mtproto_proxy(proxy_url).await?;
        let client = connect_client(session, api_id, &api_hash, None, socks5).await?;
        let authed = client
            .is_authorized()
            .await
            .map_err(|e| anyhow!("检查 Telegram 授权状态失败: {}", format_tg_rpc_err(&e)))?;
        if !authed {
            cfg.telegram.is_logged_in = false;
            return Err(anyhow!("Telegram 登录态已失效，请重新登录"));
        }
        let me = client
            .get_me()
            .await
            .map_err(|e| anyhow!("读取 Telegram 账号信息失败: {}", format_tg_rpc_err(&e)))?;
        let label = format_user_label(&me);
        cfg.telegram.is_logged_in = true;
        cfg.telegram.tg_username = label.clone();
        let _ = save_session_bytes(client.session(), &session_path);
        Ok(format!(
            "Telegram MTProto 连接正常：{} ({}ms)",
            label,
            start.elapsed().as_millis()
        ))
    })
}

pub fn resolve_channel_mtproto(
    data_dir: &Path,
    cfg: &AppConfig,
    raw_input: &str,
    proxy_url: &str,
) -> Result<TGChannelMeta> {
    let trimmed = raw_input.trim();
    ensure!(!trimmed.is_empty(), "请输入要识别的频道名称、@用户名、频道数字 ID 或链接");
    let session_path = data_dir.join(SESSION_FILENAME);
    let session = Session::load_file(&session_path).context("加载 Telegram 会话失败")?;
    let (api_id, api_hash) = effective_api_credentials(cfg);

    let rt = build_runtime()?;
    rt.block_on(async {
        let socks5 = prepare_mtproto_proxy(proxy_url).await?;
        let client = connect_client(session, api_id, &api_hash, None, socks5).await?;
        ensure!(
            client.is_authorized().await.unwrap_or(false),
            "Telegram 登录会话已失效，请重新登录账号"
        );

        let dialogs = collect_joined_chats(&client, 600).await;
        let mut matched_chat = find_chat_in_dialogs(&dialogs, trimmed, "");

        // Invite link join support (`https://t.me/+xxxx` or `https://t.me/joinchat/xxxx`)
        if matched_chat.is_none()
            && (trimmed.contains("t.me/+") || trimmed.contains("t.me/joinchat/"))
        {
            if let Ok(updates) = client.accept_invite_link(trimmed).await {
                matched_chat = extract_first_chat_from_updates(updates);
            }
        }

        // Public @username lookup via MTProto ResolveUsername
        if matched_chat.is_none() {
            let clean_uname = normalize_tg_channel(trimmed);
            if is_valid_public_username(&clean_uname) {
                if let Ok(Some(chat)) = client.resolve_username(&clean_uname).await {
                    if !matches!(chat, Chat::User(_)) {
                        matched_chat = Some(chat);
                    }
                }
            }
        }

        let chat = matched_chat.ok_or_else(|| {
            anyhow!(
                "未在您已加入的 Telegram 会话或公开频道中找到「{}」（私密频道请先在 Telegram 客户端加入后再点识别）",
                trimmed
            )
        })?;

        let identifier = canonical_chat_identifier(&chat);
        let name = if chat.name().trim().is_empty() {
            identifier.clone()
        } else {
            chat.name().trim().to_string()
        };
        let logo = download_chat_photo_base64(&client, &chat).await;
        let _ = save_session_bytes(client.session(), &session_path);
        Ok(TGChannelMeta {
            identifier,
            name,
            logo,
        })
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JoinedDialogInfo {
    pub identifier: String,
    pub name: String,
    pub is_broadcast: bool,
    pub already_added: bool,
}

pub fn list_joined_dialogs(
    data_dir: &Path,
    cfg: &AppConfig,
    proxy_url: &str,
) -> Result<Vec<JoinedDialogInfo>> {
    ensure!(
        has_session_file(data_dir),
        "请先在上方登录您的 Telegram 账号后再读取已加入频道列表"
    );
    let session_path = data_dir.join(SESSION_FILENAME);
    let session = Session::load_file(&session_path).context("加载 Telegram 会话失败")?;
    let (api_id, api_hash) = effective_api_credentials(cfg);

    let mut existing_ids = HashSet::new();
    let mut existing_names = HashSet::new();
    for ch in &cfg.telegram.channels {
        let norm = normalize_tg_channel(&ch.identifier).to_lowercase();
        if !norm.is_empty() {
            existing_ids.insert(norm);
        }
        let nm = ch.name.trim().to_lowercase();
        if !nm.is_empty() {
            existing_names.insert(nm);
        }
    }

    let rt = build_runtime()?;
    rt.block_on(async {
        let socks5 = prepare_mtproto_proxy(proxy_url).await?;
        let client = connect_client(session, api_id, &api_hash, None, socks5).await?;
        ensure!(
            client.is_authorized().await.unwrap_or(false),
            "Telegram 登录态已失效，请重新登录账号"
        );

        let dialogs = collect_joined_chats(&client, 400).await;
        let _ = save_session_bytes(client.session(), &session_path);

        let mut out = Vec::with_capacity(dialogs.len());
        let mut seen = HashSet::new();
        for chat in dialogs {
            let identifier = canonical_chat_identifier(&chat);
            let norm = normalize_tg_channel(&identifier).to_lowercase();
            if norm.is_empty() || !seen.insert(norm.clone()) {
                continue;
            }
            let name = if chat.name().trim().is_empty() {
                identifier.clone()
            } else {
                chat.name().trim().to_string()
            };
            let is_broadcast = matches!(chat, Chat::Channel(_));
            let already_added =
                existing_ids.contains(&norm) || existing_names.contains(&name.to_lowercase());
            out.push(JoinedDialogInfo {
                identifier,
                name,
                is_broadcast,
                already_added,
            });
        }
        // Sort broadcast channels first, then already_added last for easier picking
        out.sort_by_key(|d| (d.already_added, !d.is_broadcast));
        Ok(out)
    })
}

/// Search logged-in Telegram dialogs (channels, supergroups, groups, and private chats) by display name
/// to resolve real numeric Bot API chat IDs for NextBOT verification.
pub fn find_dialog_chat_ids_by_name(
    data_dir: &Path,
    cfg: &AppConfig,
    query: &str,
    proxy_url: &str,
) -> Result<Vec<String>> {
    let q = query.trim().to_lowercase();
    if q.is_empty() || !has_session_file(data_dir) {
        return Ok(Vec::new());
    }
    let session_path = data_dir.join(SESSION_FILENAME);
    let session = Session::load_file(&session_path).context("加载 Telegram 会话失败")?;
    let (api_id, api_hash) = effective_api_credentials(cfg);

    let rt = build_runtime()?;
    rt.block_on(async {
        let socks5 = prepare_mtproto_proxy(proxy_url).await?;
        let client = connect_client(session, api_id, &api_hash, None, socks5).await?;
        if !client.is_authorized().await.unwrap_or(false) {
            return Ok(Vec::new());
        }
        let mut exact = Vec::new();
        let mut partial = Vec::new();
        let mut seen = HashSet::new();
        let mut dialogs = client.iter_dialogs();
        let mut scanned = 0usize;
        while scanned < 250 {
            match tokio::time::timeout(Duration::from_secs(10), dialogs.next()).await {
                Ok(Ok(Some(d))) => {
                    scanned += 1;
                    let chat = d.chat().clone();
                    let name_low = chat.name().trim().to_lowercase();
                    if name_low.is_empty() {
                        continue;
                    }
                    let is_exact = name_low == q;
                    let is_partial = !is_exact && name_low.contains(&q);
                    if !is_exact && !is_partial {
                        continue;
                    }
                    let real_id = match &chat {
                        Chat::Channel(ch) => format!("-100{}", ch.id()),
                        Chat::Group(g) => {
                            let id_s = g.id().to_string();
                            if id_s.starts_with('-') {
                                id_s
                            } else {
                                format!("-{}", id_s)
                            }
                        }
                        Chat::User(u) => u.id().to_string(),
                    };
                    if seen.insert(real_id.clone()) {
                        if is_exact {
                            exact.push(real_id);
                        } else {
                            partial.push(real_id);
                        }
                    }
                }
                _ => break,
            }
        }
        let _ = save_session_bytes(client.session(), &session_path);
        exact.extend(partial);
        Ok(exact)
    })
}

pub fn sync_channels_mtproto(
    data_dir: &Path,
    db: &Db,
    cfg: &mut AppConfig,
    force_manual: bool,
    proxy_url: &str,
) -> Result<(usize, HashMap<String, Vec<ResourceItem>>, Vec<String>)> {
    let session_path = data_dir.join(SESSION_FILENAME);
    let session = Session::load_file(&session_path).context("加载 Telegram 会话失败")?;
    let (api_id, api_hash) = effective_api_credentials(cfg);
    let per_limit = cfg.telegram.per_channel_limit.clamp(20, 100);
    let expand_telegraph = cfg.telegram.expand_telegraph;
    let http_agent = crate::providers::build_http_agent(proxy_url, 15);

    let rt = build_runtime()?;
    rt.block_on(async {
        let socks5 = prepare_mtproto_proxy(proxy_url).await?;
        let client = connect_client(session, api_id, &api_hash, None, socks5).await?;
        ensure!(
            client.is_authorized().await.unwrap_or(false),
            "Telegram 登录态已失效，请在「资源渠道」重新登录"
        );

        let dialogs = collect_joined_chats(&client, 600).await;
        let now_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut total_added = 0usize;
        let mut by_ch: HashMap<String, Vec<ResourceItem>> = HashMap::new();
        let mut errs = Vec::new();

        for ch in &mut cfg.telegram.channels {
            ch.level = normalize_tg_level(ch.level);
            if !ch.enabled || (!force_manual && ch.level <= 0) {
                continue;
            }
            let raw_id = if !ch.identifier.trim().is_empty() {
                ch.identifier.trim().to_string()
            } else {
                ch.name.trim().to_string()
            };
            if raw_id.is_empty() {
                continue;
            }
            ch.last_sync_at = now_str.clone();

            let mut target_chat = find_chat_in_dialogs(&dialogs, &raw_id, &ch.name);
            if target_chat.is_none() {
                let clean_u = normalize_tg_channel(&raw_id);
                if is_valid_public_username(&clean_u) {
                    if let Ok(Some(resolved)) = client.resolve_username(&clean_u).await {
                        if !matches!(resolved, Chat::User(_)) {
                            target_chat = Some(resolved);
                        }
                    }
                }
            }

            let Some(chat) = target_chat else {
                let msg = format!("未找到已加入频道「{}」", ch.name);
                ch.last_error = msg.clone();
                errs.push(msg);
                continue;
            };

            let canon_id = canonical_chat_identifier(&chat);
            if ch.identifier.trim().is_empty() {
                ch.identifier = canon_id.clone();
            }
            if !chat.name().trim().is_empty() && (ch.name.is_empty() || ch.name == ch.identifier) {
                ch.name = chat.name().trim().to_string();
            }
            if ch.logo.is_empty() {
                ch.logo = download_chat_photo_base64(&client, &chat).await;
            }

            let ch_label = if !ch.name.trim().is_empty() {
                ch.name.trim().to_string()
            } else {
                canon_id.clone()
            };
            let norm_key = normalize_tg_channel(&ch.identifier).to_lowercase();

            let mut iter = client.iter_messages(&chat);
            let mut posts = Vec::new();
            let mut max_id = ch.last_msg_id;
            let mut fetched = 0usize;
            let mut fetch_err = None;

            while fetched < per_limit {
                match tokio::time::timeout(Duration::from_secs(15), iter.next()).await {
                    Ok(Ok(Some(msg))) => {
                        fetched += 1;
                        let mid = msg.id() as i64;
                        if mid > max_id {
                            max_id = mid;
                        }
                        if ch.last_msg_id > 0 && mid <= ch.last_msg_id {
                            break;
                        }
                        if let Some(p) = message_to_raw_post(&canon_id, &msg) {
                            posts.push(p);
                        }
                    }
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => {
                        fetch_err = Some(format_tg_rpc_err(&e));
                        break;
                    }
                    Err(_) => {
                        fetch_err = Some("拉取频道消息超时".into());
                        break;
                    }
                }
            }

            if let Some(err_s) = fetch_err {
                if posts.is_empty() {
                    ch.last_error = err_s.clone();
                    errs.push(format!("{}: {}", ch_label, err_s));
                    continue;
                }
            }

            ch.last_error.clear();
            if max_id > ch.last_msg_id {
                ch.last_msg_id = max_id;
            }
            let mut items = convert_tg_posts_to_resources(&posts, expand_telegraph, Some(&http_agent));
            for it in &mut items {
                it.channel_id = norm_key.clone();
                it.channel_name = format!("TG · {}", ch_label);
            }
            let added = db.insert_tg_items_with_channel(&items, &norm_key).unwrap_or(0);
            if added > 0 {
                ch.cached_count += added;
                total_added += added;
                by_ch.insert(norm_key, items);
            }
        }

        let _ = save_session_bytes(client.session(), &session_path);
        Ok((total_added, by_ch, errs))
    })
}

pub fn search_channels_mtproto(
    data_dir: &Path,
    cfg: &AppConfig,
    keyword: &str,
    include_hidden: bool,
    proxy_url: &str,
) -> Result<Vec<(String, Vec<ResourceItem>)>> {
    let kw = keyword.trim();
    if kw.is_empty() || !has_session_file(data_dir) {
        return Ok(Vec::new());
    }
    let session_path = data_dir.join(SESSION_FILENAME);
    let session = Session::load_file(&session_path)?;
    let (api_id, api_hash) = effective_api_credentials(cfg);
    let expand_telegraph = cfg.telegram.expand_telegraph;
    let http_agent = crate::providers::build_http_agent(proxy_url, 12);

    let rt = build_runtime()?;
    rt.block_on(async {
        let socks5 = prepare_mtproto_proxy(proxy_url).await?;
        let client = connect_client(session, api_id, &api_hash, None, socks5).await?;
        if !client.is_authorized().await.unwrap_or(false) {
            return Ok(Vec::new());
        }

        let dialogs = collect_joined_chats(&client, 600).await;
        let mut results = Vec::new();
        let mut searched_cnt = 0usize;

        for ch in &cfg.telegram.channels {
            if !ch.enabled || (ch.hide_in_discover && !include_hidden) || searched_cnt >= 8 {
                continue;
            }
            let raw_id = if !ch.identifier.trim().is_empty() {
                ch.identifier.trim()
            } else {
                ch.name.trim()
            };
            if raw_id.is_empty() {
                continue;
            }
            let mut target_chat = find_chat_in_dialogs(&dialogs, raw_id, &ch.name);
            if target_chat.is_none() {
                let clean_u = normalize_tg_channel(raw_id);
                if is_valid_public_username(&clean_u) {
                    if let Ok(Some(resolved)) = client.resolve_username(&clean_u).await {
                        if !matches!(resolved, Chat::User(_)) {
                            target_chat = Some(resolved);
                        }
                    }
                }
            }
            let Some(chat) = target_chat else {
                continue;
            };
            searched_cnt += 1;

            let canon_id = canonical_chat_identifier(&chat);
            let ch_label = if !ch.name.trim().is_empty() {
                ch.name.trim().to_string()
            } else {
                chat.name().trim().to_string()
            };
            let norm_key = normalize_tg_channel(&ch.identifier).to_lowercase();

            let mut search_iter = client.search_messages(&chat).query(kw);
            let mut posts = Vec::new();
            let mut count = 0usize;
            while count < 25 {
                match tokio::time::timeout(Duration::from_secs(10), search_iter.next()).await {
                    Ok(Ok(Some(msg))) => {
                        count += 1;
                        if let Some(p) = message_to_raw_post(&canon_id, &msg) {
                            posts.push(p);
                        }
                    }
                    _ => break,
                }
            }
            if !posts.is_empty() {
                let mut items =
                    convert_tg_posts_to_resources(&posts, expand_telegraph, Some(&http_agent));
                for it in &mut items {
                    it.channel_id = norm_key.clone();
                    it.channel_name = format!("TG · {}", ch_label);
                }
                results.push((norm_key, items));
            }
        }

        let _ = save_session_bytes(client.session(), &session_path);
        Ok(results)
    })
}

async fn collect_joined_chats(client: &Client, max_dialogs: usize) -> Vec<Chat> {
    let mut out = Vec::new();
    let mut dialogs = client.iter_dialogs();
    while out.len() < max_dialogs {
        match tokio::time::timeout(Duration::from_secs(12), dialogs.next()).await {
            Ok(Ok(Some(d))) => {
                let chat = d.chat().clone();
                if !matches!(chat, Chat::User(_)) {
                    out.push(chat);
                }
            }
            _ => break,
        }
    }
    out
}

fn find_chat_in_dialogs(dialogs: &[Chat], query: &str, fallback_name: &str) -> Option<Chat> {
    let q_trim = query.trim();
    let q_norm = normalize_tg_channel(q_trim);
    let q_low = q_trim.to_lowercase();
    let q_norm_low = q_norm.to_lowercase();
    let fb_low = fallback_name.trim().to_lowercase();

    // Pass 1: Exact ID or exact @username
    for chat in dialogs {
        let cid = chat.id().to_string();
        let ch_neg100 = format!("-100{}", cid);
        let ch_neg = format!("-{}", cid);
        if q_trim == cid || q_trim == ch_neg100 || q_trim == ch_neg || q_norm == ch_neg100 {
            return Some(chat.clone());
        }
        if let Some(u) = chat.username() {
            if u.eq_ignore_ascii_case(&q_norm) || u.eq_ignore_ascii_case(q_trim.trim_start_matches('@')) {
                return Some(chat.clone());
            }
        }
        for u in chat.usernames() {
            if u.eq_ignore_ascii_case(&q_norm) {
                return Some(chat.clone());
            }
        }
    }

    // Pass 2: Exact display title match (supports Chinese channel names!)
    for chat in dialogs {
        let name_low = chat.name().trim().to_lowercase();
        if !name_low.is_empty()
            && (name_low == q_low
                || (!q_norm_low.is_empty() && name_low == q_norm_low)
                || (!fb_low.is_empty() && name_low == fb_low))
        {
            return Some(chat.clone());
        }
    }

    // Pass 3: Substring match on broadcast channels first, then megagroups
    if q_low.chars().count() >= 2 {
        for prefer_broadcast in [true, false] {
            for chat in dialogs {
                let is_bcast = matches!(chat, Chat::Channel(_));
                if is_bcast != prefer_broadcast {
                    continue;
                }
                let name_low = chat.name().trim().to_lowercase();
                if name_low.contains(&q_low) {
                    return Some(chat.clone());
                }
            }
        }
    }
    None
}

fn canonical_chat_identifier(chat: &Chat) -> String {
    if let Some(u) = chat.username().filter(|s| !s.trim().is_empty()) {
        u.trim().to_string()
    } else if matches!(chat, Chat::Channel(_)) {
        format!("-100{}", chat.id())
    } else {
        format!("-{}", chat.id())
    }
}

async fn download_chat_photo_base64(client: &Client, chat: &Chat) -> String {
    let Some(dl) = chat.photo_downloadable(false) else {
        return String::new();
    };
    let mut iter = client.iter_download(&dl);
    let mut bytes = Vec::new();
    while bytes.len() < 160 * 1024 {
        match tokio::time::timeout(Duration::from_secs(6), iter.next()).await {
            Ok(Ok(Some(chunk))) => bytes.extend_from_slice(&chunk),
            _ => break,
        }
    }
    if bytes.len() > 64 {
        if let Some(mime) = detect_image_mime(&bytes, "") {
            return format!("data:{};base64,{}", mime, B64.encode(&bytes));
        }
    }
    String::new()
}

fn message_to_raw_post(channel_id: &str, msg: &grammers_client::types::Message) -> Option<RawTGPost> {
    let text = msg.text().trim().to_string();
    let mut urls = Vec::new();

    if let Some(entities) = msg.fmt_entities() {
        for ent in entities {
            if let tl::enums::MessageEntity::TextUrl(tu) = ent {
                if !tu.url.trim().is_empty() {
                    urls.push(tu.url.trim().to_string());
                }
            }
        }
    }
    if let Some(tl::enums::ReplyMarkup::ReplyInlineMarkup(inline)) = msg.reply_markup() {
        for row in inline.rows {
            let tl::enums::KeyboardButtonRow::Row(r) = row;
            for btn in r.buttons {
                if let tl::enums::KeyboardButton::Url(u) = btn {
                    if !u.url.trim().is_empty() {
                        urls.push(u.url.trim().to_string());
                    }
                }
            }
        }
    }

    if text.is_empty() && urls.is_empty() {
        return None;
    }

    let created_at = msg
        .date()
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    Some(RawTGPost {
        channel: channel_id.to_string(),
        msg_id: msg.id() as i64,
        text,
        links_blob: urls.join("\n"),
        created_at,
    })
}

fn extract_first_chat_from_updates(updates: tl::enums::Updates) -> Option<Chat> {
    let raw_chats = match updates {
        tl::enums::Updates::Updates(u) => u.chats,
        tl::enums::Updates::Combined(u) => u.chats,
        _ => Vec::new(),
    };
    raw_chats.into_iter().next().map(Chat::from_raw)
}

pub fn is_valid_public_username(s: &str) -> bool {
    let clean = s.trim().trim_start_matches('@');
    clean.len() >= 4
        && clean.len() <= 32
        && clean.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
        && clean.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn format_tg_rpc_err(err: &InvocationError) -> String {
    match err {
        InvocationError::Rpc(rpc) => match rpc.name.as_str() {
            "PHONE_NUMBER_INVALID" => "手机号格式不正确，请带国际区号（如 +8613800138000）".into(),
            "PHONE_NUMBER_BANNED" => "该手机号已被 Telegram 官方限制登录".into(),
            "PHONE_CODE_INVALID" => "验证码不正确，请重新核对".into(),
            "PHONE_CODE_EXPIRED" => "验证码已过期，请重新点击获取验证码".into(),
            "FLOOD_WAIT" => format!(
                "触发 Telegram 频率限制，请等待 {} 秒后再试",
                rpc.value.unwrap_or(60)
            ),
            "API_ID_INVALID" => "Telegram API ID 或 API Hash 无效，请检查 APPID 设置".into(),
            _ => format!("Telegram RPC {} ({})", rpc.name, rpc.code),
        },
        other => other.to_string(),
    }
}

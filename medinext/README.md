# MediNext 302 播放负载与风控引擎 (v2.1.1)

面向 **Mediary 3.4.6+** 的纯原生 Rust 302 播放调度与观影风控插件（基于宿主 `playback_hook` 常驻 Unix Socket + `rusqlite` WAL 高并发存储，**零额外反代端口**）。

在 `medifind`（找片转存进货） → `mediwash`（监控目录归档入库 + 权重洗版） → **`medinext`（302 播放负载与观影风控）** 三段式全栈媒体流水线中，承担下游核心播放调度职责，原生平替 NextEmby：

- **纯宿主原生 `playback_hook` 架构（零额外端口）**：
  - 常驻监听插件私有 `data/playback-hook.sock` Unix Domain Socket，由 Mediary 内置媒体服务（`:8118`）在 `/Videos/*/stream` 与 `/api/302` 直链解析前毫秒级同步调用，超时/异常自动 Fail-Open 回退宿主默认 `/api/302`。
- **多路 115 播放调度与原生秒传引擎**：
  - **多号负载均衡池 (`lb_accounts`)**：沿用活跃会话权重 + 调用次数均衡 + 同用户亲和偏置 + 设备粘性算法，将媒体文件自动秒传分流至各负载号缓存目录（如 `/Next`）并签发对应客户端 `User-Agent` 直链；遇 115 `405` 风控自动冷却熔断并故障转移。
  - **用户独立 115 秒传复用**：支持用户绑定独立 115 Cookie，播放时优先命中 `sha1 + uid` 秒传池，未命中时优先调用宿主原生 `POST /api/plugin/open115/range-sha1` 完成 `sign_check` 分段二次校验并秒传至 `/EmbyCache`，无需在插件内重复维护主号明文 Cookie。
  - **宿主主号原生直连与多重保护**：管理员直连与小体积文件（默认 `≤ 200 MB`）自动透传或签发直链，内置 **1800s 直链缓存自动续期**、**设备级并发互斥锁** 与风控提示流（`fallback_video_url`）。
- **115 扫码绑号与转存配额回收站清理**：工作台内置多终端 115 扫码登录（`tv` / `android` / `web` / `ios` / `clouddrive` OpenAPI），超配额时按 FIFO 自动删除最旧转存文件并清空 115 回收站。
- **原生媒体服务用户同步、会话强踢与防爬风控**：深度对接 Mediary `media-service:read` / `media-service:write`，支持同步观影用户、实时校准活跃会话、一键强踢下线（下发 WebSocket `Stop` 并吊销会话票据 12 小时）、到期自动停用账号、UA 黑白名单防爬及 **NextEmby `config.db` 一键迁移**。

详细操作请参阅 **[使用说明 (USAGE.md)](USAGE.md)**。

## 构建、测试与打包

```sh
make test && make package
```

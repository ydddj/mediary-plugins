# MediNext 使用说明 (v2.2.0)

## 1. 安装与权限审批（Mediary 3.4.6+ / 3.4.8+）

1. 将打包产物 `dist/medinext-2.2.0-linux-amd64.tar.gz` 在 Mediary「**插件中心**」上传安装（或覆盖升级）。
2. 点击 **MediNext** 卡片上的「**配置**」：
   - 勾选授予所需权限（重点包含 `media-service:read` 与高权限 `media-service:write`）；
   - 开启「**启用插件**」与「**批准播放决策钩子 (`playback_hook_approved`)**」后点击保存（注：插件每次升级版本后，Mediary 会自动重置播放钩子批准开关以确保安全，需重新保存一次确认）。
3. **播放器直连**：无需映射任何额外端口，所有客户端（Infuse / SenPlayer / VidHub / Yamby / Forward / Filmly 等）直接连接 Mediary 原生 **`8118`** 端口即可享受多号 302 负载均衡与观影风控。

## 2. 从 NextEmby 一键迁移

打开插件卡片「**工作台** → **全局、缓存与迁移**」：
- 点击「选择 `config.db` / `.bak` 文件导入」，选择原 NextEmby 数据库文件即可一键导入：
  - 负载均衡号池（自动转换 TB 配额与缓存目录 `/Next`）与可选源盘主号
  - 全部观影用户配置、到期时间与策略模板（如 `vvvvvip`）
  - 播放时插件会自动通过宿主 `media-service:read` 将 Mediary 用户 ID 与用户名无感对齐。

## 3. 工作台六大功能页

1. **概览与会话**：实时查看播放钩子审批状态、可用负载号、24h/7d 调度与缓存命中率、宿主实时活跃播放会话（支持一键**强踢下线**，联动宿主发送 WebSocket `Stop` 并吊销票据 12 小时）及 **302 调度实测沙盒**。
2. **115 号池与扫码**：管理多号负载均衡池与可选源盘主号（不配置源盘主号时自动复用 Mediary 宿主内置 115 授权），内置 **115 多端扫码登录**（推荐 `tv`），查看 405 冷却状态并支持一键探活与清缓存/回收站。
3. **用户策略与模板**：一键从 Mediary 原生媒体服务同步观影用户，配置策略模板（并发流上限 `max_streams`、设备数上限 `max_devices`、默认路由 `负载均衡 / 独立秒传 / 源盘直连`、**「禁止回退源盘大号」严格隔离策略**）、设置到期日与代绑用户专属 115 Cookie。开启「禁止回退源盘大号」后，该用户/模板下非管理员账号强制跳过小文件直连与失败回源，即使小号池暂不可用也绝不回退源盘或宿主主号播放。
4. **播放风控与防爬**：配置 UA 黑白名单模式、查看客户端 UA 实时统计并一键拉黑/解封抓包爬虫工具，配置短时间跨多 IP 异常锁、到期自动停用账号与设备名额释放。
5. **全局、缓存与迁移**：管理直链缓存 TTL（默认 1800 秒）、小文件直连阈值（默认 200 MB）、风控拦截提示视频（`fallback_video_url`）、**剧集下一集后台同负载号自动预秒传 + 预签直链**、**详情页停留小号负载预热**、**主程序通知中心联动（播放出流通知 / 风控拦截告警 / 一键发送测试通知）**、清空直链/秒传池及配置导入导出。
6. **播放调度历史**：检索每一次 302 签发、秒传池复用、智能预热、宿主透传与风控拦截明细。

## 4. 详情页、多版本切换与片尾下一集小号预热（Mediary 3.4.8+ 原生对接）

- **Mediary 3.4.8+ 双开关配置（关键）**：
  - 宿主在 `v3.4.8+` 保留了「**STRM 工具 → 详情页直链预热**」（`emby.prefetch.enabled`）作为**总闸**；需**同时开启**宿主该总闸开关与 MediNext 工作台「**全局、缓存与迁移 → 详情页 / 多版本切换 / 片尾下一集小号预热**」开关，宿主才会向 `data/playback-hook.sock` 异步转发 `POST /prefetch`。
  - **不会消耗宿主 115 主号配额**：当存在已启用且 `playback_hook_approved = true` 的 MediNext 插件时，开启宿主总闸会**自动跳过宿主内置单主号预热**，全程交由 MediNext 小号负载池完成预秒传与预签直链（独立 `4s` 超时，失败/超时静默忽略且**不计入 `/resolve` 熔断器**）。
- **三大原生触发入口与互补机制**：
  1. **详情页停留预热**（`GET /Users/:uid/Items/:id` / `GET /Items/:id`）：宿主完成 `detail_delay_ms` 停留等待与 `30s` 去重后发往 `POST /prefetch`；插件侧零额外阻塞延迟，自动兼容 `movie` / `episode` / `series` / `season`（剧集详情页自动定位首集 `S01E01`）。
  2. **多版本切换即时预热**（`POST /Items/:id/PlaybackInfo` / `POST /Videos/:id/PlaybackInfo`）：宿主将客户端所选版本透传为 `media_source_id`，MediNext 精确匹配对应版本 `sources[]`（未传 `media_source_id` 时也支持按 `pick_code` 精确反查版本），在详情页切换版本即可即时预秒传并预签该版本直链。
  3. **片尾下一集预热 & 临期续签**（`POST /Sessions/Playing/Progress` 剩余时长 `≤ 240s`）：与 MediNext 播放首帧触发的「剧集下一集后台同负载号预秒传」形成互补；若首帧预热的下一集缓存直链到片尾时剩余有效期 `< 300s`，片尾 `/prefetch` 会主动重新签发最新 `1800s` 直链覆盖刷新。
- **跨入口 `device_id` 缓存兼容**：
  - 预热写入时会同步兼容带 `device_id` 精确键与无 `device_id` 共享键（`{username}:{pickcode}:{ua_md5}:N`），彻底解决播放器详情页带 `X-Emby-Authorization`、实际拉流 `/Videos/:id/stream` 不带 `device_id`（或反之）导致的缓存 Miss；强踢设备时自动连同该用户的无 `device_id` 共享缓存一并失效，防止越权复用。
  - **协议示例**（`POST /prefetch` 或 `POST /resolve` 附带 `"prefetch": true`）：
    ```json
    {
      "prefetch": true,
      "user_id": "10",
      "item_id": "a7e2a7442958b2e6602fa2cc7588ac2e",
      "media_source_id": "ms_v2",
      "pick_code": "",
      "user_agent": "Infuse-Direct/8.0",
      "device_id": "apple-tv-01",
      "client_ip": "10.0.0.21"
    }
    ```

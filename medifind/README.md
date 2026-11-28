# MediFind 网盘聚合引擎 (v3.2.5)

面向 **Mediary 3.4.6 / 3.4.8+** 的 Rust 插件（`rusqlite` WAL 索引存储 + 纯 `action_runtime` 零常驻进程）。承担三段式媒体流水线（`medifind` 找片进货 → `mediwash` 守门整理洗版 → `medinext` 302 播放负载）中的**上游找片与转存进货**职责，同时挂载进 Mediary 原生「资源搜索」与独立可视化工作台。

- **聚合搜片与 Mediary 3.4.8 原生搜索规格徽章**：原生对接宿主参数化 `tmdb/discover`、确定性库诊断过滤（`多版本 / 单DV / ISO / STRM`）与 UI 票据同源海报代理（浏览器直连缓存，零 Base64 中转）；并发检索 HDHive 影巢、Telegram 频道、AYClub 与私有神盾索引，支持粘贴 `115` / `ed2k://` / `magnet:?` 秒解预览与**仅转缺集（含同集外挂字幕精准跟随）**；`resource_search`（`source_id: -10`）全面输出结构化 `parsed` 属性（`web_source`、`resolution`、`resource_type`、`resource_effect`、`video_encode`、`audio_encode`、`resource_team`、`season_episode`、`subtitle`），自动与 `labels` 去重，在 Mediary 3.4.8 搜索页原生展示流媒体平台与全量规格徽章。
- **边界安全流媒体平台（`web_source`）识别**：对齐 Mediary 3.4.8 口径，支持 Netflix (`NF`)、Amazon (`AMZN`)、Disney+ (`DSNP`)、Apple TV+ (`ATVP`)、HBO Max (`HMAX`)、Hulu (`HULU`)、Paramount+ (`PMTP`)、Peacock (`PCOK`)、iTunes (`ITUN`)、Crunchyroll (`CR`)、Bilibili (`BILI`)、爱奇艺 (`IQ`/`iQIYI`)、优酷 (`YK`/`Youku`)、腾讯视频 (`TX`/`WeTV`)、芒果TV (`MGTV`/`MangoTV`)、巴哈姆特 (`BAHA`)、KKTV (`KKTV`)、myTV SUPER (`MYTV`)、Viu (`VIU`)、friDay (`FRIDAY`) 等 20 个国际与国内/亚洲流媒体平台识别，严防普通 `WEB-DL` 误标为 `WEB` 或误伤正常英文片名，并实现全称/缩写解耦匹配。
- **高精准追更与在途去重**：基于宿主 `media-status?include_episodes=true` 读取分季已入库集号，严格校验 TG 频道五档权限、电影/剧集类型隔离与同名竖屏短剧集数防串台，内置 6 小时 `in_flight` 在途冷却锁避免 `MediWash` 归档前重复追更；遇到 115 官方「审核中」自动入队重试，并通过 `tick.lock` 进程互斥锁杜绝并发撞车。
- **资源渠道与主程序通知模版联动**：Telegram MTProto 登录支持私密/中文名频道识别、一键批量导入已加入频道与五档联动（`手动 / 入新 / 追更 / 洗版 / 全量`）实时保存；影巢中转 / HDHive 支持独立开关每日自动签到；通知中心动态读取并渲染宿主 `notification_templates`（附带 TMDB `w780` 横版大图卡片、年份与真实文件规格）。
- **转存前初筛与落点分流**：在接收进 115 前执行 NextFind 同款权重打分与屏蔽过滤；自动反查并展示 **115 默认接收目录名称**（如 `待洗版`），将 `115` 分享（`share/receive`）与 `ED2K / Magnet`（`open115/offline`）统一纯落入指定 115 待整理中转目录交由下游 `mediwash` 实测归档洗版。
- **转存历史与去重锁管理**：独立「转存历史」页将「影视转存历史」与「签到/巡检任务记录」分离，采用单行摘要 + `i` 悬浮详情卡片与吸顶表头布局，支持多条件组合筛选、单删/批删/清空时同步解除 `transferred_slugs` 与 `in_flight` 去重锁及一键重转。

详见 [操作说明](USAGE.md)。

## 构建与打包

```sh
docker run --rm -v "$PWD:/src" -v medifind-cargo-cache:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/tmp/medifind-target -w /src \
  rust:1.86-bookworm sh -ec 'make test && make package'
```

包内仅含 `plugin.json`、Release 编译二进制 `plugin`、工作台静态资源 `ui/` 与说明文档。配置、SQLite WAL 数据与凭据由宿主维护。宿主契约参考 [Mediary 官方插件开发指南](https://github.com/KyleYu2024/Mediary-Plugins/blob/main/docs/PLUGIN_DEVELOPMENT.md)。

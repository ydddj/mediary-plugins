# Mediary Plugins Suite (`ydddj/mediary-plugins`)

面向 **Mediary v3.4.6+** 的四款原生 Rust 插件合集，覆盖 **上游找片转存 → 中游归档洗版 → 下游 302 多号播放负载与风控 → 第三方插件源可视化与 GitHub 自动桥接** 全链路：

| 插件 ID | 名称 | 当前版本 | 核心职责 |
| --- | --- | --- | --- |
| [`medifind`](./medifind) | **MediFind 网盘聚合引擎** | `3.2.0` | 聚合 HDHive / Telegram MTProto / AYClub / RSS 搜索、分季缺集比对与 115 分享/离线进货 |
| [`mediwash`](./mediwash) | **MediWash 归档入库与权重洗版** | `3.1.5` | 115 监控目录单步归档入库（`files/organize`）、淘汰直移回收站、存量库多版本精洗与 Symedia/MediaTidy 双引擎打分 |
| [`medinext`](./medinext) | **MediNext 302 播放负载与风控** | `2.1.1` | 原生 `playback_hook` 零额外端口接管 `:8118` 播放、115 多号负载均衡、用户秒传复用、配额回收站清理与防爬风控 |
| [`mediplug`](./mediplug) | **MediPlug 插件源与 GitHub 桥接** | `1.0.0` | 可视化管理宿主 `/api/plugin-store/sources`、自动将无 `catalog.json` 的普通 GitHub Release 仓库桥接进原生「插件中心」 |

---

## 一键接入 Mediary 原生「插件中心」

本仓库根目录提供符合 Mediary v3.4.6 规范的标准索引文件：

```text
https://raw.githubusercontent.com/ydddj/mediary-plugins/main/catalog.json
```

### 方式一：通过 MediPlug 工作台图形化添加
在 **MediPlug → 宿主插件源管理** 点击「**+ 添加标准 catalog.json 源**」，填入上方 URL 保存即可。

### 方式二：通过 Mediary API 注册插件源
```bash
curl -X POST "http://127.0.0.1:8118/api/plugin-store/sources" \
  -H "Authorization: Bearer <管理员API_TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "ydddj-mediary-plugins",
    "name": "ydddj 媒体流水线插件源",
    "url": "https://raw.githubusercontent.com/ydddj/mediary-plugins/main/catalog.json",
    "enabled": true
  }'
```

# MediPlug 插件源管理与 GitHub 桥接引擎 (v1.0.4)

面向 **Mediary v3.4.6 ~ v3.4.8+** 的原生 Rust 第三方插件源可视化管理与 GitHub Releases 自动转换桥接插件（`rusqlite` WAL 持久化存储 + 轻量回环 HTTP `catalog.json` 常驻服务 + GitHub Gist 公网 HTTPS 自动托管）。

与 `medifind`（上游找片进货） → `mediwash`（中游归档入库与权重洗版） → `medinext`（下游 302 播放负载与风控）同构开发，补齐 Mediary v3.4.6 ~ v3.4.8+ 多插件源聚合能力的最后一块可视化与自动化拼图：

- **原生 `/api/plugin-store/sources` 可视化面板**：
  - 免手敲 `curl` 命令，图形化查看、新增、编辑、一键启停与删除 Mediary 已注册的第三方 `catalog.json` 插件源，自动保护内置 `official` 官方源并实时展示命名空间冲突告警（`conflicts`），兼容 v3.4.8 第三方源数量上限与公网 HTTPS 源校验。
- **无 `catalog.json` 的普通 GitHub Release 仓库自动桥接**：
  - 用户只需填入任意 `https://github.com/owner/repo`（或 `owner/repo`，支持单插件仓库与多插件合集 Release 仓库），MediPlug 自动解析其 GitHub Releases 资产（`.tar.gz`）、提取 `digest` / `SHA256SUMS` 或流式计算 64 位小写 `sha256` 并对齐包内 `plugin.json` 的 `requested_scopes` → `permissions`；
  - 常驻本地轻量服务动态提供标准 `http://127.0.0.1:18119/catalog.json`（及单仓独立源 `/catalog/<id>.json`），并在宿主强制公网 HTTPS 源时自动同步至公开 GitHub Gist 注册，让没有编写 `catalog.json` 的第三方仓库也能直接在原生「**插件中心**」卡片安装与一键热升级。
- **原生「插件中心」聚合预览与扩展能力标识**：
  - 工作台实时预览宿主合并后的官方源 + 第三方源插件列表、版本对比、「可更新」状态、插件扩展能力徽章（`playback_hook (+/prefetch)`、`resource_search`、`ui`、`定时任务` 等）与兼容状态，支持一键调用宿主原生校验安装器（`POST /api/plugin-store/:id/install`）。
- **插件发布者工具箱（对齐 v3.4.8 `.tar.gz` 与 Manifest 扩展校验器 + `catalog.json` 生成器）**：
  - 严格对齐 Mediary v3.4.6 ~ v3.4.8+ 安装器与扩展校验规范：根目录 `plugin.json`、`plugin` `0755` 可执行权限、严禁包含 `config.json` 或 `data/`、解压后总大小 `<= 250 MB`；
  - 深度校验 `playback_hook`（`transport=unix` 安全相对非符号链接 `socket` / `transport=http` 本机回环 `endpoint`、`path` 与 v3.4.8 同通道 `POST /prefetch` 4s 预热兼容、`timeout_ms`）与 `resource_search`（须同时声明 `action_runtime`、`source_id <= -2`、`action` / `transfer_action` 有效、`timeout_seconds` 合法范围），一键流式核验离线包并生成可直接提交 GitHub 的标准 `catalog.json`。

详细操作步骤请参阅 **[使用说明 (USAGE.md)](USAGE.md)**。

## 构建、测试与打包

```sh
make test && make package
python3 tests/workflows.py ./plugin
```

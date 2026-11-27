# MediPlug 使用说明 (v1.0.2)

## 1. 安装与首次配置（兼容 Mediary v3.4.6 ~ v3.4.8+）

1. 将打包产物 `dist/mediplug-1.0.2-linux-amd64.tar.gz` 在 Mediary「**插件 → 插件中心**」上传安装（或通过 `ydddj/mediary-plugins` 索引源一键安装与热更新）。
2. 在「**已安装**」页点击 **MediPlug** 卡片的「**配置**」：
   - 勾选授予 `plugins:read`、`settings:read`、`notifications:send` 权限；
   - 开启「**启用插件**」（自动拉起本地 `http://127.0.0.1:18119/catalog.json` 常驻转换服务，停用时 3 秒内自动回收退出）并保存。
3. 打开 **MediPlug「工作台」**：
   - 首次使用时，前往 Mediary「**设置 → 安全**」复制 **管理员 API Token**，粘贴到 MediPlug 工作台顶部提示框或「**全局设置与记录**」页保存，即可解锁 `/api/plugin-store/sources` 增删管理能力。

## 2. 工作台五大功能页

1. **宿主插件源管理**：
   - 可视化列出 Mediary 当前所有已注册插件源（含内置受保护的 `official` 官方源、第三方直连 `catalog.json` 源及 MediPlug 本地/Gist 桥接源）；
   - 支持一键 **添加标准 `catalog.json` 源**、**启用 / 停用**、**编辑地址** 与 **删除**，操作后即刻在原生「插件中心」生效（兼容 v3.4.8 第三方源数量上限与公网 HTTPS 源策略）。
2. **GitHub 仓库桥接（无 `catalog.json` 仓库自动转换）**：
   - 针对只发布 `.tar.gz` Release 资产、未编写 `catalog.json` 的第三方 GitHub 仓库，直接输入 `https://github.com/owner/repo` 点击「**解析并接入插件中心**」；
   - MediPlug 会自动拉取 Release 资产、校验包内 `plugin.json`（自动同步 `id`、`version`、`min_mediary_version`、`requested_scopes -> permissions` 及扩展能力标识）与 64 位小写 `sha256`，写入本地 `http://127.0.0.1:18119/catalog.json` 并自动注册进宿主 `/api/plugin-store/sources`（若宿主限制公网 HTTPS 源且已配置含 `gist` 权限的 GitHub Token，将自动发布至公开 Gist 完成注册）；
   - 支持选择「合并入统一桥接源」或「注册为独立插件源（`/catalog/<id>.json`）」，定时巡检到新 Release 后宿主「插件中心」会自动亮起「**可更新**」按钮。
3. **插件中心聚合预览**：
   - 实时读取宿主 `GET /api/plugin-store` 与已安装插件元数据，展示插件扩展能力徽章（`playback_hook (+/prefetch)`、`resource_search`、`ui` 等）、宿主兼容状态与 `conflicts` 命名空间冲突详情，支持直接在卡片点击「**一键安装 / 热升级**」。
4. **包校验与索引生成（对齐 Mediary v3.4.6 ~ v3.4.8+ 规范）**：
   - 选择本地构建的 `*-linux-amd64.tar.gz` / `*-linux-arm64.tar.gz` 包，自动按宿主 v3.4.6 ~ v3.4.8+ 标准核验：
     - 根目录直接包含 `plugin.json` 与 `0755` 权限可执行文件 `plugin`；
     - 严禁包含 `config.json` 或 `data/` 运行态目录，禁止符号链接越界；
     - 解压后体积 `<= 250 MB`；
     - `playback_hook`：`transport` 须为 `"unix"`（安全相对非符号链接 `socket`）或 `"http"`（本机回环 `127.0.0.1` / `::1` / `localhost` `endpoint`）、`path`（如 `/resolve`，v3.4.8 同通道自动转发 `POST /prefetch`，4s 独立超时、404 自动回退）、`timeout_ms` 合法；
     - `resource_search`：须同时声明 `action_runtime`，`source_id <= -2`，`action` / `transfer_action` 有效，`timeout_seconds` 在 `1 ~ 300` 秒合法范围内；
   - 自动展示检测到的扩展能力徽章与兼容说明，计算精确字节数 `size` 与 `sha256`，支持将多个插件（如 `medifind`、`mediwash`、`medinext`、`mediplug`）合并导出标准 `catalog.json`，或一键挂载到本地桥接源测试安装。
5. **全局设置与记录**：
   - 配置管理员 API Token、本地 HTTP 监听端口（默认 `127.0.0.1:18119`）、GitHub Token（防匿名 60 次/小时限流，勾选 `gist` 可启用公网 HTTPS 自动桥接）、HTTP/SOCKS 代理、GitHub 下载加速镜像前缀（如 `https://ghfast.top/`），以及配置导入导出与操作日志查询。

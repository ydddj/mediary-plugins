# MediPlug 使用说明 (v1.0.0)

## 1. 安装与首次配置（Mediary 3.4.6+）

1. 将打包产物 `dist/mediplug-1.0.0-linux-amd64.tar.gz` 在 Mediary「**插件 → 插件中心**」上传安装（或通过索引源安装）。
2. 在「**已安装**」页点击 **MediPlug** 卡片的「**配置**」：
   - 勾选授予 `plugins:read`、`settings:read`、`notifications:send` 权限；
   - 开启「**启用插件**」（启动本地 `http://127.0.0.1:18119/catalog.json` 常驻转换服务）并保存。
3. 打开 **MediPlug「工作台」**：
   - 首次使用时，前往 Mediary「**设置 → 安全**」复制 **管理员 API Token**，粘贴到 MediPlug 工作台顶部提示框或「**全局设置与记录**」页保存，即可解锁 `/api/plugin-store/sources` 增删管理能力。

## 2. 工作台五大功能页

1. **宿主插件源管理**：
   - 可视化列出 Mediary 当前所有已注册插件源（含内置受保护的 `official` 官方源、第三方直连 `catalog.json` 源及 MediPlug 本地桥接源）；
   - 支持一键 **添加标准 `catalog.json` 源**、**启用 / 停用**、**编辑地址** 与 **删除**，操作后即刻在原生「插件中心」生效。
2. **GitHub 仓库桥接（无 `catalog.json` 仓库自动转换）**：
   - 针对只发布 `.tar.gz` Release 资产、未编写 `catalog.json` 的第三方 GitHub 仓库，直接输入 `https://github.com/owner/repo` 点击「**解析并接入插件中心**」；
   - MediPlug 会自动拉取 Release 资产、校验包内 `plugin.json`（自动同步 `id`、`version`、`min_mediary_version` 及 `requested_scopes -> permissions`）与 64 位小写 `sha256`，写入本地 `http://127.0.0.1:18119/catalog.json` 并自动注册进宿主 `/api/plugin-store/sources`；
   - 支持选择「合并入统一桥接源」或「注册为独立插件源（`/catalog/<id>.json`）」，定时巡检到新 Release 后宿主「插件中心」会自动亮起「**可更新**」按钮。
3. **插件中心聚合预览**：
   - 实时读取宿主 `GET /api/plugin-store` 合并结果与 `conflicts` 命名空间冲突详情（如与官方源插件重名被拦截时直观提示），支持直接在卡片点击「**安装 / 热升级**」。
4. **包校验与索引生成（发布者工具）**：
   - 选择本地构建的 `*-linux-amd64.tar.gz` / `*-linux-arm64.tar.gz` 包，自动按宿主 v3.4.6 安装器标准核验：
     - 根目录直接包含 `plugin.json` 与 `0755` 权限可执行文件 `plugin`；
     - 严禁包含 `config.json` 或 `data/` 运行态目录；
     - 解压后体积 `<= 250 MB`；
   - 自动计算精确字节数 `size` 与 `sha256`，支持将多个插件（如 `medifind`、`mediwash`、`medinext`、`mediplug`）合并导出标准 `catalog.json`，或一键挂载到本地桥接源测试安装。
5. **全局设置与记录**：
   - 配置管理员 API Token、本地 HTTP 监听端口（默认 `127.0.0.1:18119`）、GitHub Token（防匿名 60 次/小时限流）、HTTP/SOCKS 代理、GitHub 下载加速镜像前缀（如 `https://ghfast.top/`），以及配置导入导出与操作日志查询。

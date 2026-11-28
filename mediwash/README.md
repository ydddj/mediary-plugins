# MediWash 归档入库与权重洗版引擎 (v3.2.12)

面向 **Mediary 3.4.13+**（向下兼容 3.4.6+）的 Rust 115 监控目录自动归档入库与双引擎权重洗版插件（`rusqlite` WAL 瘦身存储 + 纯 `action_runtime` 零常驻进程）。

在 `medifind`（找片转存进货） → **`mediwash`（监控目录归档入库 + 权重洗版覆盖）** → `medinext`（302 播放负载）三段式全栈媒体流水线中，承担中游核心职责：

- **115 监控目录归档入库（内置权重洗版选项）**：监控 115 待整理目录（电影/剧集/动漫全部混放即可），胜出文件直接调用 Mediary 原生 `POST /api/plugin/open115/files/organize` 一步完成二级分类（`category.yaml`）、模板重命名、字幕跟随与 STRM 入库；自动保护「蓝光原盘」专区的 `.iso` 文件。
- **全面适配 Mediary v3.4.13+ `{{resourceType}}` 片源类型与 `{{webSource}}` 流媒体平台重命名**：
  - 宿主 115 原生归档与 PT 整理双链路原生注入 **`{{resourceType}}` / `{{resource_type}}`**（14 项词边界标准化：`BDRemux` / `REMUX` / `UHD BluRay` / `BluRay` / `WEB-DL` / `WEBRip` / `BDRip` / `BDISO` / `BDMV` / `MiniBD` / `HDTV` / `HDRip` / `DVDRip` / `DVD`）与 **`{{webSource}}` / `{{web_source}}`**（20 个国际与国内/亚洲流媒体平台缩写：`NF` / `AMZN` / `DSNP` / `ATVP` / `HMAX` / `HULU` / `PMTP` / `PCOK` / `ITUN` / `CR`、`BILI` / `IQ` / `YK` / `TX` / `MGTV` / `BAHA` / `KKTV` / `MYTV` / `VIU` / `FRIDAY`），推荐组合判空写法 `{% if webSource %}.{{webSource}}{% endif %}{% if resourceType %}.{{resourceType}}{% endif %}`。
- **低分落败直移 115 回收站 + 新源确认后安全删旧**：对接 `POST /api/plugin/open115/files/delete`（`open115:write`），低分淘汰或已在库内的重复文件可直接移入 115 官方回收站；洗版升级确认新源入库后再安全清理旧低分源。
- **媒体历史与任务记录彻底分离**：工作台独立「历史记录」页面将「🎬 媒体处理历史」与「⚙️ 任务执行记录」完全分栏，支持按通道、处理动作与关键词实时检索，支持单删、批量勾选删除、历史重整与一键补齐缺失 `[tmdbid-...]` 记录；宿主卡片「数据」弹窗仅展示干净的媒体历史记录。
- **存量库低负载多版本精洗**：针对数万电影与数十万剧集提供断点续扫，严格限制在同二级分类 + 同版本槽位内比较，仅落盘拟清理与待复核记录（零 `keep` 膨胀），清理前强制二次复核并保留至少 1 个胜出版本。
- **PT 主动搜种洗版**：支持毕业分数线（默认电影 75 / 剧集 65）、站点筛选、免费与排除 H&R 限制、`mr/mp` 季集身份核验，走 `defer_organize=true` 暂缓整理下载与入库核验删旧闭环（适配 Mediary v3.4.8+ 下载拆包选集与整理就绪校验）。
- **Symedia 100 分 & MediaTidy 1000 分（27 槽）双引擎**：参考 Symedia 源码 1:1 兼容 36 项权重字段、逐维度路径黑白名单与版本控制槽位，完整支持 MediaTidy 70% 对数码率评分、27 槽位与 `MuRongYun8` 远程自动订阅及本地 `mr / mp / tmdb_override` 覆盖。

详细操作步骤请参阅 **[使用说明 (USAGE.md)](USAGE.md)**。

## 构建、测试与打包

```sh
make test && make package
python3 tests/workflows.py ./plugin
```

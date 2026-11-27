import { createMediaryBridge, downloadTextFile, copyToClipboard } from './bridge.js';

const bridge = createMediaryBridge();

const SYM_DIMS = [
  { key: 'videoFormat', wKey: 'videoFormat_weight', label: '分辨率', kind: 'map' },
  { key: 'resourceType', wKey: 'resourceType_weight', label: '片源类型', kind: 'map' },
  { key: 'videoCodec', wKey: 'videoCodec_weight', label: '视频编码', kind: 'map' },
  { key: 'audioCodec', wKey: 'audioCodec_weight', label: '音频编码', kind: 'map' },
  { key: 'color_depth', wKey: 'color_depth_weight', label: '色彩深度', kind: 'map' },
  { key: 'frame_rate', wKey: 'frame_rate_weight', label: '帧率', kind: 'map' },
  { key: 'dolby_vision', wKey: 'dolby_vision_weight', label: '杜比视界', kind: 'map' },
  { key: 'dynamic_range', wKey: 'dynamic_range_weight', label: '动态范围', kind: 'map' },
  { key: 'subtitle', wKey: 'subtitle_weight', label: '字幕', kind: 'map' },
  { key: 'file_extension', wKey: 'file_extension_weight', label: '容器后缀', kind: 'map' },
  { key: 'releaseGroup', wKey: 'releaseGroup_weight', label: '制作组', kind: 'list' },
  { key: 'custom_attributes', wKey: 'custom_weight', label: '自定义属性', kind: 'list' },
  { key: 'file_size', wKey: 'file_size_weight', label: '文件体积加分', kind: 'scalar' },
  { key: 'version_control', wKey: null, label: '多版本槽位控制', kind: 'slots' },
];

const TIDY_DIMS = [
  { key: 'video_bitrate', label: '视频码率 (对数)', group: '码率核心' },
  { key: 'bitrate', label: '总码率 (对数)', group: '码率核心' },
  { key: 'file_size', label: '文件体积 (对数)', group: '码率核心' },
  { key: 'source', label: '片源类型', group: '规格属性' },
  { key: 'resolution', label: '分辨率', group: '规格属性' },
  { key: 'audio_codec', label: '音频编码', group: '规格属性' },
  { key: 'hdr', label: '动态范围', group: '规格属性' },
  { key: 'subtitle', label: '字幕轨', group: '规格属性' },
  { key: 'frame_rate', label: '帧率', group: '规格属性' },
  { key: 'video_codec', label: '视频编码', group: '规格属性' },
  { key: 'color_depth', label: '色彩深度', group: '规格属性' },
  { key: 'file_extension', label: '容器后缀', group: '规格属性' },
];

const state = {
  tab: 'staging',
  boot: null,
  busy: false,

  // Tab 1: Staging & Ledger
  stagingSubTab: 'candidates', // 'candidates' | 'ledger'
  stagingPipelineFilter: 0,
  stagingVerdictFilter: 'all',
  stagingStateFilter: 'pending',
  stagingItems: [],
  stagingSelected: new Set(),
  expandedStagingId: null,
  ledgerStateFilter: 'active',
  ledgerItems: [],

  // Tab 2: Library Dedup
  showOverrides: false,
  libScope: 'movie',
  libStatus: 'suggestion',
  libState: 'pending',
  libCategory: '',
  libPage: { total: 0, items: [], has_more: false, next_cursor: 0 },
  libSelected: new Set(),

  // Tab 3: PT Wash
  ptLookupKeyword: '',
  ptLookupType: 'all',
  ptLookupDiag: 'all',
  ptLookupResults: [],
  ptSearchBanner: null,
  ptCandidates: [],
  ptCandState: 'pending',

  // Tab 4: Rules & Subscriptions
  ruleSubTab: 'symedia', // 'symedia' | 'mediatidy' | 'subs' | 'tester'
  selectedSymRuleId: '',
  selectedTidyRuleId: '',
  activeRuleDetail: null,
  symActiveDim: 'videoFormat',
  auxData: null,
  sampleLeft: {
    name: '光阴之外.S01E49.2160p.HDR10.WEB-DL.H265.AAC.mkv',
    folder: '光阴之外 (2025)/Season 01',
    size_gb: 0.85,
    bitrate_mbps: 5.6,
    video_bitrate_mbps: 5.1,
  },
  sampleRight: {
    name: '光阴之外.S01E49.2160p.HDR10.WEB-DL.H265.DDP5.1.ASS.简英特效.mkv',
    folder: '光阴之外 (2025)/Season 01',
    size_gb: 2.85,
    bitrate_mbps: 19.4,
    video_bitrate_mbps: 18.2,
  },
  sampleCategory: '动漫/国漫',
  sampleRuleId: '',
  sampleResult: null,

  // Tab 5: Separated Media & Task History
  histType: 'media', // 'media' | 'task'
  histKind: 'all',
  histAction: 'all',
  histStatus: 'all',
  histKeyword: '',
  histData: { items: [], media_total: 0, task_total: 0, filtered_total: 0 },
  histSelected: new Set(),
};

const esc = (v) =>
  String(v ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');

const fmtBytes = (n) => {
  const b = Number(n || 0);
  if (!b || b <= 0) return '-';
  if (b >= 1e9) return `${(b / 1e9).toFixed(2)} GB`;
  if (b >= 1e6) return `${(b / 1e6).toFixed(1)} MB`;
  return `${(b / 1e3).toFixed(0)} KB`;
};

let toastTimer = null;
function showToast(msg, kind = 'info') {
  const el = document.getElementById('mw-toast');
  if (!el) return;
  el.textContent = msg;
  el.className = `mw-toast ${kind}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add('hidden'), 4800);
}

async function callAction(name, payload = {}, silent = false) {
  try {
    state.busy = true;
    return await bridge.action(name, payload);
  } catch (err) {
    if (!silent) showToast(err.message || String(err), 'error');
    throw err;
  } finally {
    state.busy = false;
  }
}

function openModal(title, html, onMount) {
  document.getElementById('mw-modal-title').textContent = title;
  const body = document.getElementById('mw-modal-body');
  body.innerHTML = html;
  document.getElementById('mw-modal-backdrop').classList.remove('hidden');
  if (onMount) onMount(body);
}

function closeModal() {
  document.getElementById('mw-modal-backdrop').classList.add('hidden');
  document.getElementById('mw-modal-body').innerHTML = '';
}

function pickLocalFile(accept, onContent) {
  const input = document.getElementById('mw-hidden-file');
  input.accept = accept || '';
  input.value = '';
  input.onchange = async () => {
    const file = input.files && input.files[0];
    if (!file) return;
    const text = await file.text();
    onContent(file.name, text);
  };
  input.click();
}

function syncSidebarBadges() {
  const b = state.boot;
  if (!b) return;
  document.getElementById('mw-version-label').textContent = `v${b.version || '2.0.0'} · 统一规则洗版`;
  const c = b.counts || {};
  document.getElementById('badge-staging').textContent =
    Number(c.staging_pending || 0) + Number(c.ready_cleanup || 0);
  document.getElementById('badge-library').textContent = Number(c.lib_suggestions || 0);
  document.getElementById('badge-pt').textContent = Number(c.pt_pending || 0);
  document.getElementById('badge-rules').textContent = (b.rules || []).length;
  const badgeHist = document.getElementById('badge-history');
  if (badgeHist) badgeHist.textContent = Number(c.media_history || 0);

  const modePill = document.getElementById('mw-mode-pill');
  if (b.settings?.dry_run) {
    modePill.className = 'mw-status-pill warn';
    modePill.textContent = '当前：仅预览模式（干跑安全）';
  } else {
    modePill.className = 'mw-status-pill ok';
    modePill.textContent = b.settings?.auto_delete_old
      ? '当前：实盘执行 + 自动删旧'
      : '当前：实盘执行（手动删旧）';
  }

  const p115 = document.getElementById('mw-115-pill');
  const hs = b.host_status || {};
  if (hs.open_api_connected || hs.web_cookie_ready) {
    const parts = [];
    if (hs.open_api_connected) parts.push('OpenAPI');
    if (hs.web_cookie_ready) parts.push('Cookie');
    p115.className = 'mw-status-pill ok';
    p115.textContent = `宿主 115 已连 (${parts.join('+')})`;
  } else {
    p115.className = 'mw-status-pill warn';
    p115.textContent = '宿主 115 待检测/未连';
  }

  const syncNote = document.getElementById('mw-host-sync-note');
  const hsync = b.host_sync || {};
  syncNote.textContent = `已同步 ${hsync.category_count || 0} 个二级分类 · ${hsync.category_source || '等待同步'}`;
}

async function refreshBootstrap() {
  const b = await callAction('ui_bootstrap', {});
  applyBootstrap(b);
}

function applyBootstrap(b) {
  if (!b) return;
  const prevSites = state.boot?.pt_sites;
  const prevHostStatus = state.boot?.host_status;
  state.boot = b;
  if ((!b.pt_sites || !b.pt_sites.length) && prevSites?.length) {
    state.boot.pt_sites = prevSites;
  }
  if (!b.host_status && prevHostStatus) {
    state.boot.host_status = prevHostStatus;
  }
  const symRules = (b.rules || []).filter((r) => r.kind === 'symedia');
  const tidyRules = (b.rules || []).filter((r) => r.kind === 'mediatidy');
  if (!state.selectedSymRuleId || !symRules.some((r) => r.id === state.selectedSymRuleId)) {
    state.selectedSymRuleId = symRules[0]?.id || '';
  }
  if (!state.selectedTidyRuleId || !tidyRules.some((r) => r.id === state.selectedTidyRuleId)) {
    state.selectedTidyRuleId = tidyRules[0]?.id || '';
  }
  if (!state.sampleRuleId) {
    state.sampleRuleId = b.strategy?.movie_rule_id || tidyRules[0]?.id || symRules[0]?.id || '';
  }
  syncSidebarBadges();
}

async function switchTab(tab) {
  state.tab = tab;
  document.querySelectorAll('#mw-nav .mw-nav-btn').forEach((btn) => {
    btn.classList.toggle('active', btn.dataset.tab === tab);
  });
  await loadCurrentTabData();
  render();
}

async function loadCurrentTabData() {
  if (!state.boot) return;
  if (state.tab === 'staging') {
    if (state.stagingSubTab === 'candidates') {
      const res = await callAction(
        'ui_staging_list',
        {
          pipeline_id: state.stagingPipelineFilter || null,
          state: state.stagingStateFilter,
          verdict: state.stagingVerdictFilter,
        },
        true
      );
      state.stagingItems = res.items || [];
      state.boot.counts = res.counts || state.boot.counts;
    } else {
      const res = await callAction('ui_ledger_list', { state: state.ledgerStateFilter }, true);
      state.ledgerItems = res.items || [];
      state.boot.counts = res.counts || state.boot.counts;
    }
  } else if (state.tab === 'library') {
    const res = await callAction(
      'ui_lib_decisions',
      {
        scope: state.libScope,
        status: state.libStatus,
        state: state.libState,
        category: state.libCategory,
        after_id: 0,
      },
      true
    );
    state.boot.scan_jobs = res.scan_jobs || state.boot.scan_jobs;
    state.libPage = res.page || { total: 0, items: [], has_more: false, next_cursor: 0 };
    state.boot.counts = res.counts || state.boot.counts;
  } else if (state.tab === 'pt') {
    const res = await callAction('ui_pt_candidates', { state: state.ptCandState }, true);
    state.ptCandidates = res.candidates || [];
    state.boot.counts = res.counts || state.boot.counts;
  } else if (state.tab === 'rules') {
    if (state.ruleSubTab === 'symedia' && state.selectedSymRuleId) {
      if (!state.activeRuleDetail || state.activeRuleDetail.id !== state.selectedSymRuleId) {
        state.activeRuleDetail = await callAction('ui_rule_detail', { id: state.selectedSymRuleId }, true);
      }
    } else if (state.ruleSubTab === 'mediatidy' && state.selectedTidyRuleId) {
      if (!state.activeRuleDetail || state.activeRuleDetail.id !== state.selectedTidyRuleId) {
        state.activeRuleDetail = await callAction('ui_rule_detail', { id: state.selectedTidyRuleId }, true);
      }
    } else if (state.ruleSubTab === 'subs') {
      state.auxData = await callAction('ui_get_aux_yaml', {}, true);
    }
  } else if (state.tab === 'history') {
    const res = await callAction(
      'ui_history_list',
      {
        record_type: state.histType,
        kind: state.histKind,
        action_group: state.histType === 'media' ? state.histAction : 'all',
        status: state.histStatus,
        keyword: state.histKeyword,
        limit: 250,
      },
      true
    );
    state.histData = res || { items: [], media_total: 0, task_total: 0, filtered_total: 0 };
    state.boot.counts = res.counts || state.boot.counts;
  }
  syncSidebarBadges();
}

function verdictBadge(verdict) {
  switch (verdict) {
    case 'admit_new':
      return '<span class="mw-tag ok">新片归档入库</span>';
    case 'admit_upgrade':
      return '<span class="mw-tag primary">洗版胜出替换</span>';
    case 'reject':
      return '<span class="mw-tag danger">淘汰移入回收站</span>';
    case 'probing':
      return '<span class="mw-tag warn">探测排队中</span>';
    case 'already_in_lib':
      return '<span class="mw-tag danger">已在库内·回收</span>';
    default:
      return '<span class="mw-tag warn">待人工核对</span>';
  }
}

function ledgerBadge(st) {
  switch (st) {
    case 'downloading':
      return '<span class="mw-tag primary">PT 下载中</span>';
    case 'downloaded':
      return '<span class="mw-tag warn">下载完成·待整理</span>';
    case 'awaiting_ingest':
      return '<span class="mw-tag warn">等待入库核验</span>';
    case 'ready_to_clean':
      return '<span class="mw-tag ok">新源已入库·可删旧</span>';
    case 'cleaned':
      return '<span class="mw-tag ok">已完成删旧</span>';
    case 'dismissed':
      return '<span class="mw-tag">已忽略</span>';
    default:
      return '<span class="mw-tag danger">需复核</span>';
  }
}

function render() {
  const root = document.getElementById('mw-view');
  if (!state.boot) {
    root.innerHTML = '<div class="mw-empty">正在连接 Mediary 加载 MediWash 工作台...</div>';
    return;
  }
  if (state.tab === 'staging') renderStagingTab(root);
  else if (state.tab === 'library') renderLibraryTab(root);
  else if (state.tab === 'pt') renderPtTab(root);
  else if (state.tab === 'rules') renderRulesTab(root);
  else if (state.tab === 'history') renderHistoryTab(root);
}

// ============================================================================
// TAB 1: 115 监控目录归档入库（内置权重洗版选项）
// ============================================================================
function renderStagingTab(root) {
  const b = state.boot;
  const c = b.counts || {};
  const hsync = b.host_sync || {};
  const pipelines = b.pipelines || [];
  const primary = pipelines[0] || {
    id: 0,
    name: '115 待洗版监控归档',
    enabled: true,
    media_type: 'auto',
    source_cid: '',
    source_name: '',
    recursive: true,
    move_subtitles: true,
    auto_admit: true,
    wash_enabled: true,
    reject_mode: 'recycle_bin',
    probe_mode: 'smart',
    rule_id_override: '',
  };
  const rules = b.rules || [];
  const extraPipelines = pipelines.slice(1);
  const rejMode = primary.reject_mode === 'skip' ? 'skip' : 'recycle_bin';
  const probeMode = primary.probe_mode || 'smart';
  const libTargetHint = hsync.transfer_target_folder
    ? `${hsync.transfer_target_folder}${hsync.transfer_target_cid ? ` (${hsync.transfer_target_cid})` : ''}`
    : '跟随 Mediary 主程序媒体库设置';

  root.innerHTML = `
    <div class="mw-header">
      <div>
        <h2>115 监控目录归档入库（原生一步直达媒体库 + 权重洗版）</h2>
        <p>仅需设置 1 个待洗版目录：胜出文件调用 Mediary 3.4.6 原生接口一步完成二级分类、重命名、字幕跟随与 STRM 入库；低分落败与重复文件直接移入 115 官方回收站</p>
      </div>
      <div class="mw-actions">
        <button type="button" class="mw-btn" id="btn-goto-history">查看历史记录 (${Number(c.media_history || 0)})</button>
        <button type="button" class="mw-btn" id="btn-sync-host">同步主程序规则</button>
        <button type="button" class="mw-btn primary" id="btn-run-tick">立即全量巡检</button>
      </div>
    </div>

    ${
      b.settings?.dry_run
        ? `<div class="mw-card" style="border-color:#d97706;background:color-mix(in srgb, #f59e0b 10%, var(--mw-surface));">
            <div style="font-size:13px;line-height:1.55;">
              <strong>⚠️ 当前处于「仅预览模式（干跑）」——评分结果暂未执行实际归档与清理</strong><br/>
              <span class="mw-muted">确认评分规则符合预期后，关闭插件右上角「配置 → 仅预览模式（干跑）」，后续所有新片入库、高分洗版升级、低分移入回收站均<strong>全自动一步到位，无需任何中间目录</strong>。</span>
            </div>
          </div>`
        : ''
    }

    ${
      hsync.has_flowlink_transfer_template === false
        ? `<div class="mw-card" style="border-color:#d97706;background:color-mix(in srgb, #f59e0b 10%, var(--mw-surface));">
            <div style="display:flex;justify-content:space-between;align-items:flex-start;gap:12px;flex-wrap:wrap;font-size:12.5px;line-height:1.55;">
              <div style="flex:1;min-width:260px;">
                <strong>⚠️ 检测到宿主「内置 STRM 工具 → 转存整理任务」列表为空（会导致归档目录缺少 <code>[tmdbid-...]</code>）</strong><br/>
                <span class="mw-muted">主程序的 <code>open115/files/organize</code> 接口会从内置 STRM 工具的默认转存任务读取 115 电影/剧集重命名模板；当转存任务被清空时，会回退为不含 <code>[tmdbid-...]</code> 的简版模板。请在内置 STRM 工具中新建一条<strong>取消勾选「启用」（保持停用即可，不会抢跑转存）</strong>的默认任务并粘贴下方模板，随后在历史记录点「重整」即可修正。</span>
              </div>
              <div class="mw-actions" style="flex-shrink:0;">
                ${hsync.movie_rename_format ? `<button type="button" class="mw-btn sm" id="btn-copy-tpl-movie">复制电影模板</button>` : ''}
                ${hsync.tv_rename_format ? `<button type="button" class="mw-btn sm primary" id="btn-copy-tpl-tv">复制剧集模板</button>` : ''}
              </div>
            </div>
          </div>`
        : ''
    }

    <div class="mw-kpis">
      <div class="mw-kpi">
        <div class="mw-kpi-label">待处理候选记录</div>
        <div class="mw-kpi-val">${Number(c.staging_pending || 0)}</div>
        <div class="mw-kpi-sub">其中 TMDB 未识别兜底 ${Number(c.staging_review || 0)} 条</div>
      </div>
      <div class="mw-kpi">
        <div class="mw-kpi-label">洗版待删旧追踪账本</div>
        <div class="mw-kpi-val">${Number(c.awaiting_cleanup || 0)}</div>
        <div class="mw-kpi-sub">新源已入库可删旧 ${Number(c.ready_cleanup || 0)} 条</div>
      </div>
      <div class="mw-kpi">
        <div class="mw-kpi-label">存量库多版本拟清理</div>
        <div class="mw-kpi-val">${Number(c.lib_suggestions || 0)}</div>
        <div class="mw-kpi-sub">异常待复核 ${Number(c.lib_reviews || 0)} 组</div>
      </div>
      <div class="mw-kpi">
        <div class="mw-kpi-label">Mediary 原生归档联动</div>
        <div class="mw-kpi-val">${hsync.category_count || 0} <span style="font-size:13px;font-weight:500;">个二级分类</span></div>
        <div class="mw-kpi-sub" title="目标媒体库: ${esc(libTargetHint)}">目标媒体库：${esc(libTargetHint)}</div>
      </div>
    </div>

    <div class="mw-card">
      <div class="mw-card-head">
        <div>
          <h3 class="mw-card-title">115 待洗版监控目录设置（单目录一步归档生成 STRM）</h3>
          <div class="mw-card-sub">
            电影、电视剧、动漫混放同一待洗版目录即可；无需中间转存目录与隔离目录，胜出直达媒体库二级分类秒出 STRM，淘汰直进 115 官方回收站
          </div>
        </div>
        <div class="mw-actions">
          <button type="button" class="mw-btn sm" id="btn-edit-primary-pipe">编辑当前目录</button>
          <button type="button" class="mw-btn sm" id="btn-browse-115-standalone">浏览/实测 115 目录</button>
          <button type="button" class="mw-btn sm ghost" id="btn-new-pipeline">+ 额外监控目录</button>
        </div>
      </div>

      <div class="mw-grid-3">
        <div class="mw-field">
          <label>待洗版监控目录 CID（MediFind / 本地上传落点）</label>
          <div style="display:flex;gap:6px;">
            <input class="mw-input mw-mono" id="qp-src-cid" value="${esc(primary.source_cid)}" placeholder="选择 115 待洗版监控目录 CID" />
            <button type="button" class="mw-btn sm" id="btn-qp-pick-src">浏览</button>
          </div>
          <span class="mw-muted" id="qp-src-name" style="font-size:11.5px;">
            ${esc(primary.source_name ? `当前监控：${primary.source_name} (${primary.source_cid})` : '电影 / 剧集 / 动漫混放此目录，自动识别类型与季集')}
          </span>
        </div>

        <div class="mw-field">
          <label>归档目标媒体库根目录 CID（留空自动识别）</label>
          <div style="display:flex;gap:6px;">
            <input class="mw-input mw-mono" id="qp-dst-cid" value="${esc(primary.target_cid || '')}" placeholder="${esc(hsync.transfer_target_cid ? `自动使用：${libTargetHint}` : '留空自动定位「媒体库」根目录')}" />
            <button type="button" class="mw-btn sm" id="btn-qp-pick-dst">浏览</button>
          </div>
          <span class="mw-muted" id="qp-dst-name" style="font-size:11.5px;">
            ${esc(
              primary.target_cid
                ? `指定归档库根：${primary.target_name || '自定义目录'} (${primary.target_cid})${hsync.transfer_task_name ? ` · 命名模板任务：${hsync.transfer_task_name}` : ''}`
                : `默认归档至：${libTargetHint}${hsync.transfer_task_name ? ` · 复用模板任务：${hsync.transfer_task_name}（保持停用即可）` : '（无需开启转存任务）'}`
            )}
          </span>
        </div>

        <div class="mw-field">
          <label>低分落败 / 已在库内重复文件处理</label>
          <select class="mw-select" id="qp-rej-mode">
            <option value="recycle_bin" ${rejMode === 'recycle_bin' ? 'selected' : ''}>直接移入 115 官方回收站（推荐）</option>
            <option value="skip" ${rejMode === 'skip' ? 'selected' : ''}>原地跳过忽略（保留在原目录不删除）</option>
          </select>
          <span class="mw-muted" style="font-size:11.5px;">
            使用宿主原生 <code>open115/files/delete</code> 移入回收站，受保护目录自动防误删
          </span>
        </div>
      </div>

      <div class="mw-grid-2" style="margin-top:10px;">
        <div class="mw-field">
          <label>115 MediaInfo 实测模式（影响大批量积压归档速度）</label>
          <select class="mw-select" id="qp-probe-mode">
            <option value="smart" ${probeMode === 'smart' ? 'selected' : ''}>智能按需探测（推荐：优先命中缓存，未命中进 1.25s 后台队列）</option>
            <option value="filename_only" ${probeMode === 'filename_only' ? 'selected' : ''}>仅按文件名与体积快速评分（零排队秒级归档，适合百集批量消化）</option>
            <option value="always" ${probeMode === 'always' ? 'selected' : ''}>强制实测全部音视频流后再归档</option>
          </select>
        </div>

        <div class="mw-field">
          <label>专属评分方案（留空跟随全局电影/剧集统一策略）</label>
          <select class="mw-select" id="qp-rule-override">
            <option value="">跟随全局电影/剧集统一策略</option>
            ${rules
              .map(
                (r) =>
                  `<option value="${esc(r.id)}" ${r.id === (primary.rule_id_override || '') ? 'selected' : ''}>${esc(r.name)}</option>`
              )
              .join('')}
          </select>
        </div>
      </div>

      <div id="qp-inline-picker" style="margin-top:10px;"></div>

      <div style="margin-top:12px;padding-top:12px;border-top:1px solid var(--mw-border);display:flex;justify-content:space-between;align-items:center;gap:12px;flex-wrap:wrap;">
        <div style="display:flex;flex-wrap:wrap;gap:16px;align-items:center;">
          <label class="mw-check">
            <input type="checkbox" id="qp-wash" ${primary.wash_enabled !== false ? 'checked' : ''} />
            <span><strong>归档时启用权重洗版（库内已有同集时对比评分决定升级替换）</strong></span>
          </label>
          <label class="mw-check">
            <input type="checkbox" id="qp-auto" ${primary.auto_admit !== false ? 'checked' : ''} />
            <span><strong>全自动执行（免人工复检）</strong></span>
          </label>
          <label class="mw-check">
            <input type="checkbox" id="qp-enabled" ${primary.enabled !== false ? 'checked' : ''} />
            <span>启用定时巡检</span>
          </label>
          <label class="mw-check">
            <input type="checkbox" id="qp-rec" ${primary.recursive !== false ? 'checked' : ''} />
            <span>递归子目录</span>
          </label>
          <label class="mw-check">
            <input type="checkbox" id="qp-sub" ${primary.move_subtitles !== false ? 'checked' : ''} />
            <span>淘汰时同步清理同名外挂字幕</span>
          </label>
        </div>

        <div class="mw-actions">
          <button type="button" class="mw-btn" id="btn-qp-save">保存设置</button>
          <button type="button" class="mw-btn primary" id="btn-qp-save-scan">保存并立即扫描处理</button>
        </div>
      </div>

      ${
        extraPipelines.length > 0
          ? `<div style="margin-top:12px;padding-top:10px;border-top:1px dashed var(--mw-border);">
              <div class="mw-muted" style="font-size:12px;margin-bottom:6px;">附加监控目录（${extraPipelines.length}）</div>
              <div class="mw-table-wrap">
                <table class="mw-table">
                  <tbody>
                    ${extraPipelines
                      .map(
                        (p) => `
                      <tr>
                        <td><strong>${esc(p.name)}</strong> ${p.enabled ? '<span class="mw-tag ok">已启用</span>' : '<span class="mw-tag">已停用</span>'}</td>
                        <td class="mw-mono">监控 CID: ${esc(p.source_cid)}${p.source_name ? ` (${esc(p.source_name)})` : ''}</td>
                        <td>${p.auto_admit ? '<span class="mw-tag ok">按评分全自动</span>' : '<span class="mw-tag">手动</span>'}</td>
                        <td style="text-align:right;">
                          <button type="button" class="mw-btn sm primary js-scan-pipe" data-id="${p.id}">扫描</button>
                          <button type="button" class="mw-btn sm js-edit-pipe" data-id="${p.id}">编辑</button>
                          <button type="button" class="mw-btn sm danger js-del-pipe" data-id="${p.id}">删除</button>
                        </td>
                      </tr>`
                      )
                      .join('')}
                  </tbody>
                </table>
              </div>
            </div>`
          : ''
      }
    </div>

    <div class="mw-card">
      <div class="mw-card-head">
        <div class="mw-pills">
          <button type="button" class="mw-pill ${state.stagingSubTab === 'candidates' ? 'active' : ''}" data-sub="candidates">
            候选记录与异常兜底 (${Number(c.staging_pending || 0)})
          </button>
          <button type="button" class="mw-pill ${state.stagingSubTab === 'ledger' ? 'active' : ''}" data-sub="ledger">
            洗版删旧追踪账本 (${Number(c.awaiting_cleanup || 0)})
          </button>
        </div>

        ${
          state.stagingSubTab === 'candidates'
            ? `<div class="mw-actions">
                <select class="mw-select" id="sel-staging-verdict" style="width:auto;">
                  <option value="all" ${state.stagingVerdictFilter === 'all' ? 'selected' : ''}>全部结论</option>
                  <option value="review" ${state.stagingVerdictFilter === 'review' ? 'selected' : ''}>仅看 TMDB 未识别兜底</option>
                  <option value="admit_new" ${state.stagingVerdictFilter === 'admit_new' ? 'selected' : ''}>新片归档入库</option>
                  <option value="admit_upgrade" ${state.stagingVerdictFilter === 'admit_upgrade' ? 'selected' : ''}>洗版胜出替换</option>
                  <option value="reject" ${state.stagingVerdictFilter === 'reject' ? 'selected' : ''}>淘汰移入回收站</option>
                  <option value="probing" ${state.stagingVerdictFilter === 'probing' ? 'selected' : ''}>MediaInfo 探测中</option>
                </select>
                <select class="mw-select" id="sel-staging-state" style="width:auto;">
                  <option value="pending" ${state.stagingStateFilter === 'pending' ? 'selected' : ''}>待处理</option>
                  <option value="organized" ${state.stagingStateFilter === 'organized' ? 'selected' : ''}>已归档入库</option>
                  <option value="recycled_115" ${state.stagingStateFilter === 'recycled_115' ? 'selected' : ''}>已移入115回收站</option>
                  <option value="dismissed" ${state.stagingStateFilter === 'dismissed' ? 'selected' : ''}>已跳过/忽略</option>
                </select>
                <button type="button" class="mw-btn sm success" id="btn-batch-admit">执行勾选建议 (${state.stagingSelected.size})</button>
                <button type="button" class="mw-btn sm" id="btn-batch-force-organize">强制归档勾选</button>
                <button type="button" class="mw-btn sm danger" id="btn-batch-recycle">移入回收站</button>
                <button type="button" class="mw-btn sm ghost" id="btn-batch-dismiss">忽略勾选</button>
              </div>`
            : `<div class="mw-actions">
                <select class="mw-select" id="sel-ledger-state" style="width:auto;">
                  <option value="active" ${state.ledgerStateFilter === 'active' ? 'selected' : ''}>进行中 / 待删旧</option>
                  <option value="ready_to_clean" ${state.ledgerStateFilter === 'ready_to_clean' ? 'selected' : ''}>新源已入库·可删旧</option>
                  <option value="cleaned" ${state.ledgerStateFilter === 'cleaned' ? 'selected' : ''}>已完成删旧</option>
                  <option value="all" ${state.ledgerStateFilter === 'all' ? 'selected' : ''}>全部记录</option>
                </select>
                <button type="button" class="mw-btn sm primary" id="btn-reconcile-ledger">立即刷新核验入库</button>
              </div>`
        }
      </div>

      ${state.stagingSubTab === 'candidates' ? renderStagingCandidatesTable() : renderLedgerTable()}
    </div>
  `;

  document.getElementById('btn-goto-history').onclick = () => switchTab('history');

  document.getElementById('btn-sync-host').onclick = async () => {
    const res = await callAction('ui_sync_host', {});
    applyBootstrap(res);
    showToast('已同步 Mediary 主程序转存目录、二级分类与重命名模板', 'success');
    render();
  };

  const copyMovieBtn = document.getElementById('btn-copy-tpl-movie');
  if (copyMovieBtn) {
    copyMovieBtn.onclick = async () => {
      await copyToClipboard(hsync.movie_rename_format || '');
      showToast('已复制电影重命名模板（含 [tmdbid]）', 'success');
    };
  }
  const copyTvBtn = document.getElementById('btn-copy-tpl-tv');
  if (copyTvBtn) {
    copyTvBtn.onclick = async () => {
      await copyToClipboard(hsync.tv_rename_format || '');
      showToast('已复制剧集重命名模板（含 [tmdbid] 与 Season 01）', 'success');
    };
  }

  document.getElementById('btn-run-tick').onclick = async () => {
    showToast('正在执行目录扫描分流、就绪探测复检与删旧核验...', 'info');
    let lastNotice = '';
    for (let round = 1; round <= 24; round++) {
      const res = await callAction('tick', { manual: true, ui_slice: true, reset_cursor: round === 1 });
      lastNotice = res.notice || '巡检完成';
      if (res.counts && state.boot) {
        state.boot.counts = res.counts;
        syncSidebarBadges();
      }
      await loadCurrentTabData();
      render();
      if (!res.has_more) break;
      showToast(`已完成第 ${round} 批处理，正在继续下一批（${lastNotice}）...`, 'info');
    }
    showToast(lastNotice || '巡检完成', 'success');
  };

  let pickedSrcName = primary.source_name || '';
  let pickedDstName = primary.target_name || '';
  document.getElementById('btn-qp-pick-src').onclick = () => {
    mountInline115Picker(document.getElementById('qp-inline-picker'), (cid, dirName) => {
      document.getElementById('qp-src-cid').value = cid;
      document.getElementById('qp-src-name').textContent = `已选监控：${dirName} (${cid})`;
      pickedSrcName = dirName;
    });
  };
  document.getElementById('btn-qp-pick-dst').onclick = () => {
    mountInline115Picker(document.getElementById('qp-inline-picker'), (cid, dirName) => {
      document.getElementById('qp-dst-cid').value = cid;
      document.getElementById('qp-dst-name').textContent = `已选归档库根：${dirName} (${cid})`;
      pickedDstName = dirName;
    });
  };

  const savePrimaryPipeline = async (runScan) => {
    const srcCid = document.getElementById('qp-src-cid').value.trim();
    const dstCid = document.getElementById('qp-dst-cid').value.trim();
    if (!srcCid) {
      return showToast('请填写或点击「浏览」选择待洗版监控目录 CID', 'error');
    }
    const pipeline = {
      id: primary.id || 0,
      name: primary.name && primary.name !== '剧集' ? primary.name : '115 待洗版监控归档',
      enabled: document.getElementById('qp-enabled').checked,
      media_type: 'auto',
      source_cid: srcCid,
      source_name: pickedSrcName,
      target_cid: dstCid,
      target_name: dstCid ? pickedDstName : '',
      recursive: document.getElementById('qp-rec').checked,
      move_subtitles: document.getElementById('qp-sub').checked,
      auto_admit: document.getElementById('qp-auto').checked,
      wash_enabled: document.getElementById('qp-wash').checked,
      reject_mode: document.getElementById('qp-rej-mode').value,
      probe_mode: document.getElementById('qp-probe-mode').value || 'smart',
      rule_id_override: document.getElementById('qp-rule-override').value || '',
    };
    if (runScan) showToast('正在保存设置并分批执行一步归档与洗版处理...', 'info');
    let res = await callAction('ui_save_pipeline', { pipeline, run_scan: runScan });
    applyBootstrap(res);
    await loadCurrentTabData();
    render();
    let lastNotice = res.scan?.notice || '监控目录与洗版设置已保存';
    if (runScan && res.scan?.has_more && res.scan?.pipeline_id) {
      const pid = res.scan.pipeline_id;
      for (let round = 2; round <= 8; round++) {
        showToast(`第 ${round - 1} 批已完成，正在继续处理剩余文件（第 ${round} 批）...`, 'info');
        const stepRes = await callAction('ui_scan_pipeline', { id: pid, reset_cursor: false });
        state.stagingItems = stepRes.items || state.stagingItems;
        state.boot.counts = stepRes.counts || state.boot.counts;
        syncSidebarBadges();
        render();
        lastNotice = stepRes.scan?.notice || lastNotice;
        if (!stepRes.scan?.has_more) break;
      }
    }
    showToast(lastNotice, 'success');
  };

  document.getElementById('btn-qp-save').onclick = () => savePrimaryPipeline(false);
  document.getElementById('btn-qp-save-scan').onclick = () => savePrimaryPipeline(true);

  document.getElementById('btn-edit-primary-pipe').onclick = () => openPipelineModal(primary.id ? primary : null);
  document.getElementById('btn-browse-115-standalone').onclick = () => open115BrowserModal();
  document.getElementById('btn-new-pipeline').onclick = () => openPipelineModal(null);

  root.querySelectorAll('.js-scan-pipe').forEach((btn) => {
    btn.onclick = async () => {
      const pid = Number(btn.dataset.id);
      showToast('正在扫描 115 中转目录并比对媒体库...', 'info');
      const res = await callAction('ui_scan_pipeline', { id: pid, reset_cursor: true });
      showToast(res.scan?.notice || '扫描完成', 'success');
      await loadCurrentTabData();
      render();
    };
  });
  root.querySelectorAll('.js-edit-pipe').forEach((btn) => {
    btn.onclick = () => {
      const p = (state.boot.pipelines || []).find((x) => x.id === Number(btn.dataset.id));
      if (p) openPipelineModal(p);
    };
  });
  root.querySelectorAll('.js-del-pipe').forEach((btn) => {
    btn.onclick = async () => {
      const pid = Number(btn.dataset.id);
      const res = await callAction('ui_delete_pipeline', { id: pid });
      applyBootstrap(res);
      await loadCurrentTabData();
      render();
    };
  });

  root.querySelectorAll('.mw-pill[data-sub]').forEach((btn) => {
    btn.onclick = async () => {
      state.stagingSubTab = btn.dataset.sub;
      await loadCurrentTabData();
      render();
    };
  });

  if (state.stagingSubTab === 'candidates') {
    bindStagingCandidatesEvents(root);
  } else {
    bindLedgerEvents(root);
  }
}

function renderStagingCandidatesTable() {
  const items = state.stagingItems || [];
  if (!items.length) {
    return `<div class="mw-empty">当前筛选条件下暂无中转记录。</div>`;
  }
  return `
    <div class="mw-table-wrap">
      <table class="mw-table">
        <thead>
          <tr>
            <th><input type="checkbox" id="chk-staging-all" /></th>
            <th>文件名 / 相对目录</th>
            <th>识别媒体</th>
            <th>槽位与得分对比</th>
            <th>分流结论与原因</th>
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          ${items
            .map((it) => {
              const checked = state.stagingSelected.has(it.id) ? 'checked' : '';
              const se =
                it.media_type === 'tv' && it.episode > 0
                  ? ` S${String(it.season || 1).padStart(2, '0')}E${String(it.episode).padStart(2, '0')}`
                  : '';
              const expanded = state.expandedStagingId === it.id;
              const breakdown = it.detail?.breakdown || [];
              return `
                <tr>
                  <td><input type="checkbox" class="js-chk-staging" data-id="${it.id}" ${checked} /></td>
                  <td>
                    <div style="font-weight:600;word-break:break-all;">${esc(it.name)}</div>
                    <div class="mw-muted" style="font-size:11.5px;">
                      ${it.rel_path ? `📁 ${esc(it.rel_path)} · ` : ''}${fmtBytes(it.size)}
                      ${it.sidecar_count > 0 ? ` · 含 ${it.sidecar_count} 个外挂字幕` : ''}
                    </div>
                  </td>
                  <td>
                    ${
                      it.tmdb_id
                        ? `<div>
                             <strong>${esc(it.title)}${esc(se)}</strong>
                             ${it.detail?.episode_title ? `<span class="mw-muted" style="font-size:12px;"> · ${esc(it.detail.episode_title)}</span>` : ''}
                           </div>
                           <div class="mw-muted" style="font-size:11.5px;">TMDB:${esc(it.tmdb_id)} ${it.category ? `· ${esc(it.category)}` : ''}</div>`
                        : `<span class="mw-muted">未绑定 TMDB</span>`
                    }
                  </td>
                  <td>
                    ${it.slot ? `<span class="mw-tag primary">${esc(it.slot)}</span> ` : ''}
                    <strong>${Number(it.cand_score || 0).toFixed(2)}</strong>
                    ${it.lib_score != null ? `<span class="mw-muted"> vs 库内 ${Number(it.lib_score).toFixed(2)}</span>` : ''}
                  </td>
                  <td>
                    <div>${verdictBadge(it.verdict)}</div>
                    <div class="mw-muted" style="font-size:11.5px;margin-top:3px;">${esc(it.reason)}</div>
                  </td>
                  <td>
                    <div class="mw-actions">
                      <button type="button" class="mw-btn sm js-bind-tmdb" data-id="${it.id}">指定 TMDB</button>
                      ${
                        it.state === 'pending' && (it.verdict === 'admit_new' || it.verdict === 'admit_upgrade')
                          ? `<button type="button" class="mw-btn sm success js-act-one" data-id="${it.id}" data-route="organize">一步归档</button>`
                          : ''
                      }
                      ${
                        it.state === 'pending' && (it.verdict === 'reject' || it.verdict === 'already_in_lib')
                          ? `<button type="button" class="mw-btn sm danger js-act-one" data-id="${it.id}" data-route="recycle">移入回收站</button>`
                          : ''
                      }
                      <button type="button" class="mw-btn sm ghost js-toggle-detail" data-id="${it.id}">${expanded ? '收起' : '明细'}</button>
                    </div>
                  </td>
                </tr>
                ${
                  expanded
                    ? `<tr>
                        <td colspan="6" style="background:var(--mw-surface-2);padding:10px 14px;">
                          <div style="font-size:12px;">
                            <strong>识别来源：</strong>${esc(it.detail?.matched_by || '-')} ·
                            <strong>评分方案：</strong>${esc(it.detail?.rule_name || '-')} ·
                            <strong>技术规格：</strong>${esc(JSON.stringify(it.detail?.profile || {}))}
                          </div>
                          ${
                            breakdown.length
                              ? `<div class="mw-chip-group" style="margin-top:6px;">
                                  ${breakdown
                                    .map(
                                      (b) =>
                                        `<span class="mw-tag">${esc(b.dimension)}: ${esc(b.matched)} (${b.points >= 0 ? '+' : ''}${b.points})</span>`
                                    )
                                    .join('')}
                                </div>`
                              : ''
                          }
                        </td>
                      </tr>`
                    : ''
                }`;
            })
            .join('')}
        </tbody>
      </table>
    </div>
  `;
}

function bindStagingCandidatesEvents(root) {
  document.getElementById('sel-staging-verdict').onchange = async (e) => {
    state.stagingVerdictFilter = e.target.value;
    state.stagingSelected.clear();
    await loadCurrentTabData();
    render();
  };
  document.getElementById('sel-staging-state').onchange = async (e) => {
    state.stagingStateFilter = e.target.value;
    state.stagingSelected.clear();
    await loadCurrentTabData();
    render();
  };

  const chkAll = document.getElementById('chk-staging-all');
  if (chkAll) {
    chkAll.onchange = () => {
      if (chkAll.checked) {
        (state.stagingItems || []).forEach((it) => state.stagingSelected.add(it.id));
      } else {
        state.stagingSelected.clear();
      }
      render();
    };
  }
  root.querySelectorAll('.js-chk-staging').forEach((chk) => {
    chk.onchange = () => {
      const id = Number(chk.dataset.id);
      if (chk.checked) state.stagingSelected.add(id);
      else state.stagingSelected.delete(id);
      render();
    };
  });

  const runBatch = async (route) => {
    let pendingIds = Array.from(state.stagingSelected);
    if (!pendingIds.length) return showToast('请先勾选要处理的条目', 'error');
    let totalMoved = 0;
    let totalDismissed = 0;
    for (let round = 1; round <= 12 && pendingIds.length > 0; round++) {
      if (round > 1) {
        showToast(`已完成 ${totalMoved} 项，正在继续执行剩余 ${pendingIds.length} 项...`, 'info');
      } else {
        showToast(`正在执行所选 ${pendingIds.length} 项...`, 'info');
      }
      const res = await callAction('ui_staging_act', { ids: pendingIds, route });
      totalMoved += Number(res.result?.moved || 0);
      totalDismissed += Number(res.result?.dismissed || 0);
      state.stagingItems = res.items || [];
      state.boot.counts = res.counts || state.boot.counts;
      syncSidebarBadges();
      pendingIds = res.result?.remaining_ids || [];
      if (!pendingIds.length) state.stagingSelected.clear();
      render();
    }
    showToast(
      route === 'dismiss' ? `已忽略 ${totalDismissed} 条记录` : `已执行处理 ${totalMoved} 项`,
      'success'
    );
  };

  document.getElementById('btn-batch-admit').onclick = () => runBatch('');
  document.getElementById('btn-batch-force-organize').onclick = () => runBatch('organize');
  document.getElementById('btn-batch-recycle').onclick = () => runBatch('recycle');
  document.getElementById('btn-batch-dismiss').onclick = () => runBatch('dismiss');

  root.querySelectorAll('.js-act-one').forEach((btn) => {
    btn.onclick = async () => {
      const id = Number(btn.dataset.id);
      const route = btn.dataset.route;
      const res = await callAction('ui_staging_act', { ids: [id], route });
      state.stagingItems = res.items || [];
      state.boot.counts = res.counts || state.boot.counts;
      syncSidebarBadges();
      showToast('已执行分流移动', 'success');
      render();
    };
  });

  root.querySelectorAll('.js-toggle-detail').forEach((btn) => {
    btn.onclick = () => {
      const id = Number(btn.dataset.id);
      state.expandedStagingId = state.expandedStagingId === id ? null : id;
      render();
    };
  });

  root.querySelectorAll('.js-bind-tmdb').forEach((btn) => {
    btn.onclick = () => {
      const item = (state.stagingItems || []).find((x) => x.id === Number(btn.dataset.id));
      if (item) openBindTmdbModal(item);
    };
  });
}

function renderLedgerTable() {
  const items = state.ledgerItems || [];
  if (!items.length) {
    return `<div class="mw-empty">暂无洗版待删旧追踪记录。当 115 洗版胜出文件送入整理或提交 PT 洗版下载后，将在此自动追踪新源入库并安全删旧。</div>`;
  }
  return `
    <div class="mw-table-wrap">
      <table class="mw-table">
        <thead>
          <tr>
            <th>来源通道</th>
            <th>媒体作品</th>
            <th>分类与槽位</th>
            <th>得分升级</th>
            <th>状态与宿主回执</th>
            <th>更新时间</th>
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          ${items
            .map((it) => {
              const se =
                it.media_type === 'tv' && it.episode > 0
                  ? ` S${String(it.season || 1).padStart(2, '0')}E${String(it.episode).padStart(2, '0')}`
                  : '';
              return `
                <tr>
                  <td>${it.source_kind === 'pt' ? '<span class="mw-tag primary">PT 下载</span>' : '<span class="mw-tag ok">115 中转</span>'}</td>
                  <td><strong>${esc(it.title)}${esc(se)}</strong> <span class="mw-muted">(TMDB:${esc(it.tmdb_id)})</span></td>
                  <td>${esc(it.category || '宿主决定')} · <span class="mw-tag">${esc(it.slot || '默认')}</span></td>
                  <td>${Number(it.old_score || 0).toFixed(2)} → <strong>${Number(it.cand_score || 0).toFixed(2)}</strong></td>
                  <td>
                    <div>${ledgerBadge(it.state)}</div>
                    <div class="mw-muted" style="font-size:11.5px;margin-top:2px;">${esc(it.message)}</div>
                  </td>
                  <td class="mw-muted">${esc(it.updated_at)}</td>
                  <td>
                    <div class="mw-actions">
                      ${
                        it.state === 'downloaded'
                          ? `<button type="button" class="mw-btn sm primary js-ledger-act" data-id="${it.id}" data-op="organize">确认整理</button>`
                          : ''
                      }
                      ${
                        it.state === 'ready_to_clean' || it.state === 'review'
                          ? `<button type="button" class="mw-btn sm danger js-ledger-act" data-id="${it.id}" data-op="clean_old">安全删旧</button>`
                          : ''
                      }
                      ${
                        it.state !== 'cleaned' && it.state !== 'dismissed'
                          ? `<button type="button" class="mw-btn sm ghost js-ledger-act" data-id="${it.id}" data-op="dismiss">忽略</button>`
                          : ''
                      }
                    </div>
                  </td>
                </tr>`;
            })
            .join('')}
        </tbody>
      </table>
    </div>
  `;
}

function bindLedgerEvents(root) {
  document.getElementById('sel-ledger-state').onchange = async (e) => {
    state.ledgerStateFilter = e.target.value;
    await loadCurrentTabData();
    render();
  };
  document.getElementById('btn-reconcile-ledger').onclick = async () => {
    const res = await callAction('ui_ledger_reconcile', { state: state.ledgerStateFilter });
    state.ledgerItems = res.items || [];
    state.boot.counts = res.counts || state.boot.counts;
    syncSidebarBadges();
    showToast(
      `核验完成：新入库就绪 ${res.reconcile?.verified_ready || 0} 项，自动清理 ${res.reconcile?.auto_cleaned || 0} 个旧源`,
      'success'
    );
    render();
  };
  root.querySelectorAll('.js-ledger-act').forEach((btn) => {
    btn.onclick = async () => {
      const id = Number(btn.dataset.id);
      const operation = btn.dataset.op;
      const res = await callAction('ui_ledger_act', {
        id,
        operation,
        state: state.ledgerStateFilter,
      });
      state.ledgerItems = res.items || [];
      state.boot.counts = res.counts || state.boot.counts;
      syncSidebarBadges();
      showToast(res.result?.message || '操作已完成', 'success');
      render();
    };
  });
}

// ============================================================================
// TAB 2: 存量库精洗 (Unified Strategy + Resumable Scan + Safe Bulk Dedup)
// ============================================================================
function renderLibraryTab(root) {
  const b = state.boot;
  const st = b.strategy || {};
  const rules = b.rules || [];
  const cats = b.host_sync?.categories || [];
  const jobs = b.scan_jobs || {};
  const page = state.libPage || { total: 0, items: [] };

  const renderJobCard = (scope, label) => {
    const j = jobs[scope] || { mode: 'idle', scanned: 0, total: 0 };
    const pct = j.total > 0 ? Math.min(100, ((j.scanned / j.total) * 100).toFixed(1)) : 0;
    return `
      <div class="mw-card" style="margin-bottom:0;">
        <div class="mw-card-head">
          <div>
            <h3 class="mw-card-title">${label}断点扫描</h3>
            <div class="mw-card-sub">
              状态：<strong>${esc(j.mode)}</strong> · 已扫 ${j.scanned || 0} / ${j.total || 0} (${pct}%) · 多源 ${j.multi_source_items || 0}
            </div>
          </div>
          <div class="mw-actions">
            <button type="button" class="mw-btn sm primary js-lib-ctrl" data-scope="${scope}" data-op="start">重新全扫</button>
            ${
              j.mode === 'active'
                ? `<button type="button" class="mw-btn sm js-lib-ctrl" data-scope="${scope}" data-op="pause">暂停</button>`
                : `<button type="button" class="mw-btn sm js-lib-ctrl" data-scope="${scope}" data-op="resume">继续后台扫</button>`
            }
            <button type="button" class="mw-btn sm js-lib-step" data-scope="${scope}">推进一批</button>
          </div>
        </div>
        <div class="mw-progress"><div style="width:${pct}%"></div></div>
        <div style="display:flex;justify-content:space-between;font-size:12px;" class="mw-muted">
          <span>拟清理 ${j.suggestion_groups || 0} 组（${j.suggestion_sources || 0} 个重复源） · 待复核 ${j.review_groups || 0} 组</span>
          <span>${esc(j.warning || j.updated_at || '')}</span>
        </div>
      </div>
    `;
  };

  // Build unique 2-level branch prefixes for quick filtering
  const branchPrefixes = Array.from(
    new Set(
      cats.map((c) => {
        const ps = c.path.split('/');
        return ps.length >= 2 ? `${ps[0]}/${ps[1]}` : c.path;
      })
    )
  ).sort();

  root.innerHTML = `
    <div class="mw-header">
      <div>
        <h2>存量媒体库多版本精洗（沿用 Mediary 二级分类边界）</h2>
        <p>仅持久化有淘汰建议或异常的多版本条目（零数据库膨胀），严格在相同 Mediary 二级分类与相同版本槽位内择优，清理前实时二次复核</p>
      </div>
      <div class="mw-actions">
        <button type="button" class="mw-btn" id="btn-toggle-overrides">
          ${state.showOverrides ? '收起分类独立覆盖' : `按二级分类覆盖 (${(st.overrides || []).length})`}
        </button>
        <button type="button" class="mw-btn primary" id="btn-save-strategy">保存统一策略</button>
      </div>
    </div>

    <div class="mw-card">
      <div class="mw-grid-3">
        <div class="mw-field">
          <label>电影默认评分方案（中转 & 存量共用）</label>
          <select class="mw-select" id="st-movie-rule">
            ${rules
              .map(
                (r) =>
                  `<option value="${esc(r.id)}" ${r.id === st.movie_rule_id ? 'selected' : ''}>[${r.kind === 'symedia' ? 'Symedia 100分' : 'MediaTidy 1000分'}] ${esc(r.name)}</option>`
              )
              .join('')}
          </select>
        </div>
        <div class="mw-field">
          <label>剧集默认评分方案（中转 & 存量共用）</label>
          <select class="mw-select" id="st-tv-rule">
            ${rules
              .map(
                (r) =>
                  `<option value="${esc(r.id)}" ${r.id === st.tv_rule_id ? 'selected' : ''}>[${r.kind === 'symedia' ? 'Symedia 100分' : 'MediaTidy 1000分'}] ${esc(r.name)}</option>`
              )
              .join('')}
          </select>
        </div>
        <div class="mw-field">
          <label>同槽最低洗版分差 & 每槽保留数</label>
          <div style="display:flex;gap:6px;align-items:center;">
            <input type="number" step="0.5" class="mw-input" id="st-diff-100" value="${st.min_diff_100 ?? 2}" title="Symedia 100分制最低分差" placeholder="100分制分差" />
            <input type="number" step="1" class="mw-input" id="st-diff-1000" value="${st.min_diff_1000 ?? 15}" title="MediaTidy 1000分制最低分差" placeholder="1000分制分差" />
            <input type="number" min="1" max="10" class="mw-input" id="st-keep-slot" value="${st.keep_per_slot ?? 1}" title="每槽保留版本数" style="width:74px;" />
          </div>
        </div>
      </div>
      <div style="margin-top:10px;display:flex;flex-wrap:wrap;gap:16px;align-items:center;">
        <label class="mw-check">
          <input type="checkbox" id="st-protect-iso" ${st.protect_iso_zones !== false ? 'checked' : ''} />
          <span>自动保护 Mediary「蓝光原盘 / ISO 技术专区」分类下的 <code>.iso</code> 原盘不被负分淘汰</span>
        </label>
      </div>

      ${
        state.showOverrides
          ? `<div style="margin-top:14px;padding-top:12px;border-top:1px solid var(--mw-border);">
              <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;">
                <strong>按 Mediary 二级分类路径覆盖策略（子目录前缀自动继承）</strong>
                <button type="button" class="mw-btn sm" id="btn-add-override">+ 新增分类覆盖</button>
              </div>
              ${
                !(st.overrides || []).length
                  ? `<div class="mw-muted" style="font-size:12px;">暂无特殊覆盖，所有分类默认使用上方统一策略。</div>`
                  : `<table class="mw-table">
                      <thead><tr><th>类型</th><th>Mediary 分类前缀</th><th>指定评分方案</th><th>独立分差</th><th>跳过存量洗版</th><th>跳过 PT 洗版</th><th></th></tr></thead>
                      <tbody>
                        ${(st.overrides || [])
                          .map(
                            (ov, idx) => `
                          <tr>
                            <td>${ov.media_type === 'tv' ? '剧集' : ov.media_type === 'movie' ? '电影' : '全部'}</td>
                            <td class="mw-mono">${esc(ov.category_prefix)}</td>
                            <td>${esc(rules.find((r) => r.id === ov.rule_id)?.name || '跟随默认')}</td>
                            <td>${ov.min_diff ?? '默认'}</td>
                            <td>${ov.skip_library_wash ? '是' : '否'}</td>
                            <td>${ov.skip_pt_wash ? '是' : '否'}</td>
                            <td><button type="button" class="mw-btn sm danger js-del-ov" data-idx="${idx}">移除</button></td>
                          </tr>`
                          )
                          .join('')}
                      </tbody>
                    </table>`
              }
            </div>`
          : ''
      }
    </div>

    <div class="mw-grid-2" style="margin-bottom:14px;">
      ${renderJobCard('movie', '电影媒体库')}
      ${renderJobCard('tv', '剧集媒体库')}
    </div>

    <div class="mw-card">
      <div class="mw-card-head">
        <div class="mw-actions">
          <div class="mw-pills">
            <button type="button" class="mw-pill ${state.libScope === 'movie' ? 'active' : ''}" data-lscope="movie">电影</button>
            <button type="button" class="mw-pill ${state.libScope === 'tv' ? 'active' : ''}" data-lscope="tv">剧集</button>
            <button type="button" class="mw-pill ${state.libScope === 'all' ? 'active' : ''}" data-lscope="all">全部</button>
          </div>
          <select class="mw-select" id="sel-lib-status" style="width:auto;">
            <option value="suggestion" ${state.libStatus === 'suggestion' ? 'selected' : ''}>拟清理建议</option>
            <option value="review" ${state.libStatus === 'review' ? 'selected' : ''}>异常待复核</option>
          </select>
          <select class="mw-select" id="sel-lib-state" style="width:auto;">
            <option value="pending" ${state.libState === 'pending' ? 'selected' : ''}>待处理</option>
            <option value="approved" ${state.libState === 'approved' ? 'selected' : ''}>后台排队清理中</option>
            <option value="cleaned" ${state.libState === 'cleaned' ? 'selected' : ''}>已清理</option>
            <option value="dismissed" ${state.libState === 'dismissed' ? 'selected' : ''}>已忽略</option>
          </select>
          <select class="mw-select" id="sel-lib-cat" style="width:auto;">
            <option value="">全部二级分类</option>
            ${branchPrefixes
              .map(
                (bp) =>
                  `<option value="${esc(bp)}" ${state.libCategory === bp ? 'selected' : ''}>${esc(bp)}</option>`
              )
              .join('')}
          </select>
        </div>

        <div class="mw-actions">
          <button type="button" class="mw-btn sm danger" id="btn-lib-clean-selected">清理勾选 (${state.libSelected.size})</button>
          <button type="button" class="mw-btn sm primary" id="btn-lib-clean-all">一键批准筛选全部 (${page.total || 0})</button>
          <button type="button" class="mw-btn sm ghost" id="btn-lib-dismiss-selected">忽略勾选</button>
        </div>
      </div>

      ${
        !(page.items || []).length
          ? `<div class="mw-empty">当前筛选无记录。点击上方「重新全扫」或「推进一批」即可扫描媒体库多版本条目。</div>`
          : `<div class="mw-table-wrap">
              <table class="mw-table">
                <thead>
                  <tr>
                    <th><input type="checkbox" id="chk-lib-all" /></th>
                    <th>媒体名称</th>
                    <th>二级分类 / 槽位</th>
                    <th>保留胜出版本</th>
                    <th>拟清理低分版本</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  ${page.items
                    .map((it) => {
                      const checked = state.libSelected.has(it.id) ? 'checked' : '';
                      const ret = (it.retained || [])[0] || {};
                      const cands = it.candidates || [];
                      return `
                        <tr>
                          <td><input type="checkbox" class="js-chk-lib" data-id="${it.id}" ${checked} /></td>
                          <td>
                            <strong>${esc(it.title)}</strong>
                            <div class="mw-muted" style="font-size:11.5px;">ID: ${esc(it.item_id)}</div>
                          </td>
                          <td>
                            <div>${esc(it.category)}</div>
                            <span class="mw-tag primary">${esc(it.slot)}</span>
                          </td>
                          <td>
                            <span class="mw-tag ok">${Number(ret.score || it.winner_score || 0).toFixed(2)} 分</span>
                            <div style="font-size:12px;word-break:break-all;margin-top:3px;">${esc(ret.name || ret.path || '-')}</div>
                          </td>
                          <td>
                            ${
                              cands.length
                                ? cands
                                    .map(
                                      (cd) => `
                                  <div style="margin-bottom:4px;">
                                    <span class="mw-tag danger">${Number(cd.score || 0).toFixed(2)} 分</span>
                                    <span style="font-size:12px;word-break:break-all;">${esc(cd.name || cd.path)}</span>
                                  </div>`
                                    )
                                    .join('')
                                : `<span class="mw-muted">${esc(it.reason)}</span>`
                            }
                          </td>
                          <td>
                            <div class="mw-actions">
                              ${
                                it.status === 'suggestion' && it.state === 'pending'
                                  ? `<button type="button" class="mw-btn sm danger js-lib-one" data-id="${it.id}" data-op="execute">清理旧源</button>`
                                  : ''
                              }
                              ${
                                it.state === 'pending'
                                  ? `<button type="button" class="mw-btn sm ghost js-lib-one" data-id="${it.id}" data-op="dismiss">忽略</button>`
                                  : `<span class="mw-tag">${esc(it.state)}</span>`
                              }
                            </div>
                          </td>
                        </tr>`;
                    })
                    .join('')}
                </tbody>
              </table>
            </div>
            ${
              page.has_more
                ? `<div style="margin-top:12px;text-align:center;">
                    <button type="button" class="mw-btn sm" id="btn-lib-next-page">加载下一页</button>
                  </div>`
                : ''
            }`
      }
    </div>
  `;

  document.getElementById('btn-toggle-overrides').onclick = () => {
    state.showOverrides = !state.showOverrides;
    render();
  };

  document.getElementById('btn-save-strategy').onclick = async () => {
    const strategy = {
      ...st,
      movie_rule_id: document.getElementById('st-movie-rule').value,
      tv_rule_id: document.getElementById('st-tv-rule').value,
      min_diff_100: Number(document.getElementById('st-diff-100').value || 2),
      min_diff_1000: Number(document.getElementById('st-diff-1000').value || 15),
      keep_per_slot: Number(document.getElementById('st-keep-slot').value || 1),
      protect_iso_zones: document.getElementById('st-protect-iso').checked,
    };
    const res = await callAction('ui_save_strategy', { strategy });
    applyBootstrap(res);
    showToast('统一洗版策略已保存', 'success');
    render();
  };

  const addOvBtn = document.getElementById('btn-add-override');
  if (addOvBtn) {
    addOvBtn.onclick = () => openAddOverrideModal();
  }
  root.querySelectorAll('.js-del-ov').forEach((btn) => {
    btn.onclick = async () => {
      const idx = Number(btn.dataset.idx);
      const nextOvs = [...(st.overrides || [])];
      nextOvs.splice(idx, 1);
      const res = await callAction('ui_save_strategy', {
        strategy: { ...st, overrides: nextOvs },
      });
      applyBootstrap(res);
      render();
    };
  });

  root.querySelectorAll('.js-lib-ctrl').forEach((btn) => {
    btn.onclick = async () => {
      showToast('正在执行存量库扫描控制...', 'info');
      const res = await callAction('ui_lib_control', {
        scope: btn.dataset.scope,
        operation: btn.dataset.op,
      });
      state.boot.scan_jobs = res.scan_jobs;
      state.boot.counts = res.counts;
      state.libScope = btn.dataset.scope;
      state.libPage = res.page;
      syncSidebarBadges();
      render();
    };
  });

  root.querySelectorAll('.js-lib-step').forEach((btn) => {
    btn.onclick = async () => {
      showToast('正在推进一批媒体库扫描...', 'info');
      const res = await callAction('ui_lib_step', { scope: btn.dataset.scope });
      state.boot.scan_jobs = res.scan_jobs;
      state.boot.counts = res.counts;
      state.libScope = btn.dataset.scope;
      state.libPage = res.page;
      syncSidebarBadges();
      render();
    };
  });

  root.querySelectorAll('.mw-pill[data-lscope]').forEach((btn) => {
    btn.onclick = async () => {
      state.libScope = btn.dataset.lscope;
      state.libSelected.clear();
      await loadCurrentTabData();
      render();
    };
  });
  document.getElementById('sel-lib-status').onchange = async (e) => {
    state.libStatus = e.target.value;
    state.libSelected.clear();
    await loadCurrentTabData();
    render();
  };
  document.getElementById('sel-lib-state').onchange = async (e) => {
    state.libState = e.target.value;
    state.libSelected.clear();
    await loadCurrentTabData();
    render();
  };
  document.getElementById('sel-lib-cat').onchange = async (e) => {
    state.libCategory = e.target.value;
    state.libSelected.clear();
    await loadCurrentTabData();
    render();
  };

  const chkAll = document.getElementById('chk-lib-all');
  if (chkAll) {
    chkAll.onchange = () => {
      if (chkAll.checked) (page.items || []).forEach((it) => state.libSelected.add(it.id));
      else state.libSelected.clear();
      render();
    };
  }
  root.querySelectorAll('.js-chk-lib').forEach((chk) => {
    chk.onchange = () => {
      const id = Number(chk.dataset.id);
      if (chk.checked) state.libSelected.add(id);
      else state.libSelected.delete(id);
      render();
    };
  });

  const runLibAct = async (payload) => {
    const res = await callAction('ui_lib_act', {
      ...payload,
      scope: state.libScope,
      category: state.libCategory,
    });
    state.libSelected.clear();
    state.boot.scan_jobs = res.scan_jobs;
    state.boot.counts = res.counts;
    state.libPage = res.page;
    syncSidebarBadges();
    showToast(
      res.result?.deleted_sources != null
        ? `已安全删除 ${res.result.deleted_sources} 个低分重复源`
        : '操作已完成',
      'success'
    );
    render();
  };

  document.getElementById('btn-lib-clean-selected').onclick = () => {
    const ids = Array.from(state.libSelected);
    if (!ids.length) return showToast('请先勾选记录', 'error');
    runLibAct({ ids, operation: 'execute' });
  };
  document.getElementById('btn-lib-dismiss-selected').onclick = () => {
    const ids = Array.from(state.libSelected);
    if (!ids.length) return showToast('请先勾选记录', 'error');
    runLibAct({ ids, operation: 'dismiss' });
  };
  document.getElementById('btn-lib-clean-all').onclick = () => {
    if (!page.total) return showToast('当前筛选没有拟清理项', 'error');
    runLibAct({ all_scope: state.libScope, operation: 'execute' });
  };

  root.querySelectorAll('.js-lib-one').forEach((btn) => {
    btn.onclick = () =>
      runLibAct({ ids: [Number(btn.dataset.id)], operation: btn.dataset.op });
  });

  const nextBtn = document.getElementById('btn-lib-next-page');
  if (nextBtn) {
    nextBtn.onclick = async () => {
      const res = await callAction('ui_lib_decisions', {
        scope: state.libScope,
        status: state.libStatus,
        state: state.libState,
        category: state.libCategory,
        after_id: page.next_cursor || 0,
      });
      state.libPage = res.page;
      render();
    };
  }
}

// ============================================================================
// TAB 3: PT 主动洗版 (Graduation Thresholds + Site Picker + Safe Deferred Organize)
// ============================================================================
function renderPtTab(root) {
  const b = state.boot;
  const ptc = b.pt_config || {};
  const rules = b.rules || [];
  const sites = b.pt_sites || [];
  const selectedSites = new Set(ptc.site_ids || []);

  root.innerHTML = `
    <div class="mw-header">
      <div>
        <h2>PT 主动搜种洗版（暂缓整理安全闭环）</h2>
        <p>筛选未达毕业分的电影/剧集 → 经 mr/mp 识别对齐与规则打分 → 提交暂缓整理下载 → 归档确认入库后进入删旧账本清理旧源</p>
      </div>
      <div class="mw-actions">
        <button type="button" class="mw-btn primary" id="btn-save-pt">保存 PT 洗版设置</button>
      </div>
    </div>

    <div class="mw-card">
      <div class="mw-grid-3">
        <div class="mw-field">
          <label>电影 PT 洗版规则 & 毕业分数线</label>
          <div style="display:flex;gap:6px;">
            <select class="mw-select" id="pt-movie-rule">
              ${rules
                .map(
                  (r) =>
                    `<option value="${esc(r.id)}" ${r.id === ptc.movie_rule_id ? 'selected' : ''}>[${r.kind === 'symedia' ? 'Symedia 100分' : 'MediaTidy 1000分'}] ${esc(r.name)}</option>`
                )
                .join('')}
            </select>
            <input type="number" step="1" class="mw-input" id="pt-movie-grad" value="${ptc.movie_graduate ?? 75}" style="width:86px;" title="电影毕业线（100分制建议 75，1000分制建议 600）" />
          </div>
        </div>
        <div class="mw-field">
          <label>剧集 PT 洗版规则 & 毕业分数线</label>
          <div style="display:flex;gap:6px;">
            <select class="mw-select" id="pt-tv-rule">
              ${rules
                .map(
                  (r) =>
                    `<option value="${esc(r.id)}" ${r.id === ptc.tv_rule_id ? 'selected' : ''}>[${r.kind === 'symedia' ? 'Symedia 100分' : 'MediaTidy 1000分'}] ${esc(r.name)}</option>`
                )
                .join('')}
            </select>
            <input type="number" step="1" class="mw-input" id="pt-tv-grad" value="${ptc.tv_graduate ?? 65}" style="width:86px;" title="剧集毕业线（100分制建议 65，1000分制建议 520）" />
          </div>
        </div>
        <div class="mw-field">
          <label>最低分差 / 最低做种 / 每轮巡检配额</label>
          <div style="display:flex;gap:6px;">
            <input type="number" step="0.5" class="mw-input" id="pt-min-diff" value="${ptc.min_score_diff ?? 3}" title="同槽最低升级分差" />
            <input type="number" min="1" class="mw-input" id="pt-min-seed" value="${ptc.min_seeders ?? 2}" title="最低做种人数" />
            <input type="number" min="1" max="12" class="mw-input" id="pt-batch-quota" value="${ptc.batch_per_tick ?? 4}" title="每 15 分钟自动巡检条目数" />
          </div>
        </div>
      </div>

      <div style="margin-top:10px;display:flex;flex-wrap:wrap;gap:16px;align-items:center;">
        <label class="mw-check"><input type="checkbox" id="pt-enabled" ${ptc.enabled ? 'checked' : ''} /><span>开启每 15 分钟后台自动巡检搜种</span></label>
        <label class="mw-check"><input type="checkbox" id="pt-free-only" ${ptc.free_only !== false ? 'checked' : ''} /><span>仅限免费种子 (Free / 2XFree)</span></label>
        <label class="mw-check"><input type="checkbox" id="pt-skip-hr" ${ptc.skip_hr !== false ? 'checked' : ''} /><span>排除 H&R 考核种</span></label>
        <label class="mw-check"><input type="checkbox" id="pt-auto-dl" ${ptc.auto_download ? 'checked' : ''} /><span>命中后自动提交下载（否则进入下方候选队列）</span></label>
        <div style="display:inline-flex;align-items:center;gap:6px;">
          <span class="mw-muted" style="font-size:12px;">整理冲突策略：</span>
          <select class="mw-select" id="pt-conflict" style="width:auto;padding:4px 8px;">
            <option value="overwrite" ${ptc.conflict_strategy !== 'skip' ? 'selected' : ''}>覆盖同名文件 (推荐洗版)</option>
            <option value="skip" ${ptc.conflict_strategy === 'skip' ? 'selected' : ''}>同名跳过保留旧文件</option>
          </select>
        </div>
      </div>

      ${
        sites.length
          ? `<div style="margin-top:10px;">
              <div class="mw-muted" style="font-size:12px;margin-bottom:5px;">限定搜索站点（不选默认搜索 Mediary 全部已启用站点）：</div>
              <div class="mw-chip-group" id="pt-site-chips">
                ${sites
                  .map(
                    (s) =>
                      `<span class="mw-chip ${selectedSites.has(s.id) ? 'selected' : ''}" data-sid="${s.id}">${esc(s.name)} (#${s.id})</span>`
                  )
                  .join('')}
              </div>
            </div>`
          : ''
      }
    </div>

    <div class="mw-card">
      <div class="mw-card-head">
        <div>
          <h3 class="mw-card-title">手动查库与专项诊断搜种升级</h3>
          <div class="mw-card-sub">输入片名/TMDB ID，或直接按 Mediary 3.4.6+ 原生索引筛出库内「单层杜比视界 DV P5（无 HDR10 回退易偏色） / 蓝光 ISO 原盘」一键搜 PT 替换种</div>
        </div>
      </div>
      <div style="display:flex;gap:8px;flex-wrap:wrap;">
        <select class="mw-select" id="pt-lookup-type" style="width:116px;">
          <option value="all" ${state.ptLookupType === 'all' ? 'selected' : ''}>全部类型</option>
          <option value="movie" ${state.ptLookupType === 'movie' ? 'selected' : ''}>电影</option>
          <option value="tv" ${state.ptLookupType === 'tv' ? 'selected' : ''}>剧集</option>
        </select>
        <select class="mw-select" id="pt-lookup-diag" style="width:auto;max-width:290px;">
          <option value="all" ${state.ptLookupDiag === 'all' ? 'selected' : ''}>全部片源（按关键词查询）</option>
          <option value="hdr:dv_p5" ${state.ptLookupDiag === 'hdr:dv_p5' ? 'selected' : ''}>单层杜比视界 DV P5（无 HDR10 回退）</option>
          <option value="hdr:hdr10" ${state.ptLookupDiag === 'hdr:hdr10' ? 'selected' : ''}>HDR10 片源</option>
          <option value="hdr:sdr" ${state.ptLookupDiag === 'hdr:sdr' ? 'selected' : ''}>SDR 标准动态范围片源</option>
          <option value="src:iso" ${state.ptLookupDiag === 'src:iso' ? 'selected' : ''}>蓝光原盘 ISO 片源</option>
        </select>
        <input class="mw-input" id="pt-lookup-kw" value="${esc(state.ptLookupKeyword)}" placeholder="输入片名或数字 TMDB ID（已选专项片源时可留空直查）..." style="flex:1;min-width:200px;" />
        <button type="button" class="mw-btn primary" id="btn-pt-lookup">查询媒体库</button>
      </div>

      ${
        state.ptSearchBanner
          ? `<div style="margin-top:10px;padding:9px 12px;border-radius:8px;background:var(--mw-surface-2);font-size:12.5px;">
              <strong>《${esc(state.ptSearchBanner.title)}》</strong> · 当前得分 ${Number(state.ptSearchBanner.current_score || 0).toFixed(2)} / 毕业线 ${state.ptSearchBanner.graduate_score} · ${esc(state.ptSearchBanner.notice)}
            </div>`
          : ''
      }

      ${
        state.ptLookupResults.length
          ? `<div class="mw-table-wrap" style="margin-top:10px;">
              <table class="mw-table">
                <thead><tr><th>媒体库条目</th><th>二级分类 / 槽位</th><th>当前最高分 / 毕业线</th><th>操作</th></tr></thead>
                <tbody>
                  ${state.ptLookupResults
                    .map(
                      (it) => `
                    <tr>
                      <td>
                        <strong>${esc(it.name)}</strong>
                        ${it.episode > 0 ? `<span class="mw-tag">S${String(it.season || 1).padStart(2, '0')}E${String(it.episode).padStart(2, '0')}</span>` : ''}
                        ${it.dv_p5 ? `<span class="mw-tag warn">DV P5 单层</span>` : ''}
                        <span class="mw-muted"> (TMDB:${esc(it.tmdb_id)} · ${it.source_count} 源)</span>
                      </td>
                      <td>${esc(it.category || '-')} · <span class="mw-tag">${esc(it.slot)}</span></td>
                      <td>
                        <strong>${Number(it.current_score || 0).toFixed(2)}</strong> / ${it.graduate_score}
                        ${it.graduated ? '<span class="mw-tag ok">已毕业</span>' : '<span class="mw-tag warn">可升级</span>'}
                      </td>
                      <td>
                        <button type="button" class="mw-btn sm primary js-pt-search-item" data-id="${esc(it.id)}" data-name="${esc(it.name)}">搜 PT 升级种</button>
                      </td>
                    </tr>`
                    )
                    .join('')}
                </tbody>
              </table>
            </div>`
          : ''
      }
    </div>

    <div class="mw-card">
      <div class="mw-card-head">
        <h3 class="mw-card-title">PT 洗版候选队列</h3>
        <select class="mw-select" id="sel-pt-cand-state" style="width:auto;">
          <option value="pending" ${state.ptCandState === 'pending' ? 'selected' : ''}>待确认下载</option>
          <option value="submitted" ${state.ptCandState === 'submitted' ? 'selected' : ''}>已提交下载</option>
          <option value="dismissed" ${state.ptCandState === 'dismissed' ? 'selected' : ''}>已忽略</option>
        </select>
      </div>
      ${
        !(state.ptCandidates || []).length
          ? `<div class="mw-empty">当前暂无待处理的 PT 升级候选种。</div>`
          : `<div class="mw-table-wrap">
              <table class="mw-table">
                <thead><tr><th>目标媒体</th><th>PT 候选种子</th><th>站点 / 做种 / 体积</th><th>同槽分值提升</th><th>操作</th></tr></thead>
                <tbody>
                  ${state.ptCandidates
                    .map(
                      (cd) => `
                    <tr>
                      <td><strong>${esc(cd.title)}</strong><div class="mw-muted" style="font-size:11.5px;">${esc(cd.category)} · ${esc(cd.slot)}</div></td>
                      <td style="word-break:break-all;">${esc(cd.torrent_title)}</td>
                      <td>${esc(cd.site_name)} · ⬆️${cd.seeders} · ${fmtBytes(cd.size)}</td>
                      <td>${Number(cd.current_score || 0).toFixed(2)} → <strong style="color:var(--mw-success);">${Number(cd.cand_score || 0).toFixed(2)}</strong></td>
                      <td>
                        <div class="mw-actions">
                          ${
                            cd.state === 'pending'
                              ? `<button type="button" class="mw-btn sm primary js-pt-cand-act" data-id="${cd.id}" data-op="download">暂缓整理下载</button>
                                 <button type="button" class="mw-btn sm ghost js-pt-cand-act" data-id="${cd.id}" data-op="dismiss">忽略</button>`
                              : `<span class="mw-tag ok">${esc(cd.message || cd.state)}</span>`
                          }
                        </div>
                      </td>
                    </tr>`
                    )
                    .join('')}
                </tbody>
              </table>
            </div>`
      }
    </div>
  `;

  root.querySelectorAll('#pt-site-chips .mw-chip').forEach((chip) => {
    chip.onclick = () => chip.classList.toggle('selected');
  });

  const adaptPtScaleInput = (ruleSelectId, gradInputId, defaultSym, defaultTidy) => {
    const sel = document.getElementById(ruleSelectId);
    const inp = document.getElementById(gradInputId);
    const diffInp = document.getElementById('pt-min-diff');
    if (!sel || !inp) return;
    sel.onchange = () => {
      const picked = rules.find((r) => r.id === sel.value);
      if (!picked) return;
      const cur = Number(inp.value || 0);
      if (picked.kind === 'mediatidy' && cur > 0 && cur <= 120) {
        inp.value = defaultTidy;
        if (diffInp && Number(diffInp.value || 0) < 8) diffInp.value = 15;
      } else if (picked.kind === 'symedia' && cur > 150) {
        inp.value = defaultSym;
        if (diffInp && Number(diffInp.value || 0) >= 10) diffInp.value = 3;
      }
    };
  };
  adaptPtScaleInput('pt-movie-rule', 'pt-movie-grad', 75, 600);
  adaptPtScaleInput('pt-tv-rule', 'pt-tv-grad', 65, 520);

  document.getElementById('btn-save-pt').onclick = async () => {
    const pickedSites = Array.from(root.querySelectorAll('#pt-site-chips .mw-chip.selected')).map(
      (el) => Number(el.dataset.sid)
    );
    const pt_config = {
      enabled: document.getElementById('pt-enabled').checked,
      movie_rule_id: document.getElementById('pt-movie-rule').value,
      tv_rule_id: document.getElementById('pt-tv-rule').value,
      movie_graduate: Number(document.getElementById('pt-movie-grad').value || 75),
      tv_graduate: Number(document.getElementById('pt-tv-grad').value || 65),
      min_score_diff: Number(document.getElementById('pt-min-diff').value || 3),
      min_seeders: Number(document.getElementById('pt-min-seed').value || 2),
      batch_per_tick: Number(document.getElementById('pt-batch-quota').value || 4),
      free_only: document.getElementById('pt-free-only').checked,
      skip_hr: document.getElementById('pt-skip-hr').checked,
      auto_download: document.getElementById('pt-auto-dl').checked,
      conflict_strategy: document.getElementById('pt-conflict').value,
      site_ids: pickedSites,
    };
    const res = await callAction('ui_save_pt_config', { pt_config });
    applyBootstrap(res);
    showToast('PT 主动洗版配置已保存', 'success');
    render();
  };

  const doLookup = async () => {
    state.ptLookupKeyword = document.getElementById('pt-lookup-kw').value.trim();
    state.ptLookupType = document.getElementById('pt-lookup-type').value;
    state.ptLookupDiag = document.getElementById('pt-lookup-diag').value;
    if (!state.ptLookupKeyword && state.ptLookupDiag === 'all') {
      return showToast('请输入查询片名/TMDB ID，或选择一项专项片源筛选', 'error');
    }
    const hdr_format = state.ptLookupDiag.startsWith('hdr:') ? state.ptLookupDiag.slice(4) : '';
    const source_kind = state.ptLookupDiag.startsWith('src:') ? state.ptLookupDiag.slice(4) : '';
    const res = await callAction('ui_pt_lookup', {
      keyword: state.ptLookupKeyword,
      media_type: state.ptLookupType,
      hdr_format,
      source_kind,
    });
    state.ptLookupResults = res.items || [];
    if (!state.ptLookupResults.length) showToast('媒体库中未找到匹配条目', 'info');
    render();
  };
  document.getElementById('btn-pt-lookup').onclick = doLookup;
  document.getElementById('pt-lookup-diag').onchange = () => {
    const val = document.getElementById('pt-lookup-diag').value;
    if (val !== 'all' || document.getElementById('pt-lookup-kw').value.trim()) doLookup();
  };
  document.getElementById('pt-lookup-kw').onkeydown = (e) => {
    if (e.key === 'Enter') doLookup();
  };

  root.querySelectorAll('.js-pt-search-item').forEach((btn) => {
    btn.onclick = async () => {
      showToast(`正在检索《${btn.dataset.name}》的免费 PT 升级种子...`, 'info');
      const res = await callAction('ui_pt_search', { item_id: btn.dataset.id, keyword: '' });
      state.ptSearchBanner = res.search;
      state.ptCandidates = res.candidates || [];
      state.boot.counts = res.counts || state.boot.counts;
      syncSidebarBadges();
      showToast(res.search?.notice || '搜种完成', 'success');
      render();
    };
  });

  document.getElementById('sel-pt-cand-state').onchange = async (e) => {
    state.ptCandState = e.target.value;
    await loadCurrentTabData();
    render();
  };

  root.querySelectorAll('.js-pt-cand-act').forEach((btn) => {
    btn.onclick = async () => {
      const res = await callAction('ui_pt_act', {
        id: Number(btn.dataset.id),
        operation: btn.dataset.op,
      });
      state.ptCandidates = res.candidates || [];
      state.boot.counts = res.counts || state.boot.counts;
      syncSidebarBadges();
      showToast(
        btn.dataset.op === 'download'
          ? `已提交暂缓整理下载 #${res.result?.download_id}，可在删旧账本查看进度`
          : '已忽略该候选',
        'success'
      );
      render();
    };
  });
}

// ============================================================================
// TAB 4: 评分与订阅 (Symedia Visual Editor + MediaTidy 27-Slot + Subs + Tester)
// ============================================================================
function renderRulesTab(root) {
  root.innerHTML = `
    <div class="mw-header">
      <div>
        <h2>双评分引擎配置、远程订阅与对比试算</h2>
        <p>原生兼容 Symedia 100 分权重 JSON 与 MediaTidy 1000 分 27 槽 YAML 双向导入导出，支持在线可视化调参、远程自动订阅及本地识别覆盖</p>
      </div>
      <div class="mw-pills">
        <button type="button" class="mw-pill ${state.ruleSubTab === 'symedia' ? 'active' : ''}" data-rsub="symedia">Symedia 100分编辑器</button>
        <button type="button" class="mw-pill ${state.ruleSubTab === 'mediatidy' ? 'active' : ''}" data-rsub="mediatidy">MediaTidy 27槽编辑器</button>
        <button type="button" class="mw-pill ${state.ruleSubTab === 'subs' ? 'active' : ''}" data-rsub="subs">远程订阅与识别覆盖</button>
        <button type="button" class="mw-pill ${state.ruleSubTab === 'tester' ? 'active' : ''}" data-rsub="tester">双样本对比试算</button>
      </div>
    </div>
    <div id="mw-rules-subview"></div>
  `;

  root.querySelectorAll('.mw-pill[data-rsub]').forEach((btn) => {
    btn.onclick = async () => {
      state.ruleSubTab = btn.dataset.rsub;
      state.activeRuleDetail = null;
      await loadCurrentTabData();
      render();
    };
  });

  const sub = document.getElementById('mw-rules-subview');
  if (state.ruleSubTab === 'symedia') renderSymediaEditor(sub);
  else if (state.ruleSubTab === 'mediatidy') renderMediaTidyEditor(sub);
  else if (state.ruleSubTab === 'subs') renderSubscriptionsView(sub);
  else renderSampleTesterView(sub);
}

function estimateSymediaMax(body) {
  if (!body) return 0;
  let sum = 0;
  for (const d of SYM_DIMS) {
    if (!d.wKey) continue;
    const w = Number(body[d.wKey] || 0);
    if (w <= 0) continue;
    if (d.kind === 'scalar') {
      sum += Number(body.file_size_score || 0) * w;
    } else if (d.kind === 'list') {
      const arr = Array.isArray(body[d.key]) ? body[d.key] : [];
      const best = arr.reduce((m, it) => Math.max(m, Number(it.score || 0)), 0);
      sum += best * w;
    } else if (d.kind === 'map') {
      const grp = body[d.key] || {};
      let best = 0;
      for (const [k, v] of Object.entries(grp)) {
        if (k === 'blacklist' || k === 'whitelist' || !v || typeof v !== 'object') continue;
        best = Math.max(best, Number(v.score || 0));
      }
      sum += best * w;
    }
  }
  return sum;
}

function renderSymediaEditor(container) {
  const symRules = (state.boot.rules || []).filter((r) => r.kind === 'symedia');
  const rule = state.activeRuleDetail;
  if (!rule || rule.kind !== 'symedia') {
    container.innerHTML = `<div class="mw-empty">正在加载 Symedia 评分规则...</div>`;
    return;
  }
  const body = rule.body || {};
  const maxEst = estimateSymediaMax(body);
  const activeDim = SYM_DIMS.find((d) => d.key === state.symActiveDim) || SYM_DIMS[0];

  container.innerHTML = `
    <div class="mw-card">
      <div class="mw-card-head">
        <div class="mw-actions" style="flex:1;">
          <select class="mw-select" id="sel-sym-rule" style="max-width:320px;">
            ${symRules
              .map(
                (r) =>
                  `<option value="${esc(r.id)}" ${r.id === rule.id ? 'selected' : ''}>${esc(r.name)}${r.builtin ? ' (预设)' : ''}</option>`
              )
              .join('')}
          </select>
          <input class="mw-input" id="inp-sym-name" value="${esc(rule.name)}" style="max-width:260px;" placeholder="规则名称" />
          <span class="mw-tag primary">理论最高分 ≈ ${maxEst.toFixed(2)}</span>
        </div>
        <div class="mw-actions">
          <button type="button" class="mw-btn sm primary" id="btn-sym-save">保存修改</button>
          <button type="button" class="mw-btn sm" id="btn-sym-clone">另存新规则</button>
          <button type="button" class="mw-btn sm" id="btn-sym-raw">JSON 原文</button>
          <button type="button" class="mw-btn sm" id="btn-sym-import">导入 .json</button>
          <button type="button" class="mw-btn sm" id="btn-sym-export">导出 .json</button>
          ${!rule.builtin ? `<button type="button" class="mw-btn sm danger" id="btn-sym-del">删除</button>` : ''}
        </div>
      </div>

      <div style="margin-bottom:12px;display:flex;gap:18px;flex-wrap:wrap;">
        <label class="mw-check">
          <input type="checkbox" id="chk-sym-src-meta" ${body.source_file_extract_metadata !== false ? 'checked' : ''} />
          <span>源文件实测提取 MediaInfo (<code>source_file_extract_metadata</code>)</span>
        </label>
        <label class="mw-check">
          <input type="checkbox" id="chk-sym-dst-meta" ${body.dest_file_extract_metadata !== false ? 'checked' : ''} />
          <span>目标文件实测提取 MediaInfo (<code>dest_file_extract_metadata</code>)</span>
        </label>
      </div>

      <div class="mw-rule-split">
        <div class="mw-dim-list">
          ${SYM_DIMS.map((d) => {
            const wVal = d.wKey ? Number(body[d.wKey] ?? 0) : null;
            return `
              <div class="mw-dim-item ${d.key === activeDim.key ? 'active' : ''}" data-dim="${d.key}">
                <div class="mw-dim-top">
                  <span>${esc(d.label)}</span>
                  ${wVal !== null ? `<span class="mw-mono">×${wVal}</span>` : `<span class="mw-tag">${body.version_control_enabled ? '开启' : '关闭'}</span>`}
                </div>
                ${
                  wVal !== null
                    ? `<div class="mw-slider-row">
                        <input type="range" min="0" max="5" step="0.05" value="${Math.min(5, wVal)}" class="js-sym-w-range" data-wkey="${d.wKey}" />
                        <input type="number" step="0.01" value="${wVal}" class="js-sym-w-num" data-wkey="${d.wKey}" />
                      </div>`
                    : ''
                }
              </div>`;
          }).join('')}
        </div>

        <div id="sym-dim-detail" class="mw-card" style="margin-bottom:0;">
          ${renderSymDimensionDetail(body, activeDim)}
        </div>
      </div>
    </div>
  `;

  document.getElementById('sel-sym-rule').onchange = async (e) => {
    state.selectedSymRuleId = e.target.value;
    state.activeRuleDetail = await callAction('ui_rule_detail', { id: state.selectedSymRuleId });
    render();
  };
  document.getElementById('inp-sym-name').oninput = (e) => {
    rule.name = e.target.value;
  };
  document.getElementById('chk-sym-src-meta').onchange = (e) => {
    body.source_file_extract_metadata = e.target.checked;
  };
  document.getElementById('chk-sym-dst-meta').onchange = (e) => {
    body.dest_file_extract_metadata = e.target.checked;
  };

  container.querySelectorAll('.mw-dim-item').forEach((el) => {
    el.onclick = (ev) => {
      if (ev.target.tagName === 'INPUT') return;
      state.symActiveDim = el.dataset.dim;
      render();
    };
  });

  const bindWeightSync = (selector) => {
    container.querySelectorAll(selector).forEach((inp) => {
      inp.oninput = () => {
        body[inp.dataset.wkey] = Number(inp.value || 0);
        render();
      };
    });
  };
  bindWeightSync('.js-sym-w-range');
  bindWeightSync('.js-sym-w-num');

  bindSymDimensionDetailEvents(container, body, activeDim);

  const saveSym = async (saveAsNew) => {
    const name = document.getElementById('inp-sym-name').value.trim();
    const res = await callAction('ui_save_rule', {
      id: rule.id,
      save_as_new: saveAsNew,
      kind: 'symedia',
      name: saveAsNew ? `${name} 副本` : name,
      body,
    });
    applyBootstrap(res.bootstrap);
    state.selectedSymRuleId = res.saved_rule.id;
    state.activeRuleDetail = res.saved_rule;
    showToast(saveAsNew ? '已另存为新 Symedia 规则' : 'Symedia 规则已保存', 'success');
    render();
  };

  document.getElementById('btn-sym-save').onclick = () => saveSym(false);
  document.getElementById('btn-sym-clone').onclick = () => saveSym(true);
  document.getElementById('btn-sym-raw').onclick = () => openRawRuleModal(rule);
  document.getElementById('btn-sym-import').onclick = () => {
    pickLocalFile('.json', async (_fname, content) => {
      const res = await callAction('ui_import_rule', { kind: 'symedia', content });
      applyBootstrap(res.bootstrap);
      state.selectedSymRuleId = res.imported_id;
      state.activeRuleDetail = await callAction('ui_rule_detail', { id: res.imported_id });
      showToast('Symedia JSON 导入成功', 'success');
      render();
    });
  };
  document.getElementById('btn-sym-export').onclick = async () => {
    const exp = await callAction('ui_export_rule', { id: rule.id });
    downloadTextFile(exp.filename, exp.content, 'application/json;charset=utf-8');
  };
  const delBtn = document.getElementById('btn-sym-del');
  if (delBtn) {
    delBtn.onclick = async () => {
      const res = await callAction('ui_delete_rule', { id: rule.id });
      state.activeRuleDetail = null;
      applyBootstrap(res);
      await loadCurrentTabData();
      showToast('已删除自定义规则', 'success');
      render();
    };
  }
}

function renderSymDimensionDetail(body, dim) {
  if (dim.kind === 'scalar') {
    const bl = (body.file_size_blacklist || []).join(', ');
    const wl = (body.file_size_whitelist || []).join(', ');
    return `
      <h3 class="mw-card-title">文件体积对比加分</h3>
      <p class="mw-card-sub">同槽对比时，体积更大的一方获得 <code>基础分 × 权重</code> 加分</p>
      <div class="mw-grid-2" style="margin-top:10px;">
        <div class="mw-field">
          <label>体积胜出基础分 (file_size_score)</label>
          <input type="number" step="0.5" class="mw-input" id="sym-size-score" value="${body.file_size_score ?? 8}" />
        </div>
        <div class="mw-field">
          <label>分类/路径黑名单正则（逗号分隔）</label>
          <input class="mw-input" id="sym-size-bl" value="${esc(bl)}" placeholder="例如: 短剧" />
        </div>
        <div class="mw-field">
          <label>分类/路径白名单正则（逗号分隔，留空不限）</label>
          <input class="mw-input" id="sym-size-wl" value="${esc(wl)}" />
        </div>
      </div>
    `;
  }

  if (dim.kind === 'slots') {
    const slots = body.version_control_versions || [];
    return `
      <div class="mw-card-head">
        <div>
          <h3 class="mw-card-title">Symedia 多版本槽位控制</h3>
          <div class="mw-card-sub">按顺序匹配第一个满足包含/排除条件的槽位（如 DV / HDR / 非DV非HDR），仅同槽版本互相替换</div>
        </div>
        <button type="button" class="mw-btn sm" id="btn-sym-add-slot">+ 新增槽位</button>
      </div>
      <div style="display:flex;gap:18px;margin-bottom:10px;flex-wrap:wrap;">
        <label class="mw-check"><input type="checkbox" id="sym-vc-en" ${body.version_control_enabled ? 'checked' : ''} /><span>启用多版本槽位隔离</span></label>
        <label class="mw-check"><input type="checkbox" id="sym-vc-del" ${body.version_control_delete_invalid_dest_files ? 'checked' : ''} /><span>允许替换未命中任何槽位的旧文件</span></label>
      </div>
      <table class="mw-table">
        <thead><tr><th>槽位名称</th><th>包含条件 (attribute:v1,v2)</th><th>排除条件</th><th>文件名正则</th><th></th></tr></thead>
        <tbody>
          ${slots
            .map((s, idx) => {
              const fmtConds = (arr) =>
                (arr || [])
                  .map((c) => `${c.attribute}${(c.values || []).length ? ':' + c.values.join(',') : ''}`)
                  .join('; ');
              return `
                <tr>
                  <td><input class="mw-input js-slot-name" data-idx="${idx}" value="${esc(s.name)}" /></td>
                  <td><input class="mw-input mw-mono js-slot-inc" data-idx="${idx}" value="${esc(fmtConds(s.include_conditions))}" placeholder="dolby_vision" /></td>
                  <td><input class="mw-input mw-mono js-slot-exc" data-idx="${idx}" value="${esc(fmtConds(s.exclude_conditions))}" placeholder="dynamic_range:hdr10_plus,hdr10,hdr" /></td>
                  <td><input class="mw-input mw-mono js-slot-rx" data-idx="${idx}" value="${esc(s.file_name_regex || '')}" /></td>
                  <td><button type="button" class="mw-btn sm danger js-slot-del" data-idx="${idx}">✕</button></td>
                </tr>`;
            })
            .join('')}
        </tbody>
      </table>
    `;
  }

  // `map` or `list` dimension
  const isList = dim.kind === 'list';
  const grp = isList ? null : body[dim.key] || {};
  const bl = (isList ? body[`${dim.key}_blacklist`] : grp.blacklist) || [];
  const wl = (isList ? body[`${dim.key}_whitelist`] : grp.whitelist) || [];
  const rows = isList
    ? (body[dim.key] || []).map((v, i) => ({ k: String(i), ...v }))
    : Object.entries(grp)
        .filter(([k, v]) => k !== 'blacklist' && k !== 'whitelist' && v && typeof v === 'object')
        .map(([k, v]) => ({ k, ...v }));

  return `
    <div class="mw-card-head">
      <div>
        <h3 class="mw-card-title">${esc(dim.label)} · 匹配词条与分值（取最高命中项 × 权重）</h3>
        <div class="mw-card-sub">支持大小写不敏感正则匹配；黑名单命中的分类路径（如 <code>儿童动漫</code> 对应 <code>动漫/儿童</code>）自动按 0 分跳过</div>
      </div>
      <button type="button" class="mw-btn sm" id="btn-sym-add-item">+ 添加词条</button>
    </div>

    <div class="mw-grid-2" style="margin-bottom:10px;">
      <div class="mw-field">
        <label>维度路径黑名单（逗号分隔，命中则该维度计 0 分）</label>
        <input class="mw-input" id="sym-dim-bl" value="${esc(bl.join(', '))}" placeholder="例如: 儿童动漫, 短剧" />
      </div>
      <div class="mw-field">
        <label>维度路径白名单（逗号分隔，留空表示全部生效）</label>
        <input class="mw-input" id="sym-dim-wl" value="${esc(wl.join(', '))}" />
      </div>
    </div>

    <div class="mw-table-wrap" style="max-height:420px;overflow-y:auto;">
      <table class="mw-table">
        <thead><tr><th>显示名称</th><th>正则 Pattern</th><th style="width:92px;">基础分</th><th>备注说明</th><th></th></tr></thead>
        <tbody>
          ${rows
            .map(
              (r, idx) => `
            <tr>
              <td><input class="mw-input js-item-name" data-k="${esc(r.k)}" data-idx="${idx}" value="${esc(r.name || r.k)}" /></td>
              <td><input class="mw-input mw-mono js-item-pat" data-k="${esc(r.k)}" data-idx="${idx}" value="${esc(r.pattern || '')}" /></td>
              <td><input type="number" step="0.5" class="mw-input js-item-score" data-k="${esc(r.k)}" data-idx="${idx}" value="${r.score ?? 0}" /></td>
              <td><input class="mw-input js-item-desc" data-k="${esc(r.k)}" data-idx="${idx}" value="${esc(r.description || '')}" /></td>
              <td><button type="button" class="mw-btn sm danger js-item-del" data-k="${esc(r.k)}" data-idx="${idx}">✕</button></td>
            </tr>`
            )
            .join('')}
        </tbody>
      </table>
    </div>
  `;
}

function parseSlotCondStr(str) {
  return str
    .split(';')
    .map((s) => s.trim())
    .filter(Boolean)
    .map((part) => {
      const [attr, vals] = part.split(':');
      return {
        attribute: (attr || '').trim(),
        values: vals
          ? vals
              .split(',')
              .map((v) => v.trim())
              .filter(Boolean)
          : [],
      };
    })
    .filter((c) => c.attribute);
}

function bindSymDimensionDetailEvents(container, body, dim) {
  const splitCsv = (s) =>
    s
      .split(',')
      .map((x) => x.trim())
      .filter(Boolean);

  if (dim.kind === 'scalar') {
    document.getElementById('sym-size-score').oninput = (e) => {
      body.file_size_score = Number(e.target.value || 0);
    };
    document.getElementById('sym-size-bl').oninput = (e) => {
      body.file_size_blacklist = splitCsv(e.target.value);
    };
    document.getElementById('sym-size-wl').oninput = (e) => {
      body.file_size_whitelist = splitCsv(e.target.value);
    };
    return;
  }

  if (dim.kind === 'slots') {
    document.getElementById('sym-vc-en').onchange = (e) => {
      body.version_control_enabled = e.target.checked;
      render();
    };
    document.getElementById('sym-vc-del').onchange = (e) => {
      body.version_control_delete_invalid_dest_files = e.target.checked;
    };
    document.getElementById('btn-sym-add-slot').onclick = () => {
      body.version_control_versions = body.version_control_versions || [];
      body.version_control_versions.push({
        name: `新槽位${body.version_control_versions.length + 1}`,
        include_conditions: [],
        exclude_conditions: [],
        file_name_regex: '',
      });
      render();
    };
    container.querySelectorAll('.js-slot-name').forEach((inp) => {
      inp.oninput = () => {
        body.version_control_versions[Number(inp.dataset.idx)].name = inp.value;
      };
    });
    container.querySelectorAll('.js-slot-inc').forEach((inp) => {
      inp.oninput = () => {
        body.version_control_versions[Number(inp.dataset.idx)].include_conditions = parseSlotCondStr(inp.value);
      };
    });
    container.querySelectorAll('.js-slot-exc').forEach((inp) => {
      inp.oninput = () => {
        body.version_control_versions[Number(inp.dataset.idx)].exclude_conditions = parseSlotCondStr(inp.value);
      };
    });
    container.querySelectorAll('.js-slot-rx').forEach((inp) => {
      inp.oninput = () => {
        body.version_control_versions[Number(inp.dataset.idx)].file_name_regex = inp.value;
      };
    });
    container.querySelectorAll('.js-slot-del').forEach((btn) => {
      btn.onclick = () => {
        body.version_control_versions.splice(Number(btn.dataset.idx), 1);
        render();
      };
    });
    return;
  }

  const isList = dim.kind === 'list';
  document.getElementById('sym-dim-bl').oninput = (e) => {
    const arr = splitCsv(e.target.value);
    if (isList) body[`${dim.key}_blacklist`] = arr;
    else body[dim.key].blacklist = arr;
  };
  document.getElementById('sym-dim-wl').oninput = (e) => {
    const arr = splitCsv(e.target.value);
    if (isList) body[`${dim.key}_whitelist`] = arr;
    else body[dim.key].whitelist = arr;
  };

  document.getElementById('btn-sym-add-item').onclick = () => {
    const newItem = { name: '新规则项', pattern: 'Keyword', score: 10, description: '' };
    if (isList) {
      body[dim.key] = body[dim.key] || [];
      body[dim.key].push(newItem);
    } else {
      body[dim.key] = body[dim.key] || {};
      body[dim.key][`custom_${Date.now()}`] = newItem;
    }
    render();
  };

  const updateItemField = (el, field, numeric = false) => {
    const val = numeric ? Number(el.value || 0) : el.value;
    if (isList) {
      body[dim.key][Number(el.dataset.idx)][field] = val;
    } else {
      body[dim.key][el.dataset.k][field] = val;
    }
  };
  container.querySelectorAll('.js-item-name').forEach((el) => (el.oninput = () => updateItemField(el, 'name')));
  container.querySelectorAll('.js-item-pat').forEach((el) => (el.oninput = () => updateItemField(el, 'pattern')));
  container.querySelectorAll('.js-item-score').forEach((el) => (el.oninput = () => updateItemField(el, 'score', true)));
  container.querySelectorAll('.js-item-desc').forEach((el) => (el.oninput = () => updateItemField(el, 'description')));
  container.querySelectorAll('.js-item-del').forEach((btn) => {
    btn.onclick = () => {
      if (isList) body[dim.key].splice(Number(btn.dataset.idx), 1);
      else delete body[dim.key][btn.dataset.k];
      render();
    };
  });
}

function renderMediaTidyEditor(container) {
  const tidyRules = (state.boot.rules || []).filter((r) => r.kind === 'mediatidy');
  const rule = state.activeRuleDetail;
  if (!rule || rule.kind !== 'mediatidy') {
    container.innerHTML = `<div class="mw-empty">正在加载 MediaTidy 评分方案...</div>`;
    return;
  }
  const body = rule.body || {};
  const scoring = body.scoring || {};
  const excl = body.exclude || {};
  const ver = body.version || {};
  const slots = ver.slots || [];

  const totalWeight = TIDY_DIMS.reduce((acc, d) => {
    const dim = scoring[d.key] || {};
    return acc + (dim.enabled ? Number(dim.weight || 0) : 0);
  }, 0);

  container.innerHTML = `
    <div class="mw-card">
      <div class="mw-card-head">
        <div class="mw-actions" style="flex:1;">
          <select class="mw-select" id="sel-tidy-rule" style="max-width:320px;">
            ${tidyRules
              .map(
                (r) =>
                  `<option value="${esc(r.id)}" ${r.id === rule.id ? 'selected' : ''}>${esc(r.name)}${r.builtin ? ' (预设)' : ''}</option>`
              )
              .join('')}
          </select>
          <input class="mw-input" id="inp-tidy-name" value="${esc(rule.name)}" style="max-width:240px;" />
          <span class="mw-tag ${Math.abs(totalWeight - 100) < 0.01 ? 'ok' : 'warn'}">Σ 启用权重 = ${totalWeight.toFixed(1)} / 100 (满分 ${(totalWeight * 10).toFixed(0)})</span>
        </div>
        <div class="mw-actions">
          <button type="button" class="mw-btn sm primary" id="btn-tidy-save">保存修改</button>
          <button type="button" class="mw-btn sm" id="btn-tidy-clone">另存副本</button>
          <button type="button" class="mw-btn sm" id="btn-tidy-raw">YAML 原文</button>
          <button type="button" class="mw-btn sm" id="btn-tidy-import">导入 .yaml</button>
          <button type="button" class="mw-btn sm" id="btn-tidy-export">导出 .yaml</button>
          ${!rule.builtin ? `<button type="button" class="mw-btn sm danger" id="btn-tidy-del">删除</button>` : ''}
        </div>
      </div>

      <div class="mw-grid-3">
        ${TIDY_DIMS.map((d) => {
          const dim = scoring[d.key] || { enabled: false, weight: 0 };
          return `
            <div class="mw-dim-item" style="cursor:default;">
              <div class="mw-dim-top">
                <label class="mw-check">
                  <input type="checkbox" class="js-tidy-en" data-k="${d.key}" ${dim.enabled ? 'checked' : ''} />
                  <span>${esc(d.label)}</span>
                </label>
                <span class="mw-muted" style="font-size:11px;">${esc(d.group)}</span>
              </div>
              <div class="mw-slider-row">
                <input type="range" min="0" max="40" step="1" value="${dim.weight || 0}" class="js-tidy-w-range" data-k="${d.key}" />
                <input type="number" min="0" max="100" step="1" value="${dim.weight || 0}" class="js-tidy-w-num" data-k="${d.key}" />
              </div>
            </div>`;
        }).join('')}
      </div>

      <div class="mw-grid-3" style="margin-top:14px;padding-top:12px;border-top:1px solid var(--mw-border);">
        <div class="mw-field">
          <label>硬排除分辨率（逗号分隔）</label>
          <input class="mw-input" id="tidy-excl-res" value="${esc((excl.resolutions || []).join(', '))}" placeholder="360p" />
        </div>
        <div class="mw-field">
          <label>硬排除视频编码（逗号分隔）</label>
          <input class="mw-input" id="tidy-excl-codec" value="${esc((excl.codecs || []).join(', '))}" placeholder="MPEG-2, MPEG-4, Xvid, DivX" />
        </div>
        <div class="mw-field">
          <label>硬排除 HDR 类型（逗号分隔）</label>
          <input class="mw-input" id="tidy-excl-hdr" value="${esc((excl.hdr_types || []).join(', '))}" />
        </div>
      </div>

      <div style="margin-top:14px;padding-top:12px;border-top:1px solid var(--mw-border);">
        <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;">
          <label class="mw-check">
            <input type="checkbox" id="tidy-ver-en" ${ver.enabled ? 'checked' : ''} />
            <strong>启用多版本槽位隔离（共 ${slots.length} 个预设槽位，最多共存 ${ver.max_versions || 27} 版）</strong>
          </label>
        </div>
        <div class="mw-chip-group">
          ${slots
            .map(
              (s) =>
                `<span class="mw-tag" title="${esc(JSON.stringify(s.match || {}))}">${esc(s.name)}</span>`
            )
            .join('')}
        </div>
      </div>
    </div>
  `;

  document.getElementById('sel-tidy-rule').onchange = async (e) => {
    state.selectedTidyRuleId = e.target.value;
    state.activeRuleDetail = await callAction('ui_rule_detail', { id: state.selectedTidyRuleId });
    render();
  };
  document.getElementById('inp-tidy-name').oninput = (e) => {
    rule.name = e.target.value;
  };
  container.querySelectorAll('.js-tidy-en').forEach((chk) => {
    chk.onchange = () => {
      const k = chk.dataset.k;
      scoring[k] = scoring[k] || {};
      scoring[k].enabled = chk.checked;
      render();
    };
  });
  const bindTidyW = (sel) => {
    container.querySelectorAll(sel).forEach((inp) => {
      inp.oninput = () => {
        const k = inp.dataset.k;
        scoring[k] = scoring[k] || { enabled: true };
        scoring[k].weight = Number(inp.value || 0);
        render();
      };
    });
  };
  bindTidyW('.js-tidy-w-range');
  bindTidyW('.js-tidy-w-num');

  const splitCsv = (s) =>
    s
      .split(',')
      .map((x) => x.trim())
      .filter(Boolean);
  document.getElementById('tidy-excl-res').oninput = (e) => {
    body.exclude = body.exclude || {};
    body.exclude.resolutions = splitCsv(e.target.value);
  };
  document.getElementById('tidy-excl-codec').oninput = (e) => {
    body.exclude = body.exclude || {};
    body.exclude.codecs = splitCsv(e.target.value);
  };
  document.getElementById('tidy-excl-hdr').oninput = (e) => {
    body.exclude = body.exclude || {};
    body.exclude.hdr_types = splitCsv(e.target.value);
  };
  document.getElementById('tidy-ver-en').onchange = (e) => {
    body.version = body.version || {};
    body.version.enabled = e.target.checked;
  };

  const saveTidy = async (saveAsNew) => {
    const name = document.getElementById('inp-tidy-name').value.trim();
    const res = await callAction('ui_save_rule', {
      id: rule.id,
      save_as_new: saveAsNew,
      kind: 'mediatidy',
      name: saveAsNew ? `${name} 副本` : name,
      body,
    });
    applyBootstrap(res.bootstrap);
    state.selectedTidyRuleId = res.saved_rule.id;
    state.activeRuleDetail = res.saved_rule;
    showToast(saveAsNew ? '已另存为新 MediaTidy 方案' : 'MediaTidy 方案已保存', 'success');
    render();
  };

  document.getElementById('btn-tidy-save').onclick = () => saveTidy(false);
  document.getElementById('btn-tidy-clone').onclick = () => saveTidy(true);
  document.getElementById('btn-tidy-raw').onclick = () => openRawRuleModal(rule);
  document.getElementById('btn-tidy-import').onclick = () => {
    pickLocalFile('.yaml,.yml,.txt', async (_fname, content) => {
      const res = await callAction('ui_import_rule', { kind: 'mediatidy', content });
      applyBootstrap(res.bootstrap);
      state.selectedTidyRuleId = res.imported_id;
      state.activeRuleDetail = await callAction('ui_rule_detail', { id: res.imported_id });
      showToast('MediaTidy YAML 导入成功', 'success');
      render();
    });
  };
  document.getElementById('btn-tidy-export').onclick = async () => {
    const exp = await callAction('ui_export_rule', { id: rule.id });
    downloadTextFile(exp.filename, exp.content, 'text/yaml;charset=utf-8');
  };
  const delBtn = document.getElementById('btn-tidy-del');
  if (delBtn) {
    delBtn.onclick = async () => {
      const res = await callAction('ui_delete_rule', { id: rule.id });
      state.activeRuleDetail = null;
      applyBootstrap(res);
      await loadCurrentTabData();
      showToast('已删除自定义方案', 'success');
      render();
    };
  }
}

function renderSubscriptionsView(container) {
  const subs = state.boot.subscriptions || [];
  const aux = state.auxData || {};
  const mLocal = aux.local_matching_stats?.counts || {};
  const mRem = aux.remote_matching_stats?.counts || {};
  const mPre = aux.preset_matching_stats?.counts || {};
  const tLocal = aux.local_tmdb_stats?.counts || {};
  const tRem = aux.remote_tmdb_stats?.counts || {};
  const tPre = aux.preset_tmdb_stats?.counts || {};
  const bindings = aux.manual_bindings || [];

  container.innerHTML = `
    <div class="mw-card">
      <div class="mw-card-head">
        <div>
          <h3 class="mw-card-title">远程规则自动订阅（每 6 小时自动同步 · 绝不覆盖本地手工规则）</h3>
          <div class="mw-card-sub">生效优先级：<strong>人工绑定 &gt; 本地覆盖 YAML &gt; 远程订阅缓存 &gt; 内置预设</strong></div>
        </div>
        <button type="button" class="mw-btn sm primary" id="btn-sync-subs-now">立即同步全部订阅</button>
      </div>
      <div class="mw-table-wrap">
        <table class="mw-table">
          <thead><tr><th>订阅项</th><th>HTTPS 订阅地址</th><th>启用</th><th>最近同步 / 状态</th><th>操作</th></tr></thead>
          <tbody>
            ${subs
              .map(
                (s) => `
              <tr>
                <td><strong>${esc(s.name)}</strong></td>
                <td><input class="mw-input mw-mono js-sub-url" data-id="${esc(s.id)}" value="${esc(s.url)}" /></td>
                <td><input type="checkbox" class="js-sub-en" data-id="${esc(s.id)}" ${s.enabled ? 'checked' : ''} /></td>
                <td>
                  ${s.cached ? '<span class="mw-tag ok">已缓存</span>' : '<span class="mw-tag">未拉取</span>'}
                  <span class="mw-muted" style="font-size:11.5px;">${esc(s.last_sync || '')}</span>
                  ${s.error ? `<div style="color:var(--mw-danger);font-size:11.5px;">${esc(s.error)}</div>` : ''}
                </td>
                <td><button type="button" class="mw-btn sm js-sub-save" data-id="${esc(s.id)}">保存</button></td>
              </tr>`
              )
              .join('')}
          </tbody>
        </table>
      </div>
    </div>

    <div class="mw-grid-2">
      <div class="mw-card">
        <div class="mw-card-head">
          <div>
            <h3 class="mw-card-title">本地识别与季集映射覆盖 (mr / mp)</h3>
            <div class="mw-card-sub">
              本地 mr:${mLocal.mr || 0} / mp:${mLocal.mp || 0} · 远程 mr:${mRem.mr || 0} / mp:${mRem.mp || 0} · 内置 mr:${mPre.mr || 0} / mp:${mPre.mp || 0}
            </div>
          </div>
          <div class="mw-actions">
            <button type="button" class="mw-btn sm" id="btn-imp-matching">上传文件</button>
            <button type="button" class="mw-btn sm primary" id="btn-save-matching">校验并保存</button>
          </div>
        </div>
        <textarea class="mw-textarea" id="txt-local-matching" placeholder="mr:\n  - pattern: '^1917\\.'\n    replace: '一九一七.'\nmp:\n  - tmdb: 37854\n    from: { season: 1, ep: [1, 61] }\n    to: { season: 1, ep: [1, 61] }">${esc(aux.local_matching_yaml || '')}</textarea>
      </div>

      <div class="mw-card">
        <div class="mw-card-head">
          <div>
            <h3 class="mw-card-title">本地 TMDB 覆盖与黑名单 (tmdb_override)</h3>
            <div class="mw-card-sub">
              本地覆盖:${tLocal.tmdb_override || 0} / 黑名单:${tLocal.tmdb_blacklist || 0} · 远程:${tRem.tmdb_override || 0} · 内置:${tPre.tmdb_override || 0}
            </div>
          </div>
          <div class="mw-actions">
            <button type="button" class="mw-btn sm" id="btn-imp-tmdb">上传文件</button>
            <button type="button" class="mw-btn sm primary" id="btn-save-tmdb">校验并保存</button>
          </div>
        </div>
        <textarea class="mw-textarea" id="txt-local-tmdb" placeholder="tmdb_override:\n  - tmdb_id: 37854\n    media_type: tv\n    title: '海贼王'\ntmdb_blacklist:\n  - tmdb_id: 318729\n    media_type: tv">${esc(aux.local_tmdb_yaml || '')}</textarea>
      </div>
    </div>

    ${
      bindings.length
        ? `<div class="mw-card">
            <h3 class="mw-card-title" style="margin-bottom:8px;">已记住的人工 TMDB 绑定词典 (${bindings.length})</h3>
            <div class="mw-table-wrap">
              <table class="mw-table">
                <thead><tr><th>匹配键</th><th>作品名称</th><th>类型</th><th>TMDB ID</th><th>操作</th></tr></thead>
                <tbody>
                  ${bindings
                    .map(
                      (mb) => `
                    <tr>
                      <td class="mw-mono">${esc(mb.pattern_key)}</td>
                      <td>${esc(mb.title)}</td>
                      <td>${esc(mb.media_type)}</td>
                      <td>${esc(mb.tmdb_id)}</td>
                      <td><button type="button" class="mw-btn sm danger js-del-mb" data-key="${esc(mb.pattern_key)}">移除</button></td>
                    </tr>`
                    )
                    .join('')}
                </tbody>
              </table>
            </div>
          </div>`
        : ''
    }
  `;

  document.getElementById('btn-sync-subs-now').onclick = async () => {
    showToast('正在从远程拉取最新订阅规则...', 'info');
    const res = await callAction('ui_sync_rules', {});
    applyBootstrap(res.bootstrap);
    state.auxData = await callAction('ui_get_aux_yaml', {});
    showToast(
      `远程订阅同步完成：成功 ${res.sync?.ok_count || 0}，失败 ${res.sync?.fail_count || 0}`,
      res.sync?.fail_count ? 'error' : 'success'
    );
    render();
  };

  container.querySelectorAll('.js-sub-save').forEach((btn) => {
    btn.onclick = async () => {
      const id = btn.dataset.id;
      const url = container.querySelector(`.js-sub-url[data-id="${id}"]`).value.trim();
      const enabled = container.querySelector(`.js-sub-en[data-id="${id}"]`).checked;
      const res = await callAction('ui_save_subscription', { id, url, enabled });
      applyBootstrap(res);
      showToast('订阅设置已更新', 'success');
      render();
    };
  });

  document.getElementById('btn-imp-matching').onclick = () => {
    pickLocalFile('.yaml,.yml,.txt', (_n, text) => {
      document.getElementById('txt-local-matching').value = text;
    });
  };
  document.getElementById('btn-imp-tmdb').onclick = () => {
    pickLocalFile('.yaml,.yml,.txt', (_n, text) => {
      document.getElementById('txt-local-tmdb').value = text;
    });
  };

  document.getElementById('btn-save-matching').onclick = async () => {
    const content = document.getElementById('txt-local-matching').value;
    const res = await callAction('ui_save_aux_yaml', { kind: 'matching', content });
    applyBootstrap(res.bootstrap);
    state.auxData = await callAction('ui_get_aux_yaml', {});
    showToast('本地识别与季集映射规则校验通过并已保存', 'success');
    render();
  };
  document.getElementById('btn-save-tmdb').onclick = async () => {
    const content = document.getElementById('txt-local-tmdb').value;
    const res = await callAction('ui_save_aux_yaml', { kind: 'tmdb', content });
    applyBootstrap(res.bootstrap);
    state.auxData = await callAction('ui_get_aux_yaml', {});
    showToast('本地 TMDB 覆盖与黑名单规则已保存', 'success');
    render();
  };

  container.querySelectorAll('.js-del-mb').forEach((btn) => {
    btn.onclick = async () => {
      await callAction('ui_delete_manual_binding', { pattern_key: btn.dataset.key });
      state.auxData = await callAction('ui_get_aux_yaml', {});
      render();
    };
  });
}

function renderSampleTesterView(container) {
  const rules = state.boot.rules || [];
  const cats = state.boot.host_sync?.categories || [];
  const res = state.sampleResult;

  container.innerHTML = `
    <div class="mw-card">
      <div class="mw-card-head">
        <div>
          <h3 class="mw-card-title">双样本识别与评分对比试算器</h3>
          <div class="mw-card-sub">验证文件名/父目录识别（含数字片名与动漫跨季 mp 映射）、版本槽位归属及 Symedia / MediaTidy 逐项加权分差</div>
        </div>
        <div class="mw-actions">
          <button type="button" class="mw-btn sm" id="btn-preset-guangyin">示例1：同槽高码升级</button>
          <button type="button" class="mw-btn sm" id="btn-preset-iso">示例2：1917 淘汰 ISO</button>
          <button type="button" class="mw-btn primary" id="btn-run-compare">开始对比试算</button>
        </div>
      </div>

      <div class="mw-grid-2" style="margin-bottom:12px;">
        <div class="mw-field">
          <label>选择评分规则</label>
          <select class="mw-select" id="smp-rule">
            ${rules
              .map(
                (r) =>
                  `<option value="${esc(r.id)}" ${r.id === state.sampleRuleId ? 'selected' : ''}>[${r.kind}] ${esc(r.name)}</option>`
              )
              .join('')}
          </select>
        </div>
        <div class="mw-field">
          <label>模拟所属 Mediary 二级分类（用于验证分类黑白名单与原盘保护）</label>
          <input class="mw-input" list="smp-cat-list" id="smp-cat" value="${esc(state.sampleCategory)}" placeholder="例如: 动漫/国漫 或 动漫/儿童 或 电影/技术专区/蓝光原盘" />
          <datalist id="smp-cat-list">
            ${cats.map((c) => `<option value="${esc(c.path)}"></option>`).join('')}
          </datalist>
        </div>
      </div>

      <div class="mw-grid-2">
        <div class="mw-card" style="margin-bottom:0;background:var(--mw-surface-2);">
          <h4 style="margin:0 0 8px;">样本 A（库内现有版本）</h4>
          <div class="mw-field"><label>文件名</label><input class="mw-input" id="smp-l-name" value="${esc(state.sampleLeft.name)}" /></div>
          <div class="mw-field" style="margin-top:6px;"><label>父目录路径（可选）</label><input class="mw-input" id="smp-l-folder" value="${esc(state.sampleLeft.folder)}" /></div>
          <div class="mw-grid-3" style="margin-top:6px;">
            <div class="mw-field"><label>体积 (GB)</label><input type="number" step="0.1" class="mw-input" id="smp-l-sz" value="${state.sampleLeft.size_gb}" /></div>
            <div class="mw-field"><label>总码率 (Mbps)</label><input type="number" step="0.1" class="mw-input" id="smp-l-br" value="${state.sampleLeft.bitrate_mbps}" /></div>
            <div class="mw-field"><label>视频码率 (Mbps)</label><input type="number" step="0.1" class="mw-input" id="smp-l-vbr" value="${state.sampleLeft.video_bitrate_mbps}" /></div>
          </div>
        </div>

        <div class="mw-card" style="margin-bottom:0;background:var(--mw-surface-2);">
          <h4 style="margin:0 0 8px;">样本 B（新入候选版本）</h4>
          <div class="mw-field"><label>文件名</label><input class="mw-input" id="smp-r-name" value="${esc(state.sampleRight.name)}" /></div>
          <div class="mw-field" style="margin-top:6px;"><label>父目录路径（可选）</label><input class="mw-input" id="smp-r-folder" value="${esc(state.sampleRight.folder)}" /></div>
          <div class="mw-grid-3" style="margin-top:6px;">
            <div class="mw-field"><label>体积 (GB)</label><input type="number" step="0.1" class="mw-input" id="smp-r-sz" value="${state.sampleRight.size_gb}" /></div>
            <div class="mw-field"><label>总码率 (Mbps)</label><input type="number" step="0.1" class="mw-input" id="smp-r-br" value="${state.sampleRight.bitrate_mbps}" /></div>
            <div class="mw-field"><label>视频码率 (Mbps)</label><input type="number" step="0.1" class="mw-input" id="smp-r-vbr" value="${state.sampleRight.video_bitrate_mbps}" /></div>
          </div>
        </div>
      </div>

      ${
        res
          ? `<div style="margin-top:14px;padding:12px 14px;border-radius:10px;border:1px solid var(--mw-primary);background:color-mix(in srgb, var(--mw-primary) 12%, transparent);">
              <strong>试算结论：</strong> ${esc(res.verdict)}
            </div>
            <div class="mw-grid-2" style="margin-top:12px;">
              ${renderSampleScoreCard('样本 A（现有）', res.left)}
              ${renderSampleScoreCard('样本 B（候选）', res.right)}
            </div>`
          : ''
      }
    </div>
  `;

  document.getElementById('btn-preset-guangyin').onclick = () => {
    state.sampleLeft = {
      name: '光阴之外.S01E49.2160p.HDR10.WEB-DL.H265.AAC.mkv',
      folder: '光阴之外 (2025)/Season 01',
      size_gb: 0.85,
      bitrate_mbps: 5.6,
      video_bitrate_mbps: 5.1,
    };
    state.sampleRight = {
      name: '光阴之外.S01E49.2160p.HDR10.WEB-DL.H265.DDP5.1.ASS.简英特效.mkv',
      folder: '光阴之外 (2025)/Season 01',
      size_gb: 2.85,
      bitrate_mbps: 19.4,
      video_bitrate_mbps: 18.2,
    };
    state.sampleCategory = '动漫/国漫';
    const tidy = (state.boot.rules || []).find((r) => r.kind === 'mediatidy');
    if (tidy) state.sampleRuleId = tidy.id;
    render();
  };

  document.getElementById('btn-preset-iso').onclick = () => {
    state.sampleLeft = {
      name: '1917.2019.2160p.UHD.Blu-Ray.HEVC.TrueHD.Dolby.Atmos.7.1.iso',
      folder: '',
      size_gb: 62.0,
      bitrate_mbps: 55.0,
      video_bitrate_mbps: 48.0,
    };
    state.sampleRight = {
      name: '1917.2019.2160p.UHD.Blu-Ray.Remux.HEVC.TrueHD.Dolby.Atmos.7.1.ASS.中英特效-CHD.mkv',
      folder: '',
      size_gb: 54.0,
      bitrate_mbps: 52.0,
      video_bitrate_mbps: 47.0,
    };
    state.sampleCategory = '电影/外语电影/2010-2019';
    const sym = (state.boot.rules || []).find((r) => r.kind === 'symedia');
    if (sym) state.sampleRuleId = sym.id;
    render();
  };

  document.getElementById('btn-run-compare').onclick = async () => {
    state.sampleRuleId = document.getElementById('smp-rule').value;
    state.sampleCategory = document.getElementById('smp-cat').value.trim();
    state.sampleLeft = {
      name: document.getElementById('smp-l-name').value.trim(),
      folder: document.getElementById('smp-l-folder').value.trim(),
      size_gb: Number(document.getElementById('smp-l-sz').value || 0),
      bitrate_mbps: Number(document.getElementById('smp-l-br').value || 0),
      video_bitrate_mbps: Number(document.getElementById('smp-l-vbr').value || 0),
    };
    state.sampleRight = {
      name: document.getElementById('smp-r-name').value.trim(),
      folder: document.getElementById('smp-r-folder').value.trim(),
      size_gb: Number(document.getElementById('smp-r-sz').value || 0),
      bitrate_mbps: Number(document.getElementById('smp-r-br').value || 0),
      video_bitrate_mbps: Number(document.getElementById('smp-r-vbr').value || 0),
    };
    state.sampleResult = await callAction('ui_compare_samples', {
      rule_id: state.sampleRuleId,
      category: state.sampleCategory,
      left: state.sampleLeft,
      right: state.sampleRight,
    });
    render();
  };
}

function renderSampleScoreCard(label, side) {
  const p = side.recognition?.parsed || {};
  const bnd = side.recognition?.bound;
  const sc = side.score || {};
  return `
    <div class="mw-card" style="margin-bottom:0;">
      <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;">
        <strong>${esc(label)}</strong>
        <div>
          <span class="mw-tag primary">${esc(sc.slot || '默认')}</span>
          <strong style="font-size:18px;margin-left:6px;">${Number(sc.total || 0).toFixed(2)} 分</strong>
        </div>
      </div>
      <div class="mw-muted" style="font-size:12px;margin-bottom:8px;">
        识别片名：<strong>${esc(p.title || '-')}</strong> ${p.year ? `(${p.year})` : ''}
        ${p.episode ? ` · S${String(p.season || 1).padStart(2, '0')}E${String(p.episode).padStart(2, '0')}` : ''}
        ${bnd ? ` · 绑定 TMDB:${bnd.tmdb_id} (${esc(bnd.source)})` : ''}
      </div>
      <table class="mw-table">
        <thead><tr><th>评分维度</th><th>命中特征</th><th>加权得分</th></tr></thead>
        <tbody>
          ${(sc.breakdown || [])
            .map(
              (row) =>
                `<tr><td>${esc(row.dimension)}</td><td>${esc(row.matched)}</td><td><strong>${row.points >= 0 ? '+' : ''}${row.points}</strong></td></tr>`
            )
            .join('')}
        </tbody>
      </table>
    </div>
  `;
}

// ============================================================================
// Modals: 115 Pipeline Editor, 115 Directory Browser + Live Probe, TMDB Binder, Raw Rule Editor
// ============================================================================
function openPipelineModal(existing) {
  const p = existing
    ? { ...existing }
    : {
        id: 0,
        name: '115 额外监控目录',
        enabled: true,
        media_type: 'auto',
        source_cid: '',
        source_name: '',
        recursive: true,
        move_subtitles: true,
        auto_admit: true,
        wash_enabled: true,
        reject_mode: 'recycle_bin',
        probe_mode: 'smart',
        rule_id_override: '',
      };
  const rules = state.boot.rules || [];

  openModal(
    p.id ? `编辑 115 监控目录 · ${p.name}` : '新增 115 待洗版监控目录',
    `
      <div class="mw-grid-2">
        <div class="mw-field">
          <label>监控名称</label>
          <input class="mw-input" id="pm-name" value="${esc(p.name)}" />
        </div>
        <div class="mw-field">
          <label>作品类型识别方式</label>
          <select class="mw-select" id="pm-type">
            <option value="auto" ${p.media_type === 'auto' ? 'selected' : ''}>自动识别电影与剧集（推荐混放目录）</option>
            <option value="movie" ${p.media_type === 'movie' ? 'selected' : ''}>仅按电影识别</option>
            <option value="tv" ${p.media_type === 'tv' ? 'selected' : ''}>仅按剧集识别</option>
          </select>
        </div>
      </div>

      <div class="mw-grid-3" style="margin-top:10px;">
        <div class="mw-field">
          <label>待洗版监控目录 CID</label>
          <div style="display:flex;gap:6px;">
            <input class="mw-input mw-mono" id="pm-src-cid" value="${esc(p.source_cid)}" placeholder="纯数字 CID" />
            <button type="button" class="mw-btn sm" id="pm-pick-src">浏览</button>
          </div>
          <span class="mw-muted" id="pm-src-name" style="font-size:11px;">${esc(p.source_name || '')}</span>
        </div>
        <div class="mw-field">
          <label>归档目标库根目录 CID（可选）</label>
          <div style="display:flex;gap:6px;">
            <input class="mw-input mw-mono" id="pm-dst-cid" value="${esc(p.target_cid || '')}" placeholder="留空自动识别「媒体库」" />
            <button type="button" class="mw-btn sm" id="pm-pick-dst">浏览</button>
          </div>
          <span class="mw-muted" id="pm-dst-name" style="font-size:11px;">${esc(p.target_name || '')}</span>
        </div>
        <div class="mw-field">
          <label>淘汰/重复文件处理</label>
          <select class="mw-select" id="pm-rej">
            <option value="recycle_bin" ${p.reject_mode !== 'skip' ? 'selected' : ''}>直接移入 115 官方回收站</option>
            <option value="skip" ${p.reject_mode === 'skip' ? 'selected' : ''}>原地跳过忽略</option>
          </select>
        </div>
      </div>

      <div class="mw-grid-2" style="margin-top:10px;">
        <div class="mw-field">
          <label>115 MediaInfo 实测模式</label>
          <select class="mw-select" id="pm-probe">
            <option value="smart" ${p.probe_mode === 'smart' ? 'selected' : ''}>智能按需探测（推荐：优先命中宿主缓存 + 1.25s 队列）</option>
            <option value="always" ${p.probe_mode === 'always' ? 'selected' : ''}>强制实测全部流信息后再归档</option>
            <option value="filename_only" ${p.probe_mode === 'filename_only' ? 'selected' : ''}>仅按文件名与体积快速评分</option>
          </select>
        </div>
        <div class="mw-field">
          <label>专属评分方案（留空跟随全局统一策略）</label>
          <select class="mw-select" id="pm-rule">
            <option value="">跟随全局电影/剧集统一策略</option>
            ${rules
              .map(
                (r) =>
                  `<option value="${esc(r.id)}" ${r.id === p.rule_id_override ? 'selected' : ''}>${esc(r.name)}</option>`
              )
              .join('')}
          </select>
        </div>
      </div>

      <div style="margin-top:12px;display:flex;flex-wrap:wrap;gap:14px;">
        <label class="mw-check"><input type="checkbox" id="pm-wash" ${p.wash_enabled !== false ? 'checked' : ''} /><span>启用权重洗版</span></label>
        <label class="mw-check"><input type="checkbox" id="pm-auto" ${p.auto_admit !== false ? 'checked' : ''} /><span>全自动执行归档与回收</span></label>
        <label class="mw-check"><input type="checkbox" id="pm-enabled" ${p.enabled !== false ? 'checked' : ''} /><span>启用定时巡检</span></label>
        <label class="mw-check"><input type="checkbox" id="pm-rec" ${p.recursive !== false ? 'checked' : ''} /><span>递归扫描子文件夹</span></label>
        <label class="mw-check"><input type="checkbox" id="pm-sub" ${p.move_subtitles !== false ? 'checked' : ''} /><span>淘汰时同步清理同名外挂字幕</span></label>
      </div>

      <div id="pm-inline-browser" style="margin-top:12px;"></div>

      <div style="margin-top:16px;display:flex;justify-content:flex-end;gap:8px;">
        <button type="button" class="mw-btn" id="pm-cancel">取消</button>
        <button type="button" class="mw-btn primary" id="pm-save">保存配置</button>
      </div>
    `,
    (body) => {
      body.querySelector('#pm-cancel').onclick = closeModal;
      body.querySelector('#pm-pick-src').onclick = () => {
        mountInline115Picker(body.querySelector('#pm-inline-browser'), (cid, dirName) => {
          body.querySelector('#pm-src-cid').value = cid;
          body.querySelector('#pm-src-name').textContent = dirName;
          p.source_name = dirName;
        });
      };
      body.querySelector('#pm-pick-dst').onclick = () => {
        mountInline115Picker(body.querySelector('#pm-inline-browser'), (cid, dirName) => {
          body.querySelector('#pm-dst-cid').value = cid;
          body.querySelector('#pm-dst-name').textContent = dirName;
          p.target_name = dirName;
        });
      };

      body.querySelector('#pm-save').onclick = async () => {
        const dstCid = body.querySelector('#pm-dst-cid').value.trim();
        const pipeline = {
          id: p.id,
          name: body.querySelector('#pm-name').value.trim(),
          enabled: body.querySelector('#pm-enabled').checked,
          media_type: body.querySelector('#pm-type').value,
          source_cid: body.querySelector('#pm-src-cid').value.trim(),
          source_name: p.source_name || '',
          target_cid: dstCid,
          target_name: dstCid ? p.target_name || '' : '',
          recursive: body.querySelector('#pm-rec').checked,
          move_subtitles: body.querySelector('#pm-sub').checked,
          auto_admit: body.querySelector('#pm-auto').checked,
          wash_enabled: body.querySelector('#pm-wash').checked,
          reject_mode: body.querySelector('#pm-rej').value,
          probe_mode: body.querySelector('#pm-probe').value,
          rule_id_override: body.querySelector('#pm-rule').value,
        };
        const res = await callAction('ui_save_pipeline', { pipeline });
        applyBootstrap(res);
        closeModal();
        showToast('115 监控目录已保存', 'success');
        render();
      };
    }
  );
}

function mountInline115Picker(container, onPick) {
  const stack = [{ cid: '0', name: '115 根目录' }];

  const loadDir = async () => {
    const cur = stack[stack.length - 1];
    container.innerHTML = `<div class="mw-card" style="background:var(--mw-surface-2);margin-bottom:0;">正在读取 115 目录 (${esc(cur.name)})...</div>`;
    const res = await callAction('ui_browse_115', { cid: cur.cid, offset: 0, show_files: true });
    container.innerHTML = `
      <div class="mw-card" style="background:var(--mw-surface-2);margin-bottom:0;">
        <div style="display:flex;justify-content:space-between;align-items:center;gap:8px;margin-bottom:8px;flex-wrap:wrap;">
          <div>
            <strong>📂 ${stack.map((s) => esc(s.name)).join(' / ')}</strong>
            <span class="mw-mono mw-muted"> (CID: ${esc(cur.cid)})</span>
          </div>
          <div class="mw-actions">
            ${stack.length > 1 ? `<button type="button" class="mw-btn sm" id="b115-up">⬆️ 返回上级</button>` : ''}
            ${
              onPick && cur.cid !== '0'
                ? `<button type="button" class="mw-btn sm primary" id="b115-choose">选用当前目录 (${esc(cur.name)})</button>`
                : ''
            }
          </div>
        </div>
        <div style="max-height:240px;overflow-y:auto;">
          ${(res.directories || [])
            .map(
              (d) => `
            <div style="display:flex;justify-content:space-between;align-items:center;padding:5px 6px;border-bottom:1px solid var(--mw-border);">
              <span style="cursor:pointer;" class="js-b115-enter" data-cid="${esc(d.file_id)}" data-name="${esc(d.name)}">📁 ${esc(d.name)}</span>
              ${
                onPick
                  ? `<button type="button" class="mw-btn sm js-b115-pick-row" data-cid="${esc(d.file_id)}" data-name="${esc(d.name)}">选此目录</button>`
                  : `<span class="mw-mono mw-muted">${esc(d.file_id)}</span>`
              }
            </div>`
            )
            .join('')}
          ${(res.files || [])
            .map(
              (f, idx) => `
            <div style="display:flex;justify-content:space-between;align-items:center;padding:5px 6px;border-bottom:1px solid var(--mw-border);">
              <span style="font-size:12px;word-break:break-all;">🎬 ${esc(f.name)} <span class="mw-muted">(${fmtBytes(f.size)})</span></span>
              <button type="button" class="mw-btn sm js-b115-probe" data-idx="${idx}">实测打分</button>
            </div>`
            )
            .join('')}
        </div>
        <div id="b115-probe-out" style="margin-top:8px;"></div>
      </div>
    `;

    const upBtn = container.querySelector('#b115-up');
    if (upBtn) {
      upBtn.onclick = () => {
        stack.pop();
        loadDir();
      };
    }
    const chooseBtn = container.querySelector('#b115-choose');
    if (chooseBtn) {
      chooseBtn.onclick = () => {
        onPick(cur.cid, cur.name);
        container.innerHTML = '';
      };
    }
    container.querySelectorAll('.js-b115-enter').forEach((el) => {
      el.onclick = () => {
        stack.push({ cid: el.dataset.cid, name: el.dataset.name });
        loadDir();
      };
    });
    container.querySelectorAll('.js-b115-pick-row').forEach((btn) => {
      btn.onclick = () => {
        onPick(btn.dataset.cid, btn.dataset.name);
        container.innerHTML = '';
      };
    });
    container.querySelectorAll('.js-b115-probe').forEach((btn) => {
      btn.onclick = async () => {
        const f = res.files[Number(btn.dataset.idx)];
        const out = container.querySelector('#b115-probe-out');
        out.innerHTML = `<span class="mw-muted">正在调用宿主 115 MediaInfo 实测《${esc(f.name)}》...</span>`;
        const pr = await callAction('ui_probe_115_file', { file: f });
        const pf = pr.profile || {};
        out.innerHTML = `
          <div style="padding:8px 10px;border-radius:8px;background:var(--mw-surface);border:1px solid var(--mw-border);font-size:12px;">
            <strong>${esc(f.name)}</strong> →
            <span class="mw-tag primary">${esc(pr.score?.slot || '-')}</span>
            <strong style="color:var(--mw-success);">${Number(pr.score?.total || 0).toFixed(2)} 分</strong>
            （${esc(pf.resolution || '-')} · ${esc(pf.hdr || '-')} · ${pf.bitrate_mbps || 0} Mbps · ${esc((pf.audio || []).join('/') || '-')}）
          </div>
        `;
      };
    });
  };
  loadDir();
}

function open115BrowserModal() {
  openModal('浏览 115 网盘目录与实时 MediaInfo 探测打分', `<div id="standalone-115-box"></div>`, (body) => {
    mountInline115Picker(body.querySelector('#standalone-115-box'), null);
  });
}

function openBindTmdbModal(item) {
  openModal(
    `人工指定 TMDB 身份 · ${item.name}`,
    `
      <div class="mw-muted" style="margin-bottom:8px;font-size:12px;">
        文件：<code>${esc(item.rel_path ? `${item.rel_path}/${item.name}` : item.name)}</code>
      </div>
      <div style="display:flex;gap:8px;margin-bottom:10px;">
        <select class="mw-select" id="bt-type" style="width:110px;">
          <option value="movie" ${item.media_type !== 'tv' ? 'selected' : ''}>电影</option>
          <option value="tv" ${item.media_type === 'tv' ? 'selected' : ''}>剧集</option>
        </select>
        <input class="mw-input" id="bt-query" value="${esc(item.title || item.name)}" placeholder="输入片名或直接输入数字 TMDB ID..." style="flex:1;" />
        <button type="button" class="mw-btn primary" id="bt-search">搜索 TMDB</button>
      </div>

      <div class="mw-grid-3" style="margin-bottom:10px;">
        <div class="mw-field"><label>绑定 TMDB ID</label><input class="mw-input mw-mono" id="bt-id" value="${esc(item.tmdb_id || '')}" /></div>
        <div class="mw-field"><label>季号（剧集选填）</label><input type="number" min="0" class="mw-input" id="bt-season" value="${item.season || ''}" /></div>
        <div class="mw-field"><label>集号（剧集单文件选填）</label><input type="number" min="0" class="mw-input" id="bt-ep" value="${item.episode || ''}" /></div>
      </div>

      <label class="mw-check" style="margin-bottom:10px;">
        <input type="checkbox" id="bt-remember" checked />
        <span>记住此目录/片名的 TMDB 映射（后续同目录剧集自动识别）</span>
      </label>

      <div id="bt-results" style="max-height:240px;overflow-y:auto;"></div>

      <div style="margin-top:14px;display:flex;justify-content:flex-end;gap:8px;">
        <button type="button" class="mw-btn" id="bt-cancel">取消</button>
        <button type="button" class="mw-btn primary" id="bt-apply">绑定并立即重新评分</button>
      </div>
    `,
    (body) => {
      let pickedTitle = item.title || '';
      body.querySelector('#bt-cancel').onclick = closeModal;
      const runSearch = async () => {
        const query = body.querySelector('#bt-query').value.trim();
        const media_type = body.querySelector('#bt-type').value;
        if (!query) return;
        const res = await callAction('ui_search_tmdb', { query, media_type });
        const box = body.querySelector('#bt-results');
        if (!(res.items || []).length) {
          box.innerHTML = `<div class="mw-empty">未搜到匹配的 TMDB 条目</div>`;
          return;
        }
        box.innerHTML = `<table class="mw-table"><tbody>
          ${res.items
            .map(
              (h) => `
            <tr>
              <td><strong>${esc(h.title)}</strong> ${h.year ? `(${h.year})` : ''} <span class="mw-tag">${esc(h.media_type)}</span> <span class="mw-mono">TMDB:${esc(h.tmdb_id)}</span></td>
              <td style="text-align:right;"><button type="button" class="mw-btn sm primary js-pick-hit" data-id="${esc(h.tmdb_id)}" data-mt="${esc(h.media_type)}" data-title="${esc(h.title)}">填入</button></td>
            </tr>`
            )
            .join('')}
        </tbody></table>`;
        box.querySelectorAll('.js-pick-hit').forEach((btn) => {
          btn.onclick = () => {
            body.querySelector('#bt-id').value = btn.dataset.id;
            body.querySelector('#bt-type').value = btn.dataset.mt;
            pickedTitle = btn.dataset.title;
          };
        });
      };
      body.querySelector('#bt-search').onclick = runSearch;

      body.querySelector('#bt-apply').onclick = async () => {
        const tmdb_id = body.querySelector('#bt-id').value.trim();
        const media_type = body.querySelector('#bt-type').value;
        const season = Number(body.querySelector('#bt-season').value || 0);
        const episode = Number(body.querySelector('#bt-ep').value || 0);
        const remember = body.querySelector('#bt-remember').checked;
        if (!tmdb_id) return showToast('请填写或搜索选择 TMDB ID', 'error');
        const res = await callAction('ui_staging_bind_tmdb', {
          id: item.id,
          media_type,
          tmdb_id,
          title: pickedTitle,
          season: season > 0 ? season : null,
          episode: episode > 0 ? episode : null,
          remember,
        });
        state.stagingItems = res.items || [];
        state.boot.counts = res.counts || state.boot.counts;
        syncSidebarBadges();
        closeModal();
        showToast(`已绑定 TMDB:${tmdb_id} → ${res.bound?.reason || ''}`, 'success');
        render();
      };
    }
  );
}

function openAddOverrideModal() {
  const cats = state.boot.host_sync?.categories || [];
  const rules = state.boot.rules || [];
  openModal(
    '新增 Mediary 二级分类独立覆盖策略',
    `
      <div class="mw-grid-2">
        <div class="mw-field">
          <label>媒体类型</label>
          <select class="mw-select" id="ov-mt">
            <option value="movie">电影 (movie)</option>
            <option value="tv">剧集 (tv)</option>
          </select>
        </div>
        <div class="mw-field">
          <label>Mediary 分类路径或父前缀</label>
          <input class="mw-input" list="ov-cat-list" id="ov-prefix" placeholder="例如: 电影/技术专区/蓝光原盘 或 动漫/儿童" />
          <datalist id="ov-cat-list">
            ${cats.map((c) => `<option value="${esc(c.path)}"></option>`).join('')}
          </datalist>
        </div>
        <div class="mw-field">
          <label>专属评分方案（留空跟随默认）</label>
          <select class="mw-select" id="ov-rule">
            <option value="">跟随全局默认</option>
            ${rules.map((r) => `<option value="${esc(r.id)}">${esc(r.name)}</option>`).join('')}
          </select>
        </div>
        <div class="mw-field">
          <label>独立最低分差（选填）</label>
          <input type="number" step="0.5" class="mw-input" id="ov-diff" placeholder="留空跟随默认" />
        </div>
      </div>
      <div style="margin-top:12px;display:flex;gap:16px;">
        <label class="mw-check"><input type="checkbox" id="ov-skip-lib" /><span>此分类跳过存量库重复清理</span></label>
        <label class="mw-check"><input type="checkbox" id="ov-skip-pt" /><span>此分类跳过 PT 主动洗版</span></label>
      </div>
      <div style="margin-top:16px;display:flex;justify-content:flex-end;gap:8px;">
        <button type="button" class="mw-btn" id="ov-cancel">取消</button>
        <button type="button" class="mw-btn primary" id="ov-save">添加覆盖</button>
      </div>
    `,
    (body) => {
      body.querySelector('#ov-cancel').onclick = closeModal;
      body.querySelector('#ov-save').onclick = async () => {
        const prefix = body.querySelector('#ov-prefix').value.trim();
        if (!prefix) return showToast('请输入或选择分类前缀', 'error');
        const diffRaw = body.querySelector('#ov-diff').value.trim();
        const st = state.boot.strategy || {};
        const nextOvs = [
          ...(st.overrides || []),
          {
            id: `ov_${Date.now()}`,
            name: prefix,
            media_type: body.querySelector('#ov-mt').value,
            category_prefix: prefix,
            rule_id: body.querySelector('#ov-rule').value,
            min_diff: diffRaw ? Number(diffRaw) : null,
            keep_per_slot: null,
            skip_library_wash: body.querySelector('#ov-skip-lib').checked,
            skip_pt_wash: body.querySelector('#ov-skip-pt').checked,
          },
        ];
        const res = await callAction('ui_save_strategy', {
          strategy: { ...st, overrides: nextOvs },
        });
        applyBootstrap(res);
        closeModal();
        showToast('已添加二级分类覆盖策略', 'success');
        render();
      };
    }
  );
}

function openRawRuleModal(rule) {
  openModal(
    `编辑原文 · ${rule.name}`,
    `
      <textarea class="mw-textarea" id="raw-rule-txt" style="min-height:400px;">${esc(rule.raw || '')}</textarea>
      <div style="margin-top:12px;display:flex;justify-content:space-between;gap:8px;">
        <button type="button" class="mw-btn" id="raw-copy">复制全文</button>
        <div class="mw-actions">
          <button type="button" class="mw-btn" id="raw-cancel">取消</button>
          <button type="button" class="mw-btn primary" id="raw-save">校验并保存</button>
        </div>
      </div>
    `,
    (body) => {
      body.querySelector('#raw-cancel').onclick = closeModal;
      body.querySelector('#raw-copy').onclick = async () => {
        await copyToClipboard(body.querySelector('#raw-rule-txt').value);
        showToast('已复制到剪贴板', 'success');
      };
      body.querySelector('#raw-save').onclick = async () => {
        const raw = body.querySelector('#raw-rule-txt').value;
        const res = await callAction('ui_save_rule', {
          id: rule.id,
          save_as_new: false,
          kind: rule.kind,
          name: rule.name,
          raw,
        });
        applyBootstrap(res.bootstrap);
        state.activeRuleDetail = res.saved_rule;
        closeModal();
        showToast('原文校验通过并已保存', 'success');
        render();
      };
    }
  );
}

// ============================================================================
// TAB 5: 历史记录 (MediFind-aligned Compact Layout + Floating Detail Tooltip)
// ============================================================================
function historyActionBadge(act, label, status) {
  if (status === 'warning' && act === 'organized') {
    return '<span class="mw-tag warn">归档告警</span>';
  }
  switch (act) {
    case 'organized':
      return `<span class="mw-tag ok">${esc(label || '归档入库')}</span>`;
    case 'recycled_115':
      return `<span class="mw-tag danger">${esc(label || '移入回收站')}</span>`;
    case 'auto_clean_old':
    case 'clean_old':
    case 'clean_library_dup':
      return `<span class="mw-tag primary">${esc(label || '清理旧源')}</span>`;
    case 'submit_pt_download':
      return `<span class="mw-tag primary">${esc(label || 'PT下载')}</span>`;
    default:
      return `<span class="mw-tag">${esc(label || act)}</span>`;
  }
}

function fmtShortHistoryTime(tsStr) {
  const s = String(tsStr || '').trim();
  const m = s.match(/^\d{4}-(\d{2}-\d{2})\s+(\d{2}:\d{2})/);
  return m ? `${m[1]} ${m[2]}` : s || '-';
}

function splitHistorySlot(slotRaw) {
  const s = String(slotRaw || '').trim();
  if (!s || s === '-') return { cat: '-', slot: '' };
  const idx = s.indexOf(' · ');
  if (idx >= 0) {
    return {
      cat: s.slice(0, idx).trim() || '-',
      slot: s.slice(idx + 3).trim(),
    };
  }
  return { cat: s, slot: '' };
}

function parseHistoryPaths(it) {
  const rows = [];
  const srcPath = String(it.source_path || '').trim();
  const dstPath = String(it.target_path || '').trim();
  const isCleanOld =
    it.action === 'auto_clean_old' ||
    it.action === 'clean_old' ||
    it.action === 'clean_library_dup';

  if (isCleanOld) {
    if (srcPath) {
      rows.push({ badge: '删', cls: 'danger', text: srcPath, copy: srcPath, muted: true });
    }
    if (dstPath) {
      const cleanWin = dstPath.replace(/^保留胜出源[：:]\s*/, '').trim();
      rows.push({ badge: '留', cls: 'ok', text: cleanWin, copy: cleanWin, muted: false });
    }
    return rows;
  }

  if (srcPath) {
    rows.push({ badge: '源', cls: '', text: srcPath, copy: srcPath, muted: true });
  }
  if (dstPath) {
    const recMatch = dstPath.match(/^115\s*官方回收站[（(](库内已有|保留库内)[：:]\s*(.+?)[）)]$/);
    if (recMatch) {
      rows.push({
        badge: '终',
        cls: 'danger',
        text: `115 官方回收站（${recMatch[1]}）`,
        copy: '',
        muted: false,
      });
      const libKeep = recMatch[2].trim();
      if (libKeep) {
        rows.push({ badge: '库', cls: 'info', text: libKeep, copy: libKeep, muted: true });
      }
    } else {
      const canCopy = dstPath.startsWith('/') || dstPath.startsWith('115://');
      const cls =
        it.action === 'organized'
          ? it.status === 'warning'
            ? 'warn'
            : 'ok'
          : it.action === 'recycled_115'
            ? 'danger'
            : 'info';
      rows.push({ badge: '终', cls, text: dstPath, copy: canCopy ? dstPath : '', muted: false });
    }
  }
  return rows;
}

function fmtMultiLineHistoryDetail(it, isMedia) {
  const raw = String(it.detail || '').trim();
  if (!raw) return { head: '-', sub: '', warn: false };
  if (!isMedia) {
    const idx = raw.indexOf(' · ');
    if (idx > 0) {
      return { head: raw.slice(0, idx).trim(), sub: raw.slice(idx + 3).trim(), warn: it.status === 'warning' };
    }
    return { head: raw, sub: '', warn: it.status === 'warning' };
  }

  if (it.status === 'warning') {
    const errBody = raw.replace(/^主程序原生归档失败[：:]\s*/, '').trim();
    return { head: '归档异常', sub: errBody || raw, warn: true };
  }

  const isReorg = raw.includes('按最新命名模板重新整理');
  const scoreMatch = raw.match(/得分\s*([\d.]+)/);
  const byMatch = raw.match(/新入库媒体[（(]([^）)]+)[）)]/);

  if (isReorg) {
    return {
      head: scoreMatch ? `按新模板重整 · ${scoreMatch[1]}分` : '按新模板重整 STRM',
      sub: '已从目标目录重命名并刷新 STRM',
      warn: false,
    };
  }
  if (raw.includes('新入库媒体')) {
    return {
      head: scoreMatch ? `新片入库 · ${scoreMatch[1]}分` : '新片放行入库',
      sub: byMatch ? `${byMatch[1]} · 一步归档生成 STRM` : '一步移入二级分类并生成 STRM',
      warn: false,
    };
  }
  if (raw.includes('库内尚无')) {
    return {
      head: scoreMatch ? `多槽共存 · ${scoreMatch[1]}分` : '多槽位共存',
      sub: '库内尚无该槽位版本，并存保留',
      warn: false,
    };
  }
  const upgMatch = raw.match(/洗版胜出[：:]\s*([\d.]+)\s*>\s*库内\s*([\d.]+)（([+\-\d.]+)\s*≥\s*门槛\s*([\d.]+)）/);
  if (upgMatch) {
    return {
      head: `洗版胜出 · ${upgMatch[1]} > ${upgMatch[2]}`,
      sub: `分差 ${upgMatch[3]} ≥ ${upgMatch[4]} · 核验入库后删旧`,
      warn: false,
    };
  }
  if (raw.includes('相同 file_id') || raw.includes('已在 Mediary 媒体库内')) {
    return { head: '同源已在库', sub: '相同文件已在媒体库，直移回收站', warn: false };
  }
  if (raw.includes('字节数完全相等')) {
    return { head: '同体积已在库', sub: '字节数一致无需重入，直移回收站', warn: false };
  }
  const batchDup = raw.match(/同批次待洗版已有同槽更优\/同分胜出文件《([^》]+)》\(([\d.]+)\s*≥\s*([\d.]+)\)/);
  if (batchDup) {
    return {
      head: `同批重复 · ${batchDup[3]} ≤ ${batchDup[2]}`,
      sub: `保留同批胜出版本，重复移回收站`,
      warn: false,
    };
  }
  const loseMatch = raw.match(/洗版落败[：:]\s*候选\s*([\d.]+)\s*未超过库内\s*([\d.]+)\s*达最低分差\s*([\d.]+)（差值\s*([+\-\d.]+)）/);
  if (loseMatch) {
    return {
      head: `低分淘汰 · ${loseMatch[1]} < ${loseMatch[2]}`,
      sub: `差值 ${loseMatch[4]} (门槛 ${loseMatch[3]}) · 移回收站`,
      warn: false,
    };
  }
  if (raw.includes('已关闭「归档权重洗版」')) {
    return { head: '未开启洗版', sub: '库内已有该片，不覆盖并移回收站', warn: false };
  }
  if (raw.includes('命中评分排除条件')) {
    return { head: '命中排除规则', sub: '不符合准入条件，直移回收站', warn: false };
  }
  const cleanMatch = raw.match(/清理\s*(\d+)\s*个/);
  if (cleanMatch) {
    return { head: `清理旧源 ${cleanMatch[1]} 个`, sub: '新源已确认入库，安全移除旧源', warn: false };
  }

  const stripped = raw
    .replace(/^已通过主程序原生归档一步移入媒体库二级分类并生成 STRM\s*·\s*/, '')
    .replace(/^已直接移入 115 官方回收站\s*·\s*/, '');
  const splitIdx = stripped.indexOf(' · ');
  if (splitIdx > 0) {
    return { head: stripped.slice(0, splitIdx).trim(), sub: stripped.slice(splitIdx + 3).trim(), warn: false };
  }
  return { head: stripped, sub: '', warn: false };
}

function buildHistoryTooltip(it, isMedia) {
  const lines = [`【${it.title || '-'}】${it.action_label || it.action || ''}（${it.kind || '-'}）`];
  if (it.slot) {
    lines.push(`${isMedia ? '分类槽位' : '关联标识'}：${it.slot}`);
  }
  if (isMedia && it.sub_title && it.sub_title !== it.title) {
    lines.push(`原文件名：${it.sub_title}`);
  }
  if (it.source_path) {
    lines.push(`源路径：${it.source_path}`);
  }
  if (it.target_path) {
    lines.push(`终路径：${it.target_path}`);
  }
  if (it.detail) {
    lines.push(`详细说明：${it.detail}`);
  }
  if (it.updated_at) {
    lines.push(`处理时间：${it.updated_at}`);
  }
  return lines.join('\n');
}

function renderHistoryTab(root) {
  const hd = state.histData || { items: [], media_total: 0, task_total: 0, filtered_total: 0 };
  const items = hd.items || [];
  const isMedia = state.histType === 'media';
  const hasActiveFilter =
    Boolean(state.histKeyword) ||
    state.histKind !== 'all' ||
    (isMedia && state.histAction !== 'all') ||
    state.histStatus !== 'all';
  const clearBtnLabel = hasActiveFilter
    ? `清空当前筛选 (${Number(hd.filtered_total || 0)})`
    : isMedia
      ? `清空全部媒体历史 (${Number(hd.filtered_total || 0)})`
      : `清空全部任务日志 (${Number(hd.filtered_total || 0)})`;

  root.innerHTML = `
    <div class="mw-header">
      <div>
        <h2>历史记录管理（媒体处理历史 · 任务执行记录分离）</h2>
        <p>清晰展示每部影片/剧集的源路径与归档 STRM 目标路径；悬浮「执行说明」查看完整评分详情，点击任意路径可一键复制</p>
      </div>
      <div class="mw-actions">
        <button type="button" class="mw-btn" id="btn-hist-refresh">刷新列表</button>
      </div>
    </div>

    <div class="mw-card">
      <div class="mw-card-head">
        <div class="mw-pills">
          <button type="button" class="mw-pill ${isMedia ? 'active' : ''}" data-htype="media">
            🎬 媒体处理历史 (${Number(hd.media_total || 0)})
          </button>
          <button type="button" class="mw-pill ${!isMedia ? 'active' : ''}" data-htype="task">
            ⚙️ 任务执行记录 (${Number(hd.task_total || 0)})
          </button>
        </div>

        <div class="mw-actions">
          ${
            isMedia
              ? `<button type="button" class="mw-btn sm" id="btn-hist-repair-tmdbid" data-tip="自动扫描历史记录中所有缺失 [tmdbid-...] 的已归档条目，按同目录合并批量重新整理并清理空壳旧目录">
                   补齐缺 tmdbid 项
                 </button>`
              : ''
          }
          <button type="button" class="mw-btn sm primary" id="btn-hist-retry-selected">
            ${isMedia ? '重整/重试勾选' : '重试勾选'} (${state.histSelected.size})
          </button>
          <button type="button" class="mw-btn sm danger" id="btn-hist-del-selected">
            删除勾选 (${state.histSelected.size})
          </button>
          <button type="button" class="mw-btn sm ghost" id="btn-hist-clear-filtered">
            ${clearBtnLabel}
          </button>
        </div>
      </div>

      <div class="mw-hist-filter-bar">
        <select class="mw-select" id="sel-hist-kind" style="width:auto;">
          <option value="all" ${state.histKind === 'all' ? 'selected' : ''}>全部来源通道</option>
          <option value="115" ${state.histKind === '115' ? 'selected' : ''}>115 监控归档</option>
          ${isMedia ? `<option value="ledger" ${state.histKind === 'ledger' ? 'selected' : ''}>洗版删旧闭环</option>` : ''}
          <option value="library" ${state.histKind === 'library' ? 'selected' : ''}>存量库精洗</option>
          ${isMedia ? `<option value="pt" ${state.histKind === 'pt' ? 'selected' : ''}>PT 主动洗版</option>` : ''}
          ${!isMedia ? `<option value="subs" ${state.histKind === 'subs' ? 'selected' : ''}>远程规则订阅</option>` : ''}
        </select>

        ${
          isMedia
            ? `<select class="mw-select" id="sel-hist-act" style="width:auto;">
                 <option value="all" ${state.histAction === 'all' ? 'selected' : ''}>全部处理动作</option>
                 <option value="admit" ${state.histAction === 'admit' ? 'selected' : ''}>放行归档入库</option>
                 <option value="recycle" ${state.histAction === 'recycle' ? 'selected' : ''}>移入115回收站</option>
                 <option value="clean_old" ${state.histAction === 'clean_old' ? 'selected' : ''}>清理旧低分源</option>
                 <option value="pt" ${state.histAction === 'pt' ? 'selected' : ''}>PT 洗版下载</option>
               </select>`
            : ''
        }

        <select class="mw-select" id="sel-hist-status" style="width:auto;">
          <option value="all" ${state.histStatus === 'all' ? 'selected' : ''}>全部状态</option>
          <option value="done" ${state.histStatus === 'done' ? 'selected' : ''}>成功完成</option>
          <option value="warning" ${state.histStatus === 'warning' ? 'selected' : ''}>需关注告警</option>
        </select>

        <input
          class="mw-input"
          id="inp-hist-kw"
          value="${esc(state.histKeyword)}"
          placeholder="${isMedia ? '搜索片名、原文件名、目标 STRM 路径、二级分类、槽位或原因...' : '搜索任务名称、目录 CID 或扫描摘要...'}"
          style="flex:1;min-width:160px;"
        />
        <button type="button" class="mw-btn sm primary" id="btn-hist-search">筛选查找</button>
        ${
          hasActiveFilter
            ? `<button type="button" class="mw-btn sm ghost" id="btn-hist-reset">重置筛选</button>`
            : ''
        }
      </div>

      ${
        !items.length
          ? `<div class="mw-empty">${isMedia ? '暂无匹配的媒体处理历史记录。' : '暂无匹配的任务执行记录。'}</div>`
          : `<div class="mw-hist-table-wrap">
              <table class="mw-table mw-hist-table">
                <colgroup>
                  <col style="width: 26px" />
                  <col />
                  <col style="width: ${isMedia ? '15%' : '14%'}" />
                  <col style="width: 78px" />
                  <col style="width: ${isMedia ? '21%' : '31%'}" />
                  <col style="width: 74px" />
                  <col style="width: 92px" />
                </colgroup>
                <thead>
                  <tr>
                    <th><input type="checkbox" id="chk-hist-all" /></th>
                    <th>${isMedia ? '媒体名称 · 源 / 终路径（点击路径复制）' : '任务名称'}</th>
                    <th>${isMedia ? '二级分类 / 槽位' : '关联标识'}</th>
                    <th>动作 / 来源</th>
                    <th>执行说明</th>
                    <th>时间</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  ${items
                    .map((it) => {
                      const checked = state.histSelected.has(it.id) ? 'checked' : '';
                      const warnMark =
                        it.status === 'warning'
                          ? `<span class="mw-tag warn" style="padding:1px 5px;font-size:10px;">需关注</span>`
                          : '';
                      const pathRows = isMedia ? parseHistoryPaths(it) : [];
                      const { cat, slot } = splitHistorySlot(it.slot);
                      const detailObj = fmtMultiLineHistoryDetail(it, isMedia);
                      const fullTip = buildHistoryTooltip(it, isMedia);
                      const shortTime = fmtShortHistoryTime(it.updated_at);
                      const retryLabel = it.action === 'organized' ? '重整' : '重试';
                      const retryTip =
                        it.action === 'organized'
                          ? '若仍在待洗版则重评判；若已归档入库则从媒体库目标目录按最新命名模板重新整理'
                          : '重新执行该条记录';
                      return `
                        <tr>
                          <td><input type="checkbox" class="js-chk-hist" data-id="${it.id}" ${checked} /></td>
                          <td>
                            <div class="mw-hist-title-row" data-tip="${esc(it.title || '')}">
                              ${warnMark}
                              <span class="mw-hist-title-txt">${esc(it.title)}</span>
                            </div>
                            ${
                              isMedia && pathRows.length
                                ? `<div class="mw-hist-paths mw-mono">
                                     ${pathRows
                                       .map(
                                         (pr) => `
                                       <div class="mw-path-row ${pr.copy ? 'copyable js-copy-path' : ''}" ${pr.copy ? `data-copy="${esc(pr.copy)}" data-tip="点击复制路径：\n${esc(pr.copy)}"` : `data-tip="${esc(pr.text)}"`}>
                                         <span class="mw-path-badge ${pr.cls}">${esc(pr.badge)}</span>
                                         <span class="mw-path-txt ${pr.muted ? 'muted' : ''}">${esc(pr.text)}</span>
                                       </div>`
                                       )
                                       .join('')}
                                   </div>`
                                : it.sub_title && it.sub_title !== it.title
                                  ? `<div class="mw-muted mw-mono" style="margin-top:2px;font-size:11px;word-break:break-all;">${esc(it.sub_title)}</div>`
                                  : ''
                            }
                          </td>
                          <td>
                            ${
                              isMedia
                                ? `<div class="mw-slot-stack" data-tip="二级分类与版本槽位：${esc(it.slot || '-')}">
                                     <div class="mw-slot-cat">${esc(cat)}</div>
                                     ${slot ? `<span class="mw-tag primary">${esc(slot)}</span>` : ''}
                                   </div>`
                                : `<span class="mw-mono mw-muted" style="font-size:11px;word-break:break-all;">${esc(it.slot || '-')}</span>`
                            }
                          </td>
                          <td>
                            <div class="mw-status-stack">
                              ${historyActionBadge(it.action, it.action_label, it.status)}
                              <span class="mw-status-src">${esc(it.kind)}</span>
                            </div>
                          </td>
                          <td>
                            <div class="mw-detail-multi" data-tip="${esc(fullTip)}">
                              <div class="mw-detail-head ${detailObj.warn ? 'warn' : ''}">${esc(detailObj.head)}</div>
                              ${detailObj.sub ? `<div class="mw-detail-sub">${esc(detailObj.sub)}</div>` : ''}
                            </div>
                          </td>
                          <td>
                            <span class="mw-hist-time mw-mono" data-tip="完整时间：${esc(it.updated_at || '-')}">${esc(shortTime)}</span>
                          </td>
                          <td>
                            <div class="mw-hist-ops">
                              <button type="button" class="mw-btn sm primary js-hist-retry-one" data-id="${it.id}" data-tip="${esc(retryTip)}">${retryLabel}</button>
                              <button type="button" class="mw-btn sm danger js-hist-del-one" data-id="${it.id}">删除</button>
                            </div>
                          </td>
                        </tr>`;
                    })
                    .join('')}
                </tbody>
              </table>
            </div>`
      }
    </div>
  `;

  document.getElementById('btn-hist-refresh').onclick = async () => {
    await loadCurrentTabData();
    render();
  };

  root.querySelectorAll('.js-copy-path').forEach((rowEl) => {
    rowEl.onclick = async () => {
      const val = rowEl.dataset.copy || '';
      if (!val) return;
      await copyToClipboard(val);
      showToast('已复制路径到剪贴板', 'success');
    };
  });

  root.querySelectorAll('.mw-pill[data-htype]').forEach((btn) => {
    btn.onclick = async () => {
      state.histType = btn.dataset.htype;
      state.histKind = 'all';
      state.histAction = 'all';
      state.histStatus = 'all';
      state.histSelected.clear();
      await loadCurrentTabData();
      render();
    };
  });

  const applyFilter = async () => {
    state.histKind = document.getElementById('sel-hist-kind').value;
    const actEl = document.getElementById('sel-hist-act');
    if (actEl) state.histAction = actEl.value;
    const stEl = document.getElementById('sel-hist-status');
    if (stEl) state.histStatus = stEl.value;
    state.histKeyword = document.getElementById('inp-hist-kw').value.trim();
    state.histSelected.clear();
    await loadCurrentTabData();
    render();
  };

  document.getElementById('sel-hist-kind').onchange = applyFilter;
  const actEl = document.getElementById('sel-hist-act');
  if (actEl) actEl.onchange = applyFilter;
  const stEl = document.getElementById('sel-hist-status');
  if (stEl) stEl.onchange = applyFilter;
  document.getElementById('btn-hist-search').onclick = applyFilter;
  document.getElementById('inp-hist-kw').onkeydown = (e) => {
    if (e.key === 'Enter') applyFilter();
  };

  const resetBtn = document.getElementById('btn-hist-reset');
  if (resetBtn) {
    resetBtn.onclick = async () => {
      state.histKind = 'all';
      state.histAction = 'all';
      state.histStatus = 'all';
      state.histKeyword = '';
      state.histSelected.clear();
      await loadCurrentTabData();
      render();
    };
  }

  const chkAll = document.getElementById('chk-hist-all');
  if (chkAll) {
    chkAll.onchange = () => {
      if (chkAll.checked) items.forEach((it) => state.histSelected.add(it.id));
      else state.histSelected.clear();
      render();
    };
  }
  root.querySelectorAll('.js-chk-hist').forEach((chk) => {
    chk.onchange = () => {
      const id = Number(chk.dataset.id);
      if (chk.checked) state.histSelected.add(id);
      else state.histSelected.delete(id);
      render();
    };
  });

  const doDelete = async (ids, clearFiltered) => {
    const res = await callAction('ui_history_delete', {
      ids,
      clear_filtered: clearFiltered,
      record_type: state.histType,
      kind: state.histKind,
      action_group: isMedia ? state.histAction : 'all',
      status: state.histStatus,
      keyword: state.histKeyword,
      limit: 250,
    });
    state.histSelected.clear();
    state.histData = res;
    state.boot.counts = res.counts || state.boot.counts;
    syncSidebarBadges();
    showToast(`已删除 ${res.deleted || 0} 条记录`, 'success');
    render();
  };

  const doRetry = async (ids) => {
    let pendingIds = [...ids];
    let lastNotice = '';
    for (let round = 1; round <= 40 && pendingIds.length > 0; round++) {
      showToast(
        round === 1
          ? `正在执行重整 / 重试（共 ${pendingIds.length} 条）...`
          : `第 ${round - 1} 批已完成，正在继续处理剩余 ${pendingIds.length} 条记录...`,
        'info'
      );
      const res = await callAction('ui_history_retry', {
        ids: pendingIds,
        record_type: state.histType,
        kind: state.histKind,
        action_group: isMedia ? state.histAction : 'all',
        status: state.histStatus,
        keyword: state.histKeyword,
        limit: 250,
      });
      state.histData = res;
      state.boot.counts = res.counts || state.boot.counts;
      syncSidebarBadges();
      lastNotice = res.retry?.notice || '重试完成';
      pendingIds = res.retry?.remaining_ids || [];
      if (!pendingIds.length) state.histSelected.clear();
      render();
    }
    showToast(lastNotice || '重试完成', 'success');
  };

  root.querySelectorAll('.js-hist-retry-one').forEach((btn) => {
    btn.onclick = () => doRetry([Number(btn.dataset.id)]);
  });

  document.getElementById('btn-hist-retry-selected').onclick = () => {
    const ids = Array.from(state.histSelected);
    if (!ids.length) return showToast('请先勾选要重试的历史记录', 'error');
    doRetry(ids);
  };

  const repairBtn = document.getElementById('btn-hist-repair-tmdbid');
  if (repairBtn) {
    repairBtn.onclick = async () => {
      await runBusy('正在按目录分组批量补齐 [tmdbid-...] 标签...', async () => {
        let totalRepaired = 0;
        let lastNotice = '';
        for (let round = 1; round <= 20; round++) {
          const res = await bridge.runAction('ui_history_repair_tmdbid', {
            record_type: state.histType,
            kind: state.histKind,
            action_group: state.histAction,
            status: state.histStatus,
            keyword: state.histKeyword,
            limit: 200,
          });
          state.histData = res;
          if (res.counts && state.boot) state.boot.counts = res.counts;
          const rep = res.repair || {};
          totalRepaired += Number(rep.repaired || 0);
          if (rep.notice) lastNotice = rep.notice;
          render();
          if (!rep.has_more) break;
          setBusyText(`正在分批重整缺 [tmdbid] 条目（已完成 ${totalRepaired} 条，剩余 ${rep.remaining || 0} 条）...`);
        }
        showToast(lastNotice || `已完成 ${totalRepaired} 条记录的 [tmdbid] 模板重整`, 'success');
      });
    };
  }

  root.querySelectorAll('.js-hist-del-one').forEach((btn) => {
    btn.onclick = () => doDelete([Number(btn.dataset.id)], false);
  });

  document.getElementById('btn-hist-del-selected').onclick = () => {
    const ids = Array.from(state.histSelected);
    if (!ids.length) return showToast('请先勾选要删除的历史记录', 'error');
    doDelete(ids, false);
  };

  document.getElementById('btn-hist-clear-filtered').onclick = () => {
    const cnt = Number(hd.filtered_total || 0);
    if (!cnt) return showToast('当前筛选下没有可清空的记录', 'info');
    openModal(
      '确认清空历史记录',
      `
        <p style="margin:0 0 14px;font-size:13px;line-height:1.6;">
          确定要删除当前筛选条件下的 <strong>${cnt}</strong> 条${isMedia ? '媒体处理历史' : '任务执行日志'}吗？（仅清理日志记录，不会影响媒体库或 115 网盘文件）
        </p>
        <div class="mw-actions" style="justify-content:flex-end;">
          <button type="button" class="mw-btn" id="hist-clr-cancel">取消</button>
          <button type="button" class="mw-btn danger" id="hist-clr-confirm">确认清空 (${cnt})</button>
        </div>
      `,
      (body) => {
        body.querySelector('#hist-clr-cancel').onclick = closeModal;
        body.querySelector('#hist-clr-confirm').onclick = async () => {
          closeModal();
          await doDelete([], true);
        };
      }
    );
  };
}

// ============================================================================
// Global Floating Tooltip (MediFind-aligned) & Boot
// ============================================================================
function bindGlobalFloatingTooltip() {
  const tipEl = document.getElementById('mw-floating-tip');
  if (!tipEl) return;
  let activeTarget = null;

  const placeTip = (x, y) => {
    const pad = 12;
    const rect = tipEl.getBoundingClientRect();
    let left = x + 14;
    let top = y + 16;
    if (left + rect.width + pad > window.innerWidth) {
      left = Math.max(pad, x - rect.width - 14);
    }
    if (top + rect.height + pad > window.innerHeight) {
      top = Math.max(pad, y - rect.height - 12);
    }
    tipEl.style.left = `${left}px`;
    tipEl.style.top = `${top}px`;
  };

  document.addEventListener('mouseover', (e) => {
    const el = e.target.closest?.('[data-tip]');
    if (!el || !el.dataset.tip) {
      activeTarget = null;
      tipEl.classList.add('hidden');
      return;
    }
    activeTarget = el;
    tipEl.textContent = el.dataset.tip;
    tipEl.classList.remove('hidden');
    placeTip(e.clientX, e.clientY);
  });

  document.addEventListener('mousemove', (e) => {
    if (!activeTarget || tipEl.classList.contains('hidden')) return;
    placeTip(e.clientX, e.clientY);
  });

  document.addEventListener(
    'scroll',
    () => {
      activeTarget = null;
      tipEl.classList.add('hidden');
    },
    true
  );

  document.addEventListener('mousedown', () => {
    activeTarget = null;
    tipEl.classList.add('hidden');
  });
}

async function init() {
  bindGlobalFloatingTooltip();
  document.getElementById('mw-modal-close').onclick = closeModal;
  document.getElementById('mw-modal-backdrop').onclick = (e) => {
    if (e.target.id === 'mw-modal-backdrop') closeModal();
  };
  document.querySelectorAll('#mw-nav .mw-nav-btn').forEach((btn) => {
    btn.onclick = () => switchTab(btn.dataset.tab);
  });

  await bridge.getTheme();
  await refreshBootstrap();
  await loadCurrentTabData();
  render();
}

init().catch((err) => {
  const root = document.getElementById('mw-view');
  if (root) {
    root.innerHTML = `<div class="mw-empty">加载 MediWash 工作台失败：${esc(err.message || String(err))}</div>`;
  }
});

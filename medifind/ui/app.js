import { createMediaryBridge, copyToClipboard } from './bridge.js';

const bridge = createMediaryBridge();

const TABS = [
  { id: 'search', label: '聚合搜片' },
  { id: 'chase', label: '追更与待审' },
  { id: 'channels', label: '资源渠道' },
  { id: 'rules', label: '初筛与神盾' },
  { id: 'history', label: '转存历史' },
];

const DISCOVER_FILTER_LABELS = {
  type: '类型',
  sort: '排序',
  region: '地区',
  year: '年份',
  genre: '风格',
  status: '状态',
  troubleshoot: '诊断',
  channel: '频道',
};

const DISCOVER_FILTER_OPTIONS = {
  type: ['全部', '电影', '电视剧'],
  sort: ['更新', '热度', '上映', '评分', '连载', '完结'],
  region: ['全部', '欧美', '大陆', '港台', '韩国', '日本', '亚太'],
  year: ['全部', '2026', '2025', '2024', '2023', '2022', '2021', '2020', '2010年代', '2000年代', '90年代', '更早'],
  genre: ['全部', '剧情', '动作冒险', '喜剧', '科幻奇幻', '爱情家庭', '惊悚犯罪', '恐怖悬疑', '动漫', '少儿', '真人秀', '纪录片', '战争历史', '音乐会'],
  status: ['全部', '已入库', '未入库', '订阅中', '已忽略', '审核中'],
  troubleshoot: ['全部', '多版本', '单DV', 'ISO', 'STRM'],
};

const DEFAULT_DISCOVER_FILTERS = {
  type: '全部',
  sort: '更新',
  region: '全部',
  year: '全部',
  genre: '全部',
  status: '全部',
  troubleshoot: '全部',
  channel: '全部',
};

function loadSavedDiscoverFilters() {
  try {
    const raw = JSON.parse(localStorage.getItem('medifind_discover_filters') || '{}');
    if (raw && typeof raw === 'object') {
      const merged = { ...DEFAULT_DISCOVER_FILTERS, ...raw };
      if (!DISCOVER_FILTER_OPTIONS.troubleshoot.includes(merged.troubleshoot)) {
        merged.troubleshoot = '全部';
      }
      return merged;
    }
  } catch (_) {}
  return { ...DEFAULT_DISCOVER_FILTERS };
}

function saveDiscoverFilters(filters) {
  try {
    localStorage.setItem('medifind_discover_filters', JSON.stringify(filters));
  } catch (_) {}
}

function buildTmdbPosterSrc(posterPath, size = 'w342') {
  const p = String(posterPath || '').trim();
  if (!p) return '';
  const clean = p.startsWith('/') ? p : `/${p}`;
  return `./_tmdb/image/${size}${clean}`;
}

const state = {
  tab: 'search',
  boot: null,
  busy: false,
  searchQuery: '',
  discoverFilters: loadSavedDiscoverFilters(),
  discoverPage: 1,
  discoverTotalPages: 1,
  discoverTotalResults: 0,
  discoverPageSize: 24,
  tmdbItems: [],
  tmdbMode: 'discover',
  tmdbLoading: false,
  selectedMedia: null,
  aggResult: null,
  aggLoading: false,
  ruleTestInput: {
    text: '名称：《庆余年 第二季》 (2024) S02E01-E36 2160p WEB-DL DV&HDR TrueHD 特效双语中字 [tmdbid-12345]\nhttps://115.com/s/sw123456789?password=a8B9',
    size_str: '58.4GB',
    media_type: 'tv',
    source: 'telegram',
    unlocked: true,
    points: 0,
  },
  ruleTestOut: null,
  playTestOut: '',
  channelDraft: null,
  rulesDraft: null,
  dragTGIndex: null,
  dragRuleChip: null,
  dragWeightCat: null,
  addingGroupKw: false,
  newGroupKw: '',
  addingIgnoredTxt: false,
  newIgnoredTxt: '',
  subFilter: 'all',
  tgAuthPhone: '',
  tgAuthCode: '',
  tgAuthBusy: false,
  tgNetStatus: 'idle',
  histType: 'transfer',
  histSource: 'all',
  histAction: 'all',
  histStatus: 'all',
  histKeyword: '',
  histUnlockDedupe: true,
  histSelected: new Set(),
  histData: null,
  histLoading: false,
};

function esc(v) {
  return String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function fmtBytes(n) {
  if (!n || n <= 0) return '-';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0, v = Number(n);
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(i >= 2 ? 2 : 0)} ${units[i]}`;
}

let toastTimer = null;
function showToast(msg, kind = 'info') {
  const el = document.getElementById('toast');
  if (!msg) return el.classList.add('hidden');
  el.textContent = msg;
  el.className = `mf-toast ${kind}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add('hidden'), 5500);
}

function mergeServerBootIntoDrafts(actionName, bootRes, opts = {}) {
  const srvState = bootRes?.state || {};
  const srvChannels = srvState.telegram?.channels || [];

  if (opts.resetChannelDraft) {
    state.channelDraft = null;
  } else if (state.channelDraft) {
    const d = state.channelDraft;
    if (srvState.hdhive) {
      d.hdhive.username = srvState.hdhive.username;
      d.hdhive.points = srvState.hdhive.points;
      d.hdhive.protocol = srvState.hdhive.protocol;
    }
    if (actionName === 'ui_save_channels') {
      // Keep any blank or actively-typed unsaved rows while syncing persisted server channels
      const unsavedBlankRows = d.telegram.channels.filter((c) => {
        const raw = c._dirtyInput !== undefined ? c._dirtyInput : (c.identifier || c.name || '');
        return !cleanTGInputId(raw) || c._fetching;
      });
      const mappedSrv = srvChannels.map((c, idx) => ({
        id: c.id || `ch-${idx + 1}`,
        identifier: c.identifier || '',
        name: c.name || c.identifier || '',
        logo: c.logo || '',
        level: normalizeTGLevelJS(c.level),
        enabled: c.enabled !== false,
        hide_in_discover: !!c.hide_in_discover,
        cached_count: c.cached_count || 0,
        transferred_count: c.transferred_count || 0,
        _fetching: false,
        _deleting: false,
      }));
      d.telegram.channels = [...mappedSrv, ...unsavedBlankRows];
    } else {
      // Non-channel actions (e.g. RSS, notify test, TG check, sync): merge stats/logos without wiping draft rows
      const byId = new Map();
      const byNorm = new Map();
      srvChannels.forEach((sc) => {
        if (sc.id) byId.set(sc.id, sc);
        const nk = cleanTGInputId(sc.identifier || sc.name).toLowerCase();
        if (nk) byNorm.set(nk, sc);
      });
      d.telegram.channels.forEach((c) => {
        const nk = cleanTGInputId(c.identifier || c.name).toLowerCase();
        const sc = (c.id && byId.get(c.id)) || (nk && byNorm.get(nk));
        if (sc) {
          c.cached_count = sc.cached_count || 0;
          c.transferred_count = sc.transferred_count || 0;
          if (!c.logo && sc.logo) c.logo = sc.logo;
          if (c._dirtyInput === undefined) {
            if (sc.identifier) c.identifier = sc.identifier;
            if (sc.name) c.name = sc.name;
          }
        }
      });
    }
  }

  if (actionName === 'ui_save_rules' || opts.resetRulesDraft) {
    state.rulesDraft = null;
  } else if (state.rulesDraft) {
    syncDraftSubSourcesWithChannels(state.rulesDraft, srvChannels);
  }
}

async function callAction(name, payload = {}, fallbackMsg = '', opts = {}) {
  try {
    const res = await bridge.action(name, payload);
    if (res && res.state) {
      state.boot = res;
      mergeServerBootIntoDrafts(name, res, opts);
      updateSidebarStatus();
      if (!opts.skipRender) render();
      if (!opts.silent) {
        if (res.notice) showToast(res.notice, 'success');
        else if (fallbackMsg) showToast(fallbackMsg, 'success');
      }
    } else if (fallbackMsg && !opts.silent) {
      showToast(fallbackMsg, 'success');
    }
    return res;
  } catch (err) {
    showToast(err.message || String(err), 'error');
    throw err;
  }
}

function updateSidebarStatus() {
  const hs = state.boot?.host_status || {};
  const pill = document.getElementById('status-115');
  const note = document.getElementById('status-proxy');
  if (hs.open_api_connected || hs.web_cookie_ready) {
    const parts = [];
    if (hs.open_api_connected) parts.push('OpenAPI');
    if (hs.web_cookie_ready) parts.push('Cookie');
    pill.className = 'mf-pill success';
    pill.textContent = `115 已就绪 (${parts.join('+')})`;
  } else {
    pill.className = 'mf-pill warn';
    pill.textContent = '115 未连接或未授权';
  }
  const px = state.boot?.state?.custom_proxy || hs.proxy_url || '';
  note.textContent = px ? `代理: ${px}` : '代理: 直连 (未配系统代理)';
}

function renderNav() {
  const st = state.boot?.state || {};
  const histCnt = state.histData ? state.histData.transfer_total : st.transfer_history_total;
  const badges = {
    chase: (st.reviewing_queue || []).length || '',
    rules: st.shield_files_total || '',
    history: histCnt || '',
  };
  const nav = document.getElementById('nav');
  nav.innerHTML = TABS.map((t) => `
    <button class="mf-nav-btn ${state.tab === t.id ? 'active' : ''}" data-tab="${t.id}">
      <span>${esc(t.label)}</span>
      ${badges[t.id] ? `<span class="mf-nav-badge">${esc(badges[t.id])}</span>` : ''}
    </button>
  `).join('');
  nav.querySelectorAll('[data-tab]').forEach((btn) => {
    btn.onclick = () => {
      state.tab = btn.dataset.tab;
      if (state.tab === 'history') {
        loadHistory();
      } else {
        render();
      }
    };
  });
}

function render() {
  renderNav();
  const root = document.getElementById('view');
  if (!state.boot) {
    root.innerHTML = `<div class="mf-loading">正在连接 Mediary 宿主…</div>`;
    return;
  }
  switch (state.tab) {
    case 'search': renderSearchTab(root); break;
    case 'chase': renderChaseTab(root); break;
    case 'channels': renderChannelsTab(root); break;
    case 'rules': renderRulesTab(root); break;
    case 'history': renderHistoryTab(root); break;
  }
}

/* ==================== TAB 1: 聚合搜片 (NextFind 1:1 八维筛选栏 + 胶囊搜索框) ==================== */
function getVisibleDiscoverChannels() {
  const srcList = state.channelDraft?.telegram?.channels || state.boot?.state?.telegram?.channels || [];
  const out = [];
  const seen = new Set();
  for (const ch of srcList) {
    if (ch.enabled === false || ch.hide_in_discover) continue;
    const rawId = ch._dirtyInput !== undefined ? ch._dirtyInput : (ch.identifier || ch.name || '');
    const norm = cleanTGInputId(rawId);
    if (!norm) continue;
    const key = norm.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    const label = (ch.name || ch.identifier || norm).trim();
    out.push({ id: ch.id || key, label, value: label, norm: key });
  }
  return out;
}

function isDefaultDiscoverFilters(f) {
  return Object.keys(DEFAULT_DISCOVER_FILTERS).every((k) => (f[k] || DEFAULT_DISCOVER_FILTERS[k]) === DEFAULT_DISCOVER_FILTERS[k]);
}

function selectDiscoverFilter(category, opt) {
  const f = state.discoverFilters;
  f[category] = opt;

  // NextFind 1:1 互斥联动规则
  if (category === 'sort' && (opt === '连载' || opt === '完结')) {
    if (f.type === '电影') {
      f.type = '电视剧';
    }
  }
  if (category === 'type' && opt === '电影') {
    if (f.sort === '连载' || f.sort === '完结') {
      f.sort = '更新';
    }
    if (!['多版本', '单DV', 'ISO', 'STRM'].includes(f.troubleshoot)) {
      f.troubleshoot = '全部';
    }
  }
  if (category === 'troubleshoot' && opt !== '全部') {
    f.status = '已入库';
  }
  if (category === 'status' && opt !== '已入库') {
    f.troubleshoot = '全部';
  }

  saveDiscoverFilters(f);
  state.discoverPage = 1;
  state.selectedMedia = null;
  state.aggResult = null;
  loadTMDB(false);
}

function renderSearchTab(root) {
  const sel = state.selectedMedia;
  const agg = state.aggResult;
  const snap = agg?.library;
  const f = state.discoverFilters;
  const visibleChannels = getVisibleDiscoverChannels();

  if (f.channel !== '全部' && !visibleChannels.some((c) => c.value === f.channel || c.norm === cleanTGInputId(f.channel).toLowerCase())) {
    f.channel = '全部';
    saveDiscoverFilters(f);
  }

  const filterRowsHtml = Object.entries(DISCOVER_FILTER_OPTIONS).map(([cat, options]) => {
    const firstOpt = options[0];
    const firstLabel = firstOpt === '全部'
      ? `全部${DISCOVER_FILTER_LABELS[cat]}`
      : (cat === 'sort' && firstOpt === '更新' ? '更新排序' : firstOpt);
    const curVal = f[cat] || firstOpt;
    return `
      <div class="nf-filter-row">
        <button type="button" class="nf-filter-head ${curVal === firstOpt ? 'active' : ''}" data-nf-filter="${esc(cat)}" data-nf-val="${esc(firstOpt)}">${esc(firstLabel)}</button>
        <div class="nf-filter-opts">
          ${options.slice(1).map((opt) => `
            <button type="button" class="nf-filter-opt ${curVal === opt ? 'active' : ''}" data-nf-filter="${esc(cat)}" data-nf-val="${esc(opt)}">${esc(opt)}</button>
          `).join('')}
        </div>
      </div>
    `;
  }).join('');

  const channelRowHtml = visibleChannels.length > 0 ? `
    <div class="nf-filter-row">
      <button type="button" class="nf-filter-head ${f.channel === '全部' ? 'active' : ''}" data-nf-filter="channel" data-nf-val="全部">全部频道</button>
      <div class="nf-filter-opts">
        ${visibleChannels.map((ch) => `
          <button type="button" class="nf-filter-opt ${f.channel === ch.value ? 'active' : ''}" data-nf-filter="channel" data-nf-val="${esc(ch.value)}">${esc(ch.label)}</button>
        `).join('')}
      </div>
    </div>
  ` : '';

  const activeSummaryParts = [];
  if (f.channel !== '全部') activeSummaryParts.push(`频道: ${f.channel}`);
  if (f.type !== '全部') activeSummaryParts.push(f.type);
  if (f.region !== '全部') activeSummaryParts.push(f.region);
  if (f.year !== '全部') activeSummaryParts.push(f.year);
  if (f.genre !== '全部') activeSummaryParts.push(f.genre);
  if (f.status !== '全部') activeSummaryParts.push(f.status);
  if (f.troubleshoot !== '全部') activeSummaryParts.push(`诊断: ${f.troubleshoot}`);
  if (state.searchQuery.trim()) activeSummaryParts.push(`搜索: "${state.searchQuery.trim()}"`);

  root.innerHTML = `
    <div class="nf-discover-top">
      <div class="nf-discover-filters">
        ${filterRowsHtml}
        ${channelRowHtml}
      </div>

      <form id="search-form" class="nf-pill-search-wrap">
        <input id="search-input" type="text" class="nf-pill-search-input" placeholder="搜索电影、电视剧、TMDB ID，或直接粘贴 115 / hdhive / ed2k / magnet 链接..." value="${esc(state.searchQuery)}" ${state.tmdbLoading ? 'disabled' : ''} />
        <div class="nf-pill-search-actions">
          ${state.searchQuery && !state.tmdbLoading ? `
            <button type="button" id="btn-clear-search" class="nf-pill-icon-btn" title="清空搜索">
              <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1"><path d="M18 6L6 18M6 6l12 12" stroke-linecap="round"/></svg>
            </button>
          ` : ''}
          <button type="submit" class="nf-pill-icon-btn" title="搜索或解析直链" ${state.tmdbLoading ? 'disabled' : ''}>
            ${state.tmdbLoading
              ? `<svg class="nf-spin" width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="#3b82f6" stroke-width="2.4"><path d="M12 3a9 9 0 1 0 9 9" stroke-linecap="round"/></svg>`
              : `<svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1"><circle cx="11" cy="11" r="7"/><path d="M20 20l-3.6-3.6" stroke-linecap="round"/></svg>`}
          </button>
        </div>
      </form>
    </div>

    ${sel ? renderSelectedMediaPanel(sel, agg, snap) : ''}

    <div id="tmdb-list-card">
      <div class="nf-discover-bar">
        <div>
          ${state.tmdbLoading
            ? '正在检索影视与本地媒体库状态…'
            : `共 ${state.discoverTotalResults || state.tmdbItems.length} 部影视${activeSummaryParts.length ? ` · ${esc(activeSummaryParts.join(' / '))}` : ` · 按${esc(f.sort)}排序`}`}
        </div>
        <div class="mf-actions">
          ${(!isDefaultDiscoverFilters(f) || state.searchQuery.trim())
            ? `<button type="button" class="mf-btn sm" id="btn-reset-discover">重置全部筛选</button>`
            : ''}
          <button type="button" class="mf-btn sm" id="btn-refresh-discover" ${state.tmdbLoading ? 'disabled' : ''}>刷新列表</button>
        </div>
      </div>

      ${state.tmdbLoading
        ? `<div class="mf-card"><div class="mf-empty">正在加载影视列表…</div></div>`
        : state.tmdbItems.length === 0
          ? `<div class="mf-card"><div class="mf-empty">暂无符合当前筛选条件的影视条目，可点击上方「全部XX」放宽筛选或更换关键词</div></div>`
          : `<div class="mf-poster-grid">
              ${state.tmdbItems.map((it, idx) => {
                const pSrc = buildTmdbPosterSrc(it.poster_path);
                const scoreVal = Number(it.vote_average || 0);
                const isSelected = sel && ((it.tmdb_id && sel.tmdb_id === it.tmdb_id) || (!it.tmdb_id && sel.title === it.title));
                return `
                  <div class="mf-poster-card ${isSelected ? 'selected' : ''}" data-tmdb-idx="${idx}">
                    <div class="mf-poster-wrap">
                      ${it.status_label ? `<span class="mf-poster-badge ${esc(it.status_tone || '')}">${esc(it.status_label)}</span>` : ''}
                      ${scoreVal > 0 ? `<span class="mf-poster-score">★ ${scoreVal.toFixed(1)}</span>` : ''}
                      ${pSrc
                        ? `<img class="mf-poster-img" loading="lazy" src="${esc(pSrc)}" alt="${esc(it.title)}" />`
                        : `<div class="mf-poster-fallback">
                             <span style="font-size:20px">${it.media_type === 'tv' ? '📺' : '🎬'}</span>
                             <span>${esc(it.title)}</span>
                           </div>`}
                    </div>
                    <div class="mf-poster-body">
                      <div class="mf-poster-title">${esc(it.title)}</div>
                      <div class="mf-poster-meta">
                        <span>${it.media_type === 'tv' ? '剧集' : '电影'} · ${esc(it.year || '-')}</span>
                        <span>${it.tmdb_id ? `#${esc(it.tmdb_id)}` : '频道源'}</span>
                      </div>
                    </div>
                  </div>
                `;
              }).join('')}
            </div>
            ${state.discoverTotalPages > 1 ? `
              <div class="nf-pager">
                <button type="button" class="mf-btn sm" id="pg-first" ${state.discoverPage <= 1 || state.tmdbLoading ? 'disabled' : ''}>首页</button>
                <button type="button" class="mf-btn sm" id="pg-prev" ${state.discoverPage <= 1 || state.tmdbLoading ? 'disabled' : ''}>上一页</button>
                <span style="padding:0 8px;color:var(--mf-muted);font-size:12.5px">第 <strong style="color:var(--mf-text)">${state.discoverPage}</strong> / ${state.discoverTotalPages} 页</span>
                <button type="button" class="mf-btn sm" id="pg-next" ${state.discoverPage >= state.discoverTotalPages || state.tmdbLoading ? 'disabled' : ''}>下一页</button>
              </div>
            ` : ''}`}
    </div>
  `;

  root.querySelectorAll('[data-nf-filter]').forEach((btn) => {
    btn.onclick = () => {
      if (state.tmdbLoading) return;
      selectDiscoverFilter(btn.dataset.nfFilter, btn.dataset.nfVal);
    };
  });

  root.querySelector('#search-form').onsubmit = async (e) => {
    e.preventDefault();
    state.searchQuery = root.querySelector('#search-input').value.trim();
    if (state.searchQuery && isDirectLinkText(state.searchQuery)) {
      const mtype = state.discoverFilters.type === '电视剧' ? 'tv' : 'movie';
      state.selectedMedia = { title: '手动直链解析', tmdb_id: '', media_type: mtype, direct_query: state.searchQuery };
      await triggerAggregateSearch(false);
      return;
    }
    state.discoverPage = 1;
    state.selectedMedia = null;
    state.aggResult = null;
    await loadTMDB(!!state.searchQuery);
  };

  const clearSearchBtn = root.querySelector('#btn-clear-search');
  if (clearSearchBtn) {
    clearSearchBtn.onclick = async () => {
      state.searchQuery = '';
      state.discoverPage = 1;
      state.selectedMedia = null;
      state.aggResult = null;
      await loadTMDB(false);
    };
  }

  const resetBtn = root.querySelector('#btn-reset-discover');
  if (resetBtn) {
    resetBtn.onclick = async () => {
      state.discoverFilters = { ...DEFAULT_DISCOVER_FILTERS };
      saveDiscoverFilters(state.discoverFilters);
      state.searchQuery = '';
      state.discoverPage = 1;
      state.selectedMedia = null;
      state.aggResult = null;
      await loadTMDB(false);
    };
  }

  const refreshBtn = root.querySelector('#btn-refresh-discover');
  if (refreshBtn) {
    refreshBtn.onclick = () => loadTMDB(false);
  }

  const jumpPage = async (p) => {
    state.discoverPage = Math.max(1, Math.min(state.discoverTotalPages || 1, p));
    const mainEl = document.querySelector('.mf-main');
    if (mainEl) mainEl.scrollTo({ top: 0, behavior: 'smooth' });
    await loadTMDB(false);
  };
  const pgFirst = root.querySelector('#pg-first');
  if (pgFirst) pgFirst.onclick = () => jumpPage(1);
  const pgPrev = root.querySelector('#pg-prev');
  if (pgPrev) pgPrev.onclick = () => jumpPage(state.discoverPage - 1);
  const pgNext = root.querySelector('#pg-next');
  if (pgNext) pgNext.onclick = () => jumpPage(state.discoverPage + 1);

  root.querySelectorAll('.mf-poster-img').forEach((img) => {
    img.onerror = () => {
      const cardIdx = img.closest('[data-tmdb-idx]')?.dataset?.tmdbIdx;
      const it = cardIdx !== undefined ? state.tmdbItems[Number(cardIdx)] : null;
      const fb = document.createElement('div');
      fb.className = 'mf-poster-fallback';
      fb.innerHTML = `<span style="font-size:20px">${it?.media_type === 'tv' ? '📺' : '🎬'}</span><span>${esc(it?.title || img.alt || '')}</span>`;
      img.replaceWith(fb);
    };
  });

  root.querySelectorAll('[data-tmdb-idx]').forEach((card) => {
    card.onclick = async () => {
      const it = state.tmdbItems[Number(card.dataset.tmdbIdx)];
      if (!it) return;
      state.selectedMedia = it;
      const mainEl = document.querySelector('.mf-main');
      if (mainEl) mainEl.scrollTo({ top: 0, behavior: 'smooth' });
      await triggerAggregateSearch(false);
    };
  });

  bindResourceListEvents(root);
}

function renderSelectedMediaPanel(sel, agg, snap) {
  const items = agg?.items || [];
  const errs = agg?.errors || [];
  let libBadge = '';
  if (snap && snap.tmdb_id) {
    if (snap.media_type === 'movie') {
      libBadge = snap.in_library
        ? `<span class="mf-pill success">媒体库已有</span>`
        : `<span class="mf-pill warn">媒体库未入库</span>`;
    } else {
      const missCnt = (snap.missing_episodes || []).length;
      const locCnt = (snap.local_episodes || []).length;
      libBadge = `<span class="mf-pill ${missCnt > 0 ? 'warn' : 'success'}">本地已有 ${locCnt} 集 · 缺集 ${missCnt} 集</span>`;
    }
  }

  return `
    <div class="mf-card">
      <div class="mf-card-head">
        <div>
          <div class="mf-card-title">
            ${esc(sel.title)}
            ${sel.year ? `(${esc(sel.year)})` : ''}
            ${sel.tmdb_id ? `<span class="mf-pill info">TMDB ${esc(sel.tmdb_id)}</span>` : ''}
            ${libBadge}
          </div>
          <div class="mf-card-sub">
            ${snap?.missing_episodes?.length ? `待补缺集：${esc(snap.missing_episodes.slice(0, 14).join(', '))}${snap.missing_episodes.length > 14 ? ' …' : ''}` : '并发汇总已按优选规则打分去重'}
            ${errs.length ? ` · <span style="color:var(--mf-warn)">渠道提示: ${esc(errs.join(' | '))}</span>` : ''}
          </div>
        </div>
        <div class="mf-actions">
          <button class="mf-btn sm" id="btn-force-agg" ${state.aggLoading ? 'disabled' : ''}>强制刷新缓存</button>
          <button class="mf-btn sm" id="btn-clear-sel">收起</button>
        </div>
      </div>

      ${state.aggLoading
        ? `<div class="mf-empty">正在并发检索 HDHive · Telegram · AYClub · 神盾索引…</div>`
        : items.length === 0
          ? `<div class="mf-empty">未找到可用网盘资源（请确认已在「资源渠道」配置 HDHive Key 或启用 TG 频道）</div>`
          : `<div class="mf-res-list">
              ${items.map((it, idx) => renderResourceRow(it, idx)).join('')}
            </div>`}
    </div>
  `;
}

function renderResourceRow(it, idx) {
  const attrs = it.movie_attributes || {};
  const tags = [];
  tags.push(`<span class="mf-pill info">${esc(it.channel_name || it.source_type)}</span>`);
  tags.push(`<span class="mf-pill">${esc(it.pan_type.toUpperCase())}</span>`);
  if (attrs.resolution) tags.push(`<span class="mf-pill success">${esc(attrs.resolution)}</span>`);
  if (attrs.web_source) tags.push(`<span class="mf-pill stream">${esc(attrs.web_source)}</span>`);
  if (attrs.remux) tags.push(`<span class="mf-pill">${esc(attrs.remux)}</span>`);
  if (attrs.color) tags.push(`<span class="mf-pill info">${esc(attrs.color)}</span>`);
  if (attrs.audio) tags.push(`<span class="mf-pill">${esc(attrs.audio)}</span>`);
  if (attrs.subtitle) tags.push(`<span class="mf-pill success">${esc(attrs.subtitle)}</span>`);
  if (it.parsed_episodes) tags.push(`<span class="mf-pill warn">${esc(it.parsed_episodes)}</span>`);
  if (it.share_size) tags.push(`<span class="mf-pill">${esc(it.share_size)}</span>`);
  if (it.shield_matched) tags.push(`<span class="mf-pill success">🛡️ 神盾匹配</span>`);
  if (it.missing_eps_hit?.length) tags.push(`<span class="mf-pill warn">🆕 命中缺集 ${esc(it.missing_eps_hit.join(','))}</span>`);
  if (it.local_in_library) tags.push(`<span class="mf-pill">📦 本地已有</span>`);
  if (it.is_reviewing) tags.push(`<span class="mf-pill warn">⏳ 115审核中</span>`);
  if (!it.is_unlocked && it.unlock_points > 0) {
    tags.push(`<span class="mf-pill warn">🔒 需 ${it.unlock_points} 积分</span>`);
  } else {
    tags.push(`<span class="mf-pill success">免积分/已解锁</span>`);
  }
  tags.push(`<span class="mf-pill">评分 ${Number(it.rule_score || 0).toFixed(1)}</span>`);

  return `
    <div class="mf-res-item">
      <div class="mf-res-main">
        <div class="mf-res-title">${esc(it.title)}</div>
        <div class="mf-res-tags">${tags.join('')}</div>
        ${it.remark ? `<div class="mf-res-desc">${esc(it.remark)}</div>` : ''}
      </div>
      <div class="mf-actions">
        <button class="mf-btn sm" data-inspect-idx="${idx}">预览目录树</button>
        ${it.media_type === 'tv' && it.tmdb_id
          ? `<button class="mf-btn sm warn" data-transfer-missing-idx="${idx}">仅转缺集</button>`
          : ''}
        <button class="mf-btn sm primary" data-transfer-full-idx="${idx}">
          ${!it.is_unlocked && it.unlock_points > 0 ? `解锁转存(${it.unlock_points}分)` : '一键转存'}
        </button>
      </div>
    </div>
  `;
}

function bindResourceListEvents(root) {
  const forceBtn = root.querySelector('#btn-force-agg');
  if (forceBtn) forceBtn.onclick = () => triggerAggregateSearch(true);
  const clearBtn = root.querySelector('#btn-clear-sel');
  if (clearBtn) clearBtn.onclick = () => { state.selectedMedia = null; state.aggResult = null; render(); };

  root.querySelectorAll('[data-inspect-idx]').forEach((btn) => {
    btn.onclick = () => {
      const it = state.aggResult?.items?.[Number(btn.dataset.inspectIdx)];
      if (it) openInspectModal(it);
    };
  });
  root.querySelectorAll('[data-transfer-full-idx]').forEach((btn) => {
    btn.onclick = async () => {
      const it = state.aggResult?.items?.[Number(btn.dataset.transferFullIdx)];
      if (!it) return;
      btn.disabled = true;
      try {
        await callAction('ui_transfer_resource', {
          slug: it.slug,
          title: state.selectedMedia?.title || it.title,
          tmdb_id: state.selectedMedia?.tmdb_id || it.tmdb_id,
          media_type: state.selectedMedia?.media_type || it.media_type,
          source_type: it.source_type,
          channel_name: it.channel_name || '',
          unlock_points: it.unlock_points,
          allow_unlock: true,
          only_missing_episodes: false,
          trigger_reason: '工作台一键转存',
        });
      } finally {
        btn.disabled = false;
      }
    };
  });
  root.querySelectorAll('[data-transfer-missing-idx]').forEach((btn) => {
    btn.onclick = async () => {
      const it = state.aggResult?.items?.[Number(btn.dataset.transferMissingIdx)];
      if (!it) return;
      btn.disabled = true;
      try {
        await callAction('ui_transfer_resource', {
          slug: it.slug,
          title: state.selectedMedia?.title || it.title,
          tmdb_id: state.selectedMedia?.tmdb_id || it.tmdb_id,
          media_type: 'tv',
          source_type: it.source_type,
          channel_name: it.channel_name || '',
          unlock_points: it.unlock_points,
          allow_unlock: true,
          only_missing_episodes: true,
          trigger_reason: '工作台仅转缺集',
        });
      } finally {
        btn.disabled = false;
      }
    };
  });
}

let tmdbFetchSeq = 0;
async function loadTMDB(notifyToast = false) {
  const seq = ++tmdbFetchSeq;
  state.tmdbLoading = true;
  render();
  const f = state.discoverFilters;
  const q = state.searchQuery.trim();
  try {
    const res = await bridge.action('ui_search_tmdb', {
      query: q,
      type: f.type || '全部',
      sort: f.sort || '更新',
      region: f.region || '全部',
      year: f.year || '全部',
      genre: f.genre || '全部',
      status: f.status || '全部',
      troubleshoot: f.troubleshoot || '全部',
      channel: f.channel || '全部',
      page: state.discoverPage || 1,
      page_size: state.discoverPageSize || 24,
    });
    if (seq !== tmdbFetchSeq) return;
    const items = res?.items || [];
    state.tmdbItems = items;
    state.tmdbMode = res?.mode || (q ? 'search' : 'discover');
    state.discoverPage = Number(res?.page || state.discoverPage || 1);
    state.discoverTotalPages = Math.max(1, Number(res?.total_pages || 1));
    state.discoverTotalResults = Number(res?.total_results ?? items.length);
    if (notifyToast && q) {
      showToast(`找到 ${state.discoverTotalResults} 部匹配影视`, 'success');
    }
  } catch (err) {
    if (seq === tmdbFetchSeq) {
      showToast(err.message || String(err), 'error');
    }
  } finally {
    if (seq === tmdbFetchSeq) {
      state.tmdbLoading = false;
      render();
    }
  }
}

async function triggerAggregateSearch(forceRefresh = false) {
  const sel = state.selectedMedia;
  if (!sel) return;
  state.aggLoading = true;
  render();
  try {
    const res = await bridge.action('ui_aggregate_search', {
      query: sel.direct_query || sel.title,
      tmdb_id: sel.tmdb_id || '',
      media_type: sel.media_type || 'movie',
      title: sel.direct_query ? '' : sel.title,
      force_refresh: forceRefresh,
      live_tg: true,
    });
    state.aggResult = res;
  } catch (err) {
    showToast(err.message || String(err), 'error');
  } finally {
    state.aggLoading = false;
    render();
  }
}

/* ==================== 115 目录树预览与选集弹窗 ==================== */
async function openInspectModal(item) {
  const modalRoot = document.getElementById('modal-root');
  modalRoot.classList.remove('hidden');
  modalRoot.innerHTML = `
    <div class="mf-modal">
      <div class="mf-modal-head">
        <strong>正在解析目录树：${esc(item.title)}</strong>
        <button class="mf-btn sm" id="modal-close">关闭</button>
      </div>
      <div class="mf-modal-body"><div class="mf-empty">正在读取 115 分享树 / 解锁信息…</div></div>
    </div>
  `;
  modalRoot.querySelector('#modal-close').onclick = () => modalRoot.classList.add('hidden');

  try {
    const data = await bridge.action('ui_inspect_resource', {
      slug: item.slug,
      title: state.selectedMedia?.title || item.title,
      tmdb_id: state.selectedMedia?.tmdb_id || item.tmdb_id,
      media_type: state.selectedMedia?.media_type || item.media_type,
      source_type: item.source_type,
      unlock_points: item.unlock_points,
      allow_unlock: true,
    });

    if (data.is_reviewing) {
      modalRoot.querySelector('.mf-modal-body').innerHTML = `
        <div class="mf-empty">⏳ 该 115 分享当前处于官方「审核中」，您可直接点击下方按钮将其加入待审自动重试队列。</div>
      `;
      return;
    }

    const files = data.files || [];
    const defaultCID = state.boot?.state?.rules?.default_target_cid || '';
    const defaultDirName = resolveKnownCidName(defaultCID);
    modalRoot.innerHTML = `
      <div class="mf-modal">
        <div class="mf-modal-head">
          <div>
            <strong>${esc(data.precise_title || item.title)}</strong>
            ${data.movie_attributes?.web_source ? ` <span class="mf-pill stream">${esc(data.movie_attributes.web_source)}</span>` : ''}
            ${data.share_url ? `<div class="mf-card-sub">${esc(data.share_url)}</div>` : ''}
          </div>
          <div class="mf-actions">
            ${data.share_url ? `<button class="mf-btn sm" id="btn-copy-share">复制分享链接</button>` : ''}
            <button class="mf-btn sm" id="modal-close">关闭</button>
          </div>
        </div>
        <div class="mf-modal-body">
          <div class="mf-row" style="margin-bottom:10px">
            <button class="mf-btn sm" id="sel-all-video">全选视频+字幕</button>
            <button class="mf-btn sm warn" id="sel-missing-only">仅勾选媒体库缺集 (${data.missing_count || 0})</button>
            <button class="mf-btn sm" id="sel-none">取消全选</button>
            <div class="mf-row" style="margin-left:auto;gap:6px">
              <span id="inspect-cid-badge" class="mf-cid-badge ${defaultDirName ? '' : 'hidden'}">📂 ${esc(defaultDirName)}</span>
              <input id="inspect-target-cid" class="mf-input" style="width:190px" placeholder="选集接收 115 CID (留空走 FlowLink)" value="${esc(defaultCID)}" />
            </div>
          </div>
          ${files.length === 0
            ? `<div class="mf-empty">分享目录为空</div>`
            : files.map((f, i) => `
                <label class="mf-tree-row" style="padding-left:${8 + (f.depth || 0) * 18}px">
                  ${f.is_dir
                    ? `<span>📁</span><strong>${esc(f.name)}</strong>`
                    : `<input type="checkbox" class="mf-file-chk" data-fid="${esc(f.id)}" data-missing="${f.is_missing_in_db ? '1' : '0'}" data-vidsub="${f.is_video || f.is_subtitle ? '1' : '0'}" ${f.is_missing_in_db ? 'checked' : ''} />
                       <span>${f.is_video ? '🎬' : f.is_subtitle ? '💬' : '📄'}</span>
                       <span style="flex:1;word-break:break-all">${esc(f.name)}</span>
                       ${f.episode_label ? `<span class="mf-pill info">${esc(f.episode_label)}</span>` : ''}
                       ${f.is_video ? (f.is_missing_in_db ? `<span class="mf-pill warn">缺集</span>` : `<span class="mf-pill success">库内已有</span>`) : ''}
                       <span style="color:var(--mf-muted)">${fmtBytes(f.size)}</span>`
                  }
                </label>
              `).join('')}
        </div>
        <div class="mf-modal-foot">
          <span class="mf-card-sub">整包走 FlowLink 可留空 CID；勾选部分单集精准接收请填写或配置默认接收目录 CID</span>
          <div class="mf-actions">
            <button class="mf-btn" id="btn-submit-full">整包提交 FlowLink</button>
            <button class="mf-btn primary" id="btn-submit-selected">转存已勾选文件</button>
          </div>
        </div>
      </div>
    `;

    modalRoot.querySelector('#modal-close').onclick = () => modalRoot.classList.add('hidden');
    const copyBtn = modalRoot.querySelector('#btn-copy-share');
    if (copyBtn) copyBtn.onclick = async () => { await copyToClipboard(data.share_url); showToast('分享链接已复制', 'success'); };

    const chks = () => Array.from(modalRoot.querySelectorAll('.mf-file-chk'));
    modalRoot.querySelector('#sel-all-video').onclick = () => chks().forEach((c) => { c.checked = c.dataset.vidsub === '1'; });
    modalRoot.querySelector('#sel-missing-only').onclick = () => chks().forEach((c) => { c.checked = c.dataset.missing === '1'; });
    modalRoot.querySelector('#sel-none').onclick = () => chks().forEach((c) => { c.checked = false; });
    const cidInpModal = modalRoot.querySelector('#inspect-target-cid');
    const cidBadgeModal = modalRoot.querySelector('#inspect-cid-badge');
    if (cidInpModal && cidBadgeModal) {
      cidInpModal.oninput = () => {
        const nm = resolveKnownCidName(cidInpModal.value.trim());
        if (nm) {
          cidBadgeModal.textContent = `📂 ${nm}`;
          cidBadgeModal.classList.remove('hidden');
        } else {
          cidBadgeModal.classList.add('hidden');
        }
      };
    }

    modalRoot.querySelector('#btn-submit-full').onclick = async () => {
      await callAction('ui_transfer_resource', {
        slug: data.share_url || item.slug,
        title: state.selectedMedia?.title || item.title,
        tmdb_id: state.selectedMedia?.tmdb_id || item.tmdb_id,
        media_type: state.selectedMedia?.media_type || item.media_type,
        source_type: item.source_type,
        allow_unlock: true,
        trigger_reason: '预览弹窗整包 FlowLink 转存',
      });
      modalRoot.classList.add('hidden');
    };

    modalRoot.querySelector('#btn-submit-selected').onclick = async () => {
      const ids = chks().filter((c) => c.checked).map((c) => c.dataset.fid).filter(Boolean);
      if (ids.length === 0) return showToast('请至少勾选 1 个文件', 'error');
      const targetCID = modalRoot.querySelector('#inspect-target-cid').value.trim();
      await callAction('ui_transfer_resource', {
        slug: data.share_url || item.slug,
        title: state.selectedMedia?.title || item.title,
        tmdb_id: state.selectedMedia?.tmdb_id || item.tmdb_id,
        media_type: state.selectedMedia?.media_type || item.media_type,
        source_type: item.source_type,
        file_ids: ids,
        target_cid: targetCID,
        allow_unlock: true,
        trigger_reason: '预览弹窗选集转存',
      });
      modalRoot.classList.add('hidden');
    };
  } catch (err) {
    modalRoot.querySelector('.mf-modal-body').innerHTML = `<div class="mf-empty" style="color:var(--mf-danger)">解析失败：${esc(err.message || err)}</div>`;
  }
}

/* ==================== 115 目录浏览器点选 CID ==================== */
async function open115FolderPicker(onPick) {
  const modalRoot = document.getElementById('modal-root');
  modalRoot.classList.remove('hidden');
  let stack = [{ cid: '0', name: '115 根目录' }];

  const buildRelDirName = (leafName = '') => {
    const parts = stack.slice(1).map((s) => s.name);
    if (leafName) parts.push(leafName);
    return parts.join(' / ') || '115 根目录';
  };

  async function renderLevel() {
    const cur = stack[stack.length - 1];
    modalRoot.innerHTML = `
      <div class="mf-modal">
        <div class="mf-modal-head">
          <strong>📂 浏览选择 115 目录：${stack.map((s) => esc(s.name)).join(' / ')} <span style="color:var(--mf-muted);font-weight:400">(CID: ${esc(cur.cid)})</span></strong>
          <button class="mf-btn sm" id="picker-close">关闭</button>
        </div>
        <div class="mf-modal-body"><div class="mf-empty">正在读取 115 目录…</div></div>
        <div class="mf-modal-foot">
          <div>${stack.length > 1 ? `<button class="mf-btn sm" id="picker-up">⬆️ 返回上级</button>` : '<span class="mf-card-sub">点击文件夹名称进入下级，或直接点击右侧「选此目录」</span>'}</div>
          ${cur.cid !== '0' ? `<button class="mf-btn primary" id="picker-choose">选用当前目录 (${esc(cur.name)})</button>` : ''}
        </div>
      </div>
    `;
    modalRoot.querySelector('#picker-close').onclick = () => modalRoot.classList.add('hidden');
    const upBtn = modalRoot.querySelector('#picker-up');
    if (upBtn) upBtn.onclick = () => { stack.pop(); renderLevel(); };
    const chooseBtn = modalRoot.querySelector('#picker-choose');
    if (chooseBtn) {
      chooseBtn.onclick = () => {
        modalRoot.classList.add('hidden');
        onPick(cur.cid, buildRelDirName());
      };
    }

    try {
      const page = await bridge.action('ui_browse_115', { cid: cur.cid, offset: 0, limit: 200 });
      const dirs = (page.items || []).filter((x) => x.is_dir);
      const body = modalRoot.querySelector('.mf-modal-body');
      body.innerHTML = dirs.length === 0
        ? `<div class="mf-empty">当前目录下无子文件夹${cur.cid !== '0' ? '，可直接点击右下角「选用当前目录」' : ''}</div>`
        : dirs.map((d) => `
            <div class="mf-tree-row" style="justify-content:space-between">
              <div style="display:flex;align-items:center;gap:8px;flex:1;cursor:pointer" data-dir-cid="${esc(d.file_id)}" data-dir-name="${esc(d.name)}">
                <span>📁</span>
                <strong>${esc(d.name)}</strong>
                <span class="mf-pill" style="opacity:0.75">CID: ${esc(d.file_id)}</span>
              </div>
              <button type="button" class="mf-btn sm primary" data-pick-cid="${esc(d.file_id)}" data-pick-name="${esc(d.name)}">选此目录</button>
            </div>
          `).join('');
      body.querySelectorAll('[data-dir-cid]').forEach((row) => {
        row.onclick = () => {
          stack.push({ cid: row.dataset.dirCid, name: row.dataset.dirName });
          renderLevel();
        };
      });
      body.querySelectorAll('[data-pick-cid]').forEach((btn) => {
        btn.onclick = (e) => {
          e.stopPropagation();
          modalRoot.classList.add('hidden');
          onPick(btn.dataset.pickCid, buildRelDirName(btn.dataset.pickName));
        };
      });
    } catch (err) {
      modalRoot.querySelector('.mf-modal-body').innerHTML = `<div class="mf-empty" style="color:var(--mf-danger)">${esc(err.message || err)}</div>`;
    }
  }
  renderLevel();
}

/* ==================== TAB 2: 追更与待审 ==================== */
function renderChaseTab(root) {
  const st = state.boot.state;
  const subs = state.boot.subscriptions || [];
  const subErr = state.boot.subscriptions_error || '';
  const rev = st.reviewing_queue || [];
  const ignored = st.ignored_episodes || [];
  const recs = st.records || [];

  const normState = (s) => String(s.status || '').trim().toLowerCase();
  const activeSubs = subs.filter((s) => normState(s) !== 'completed' && normState(s) !== 'paused');
  const completedSubs = subs.filter((s) => normState(s) === 'completed');
  const filteredSubs = state.subFilter === 'active'
    ? activeSubs
    : state.subFilter === 'completed'
      ? completedSubs
      : subs;

  const fmtSubBadge = (s) => {
    const stVal = normState(s);
    if (stVal === 'completed') return `<span class="mf-pill success">已完结</span>`;
    if (stVal === 'paused') return `<span class="mf-pill warn">已暂停</span>`;
    return `<span class="mf-pill info">追更中</span>`;
  };

  root.innerHTML = `
    <div class="mf-header">
      <div>
        <h2>订阅追更与待审队列</h2>
        <p>${esc(st.last_result || '')}</p>
      </div>
      <div class="mf-actions">
        <button class="mf-btn" id="btn-goto-history">转存历史 (${st.transfer_history_total || recs.length || 0})</button>
        <button class="mf-btn" id="btn-checkin">每日签到 / 刷额度</button>
        <button class="mf-btn primary" id="btn-run-tick">立即执行追更巡检</button>
      </div>
    </div>

    <div class="mf-grid-4">
      <div class="mf-stat"><div class="mf-stat-label">Mediary 追更中 / 总订阅</div><div class="mf-stat-val">${activeSubs.length} <span style="font-size:13px;font-weight:500;color:var(--mf-muted)">/ ${subs.length}</span></div></div>
      <div class="mf-stat"><div class="mf-stat-label">115 审核待重试</div><div class="mf-stat-val">${rev.length}</div></div>
      <div class="mf-stat">
        <div style="display:flex;align-items:center;justify-content:space-between;gap:6px">
          <div class="mf-stat-label">TG 缓存资源数</div>
          <button type="button" class="mf-btn sm danger" id="btn-clear-tg-cache" style="padding:1px 7px;font-size:11px" data-tip="清空本地 SQLite 缓存的 Telegram 资源并重置频道抓取游标" ${!st.tg_cached_total ? 'disabled' : ''}>清空</button>
        </div>
        <div class="mf-stat-val">${st.tg_cached_total || 0}</div>
      </div>
      <div class="mf-stat">
        <div style="display:flex;align-items:center;justify-content:space-between;gap:6px">
          <div class="mf-stat-label">累计已完成转存</div>
          <button type="button" class="mf-btn sm warn" id="btn-reset-transfer-cnt" style="padding:1px 7px;font-size:11px" data-tip="将累计已完成转存统计数字清零（去重锁与历史记录管理请前往「转存历史」页）" ${!st.transferred_count ? 'disabled' : ''}>清零</button>
        </div>
        <div class="mf-stat-val">${st.transferred_count || 0}</div>
      </div>
    </div>

    ${rev.length > 0 ? `
      <div class="mf-card">
        <div class="mf-card-head">
          <div>
            <div class="mf-card-title">⏳ 115 审核中暂存队列（自动重试）</div>
            <div class="mf-card-sub">遇到 115 分享审核中自动暂存，每次巡检自动重试直至入库</div>
          </div>
          <button class="mf-btn sm warn" id="btn-retry-all">全部立即重试</button>
        </div>
        <table class="mf-table">
          <thead><tr><th>标题</th><th>来源</th><th>分享码</th><th>已重试</th><th>最近提示</th><th>操作</th></tr></thead>
          <tbody>
            ${rev.map((r) => `
              <tr>
                <td><strong>${esc(r.title)}</strong></td>
                <td>${esc(r.source_type)}</td>
                <td>${esc(r.share_code || r.slug)}</td>
                <td>${r.retry_count || 0} 次</td>
                <td>${esc(r.last_error || '等待重试')}</td>
                <td>
                  <button class="mf-btn sm" data-retry-slug="${esc(r.clean_slug)}">重试</button>
                  <button class="mf-btn sm danger" data-rm-rev="${esc(r.clean_slug)}">移除</button>
                </td>
              </tr>
            `).join('')}
          </tbody>
        </table>
      </div>
    ` : ''}

    <div class="mf-card">
      <div class="mf-card-head">
        <div>
          <div class="mf-card-title">Mediary 订阅联动补缺</div>
          <div class="mf-card-sub">对比宿主媒体库已有集数，仅针对未入库电影或缺失单集检索网盘资源</div>
        </div>
        <div class="mf-cat-tabs" style="margin-bottom:0">
          <button type="button" class="mf-cat-btn ${state.subFilter === 'all' ? 'active' : ''}" data-sub-filter="all">全部 (${subs.length})</button>
          <button type="button" class="mf-cat-btn ${state.subFilter === 'active' ? 'active' : ''}" data-sub-filter="active">追更中 (${activeSubs.length})</button>
          <button type="button" class="mf-cat-btn ${state.subFilter === 'completed' ? 'active' : ''}" data-sub-filter="completed">已完结 (${completedSubs.length})</button>
        </div>
      </div>
      ${subErr
        ? `<div class="mf-empty" style="color:var(--mf-warn)">读取 Mediary 订阅列表失败：${esc(subErr)}（请在插件中心确认已授予 subscriptions:read 权限）</div>`
        : filteredSubs.length === 0
          ? `<div class="mf-empty">${subs.length === 0 ? '当前 Mediary 暂无订阅条目' : '该筛选分类下暂无订阅条目'}</div>`
          : `<table class="mf-table">
              <thead><tr><th>订阅影视</th><th>类型</th><th>TMDB ID</th><th>季 / 收集进度</th><th>状态</th><th>操作</th></tr></thead>
              <tbody>
                ${filteredSubs.slice(0, 100).map((s, idx) => {
                  const isTv = (s.media_type || s.type) === 'tv' || s.season > 0;
                  const prog = isTv && s.expected_episodes > 0
                    ? ` · ${s.collected_episodes || 0}/${s.expected_episodes}集`
                    : (s.collected_episodes > 0 ? ` · 已收 ${s.collected_episodes}` : '');
                  return `
                    <tr>
                      <td><strong>${esc(s.title || s.name)}</strong>${s.year ? ` <span style="color:var(--mf-muted)">(${esc(s.year)})</span>` : ''}</td>
                      <td>${isTv ? '剧集' : '电影'}</td>
                      <td>${esc(s.tmdb_id)}</td>
                      <td>${isTv ? (s.season > 0 ? `S${s.season}${prog}` : `全季${prog}`) : (s.collected_episodes > 0 ? '已入库' : '待入库')}</td>
                      <td>${fmtSubBadge(s)}</td>
                      <td class="mf-actions">
                        <button class="mf-btn sm" data-jump-sub="${idx}">搜源预览</button>
                        <button class="mf-btn sm primary" data-chase-sub="${idx}">立即追更</button>
                      </td>
                    </tr>
                  `;
                }).join('')}
              </tbody>
            </table>`}
    </div>

    <div class="mf-card">
      <div class="mf-card-head">
        <div>
          <div class="mf-card-title">缺集忽略规则（可选）</div>
          <div class="mf-card-sub">指定某部剧整季或单集视为已存在，追更时跳过补缺</div>
        </div>
      </div>
      <form id="ignore-form" class="mf-row" style="margin-bottom:10px">
        <input id="ig-tmdb" class="mf-input" style="width:130px" placeholder="TMDB ID" required />
        <input id="ig-title" class="mf-input" style="width:160px" placeholder="备注片名(选填)" />
        <input id="ig-season" type="number" min="1" class="mf-input" style="width:100px" placeholder="季号(如1)" required />
        <input id="ig-ep" type="number" min="0" class="mf-input" style="width:140px" placeholder="集号(0代表整季)" value="0" />
        <button type="submit" class="mf-btn sm">添加忽略</button>
      </form>
      ${ignored.length > 0 ? `
        <div class="mf-res-tags">
          ${ignored.map((ig, i) => `
            <span class="mf-pill">
              ${esc(ig.title || `TMDB:${ig.tmdb_id}`)} S${ig.season}${ig.episode > 0 ? `E${ig.episode}` : '整季'}
              <button class="mf-btn sm danger" style="padding:0 5px;border:none" data-del-ig="${i}">×</button>
            </span>
          `).join('')}
        </div>
      ` : ''}
    </div>

    <div class="mf-card">
      <div class="mf-card-head">
        <div>
          <div class="mf-card-title">最近影视转存概览</div>
          <div class="mf-card-sub">多条件筛选、清除去重锁与重新转存请前往「转存历史」页操作</div>
        </div>
        <button type="button" class="mf-btn sm" id="btn-jump-history-card">筛选与管理全部历史 →</button>
      </div>
      ${recs.length === 0
        ? `<div class="mf-empty">暂无转存记录</div>`
        : `<table class="mf-table">
            <thead><tr><th>标题</th><th>来源</th><th>季集</th><th>动作</th><th>执行说明</th></tr></thead>
            <tbody>
              ${recs.slice(0, 8).map((r) => `
                <tr>
                  <td><strong>${esc(r.title)}</strong></td>
                  <td style="white-space:nowrap">${fmtHistorySourceBadge(r)}</td>
                  <td style="white-space:nowrap">${esc(r.episodes || '-')}</td>
                  <td style="white-space:nowrap"><span class="mf-pill ${r.status === 'success' ? 'success' : r.status === 'reviewing' ? 'warn' : 'danger'}">${esc(r.action)}</span></td>
                  <td style="white-space:nowrap">${fmtCompactDetailCell(r)}</td>
                </tr>
              `).join('')}
            </tbody>
          </table>`}
    </div>
  `;

  const gotoHist = () => {
    state.tab = 'history';
    loadHistory();
  };
  root.querySelector('#btn-goto-history').onclick = gotoHist;
  const clearTgBtn = root.querySelector('#btn-clear-tg-cache');
  if (clearTgBtn) clearTgBtn.onclick = () => callAction('ui_clear_tg_cache', {});
  const resetCntBtn = root.querySelector('#btn-reset-transfer-cnt');
  if (resetCntBtn) resetCntBtn.onclick = () => callAction('ui_reset_transfer_stats', {});
  root.querySelector('#btn-jump-history-card').onclick = gotoHist;
  root.querySelector('#btn-run-tick').onclick = () => callAction('ui_run_tick', {});
  root.querySelector('#btn-checkin').onclick = () => callAction('ui_daily_checkin', {});
  const retryAll = root.querySelector('#btn-retry-all');
  if (retryAll) retryAll.onclick = () => callAction('ui_retry_reviewing', {});

  root.querySelectorAll('[data-retry-slug]').forEach((b) => {
    b.onclick = () => callAction('ui_retry_reviewing', { clean_slug: b.dataset.retrySlug });
  });
  root.querySelectorAll('[data-rm-rev]').forEach((b) => {
    b.onclick = () => callAction('ui_remove_reviewing', { clean_slug: b.dataset.rmRev });
  });
  root.querySelectorAll('[data-sub-filter]').forEach((b) => {
    b.onclick = () => {
      state.subFilter = b.dataset.subFilter;
      renderChaseTab(root);
    };
  });
  root.querySelectorAll('[data-jump-sub]').forEach((b) => {
    b.onclick = async () => {
      const s = filteredSubs[Number(b.dataset.jumpSub)];
      if (!s) return;
      state.selectedMedia = {
        tmdb_id: String(s.tmdb_id || ''),
        title: s.title || s.name,
        media_type: (s.media_type || s.type) === 'tv' || s.season > 0 ? 'tv' : 'movie',
      };
      state.tab = 'search';
      await triggerAggregateSearch(false);
    };
  });
  root.querySelectorAll('[data-chase-sub]').forEach((b) => {
    b.onclick = () => {
      const s = filteredSubs[Number(b.dataset.chaseSub)];
      if (!s) return;
      callAction('ui_chase_subscription', {
        tmdb_id: String(s.tmdb_id || ''),
        title: s.title || s.name,
        media_type: (s.media_type || s.type) === 'tv' || s.season > 0 ? 'tv' : 'movie',
        season: s.season || 0,
      });
    };
  });

  root.querySelector('#ignore-form').onsubmit = (e) => {
    e.preventDefault();
    callAction('ui_save_ignored_ep', {
      tmdb_id: root.querySelector('#ig-tmdb').value.trim(),
      title: root.querySelector('#ig-title').value.trim(),
      media_type: 'tv',
      season: Number(root.querySelector('#ig-season').value || 1),
      episode: Number(root.querySelector('#ig-ep').value || 0),
    });
  };
  root.querySelectorAll('[data-del-ig]').forEach((b) => {
    b.onclick = () => {
      const ig = ignored[Number(b.dataset.delIg)];
      if (ig) callAction('ui_delete_ignored_ep', ig);
    };
  });
}

/* ==================== TAB 3: 资源渠道 (NextFind 1:1 TG 频道样式) ==================== */
function normalizeTGLevelJS(level) {
  const n = Number(level || 0);
  if (n === 1) return 16;
  if (n === 2) return 48;
  if (n === 3 || n === 99) return 99;
  if (n <= 0) return 0;
  const mask = n & (16 | 32 | 64);
  return mask ? (mask | 16) : 16;
}

function isTGLevelBitActive(level, bit) {
  const lv = normalizeTGLevelJS(level);
  if (!lv || lv === 0 || lv === 99) return false;
  return (lv & bit) === bit;
}

function toggleTGChannelLevel(ch, action) {
  let lv = normalizeTGLevelJS(ch.level);
  if (action === 0) {
    ch.level = 0;
    return;
  }
  if (action === 99) {
    ch.level = 99;
    return;
  }
  if (!lv || lv === 0 || lv === 99) lv = 0;
  if (action === 16) {
    if ((lv & 64) || (lv & 32)) {
      lv = 16;
    } else {
      lv ^= 16;
      if (!(lv & 16)) lv = 0;
    }
  } else if (action === 32) {
    lv ^= 32;
    if (lv & 32) lv |= 16;
  } else if (action === 64) {
    lv ^= 64;
    if (lv & 64) lv |= 16;
  }
  ch.level = lv || 0;
}

function cleanTGInputId(raw) {
  let s = String(raw || '').trim();
  if (!s) return '';

  // 1. Private channel message link: https://t.me/c/<digits>/<msg_id> -> -100<digits>
  const cMatch = s.match(/(?:https?:\/\/)?t\.me\/c\/(\d{5,})(?:\/|$|\?)/i);
  if (cMatch) {
    return `-100${cMatch[1]}`;
  }

  // 2. Invite link: t.me/+<hash> or t.me/joinchat/<hash> -> https://t.me/+<hash>
  const invMatch = s.match(/(?:https?:\/\/)?t\.me\/(?:\+|joinchat\/)([A-Za-z0-9_-]+)/i);
  if (invMatch) {
    return `https://t.me/+${invMatch[1]}`;
  }

  // 3. Only strip URL/username suffixes when input is an explicit URL or @username
  const low = s.toLowerCase();
  const isUrlOrHandle = low.startsWith('http://') || low.startsWith('https://') || low.startsWith('t.me/') || s.startsWith('@');
  if (isUrlOrHandle) {
    s = s.replace(/^https?:\/\/t\.me\/s\//i, '')
         .replace(/^https?:\/\/t\.me\//i, '')
         .replace(/^t\.me\/s\//i, '')
         .replace(/^t\.me\//i, '')
         .replace(/^@/, '');
    const cut = s.search(/[/?#]/);
    if (cut >= 0) s = s.slice(0, cut);
  }
  return s.trim();
}

function ensureChannelDraft() {
  if (state.channelDraft) return state.channelDraft;
  const st = state.boot.state;
  const hd = st.hdhive || {};
  const tg = st.telegram || {};
  const ay = st.ayclub || {};
  state.channelDraft = {
    custom_proxy: st.custom_proxy || '',
    hdhive: {
      enabled: !!hd.enabled,
      auto_checkin: hd.auto_checkin !== false,
      base_url: hd.base_url || 'https://hdhive.nextemby.com',
      api_key: hd.api_key || '',
      refresh_token: hd.refresh_token || '',
      max_unlock_points: hd.max_unlock_points ?? 10,
    },
    telegram: {
      enabled: tg.enabled !== false,
      expand_telegraph: tg.expand_telegraph !== false,
      bot_token: tg.bot_token || '',
      bot_chat_id: tg.bot_chat_id || '',
      channels: (tg.channels || []).map((c, idx) => ({
        id: c.id || `ch-${idx + 1}`,
        identifier: c.identifier || '',
        name: c.name || c.identifier || '',
        logo: c.logo || '',
        level: normalizeTGLevelJS(c.level),
        enabled: c.enabled !== false,
        hide_in_discover: !!c.hide_in_discover,
        cached_count: c.cached_count || 0,
        transferred_count: c.transferred_count || 0,
        _fetching: false,
        _deleting: false,
      })),
    },
    ayclub: {
      enabled: !!ay.enabled,
      base_url: ay.base_url || 'http://api.ayclub.vip:5050/api/next_find',
      nf_token: ay.nf_token || '',
      tg_user_id: ay.tg_user_id || '',
    },
    notifications: {
      enabled: st.notifications?.enabled !== false,
      on_transfer: st.notifications?.on_transfer !== false,
      on_rss_subscribe: st.notifications?.on_rss_subscribe !== false,
      on_checkin: st.notifications?.on_checkin !== false,
      include_poster: st.notifications?.include_poster !== false,
    },
  };
  return state.channelDraft;
}

function syncDraftFromChannelsDOM(root) {
  const d = ensureChannelDraft();
  const q = (sel) => root.querySelector(sel);
  if (q('#custom-proxy')) d.custom_proxy = q('#custom-proxy').value.trim();
  if (q('#hd-enabled')) d.hdhive.enabled = q('#hd-enabled').checked;
  if (q('#hd-checkin')) d.hdhive.auto_checkin = q('#hd-checkin').checked;
  if (q('#hd-url')) d.hdhive.base_url = q('#hd-url').value.trim();
  if (q('#hd-key')) d.hdhive.api_key = q('#hd-key').value.trim();
  if (q('#hd-refresh')) d.hdhive.refresh_token = q('#hd-refresh').value.trim();
  if (q('#hd-max-pts')) d.hdhive.max_unlock_points = Number(q('#hd-max-pts').value || 0);
  if (q('#tg-enabled')) d.telegram.enabled = q('#tg-enabled').checked;
  if (q('#tg-telegraph')) d.telegram.expand_telegraph = q('#tg-telegraph').checked;
  if (q('#tg-bot-token')) d.telegram.bot_token = q('#tg-bot-token').value.trim();
  if (q('#tg-bot-chat')) d.telegram.bot_chat_id = q('#tg-bot-chat').value.trim();
  if (q('#ay-enabled')) d.ayclub.enabled = q('#ay-enabled').checked;
  if (q('#ay-url')) d.ayclub.base_url = q('#ay-url').value.trim();
  if (q('#ay-token')) d.ayclub.nf_token = q('#ay-token').value.trim();
  if (q('#ay-tgid')) d.ayclub.tg_user_id = q('#ay-tgid').value.trim();
  if (q('#nt-enabled')) d.notifications.enabled = q('#nt-enabled').checked;
  if (q('#nt-transfer')) d.notifications.on_transfer = q('#nt-transfer').checked;
  if (q('#nt-rss')) d.notifications.on_rss_subscribe = q('#nt-rss').checked;
  if (q('#nt-checkin')) d.notifications.on_checkin = q('#nt-checkin').checked;
  if (q('#nt-poster')) d.notifications.include_poster = q('#nt-poster').checked;
}

function buildSaveChannelsPayload(d) {
  const channels = d.telegram.channels
    .map((c, idx) => {
      const idSource = c._dirtyInput !== undefined ? c._dirtyInput : (c.identifier || c.name);
      const idVal = cleanTGInputId(idSource);
      if (!idVal) return null;
      return {
        id: c.id || `ch-${idx + 1}`,
        identifier: idVal,
        name: (c.name || idVal).trim(),
        logo: c.logo || '',
        level: normalizeTGLevelJS(c.level),
        enabled: c.enabled !== false,
        hide_in_discover: !!c.hide_in_discover,
      };
    })
    .filter(Boolean);

  return {
    custom_proxy: d.custom_proxy,
    hdhive: d.hdhive,
    telegram: {
      enabled: d.telegram.enabled,
      channels,
      per_channel_limit: 40,
      expand_telegraph: d.telegram.expand_telegraph,
      bot_token: d.telegram.bot_token,
      bot_chat_id: d.telegram.bot_chat_id,
    },
    ayclub: d.ayclub,
    notifications: d.notifications,
  };
}

let channelAutoSaveTimer = null;
function scheduleAutoSaveChannels() {
  clearTimeout(channelAutoSaveTimer);
  channelAutoSaveTimer = setTimeout(async () => {
    if (!state.channelDraft) return;
    try {
      const res = await bridge.action('ui_save_channels', buildSaveChannelsPayload(state.channelDraft));
      if (res && res.state) {
        state.boot = res;
        mergeServerBootIntoDrafts('ui_save_channels', res, {});
        updateSidebarStatus();
        renderNav();
      }
    } catch (err) {
      showToast(`自动保存失败：${err.message || err}`, 'error');
    }
  }, 380);
}

async function openTGBatchImportModal(root) {
  const modalRoot = document.getElementById('modal-root');
  modalRoot.classList.remove('hidden');
  modalRoot.innerHTML = `
    <div class="mf-modal" style="width:min(680px, 95vw)">
      <div class="mf-modal-head">
        <strong>批量导入已加入的 Telegram 频道 / 群组</strong>
        <button type="button" class="mf-btn sm" id="tg-imp-close">关闭</button>
      </div>
      <div class="mf-modal-body"><div class="mf-empty">正在通过 MTProto 读取账号已加入的频道列表…</div></div>
    </div>
  `;
  modalRoot.querySelector('#tg-imp-close').onclick = () => modalRoot.classList.add('hidden');

  try {
    const res = await bridge.action('ui_tg_list_dialogs', {});
    const dialogs = res?.dialogs || [];
    const d = ensureChannelDraft();
    const existingSet = new Set(
      d.telegram.channels
        .map((c) => cleanTGInputId(c.identifier || c.name).toLowerCase())
        .filter(Boolean),
    );
    const selected = new Set();
    let keyword = '';
    let defaultLevel = 16; // 默认追更

    const renderList = () => {
      const kw = keyword.trim().toLowerCase();
      const filtered = dialogs.filter((it) => {
        if (!kw) return true;
        return `${it.name || ''} ${it.username || ''} ${it.identifier || ''}`.toLowerCase().includes(kw);
      });
      const selectableCount = filtered.filter((it) => !existingSet.has(cleanTGInputId(it.identifier).toLowerCase())).length;

      modalRoot.innerHTML = `
        <div class="mf-modal" style="width:min(680px, 95vw)">
          <div class="mf-modal-head">
            <div>
              <strong>批量导入已加入的 Telegram 频道 / 群组（共扫描到 ${dialogs.length} 个）</strong>
              <div class="mf-card-sub">勾选后一键加入订阅列表并自动保存；后续可在列表中单独点左侧圆形图标拉取高清头像</div>
            </div>
            <button type="button" class="mf-btn sm" id="tg-imp-close">关闭</button>
          </div>
          <div class="mf-modal-body">
            <div class="mf-row" style="margin-bottom:10px">
              <input id="tg-imp-filter" class="mf-input grow" placeholder="输入关键词过滤频道名或 @username…" value="${esc(keyword)}" />
              <select id="tg-imp-level" class="mf-select" style="width:146px">
                <option value="0" ${defaultLevel === 0 ? 'selected' : ''}>默认档位：手动</option>
                <option value="16" ${defaultLevel === 16 ? 'selected' : ''}>默认档位：追更</option>
                <option value="80" ${defaultLevel === 80 ? 'selected' : ''}>默认档位：入新+追更</option>
                <option value="48" ${defaultLevel === 48 ? 'selected' : ''}>默认档位：追更+洗版</option>
                <option value="99" ${defaultLevel === 99 ? 'selected' : ''}>默认档位：全量</option>
              </select>
              <button type="button" class="mf-btn sm" id="tg-imp-sel-all" ${selectableCount === 0 ? 'disabled' : ''}>全选未添加 (${selectableCount})</button>
              <button type="button" class="mf-btn sm" id="tg-imp-sel-none">清空勾选</button>
            </div>
            <div style="max-height:50vh;overflow-y:auto;border:1px solid var(--mf-border);border-radius:10px">
              ${filtered.length === 0
                ? `<div class="mf-empty">没有匹配的已加入频道</div>`
                : filtered.map((it) => {
                    const norm = cleanTGInputId(it.identifier).toLowerCase();
                    const already = existingSet.has(norm);
                    const checked = already || selected.has(norm);
                    return `
                      <label class="nf-dialog-item" style="${already ? 'opacity:0.58;cursor:default' : ''}">
                        <input type="checkbox" data-dlg-id="${esc(norm)}" ${checked ? 'checked' : ''} ${already ? 'disabled' : ''} />
                        <span class="mf-pill ${it.is_megagroup ? '' : 'info'}">${it.is_megagroup ? '群组' : '频道'}</span>
                        <strong style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(it.name)}</strong>
                        <span style="font-size:11.5px;color:var(--mf-muted)">${it.username ? `@${esc(it.username)}` : esc(it.identifier)}</span>
                        ${already ? `<span class="mf-pill success">已添加</span>` : ''}
                      </label>
                    `;
                  }).join('')}
            </div>
          </div>
          <div class="mf-modal-foot">
            <span class="mf-card-sub">当前已勾选 <strong>${selected.size}</strong> 个新频道</span>
            <div class="mf-actions">
              <button type="button" class="mf-btn" id="tg-imp-cancel">取消</button>
              <button type="button" class="mf-btn primary" id="tg-imp-confirm" ${selected.size === 0 ? 'disabled' : ''}>一键导入并保存 (${selected.size})</button>
            </div>
          </div>
        </div>
      `;

      const close = () => modalRoot.classList.add('hidden');
      modalRoot.querySelector('#tg-imp-close').onclick = close;
      modalRoot.querySelector('#tg-imp-cancel').onclick = close;

      const filterInp = modalRoot.querySelector('#tg-imp-filter');
      filterInp.oninput = () => {
        const pos = filterInp.selectionStart;
        keyword = filterInp.value;
        renderList();
        const nextInp = modalRoot.querySelector('#tg-imp-filter');
        if (nextInp) {
          nextInp.focus();
          try { nextInp.setSelectionRange(pos, pos); } catch (_) {}
        }
      };

      modalRoot.querySelector('#tg-imp-level').onchange = (e) => {
        defaultLevel = Number(e.target.value || 0);
      };

      modalRoot.querySelector('#tg-imp-sel-all').onclick = () => {
        filtered.forEach((it) => {
          const norm = cleanTGInputId(it.identifier).toLowerCase();
          if (norm && !existingSet.has(norm)) selected.add(norm);
        });
        renderList();
      };

      modalRoot.querySelector('#tg-imp-sel-none').onclick = () => {
        selected.clear();
        renderList();
      };

      modalRoot.querySelectorAll('[data-dlg-id]').forEach((chk) => {
        chk.onchange = () => {
          const k = chk.dataset.dlgId;
          if (chk.checked) selected.add(k);
          else selected.delete(k);
          renderList();
        };
      });

      modalRoot.querySelector('#tg-imp-confirm').onclick = async () => {
        const btn = modalRoot.querySelector('#tg-imp-confirm');
        btn.disabled = true;
        btn.textContent = '正在导入保存…';
        let addedCnt = 0;
        dialogs.forEach((it, i) => {
          const norm = cleanTGInputId(it.identifier).toLowerCase();
          if (!selected.has(norm) || existingSet.has(norm)) return;
          d.telegram.channels.push({
            id: `ch-${Date.now().toString(36)}-${i}`,
            identifier: it.identifier,
            name: it.name || it.identifier,
            logo: '',
            level: defaultLevel,
            enabled: true,
            hide_in_discover: false,
            cached_count: 0,
            transferred_count: 0,
            _fetching: false,
            _deleting: false,
          });
          existingSet.add(norm);
          addedCnt++;
        });
        try {
          await callAction('ui_save_channels', buildSaveChannelsPayload(d), `成功导入并保存 ${addedCnt} 个 Telegram 频道`);
          close();
        } catch (_) {
          btn.disabled = false;
          btn.textContent = `一键导入并保存 (${selected.size})`;
        }
      };
    };

    renderList();
  } catch (err) {
    modalRoot.querySelector('.mf-modal-body').innerHTML = `<div class="mf-empty" style="color:var(--mf-danger)">读取频道会话失败：${esc(err.message || err)}</div>`;
  }
}

function openTGAppIdModal() {
  const tg = state.boot?.state?.telegram || {};
  const modalRoot = document.getElementById('modal-root');
  modalRoot.classList.remove('hidden');
  modalRoot.innerHTML = `
    <div class="mf-modal" style="width:min(460px, 94vw)">
      <div class="mf-modal-head">
        <strong>Telegram 开发者凭证 (APPID)</strong>
        <button class="mf-btn sm" id="tg-api-close">关闭</button>
      </div>
      <div class="mf-modal-body">
        <div class="mf-card-sub" style="margin-bottom:12px">
          留空即默认使用 NextFind 同款内置官方 API ID (<code>3212490</code>)，绝大多数用户无需填写；如需使用自建 <a href="https://my.telegram.org/auth" target="_blank" rel="noreferrer" style="color:var(--mf-primary)">my.telegram.org</a> 应用可在此覆盖。
        </div>
        <div class="mf-field">
          <label>API ID（留空使用内置 3212490）</label>
          <input id="modal-tg-api-id" class="mf-input" value="${esc(tg.api_id || '')}" placeholder="默认内置 3212490" />
        </div>
        <div class="mf-field" style="margin-bottom:0">
          <label>API Hash（留空使用内置凭证）</label>
          <input id="modal-tg-api-hash" type="password" class="mf-input" value="${esc(tg.api_hash || '')}" placeholder="默认内置凭证" />
        </div>
      </div>
      <div class="mf-modal-foot">
        <button class="mf-btn sm" id="tg-api-reset">恢复默认内置凭证</button>
        <button class="mf-btn primary" id="tg-api-save">保存凭证</button>
      </div>
    </div>
  `;
  modalRoot.querySelector('#tg-api-close').onclick = () => modalRoot.classList.add('hidden');
  modalRoot.querySelector('#tg-api-reset').onclick = async () => {
    await callAction('ui_tg_save_api', { api_id: '', api_hash: '' });
    modalRoot.classList.add('hidden');
  };
  modalRoot.querySelector('#tg-api-save').onclick = async () => {
    const api_id = modalRoot.querySelector('#modal-tg-api-id').value.trim();
    const api_hash = modalRoot.querySelector('#modal-tg-api-hash').value.trim();
    await callAction('ui_tg_save_api', { api_id, api_hash });
    modalRoot.classList.add('hidden');
  };
}

function renderChannelsTab(root) {
  const st = state.boot.state;
  const hd = st.hdhive || {};
  const tg = st.telegram || {};
  const ay = st.ayclub || {};
  const feeds = st.rss_feeds || [];
  const d = ensureChannelDraft();

  const tgLoggedIn = !!tg.is_logged_in;
  const tgPwdReq = !!tg.password_required;
  const tgHasPending = !!st.tg_has_pending_code || tgPwdReq;
  const phoneVal = state.tgAuthPhone || tg.pending_phone || '';
  const codePlaceholder = tgPwdReq
    ? `请输入 Telegram 两步验证密码${tg.password_hint ? ` (提示: ${tg.password_hint})` : ''}`
    : (tgHasPending ? '输入收到的 Telegram 验证码' : '获取验证码后在此输入');
  const authBtnText = state.tgAuthBusy
    ? '请求中…'
    : (tgPwdReq ? '两步验证' : (state.tgAuthCode.trim() ? '确认登录' : '获取验证码'));

  const curHdUrl = (d.hdhive.base_url || hd.base_url || '').toLowerCase();
  const isRelay = hd.protocol === 'relay' || (curHdUrl && !curHdUrl.includes('hdhive.'));

  root.innerHTML = `
    <div class="mf-header">
      <div>
        <h2>多源资源渠道管理</h2>
        <p>所有凭证与会话安全保存在插件私有目录并在前端自动脱敏，默认自动复用 Mediary 宿主系统代理</p>
      </div>
      <button class="mf-btn primary" id="btn-save-channels">保存全部渠道配置</button>
    </div>

    <!-- 1. NextFind 同款 Telegram 账号登录与频道订阅卡片 -->
    <div class="mf-card" style="padding:20px 22px">
      <div class="mf-card-head" style="margin-bottom:14px">
        <div>
          <div class="nf-tg-head-title">
            <div class="nf-tg-icon-box">
              <svg width="18" height="18" viewBox="0 0 1024 1024" fill="currentColor"><path d="M883.626667 155.946667s82.858667-32.341333 75.946666 46.165333c-2.261333 32.298667-22.997333 145.408-39.125333 267.776l-55.253333 362.453333s-4.608 53.077333-46.08 62.293334c-41.386667 9.258667-103.552-32.298667-115.072-41.514667-9.216-6.912-172.672-110.805333-230.272-161.621333-16.085333-13.824-34.517333-41.557333 2.346666-73.856l241.749334-230.826667c27.605333-27.733333 55.253333-92.373333-59.861334-13.866667l-322.346666 219.306667s-36.864 23.04-105.898667 2.304l-149.674667-46.165333s-55.253333-34.645333 39.125334-69.248C349.482667 370.56 632.661333 259.84 883.626667 155.904z"></path></svg>
            </div>
            <span>Telegram 自动订阅</span>
            <span class="mf-pill ${tgLoggedIn ? 'success' : 'warn'}">${tgLoggedIn ? `MTProto 已登录 · 缓存 ${st.tg_cached_total || 0} 条` : '未登录账号（仅限公开 @用户名）'}</span>
          </div>
          <div class="mf-card-sub" style="margin-top:4px">登录 Telegram 账号后可像 NextFind 一样直接输入<strong>已加入的私密/公开频道中文名称、数字 ID 或邀请链接</strong>按回车识别头像与抓源</div>
        </div>
        <div class="mf-actions">
          <label class="mf-row" style="font-size:12.5px"><input type="checkbox" id="tg-telegraph" ${d.telegram.expand_telegraph ? 'checked' : ''} /> 展开 Telegra.ph</label>
          <button type="button" class="mf-btn sm" id="btn-tg-sync">立即抓取更新</button>
          <label class="mf-row"><input type="checkbox" id="tg-enabled" ${d.telegram.enabled ? 'checked' : ''} /> 启用</label>
        </div>
      </div>

      <!-- NextFind 1:1 手机号 + APPID + 验证码/退出登录 顶栏 -->
      <div class="nf-tg-auth-grid">
        <div class="nf-tg-auth-col">
          <div class="nf-float-box">
            <label class="nf-float-label">手机号</label>
            ${tgLoggedIn
              ? `<div class="nf-auth-badge">
                   <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4"><path d="M20 6L9 17l-5-5" stroke-linecap="round" stroke-linejoin="round"/></svg>
                   <span class="truncate">已登录${tg.tg_username ? ` · ${esc(tg.tg_username)}` : (tg.tg_phone ? ` · ${esc(tg.tg_phone)}` : '')}</span>
                 </div>
                 <button type="button" id="btn-tg-net-check" class="nf-net-dot ${esc(state.tgNetStatus)}" title="点击检测 Telegram MTProto 连接状态">
                   ${state.tgNetStatus === 'checking'
                     ? `<svg class="nf-spin" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="#3b82f6" stroke-width="2.8"><path d="M12 3a9 9 0 1 0 9 9"/></svg>`
                     : state.tgNetStatus === 'connected'
                       ? `<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="#22c55e" stroke-width="3"><path d="M5 13l4 4L19 7" stroke-linecap="round" stroke-linejoin="round"/></svg>`
                       : state.tgNetStatus === 'error'
                         ? `<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="#ef4444" stroke-width="3"><path d="M6 18L18 6M6 6l12 12" stroke-linecap="round"/></svg>`
                         : `<span style="width:6px;height:6px;border-radius:50%;background:#22c55e;display:block"></span>`}
                 </button>`
              : `<input id="tg-auth-phone" type="tel" class="nf-tg-input" placeholder="+8613800138000 (支持直接填 11 位手机号)" value="${esc(phoneVal)}" ${state.tgAuthBusy ? 'disabled' : ''} />`}
          </div>
          <button type="button" id="btn-tg-appid" class="nf-appid-btn" title="自定义 Telegram API ID / Hash">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9c.26.6.77 1.02 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>
            <span>APPID</span>
          </button>
        </div>

        <div class="nf-tg-auth-col">
          ${!tgLoggedIn
            ? `<div class="nf-float-box">
                 <input id="tg-auth-code" type="${tgPwdReq ? 'password' : 'text'}" class="nf-tg-input" placeholder="${esc(codePlaceholder)}" value="${esc(state.tgAuthCode)}" ${state.tgAuthBusy ? 'disabled' : ''} />
                 ${tgHasPending ? `<button type="button" id="btn-tg-resend" class="mf-btn sm" style="padding:2px 7px;font-size:11px;flex-shrink:0" ${state.tgAuthBusy ? 'disabled' : ''}>重发</button>` : ''}
               </div>
               <button type="button" id="btn-tg-auth-submit" class="nf-auth-btn ${tgPwdReq ? 'purple' : ''}" ${state.tgAuthBusy ? 'disabled' : ''}>
                 ${esc(authBtnText)}
               </button>`
            : `<div class="nf-float-box" style="justify-content:space-between">
                 <span style="font-size:12.5px;color:var(--mf-muted)">已加入私密/公开频道均支持直接按中文名或 ID 识别</span>
               </div>
               <button type="button" id="btn-tg-logout" class="nf-logout-btn" ${state.tgAuthBusy ? 'disabled' : ''}>
                 <span>退出登录</span>
               </button>`}
        </div>
      </div>

      <div class="nf-tg-list" id="nf-tg-list">
        ${d.telegram.channels.map((ch, idx) => {
          const lv = normalizeTGLevelJS(ch.level);
          const on64 = isTGLevelBitActive(lv, 64);
          const on16 = isTGLevelBitActive(lv, 16);
          const on32 = isTGLevelBitActive(lv, 32);
          const displayVal = ch._dirtyInput !== undefined ? ch._dirtyInput : (ch.name || ch.identifier || '');
          const showIdBadge = ch.identifier && ch.name && ch.name !== ch.identifier && ch._dirtyInput === undefined;
          const idBadgeText = ch.identifier && ch.identifier.startsWith('-') ? '私密频道' : `@${ch.identifier}`;
          return `
            <div class="nf-tg-row" data-tg-row="${idx}">
              <!-- Left Half: Avatar/Search + Input + Purple Stats + Discover Toggle -->
              <div class="nf-tg-left ${ch._deleting ? 'deleting' : ''}">
                <button type="button" class="nf-tg-avatar" data-tg-fetch="${idx}" title="点击搜索识别频道头像与名称">
                  ${ch._fetching
                    ? `<svg class="nf-spin" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="#3b82f6" stroke-width="2.6"><path d="M12 3a9 9 0 1 0 9 9" stroke-linecap="round"/></svg>`
                    : ch.logo
                      ? `<img src="${esc(ch.logo)}" alt="" />`
                      : `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><circle cx="11" cy="11" r="7"></circle><path d="M20 20l-3.5-3.5" stroke-linecap="round"></path></svg>`}
                </button>

                <input type="text" class="nf-tg-input" data-tg-input="${idx}" value="${esc(displayVal)}" placeholder="${tgLoggedIn ? '输入已加入频道中文名、@用户名或数字 ID (回车识别)' : '输入公开频道 @用户名 (私密/中文名请先登录上方 TG)'}" />
                ${showIdBadge ? `<span class="nf-tg-id-tag" title="${esc(ch.identifier)}">${esc(idBadgeText)}</span>` : ''}

                <div class="nf-tg-stats" data-tip="有效转存数 / 捕获链接数">
                  <span>${ch.transferred_count || 0}</span><span style="margin:0 2px">/</span><span>${ch.cached_count || 0}</span>
                </div>

                <button type="button" class="nf-tg-toggle ${ch.hide_in_discover ? 'hidden-mode' : ''}" data-tg-vis="${idx}" data-tip="${ch.hide_in_discover ? '已在搜片页隐藏 (后台雷达静默生效)' : '在搜片页展示'}">
                  <span class="nf-tg-toggle-knob"></span>
                </button>
              </div>

              <!-- Right Half: 5-Segment Level + Two-Step Delete + Drag Handle -->
              <div class="nf-tg-right">
                <div class="nf-seg-bar">
                  <button type="button" class="nf-seg-btn ${lv === 0 ? 'active' : ''}" data-tg-lvl="${idx}:0">手动</button>
                  <button type="button" class="nf-seg-btn ${on64 ? 'active' : ''} ${on64 && on16 ? 'join-right' : ''}" data-tg-lvl="${idx}:64">入新</button>
                  <button type="button" class="nf-seg-btn ${on16 ? 'active' : ''} ${on16 && on64 ? 'join-left' : ''} ${on16 && on32 ? 'join-right' : ''}" data-tg-lvl="${idx}:16">追更</button>
                  <button type="button" class="nf-seg-btn ${on32 ? 'active' : ''} ${on32 && on16 ? 'join-left' : ''}" data-tg-lvl="${idx}:32">洗版</button>
                  <button type="button" class="nf-seg-btn ${lv === 99 ? 'active' : ''}" data-tg-lvl="${idx}:99">全量</button>
                </div>

                <div class="nf-tg-ops" data-tg-ops="${idx}">
                  ${!ch._deleting
                    ? `<button type="button" class="nf-tg-icon-btn" data-tg-del-arm="${idx}" title="删除频道">
                         <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 12a9 9 0 11-18 0 9 9 0 0118 0z"></path><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2.6" d="M15 12H9"></path></svg>
                       </button>`
                    : `<button type="button" class="nf-tg-icon-btn nf-tg-del-confirm" data-tg-del-confirm="${idx}" data-tip="确认删除">
                         <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path stroke-linecap="round" stroke-linejoin="round" d="M3 6h18M8 6V4a1 1 0 011-1h6a1 1 0 011 1v2m2 0v14a2 2 0 01-2 2H8a2 2 0 01-2-2V6h12zM10 11v6M14 11v6"/></svg>
                       </button>`}

                  <div class="nf-tg-drag" draggable="true" data-tg-drag="${idx}" title="按住拖动排序">
                    <svg width="19" height="19" viewBox="0 0 1024 1024" style="transform: rotate(90deg)" fill="currentColor"><path d="M856.2 442.7H167.1c-22.1 0-40-17.9-40-40s17.9-40 40-40h689.1c22.1 0 40 17.9 40 40s-17.9 40-40 40z"></path><path d="M856.9 442.6c-10.6 0-21.1-4.2-29-12.4L674.4 268.9c-15.2-16-14.6-41.3 1.4-56.6 16-15.2 41.3-14.6 56.6 1.4L885.9 375c15.2 16 14.6 41.3-1.4 56.6-7.8 7.4-17.7 11-27.6 11z"></path><path d="M856.9 661.3H167.8c-22.1 0-40-17.9-40-40s17.9-40 40-40h689.1c22.1 0 40 17.9 40 40s-17.9 40-40 40z"></path><path d="M320.6 822.7c-10.6 0-21.1-4.2-29-12.4L138.1 649c-15.2-16-14.6-41.3 1.4-56.6 16-15.2 41.3-14.6 56.6 1.4l153.5 161.3c15.2 16 14.6 41.3-1.4 56.6-7.7 7.4-17.7 11-27.6 11z"></path></svg>
                  </div>
                </div>
              </div>
            </div>
          `;
        }).join('')}
      </div>

      <div class="nf-tg-add-row">
        <button type="button" class="nf-tg-add" id="btn-add-tg-channel">
          <span style="font-size:18px;line-height:1">+</span>
          <span>添加频道</span>
        </button>
        <button type="button" class="nf-tg-import-btn" id="btn-batch-import-tg" title="从当前已登录 Telegram 账号一键勾选导入已加入的公开/私密频道与群组">
          <span>📥</span>
          <span>批量导入已加入频道</span>
        </button>
      </div>

      <details style="margin-top:12px">
        <summary style="cursor:pointer;font-size:12px;color:var(--mf-muted)">可选：独立 Telegram Bot 通知测试（默认已接入 Mediary 宿主通知）</summary>
        <div class="mf-row" style="margin-top:8px">
          <div class="mf-field grow" style="margin-bottom:0">
            <label>Bot Token</label>
            <input id="tg-bot-token" type="password" class="mf-input" value="${esc(d.telegram.bot_token)}" placeholder="选填，默认走 Mediary 通知" />
          </div>
          <div class="mf-field" style="width:140px;margin-bottom:0">
            <label>Chat ID</label>
            <input id="tg-bot-chat" class="mf-input" value="${esc(d.telegram.bot_chat_id)}" placeholder="选填" />
          </div>
          <button type="button" class="mf-btn sm" id="btn-tg-bot-test" style="margin-top:18px">测试 Bot</button>
        </div>
      </details>
    </div>

    <div class="mf-grid-2">
      <div class="mf-card">
        <div class="mf-card-head">
          <div>
            <div class="mf-card-title">${isRelay ? '2. 🧩 影巢中转 / HDHive 渠道' : '2. HDHive 影巢渠道'}</div>
            <div class="mf-card-sub">
              ${hd.username
                ? (isRelay
                  ? `<span class="mf-pill info">已授权: 授权: ${esc(hd.username)} · 剩余次数: ${hd.points ?? 0}</span>`
                  : `<span class="mf-pill info">已绑定: ${esc(hd.username)} · 积分: ${hd.points ?? 0}</span>`)
                : '自动识别官方 HDHive 与影巢中转网关 (如 https://nf.xgzz.de)'}
              ${!isRelay && hd.last_checkin_date ? ` · 最近签到 ${esc(hd.last_checkin_date)}` : ''}
            </div>
          </div>
          <div class="mf-actions" style="gap:12px">
            <label class="mf-row" data-tip="每天定时自动执行影巢签到或刷新中转剩余额度（关闭则定时签到跳过影巢）"><input type="checkbox" id="hd-checkin" ${d.hdhive.auto_checkin !== false ? 'checked' : ''} /> 每日签到</label>
            <label class="mf-row"><input type="checkbox" id="hd-enabled" ${d.hdhive.enabled ? 'checked' : ''} /> 启用</label>
          </div>
        </div>
        <div class="mf-field">
          <label>服务地址（支持影巢中转网关或官方 HDHive 地址）</label>
          <input id="hd-url" class="mf-input" value="${esc(d.hdhive.base_url)}" placeholder="如 https://nf.xgzz.de 或 https://hdhive.nextemby.com" />
        </div>
        <div class="mf-row">
          <div class="mf-field grow">
            <label>API Key / Token</label>
            <input id="hd-key" type="password" class="mf-input" value="${esc(d.hdhive.api_key)}" placeholder="填入中转 Key 或 HDHive Token" />
          </div>
          <div class="mf-field" style="width:145px">
            <label>自动解锁 ≤ (积分)</label>
            <input id="hd-max-pts" type="number" min="0" class="mf-input" value="${d.hdhive.max_unlock_points}" />
          </div>
        </div>
        <details style="margin-bottom:10px" ${!isRelay && d.hdhive.refresh_token ? 'open' : ''}>
          <summary style="cursor:pointer;font-size:12px;color:var(--mf-muted)">可选：官方 HDHive Refresh Token（影巢中转无需填写）</summary>
          <div class="mf-field" style="margin-top:6px;margin-bottom:0">
            <input id="hd-refresh" type="password" class="mf-input" value="${esc(d.hdhive.refresh_token)}" placeholder="仅官方 HDHive OAuth 自动续期使用" />
          </div>
        </details>
        <div class="mf-actions">
          <button class="mf-btn sm primary" id="btn-hd-test">测试连接 / 验证授权</button>
          <button class="mf-btn sm success" id="btn-hd-checkin">${isRelay ? '刷新剩余次数' : '立即签到'}</button>
        </div>
      </div>

      <div class="mf-card">
        <div class="mf-card-head">
          <div>
            <div class="mf-card-title">3. AYClub (NF Token) 与代理覆盖</div>
            <div class="mf-card-sub">${esc(ay.last_checked_status || '30 天本地缓存保护，耗尽自动熔断至次日')}</div>
          </div>
          <label class="mf-row"><input type="checkbox" id="ay-enabled" ${d.ayclub.enabled ? 'checked' : ''} /> 启用</label>
        </div>
        <div class="mf-field">
          <label>AYClub 接口地址</label>
          <input id="ay-url" class="mf-input" value="${esc(d.ayclub.base_url)}" />
        </div>
        <div class="mf-row">
          <div class="mf-field grow">
            <label>NF Token</label>
            <input id="ay-token" type="password" class="mf-input" value="${esc(d.ayclub.nf_token)}" placeholder="输入 NF Token" />
          </div>
          <div class="mf-field" style="width:145px">
            <label>绑定 TG 数字 ID</label>
            <input id="ay-tgid" class="mf-input" value="${esc(d.ayclub.tg_user_id)}" placeholder="如 123456789" />
          </div>
        </div>
        <div class="mf-field" style="margin-top:4px">
          <label>自定义网络代理 URL（留空自动继承 Mediary 系统代理：${esc(state.boot.host_status?.proxy_url || '当前直连')}）</label>
          <input id="custom-proxy" class="mf-input" value="${esc(d.custom_proxy)}" placeholder="支持 http:// 或 socks5://，留空自动复用宿主代理" />
        </div>
        <button class="mf-btn sm" id="btn-ay-check">探测 AYClub 剩余次数</button>
      </div>
    </div>

    <div class="mf-card">
      <div class="mf-card-head">
        <div>
          <div class="mf-card-title">4. RSS 订阅源联动</div>
          <div class="mf-card-sub">支持解析 RSS / Atom，提取内嵌 115/ED2K/磁力直接转存，或解析 TMDB 自动创建 Mediary 订阅</div>
        </div>
        <button class="mf-btn sm" id="btn-sync-rss">立即拉取全部 RSS</button>
      </div>
      <form id="rss-form" class="mf-row" style="margin-bottom:12px">
        <input id="rss-name" class="mf-input" style="width:150px" placeholder="名称备注" />
        <input id="rss-url" class="mf-input grow" placeholder="https://.../rss.xml" required />
        <select id="rss-interval" class="mf-select" style="width:118px">
          <option value="30M">每 30 分钟</option>
          <option value="1H">每 1 小时</option>
          <option value="3H">每 3 小时</option>
          <option value="6H" selected>每 6 小时</option>
          <option value="12H">每 12 小时</option>
          <option value="24H">每 24 小时</option>
        </select>
        <label class="mf-row"><input type="checkbox" id="rss-sub" checked /> 自动建订</label>
        <label class="mf-row"><input type="checkbox" id="rss-trans" checked /> 直链转存</label>
        <button type="submit" class="mf-btn sm primary">+ 添加 RSS</button>
      </form>
      ${feeds.length === 0
        ? `<div class="mf-empty">暂未添加 RSS 订阅源</div>`
        : `<table class="mf-table">
            <thead><tr><th>名称</th><th>周期</th><th>状态 / 动作</th><th>最近结果</th><th>操作</th></tr></thead>
            <tbody>
              ${feeds.map((f) => `
                <tr>
                  <td><strong>${esc(f.name)}</strong><div class="mf-card-sub">${esc(f.url)}</div></td>
                  <td>${esc(f.refresh_interval)}</td>
                  <td>
                    <span class="mf-pill ${f.enabled ? 'success' : 'warn'}">${f.enabled ? '已启用' : '已暂停'}</span>
                    ${f.auto_subscribe ? '<span class="mf-pill info">建订</span>' : ''}
                    ${f.auto_transfer ? '<span class="mf-pill">转存</span>' : ''}
                  </td>
                  <td>${esc(f.last_result || '未运行')}</td>
                  <td class="mf-actions">
                    <button type="button" class="mf-btn sm" data-edit-rss="${esc(f.id)}">修改</button>
                    <button type="button" class="mf-btn sm" data-toggle-rss="${esc(f.id)}">${f.enabled ? '暂停' : '启用'}</button>
                    <button type="button" class="mf-btn sm danger" data-del-rss="${esc(f.id)}">删除</button>
                  </td>
                </tr>
              `).join('')}
            </tbody>
          </table>`}
    </div>

    <div class="mf-card">
      <div class="mf-card-head">
        <div>
          <div class="mf-card-title">5. 🔔 Mediary 宿主通知中心联动</div>
          <div class="mf-card-sub">已原生对接 Mediary <code>POST /plugin/notifications</code> 接口，自动复用您在 Mediary 宿主中配置的 Telegram / Bark / 企业微信 / Webhook 等全部通知渠道</div>
        </div>
        <div class="mf-actions">
          <button type="button" class="mf-btn sm primary" id="btn-test-mediary-notify">发送测试通知</button>
          <label class="mf-row"><input type="checkbox" id="nt-enabled" ${d.notifications.enabled ? 'checked' : ''} /> 启用宿主通知</label>
        </div>
      </div>
      <div class="mf-row" style="gap:18px;padding-top:4px">
        <label class="mf-row"><input type="checkbox" id="nt-transfer" ${d.notifications.on_transfer ? 'checked' : ''} /> 转存与追更命中通知</label>
        <label class="mf-row"><input type="checkbox" id="nt-rss" ${d.notifications.on_rss_subscribe ? 'checked' : ''} /> RSS 自动建订通知</label>
        <label class="mf-row"><input type="checkbox" id="nt-checkin" ${d.notifications.on_checkin ? 'checked' : ''} /> 每日签到与额度报告</label>
        <label class="mf-row"><input type="checkbox" id="nt-poster" ${d.notifications.include_poster ? 'checked' : ''} /> 推送附带 TMDB 影视海报封面</label>
      </div>
    </div>
  `;

  const collectAndSave = async (toastMsg = '已保存资源渠道配置', silent = false) => {
    clearTimeout(channelAutoSaveTimer);
    syncDraftFromChannelsDOM(root);
    await callAction('ui_save_channels', buildSaveChannelsPayload(d), toastMsg, { silent });
  };

  const scheduleAutoSave = () => {
    syncDraftFromChannelsDOM(root);
    scheduleAutoSaveChannels();
  };

  const fetchChannelMeta = async (channelId) => {
    clearTimeout(channelAutoSaveTimer);
    syncDraftFromChannelsDOM(root);
    const ch = d.telegram.channels.find((c) => c.id === channelId);
    if (!ch || ch._fetching) return;
    const rawTarget = (ch._dirtyInput !== undefined ? ch._dirtyInput : (ch.name || ch.identifier || '')).trim();
    if (!rawTarget) {
      showToast('请输入要识别的频道名称、@用户名或频道链接', 'error');
      return;
    }
    ch._fetching = true;
    renderChannelsTab(root);
    try {
      const res = await bridge.action('ui_tg_resolve_and_save', {
        channel_id: ch.id,
        identifier: rawTarget,
        level: normalizeTGLevelJS(ch.level),
        enabled: ch.enabled !== false,
        hide_in_discover: !!ch.hide_in_discover,
      });
      const meta = res?.resolved || {};
      const target = state.channelDraft?.telegram?.channels?.find((c) => c.id === channelId);
      if (target) {
        target.identifier = meta.identifier || cleanTGInputId(rawTarget);
        target.name = meta.name || rawTarget;
        if (meta.logo) target.logo = meta.logo;
        delete target._dirtyInput;
        target._fetching = false;
      }
      // Deduplicate any duplicate identifier rows in draft while preserving unsaved blank rows
      if (state.channelDraft?.telegram?.channels) {
        const seenNorm = new Set();
        state.channelDraft.telegram.channels = state.channelDraft.telegram.channels.filter((c) => {
          const rVal = c._dirtyInput !== undefined ? c._dirtyInput : (c.identifier || c.name || '');
          const nk = cleanTGInputId(rVal).toLowerCase();
          if (!nk) return true;
          if (seenNorm.has(nk)) return false;
          seenNorm.add(nk);
          return true;
        });
      }
      if (res && res.state) {
        state.boot = res;
        mergeServerBootIntoDrafts('ui_tg_resolve_and_save', res, {});
        updateSidebarStatus();
      }
      if (state.tab === 'channels') render();
      showToast(res?.notice || `已识别并保存频道：${meta.name || rawTarget}`, 'success');
    } catch (err) {
      const target = state.channelDraft?.telegram?.channels?.find((c) => c.id === channelId);
      if (target) target._fetching = false;
      if (state.tab === 'channels') renderChannelsTab(root);
      showToast(err.message || String(err), 'error');
    }
  };

  // Telegram MTProto login controls
  const appIdBtn = root.querySelector('#btn-tg-appid');
  if (appIdBtn) appIdBtn.onclick = () => openTGAppIdModal();

  const phoneInp = root.querySelector('#tg-auth-phone');
  if (phoneInp) {
    phoneInp.oninput = () => { state.tgAuthPhone = phoneInp.value; };
  }
  const codeInp = root.querySelector('#tg-auth-code');
  if (codeInp) {
    codeInp.oninput = () => {
      const wasEmpty = !state.tgAuthCode.trim();
      state.tgAuthCode = codeInp.value;
      const nowEmpty = !state.tgAuthCode.trim();
      if (wasEmpty !== nowEmpty && !tgPwdReq) {
        const subBtn = root.querySelector('#btn-tg-auth-submit');
        if (subBtn) subBtn.textContent = nowEmpty ? '获取验证码' : '确认登录';
      }
    };
    codeInp.onkeydown = (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        root.querySelector('#btn-tg-auth-submit')?.click();
      }
    };
  }

  const doSendCode = async () => {
    syncDraftFromChannelsDOM(root);
    const phone = (root.querySelector('#tg-auth-phone')?.value || state.tgAuthPhone || tg.pending_phone || '').trim();
    if (!phone) return showToast('请先输入 Telegram 手机号', 'error');
    state.tgAuthPhone = phone;
    state.tgAuthBusy = true;
    renderChannelsTab(root);
    try {
      await callAction('ui_tg_send_code', { phone });
    } finally {
      state.tgAuthBusy = false;
      render();
    }
  };

  const resendBtn = root.querySelector('#btn-tg-resend');
  if (resendBtn) resendBtn.onclick = doSendCode;

  const authSubBtn = root.querySelector('#btn-tg-auth-submit');
  if (authSubBtn) {
    authSubBtn.onclick = async () => {
      syncDraftFromChannelsDOM(root);
      const code = (root.querySelector('#tg-auth-code')?.value || state.tgAuthCode || '').trim();
      if (tgPwdReq || code) {
        if (!code) return showToast(tgPwdReq ? '请输入两步验证密码' : '请输入收到的验证码', 'error');
        state.tgAuthBusy = true;
        renderChannelsTab(root);
        try {
          await callAction('ui_tg_sign_in', { code });
          state.tgAuthCode = '';
          state.tgNetStatus = 'connected';
        } finally {
          state.tgAuthBusy = false;
          render();
        }
      } else {
        await doSendCode();
      }
    };
  }

  const logoutBtn = root.querySelector('#btn-tg-logout');
  if (logoutBtn) {
    logoutBtn.onclick = async () => {
      state.tgAuthBusy = true;
      renderChannelsTab(root);
      try {
        await callAction('ui_tg_logout', {});
        state.tgNetStatus = 'idle';
      } finally {
        state.tgAuthBusy = false;
        render();
      }
    };
  }

  const netCheckBtn = root.querySelector('#btn-tg-net-check');
  if (netCheckBtn) {
    netCheckBtn.onclick = async () => {
      state.tgNetStatus = 'checking';
      renderChannelsTab(root);
      try {
        await callAction('ui_tg_check', {});
        state.tgNetStatus = 'connected';
        render();
      } catch (_) {
        state.tgNetStatus = 'error';
        render();
      }
    };
  }

  // Channel input typing, blur auto-save & Enter to resolve
  root.querySelectorAll('[data-tg-input]').forEach((inp) => {
    const idx = Number(inp.dataset.tgInput);
    const ch = d.telegram.channels[idx];
    if (!ch) return;
    const chId = ch.id;
    inp.oninput = () => {
      ch._dirtyInput = inp.value;
      ch.name = inp.value;
    };
    inp.onblur = () => {
      if (ch._fetching) return;
      const val = inp.value.trim();
      if (val && ch._dirtyInput !== undefined) {
        ch.identifier = cleanTGInputId(val);
        ch.name = val;
        delete ch._dirtyInput;
        scheduleAutoSave();
      }
    };
    inp.onkeydown = (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        fetchChannelMeta(chId);
      }
    };
  });

  root.querySelectorAll('[data-tg-fetch]').forEach((btn) => {
    const ch = d.telegram.channels[Number(btn.dataset.tgFetch)];
    if (ch) btn.onclick = () => fetchChannelMeta(ch.id);
  });

  root.querySelectorAll('[data-tg-vis]').forEach((btn) => {
    btn.onclick = () => {
      syncDraftFromChannelsDOM(root);
      const ch = d.telegram.channels[Number(btn.dataset.tgVis)];
      if (!ch) return;
      ch.hide_in_discover = !ch.hide_in_discover;
      renderChannelsTab(root);
      scheduleAutoSave();
    };
  });

  root.querySelectorAll('[data-tg-lvl]').forEach((btn) => {
    btn.onclick = () => {
      syncDraftFromChannelsDOM(root);
      const [idxStr, actStr] = btn.dataset.tgLvl.split(':');
      const ch = d.telegram.channels[Number(idxStr)];
      if (!ch) return;
      toggleTGChannelLevel(ch, Number(actStr));
      renderChannelsTab(root);
      scheduleAutoSave();
    };
  });

  root.querySelectorAll('[data-tg-del-arm]').forEach((btn) => {
    btn.onclick = (e) => {
      e.stopPropagation();
      syncDraftFromChannelsDOM(root);
      d.telegram.channels.forEach((c, i) => { c._deleting = i === Number(btn.dataset.tgDelArm); });
      renderChannelsTab(root);
    };
  });

  root.querySelectorAll('[data-tg-ops]').forEach((ops) => {
    ops.onmouseleave = () => {
      const ch = d.telegram.channels[Number(ops.dataset.tgOps)];
      if (ch && ch._deleting) {
        syncDraftFromChannelsDOM(root);
        ch._deleting = false;
        renderChannelsTab(root);
      }
    };
  });

  root.querySelectorAll('[data-tg-del-confirm]').forEach((btn) => {
    btn.onclick = (e) => {
      e.stopPropagation();
      syncDraftFromChannelsDOM(root);
      d.telegram.channels.splice(Number(btn.dataset.tgDelConfirm), 1);
      renderChannelsTab(root);
      scheduleAutoSave();
    };
  });

  // Drag-and-drop sorting just like NextFind's vuedraggable handle
  root.querySelectorAll('[data-tg-drag]').forEach((handle) => {
    const idx = Number(handle.dataset.tgDrag);
    const row = root.querySelector(`[data-tg-row="${idx}"]`);
    handle.ondragstart = (e) => {
      syncDraftFromChannelsDOM(root);
      state.dragTGIndex = idx;
      if (row) row.classList.add('dragging');
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData('text/plain', String(idx));
    };
    handle.ondragend = () => {
      state.dragTGIndex = null;
      root.querySelectorAll('.nf-tg-row').forEach((r) => r.classList.remove('dragging', 'drag-over'));
    };
  });

  root.querySelectorAll('[data-tg-row]').forEach((row) => {
    const targetIdx = Number(row.dataset.tgRow);
    row.ondragover = (e) => {
      if (state.dragTGIndex === null || state.dragTGIndex === targetIdx) return;
      e.preventDefault();
      row.classList.add('drag-over');
    };
    row.ondragleave = () => row.classList.remove('drag-over');
    row.ondrop = (e) => {
      e.preventDefault();
      row.classList.remove('drag-over');
      const from = state.dragTGIndex;
      state.dragTGIndex = null;
      if (from === null || from === targetIdx) return;
      const [moved] = d.telegram.channels.splice(from, 1);
      d.telegram.channels.splice(targetIdx, 0, moved);
      renderChannelsTab(root);
      scheduleAutoSave();
    };
  });

  root.querySelector('#btn-add-tg-channel').onclick = () => {
    syncDraftFromChannelsDOM(root);
    d.telegram.channels.push({
      id: `ch-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 5)}`,
      identifier: '',
      name: '',
      logo: '',
      level: 0,
      enabled: true,
      hide_in_discover: false,
      cached_count: 0,
      transferred_count: 0,
      _fetching: false,
      _deleting: false,
    });
    renderChannelsTab(root);
    const inputs = root.querySelectorAll('[data-tg-input]');
    if (inputs.length > 0) inputs[inputs.length - 1].focus();
  };

  const batchImportBtn = root.querySelector('#btn-batch-import-tg');
  if (batchImportBtn) {
    batchImportBtn.onclick = () => {
      syncDraftFromChannelsDOM(root);
      if (!tgLoggedIn) {
        showToast('请先在上方登录 Telegram 账号后再批量拉取已加入频道', 'error');
        return;
      }
      openTGBatchImportModal(root);
    };
  }

  const tgEnabledChk = root.querySelector('#tg-enabled');
  if (tgEnabledChk) tgEnabledChk.onchange = () => scheduleAutoSave();
  const tgTelegraphChk = root.querySelector('#tg-telegraph');
  if (tgTelegraphChk) tgTelegraphChk.onchange = () => scheduleAutoSave();
  const hdEnabledChk = root.querySelector('#hd-enabled');
  if (hdEnabledChk) hdEnabledChk.onchange = () => scheduleAutoSave();
  const hdCheckinChk = root.querySelector('#hd-checkin');
  if (hdCheckinChk) hdCheckinChk.onchange = () => scheduleAutoSave();

  root.querySelector('#btn-save-channels').onclick = () => collectAndSave();
  root.querySelector('#btn-hd-test').onclick = async () => { await collectAndSave(); await callAction('ui_hdhive_test', {}); };
  root.querySelector('#btn-hd-checkin').onclick = async () => { await collectAndSave(); await callAction('ui_daily_checkin', { force_hdhive: true }); };
  root.querySelector('#btn-tg-sync').onclick = async () => { await collectAndSave(); await callAction('ui_tg_sync', {}); };
  root.querySelector('#btn-tg-bot-test').onclick = async () => { await collectAndSave(); await callAction('ui_tg_test_bot', {}); };
  root.querySelector('#btn-ay-check').onclick = async () => { await collectAndSave(); await callAction('ui_ayclub_refresh', {}); };
  root.querySelector('#btn-sync-rss').onclick = () => callAction('ui_sync_rss', {});
  root.querySelector('#btn-test-mediary-notify').onclick = async () => {
    syncDraftFromChannelsDOM(root);
    await callAction('ui_test_mediary_notify', { notifications: d.notifications });
  };

  root.querySelector('#rss-form').onsubmit = (e) => {
    e.preventDefault();
    callAction('ui_save_rss', {
      name: root.querySelector('#rss-name').value.trim(),
      url: root.querySelector('#rss-url').value.trim(),
      enabled: true,
      refresh_interval: root.querySelector('#rss-interval').value,
      auto_subscribe: root.querySelector('#rss-sub').checked,
      auto_transfer: root.querySelector('#rss-trans').checked,
    });
  };
  root.querySelectorAll('[data-edit-rss]').forEach((b) => {
    b.onclick = () => {
      const target = feeds.find((x) => x.id === b.dataset.editRss);
      if (target) openEditRSSModal(target);
    };
  });
  root.querySelectorAll('[data-toggle-rss]').forEach((b) => { b.onclick = () => callAction('ui_toggle_rss', { id: b.dataset.toggleRss }); });
  root.querySelectorAll('[data-del-rss]').forEach((b) => { b.onclick = () => callAction('ui_delete_rss', { id: b.dataset.delRss }); });
}

function openEditRSSModal(feed) {
  const modalRoot = document.getElementById('modal-root');
  modalRoot.classList.remove('hidden');
  const intervals = [
    { v: '30M', label: '每 30 分钟' },
    { v: '1H', label: '每 1 小时' },
    { v: '3H', label: '每 3 小时' },
    { v: '6H', label: '每 6 小时' },
    { v: '12H', label: '每 12 小时' },
    { v: '24H', label: '每 24 小时' },
  ];
  const curInt = String(feed.refresh_interval || '6H').toUpperCase();
  modalRoot.innerHTML = `
    <div class="mf-modal" style="width:min(540px, 94vw)">
      <div class="mf-modal-head">
        <strong>修改 RSS 订阅源</strong>
        <button type="button" class="mf-btn sm" id="rss-edit-close">关闭</button>
      </div>
      <form id="rss-edit-form">
        <div class="mf-modal-body">
          <div class="mf-row">
            <div class="mf-field grow">
              <label>名称备注</label>
              <input id="edit-rss-name" class="mf-input" value="${esc(feed.name || '')}" placeholder="名称备注" />
            </div>
            <div class="mf-field" style="width:150px">
              <label>刷新周期</label>
              <select id="edit-rss-interval" class="mf-select">
                ${intervals.map((it) => `<option value="${it.v}" ${curInt === it.v ? 'selected' : ''}>${esc(it.label)}</option>`).join('')}
              </select>
            </div>
          </div>
          <div class="mf-field">
            <label>RSS / Atom 订阅地址</label>
            <input id="edit-rss-url" class="mf-input" value="${esc(feed.url || '')}" placeholder="https://.../rss.xml" required />
          </div>
          <div class="mf-row" style="gap:16px;margin-top:6px">
            <label class="mf-row"><input type="checkbox" id="edit-rss-enabled" ${feed.enabled !== false ? 'checked' : ''} /> 启用该订阅源</label>
            <label class="mf-row"><input type="checkbox" id="edit-rss-sub" ${feed.auto_subscribe ? 'checked' : ''} /> 自动创建 Mediary 订阅</label>
            <label class="mf-row"><input type="checkbox" id="edit-rss-trans" ${feed.auto_transfer ? 'checked' : ''} /> 提取内嵌直链自动转存</label>
          </div>
          <div class="mf-row" style="margin-top:10px">
            <label class="mf-row" style="color:var(--mf-muted);font-size:12px">
              <input type="checkbox" id="edit-rss-reset" /> 清空已读 GUID 缓存（下次拉取时重新扫描当前全部条目）
            </label>
          </div>
        </div>
        <div class="mf-modal-foot">
          <button type="button" class="mf-btn" id="rss-edit-cancel">取消</button>
          <button type="submit" class="mf-btn primary">保存修改</button>
        </div>
      </form>
    </div>
  `;
  const close = () => modalRoot.classList.add('hidden');
  modalRoot.querySelector('#rss-edit-close').onclick = close;
  modalRoot.querySelector('#rss-edit-cancel').onclick = close;
  modalRoot.querySelector('#rss-edit-form').onsubmit = async (e) => {
    e.preventDefault();
    await callAction('ui_save_rss', {
      id: feed.id,
      name: modalRoot.querySelector('#edit-rss-name').value.trim(),
      url: modalRoot.querySelector('#edit-rss-url').value.trim(),
      enabled: modalRoot.querySelector('#edit-rss-enabled').checked,
      refresh_interval: modalRoot.querySelector('#edit-rss-interval').value,
      auto_subscribe: modalRoot.querySelector('#edit-rss-sub').checked,
      auto_transfer: modalRoot.querySelector('#edit-rss-trans').checked,
      reset_seen: modalRoot.querySelector('#edit-rss-reset').checked,
    });
    close();
  };
}

/* ==================== TAB 4: 初筛与神盾 (NextFind 1:1 资源权重 + 落点分流 + 私有神盾) ==================== */
const WEIGHT_CAT_META = {
  resolutions: { field: 'resolution_items', label: '分辨率' },
  colors: { field: 'color_items', label: '色彩模式' },
  audios: { field: 'audio_items', label: '音频规格' },
  extensions: { field: 'extension_items', label: '扩展名' },
  sizes: { field: 'size_items', label: '文件体积' },
  groups: { field: 'group_items', label: '关键词' },
};

const STREAM_PLATFORMS_INTL = [
  'Netflix', 'Amazon', 'Disney+', 'Apple TV+', 'HBO Max',
  'Hulu', 'Paramount+', 'Peacock', 'iTunes', 'Crunchyroll',
];
const STREAM_PLATFORMS_ASIA = [
  'Bilibili', '爱奇艺', '优酷', '腾讯视频', '芒果TV',
  '巴哈姆特', 'KKTV', 'myTV SUPER', 'Viu', 'friDay',
];
const STREAM_PLATFORMS_ALL = [...STREAM_PLATFORMS_INTL, ...STREAM_PLATFORMS_ASIA];

function buildDefaultRulesDraft() {
  return {
    resource_types: [
      { name: '115', enabled: true },
      { name: 'ED2K', enabled: true },
      { name: 'MAGNET', enabled: true },
    ],
    subscription_sources: [
      { id: 'shield', name: '私有神盾', enabled: true },
      { id: 'telegram', name: 'Telegram', enabled: true },
      { id: 'hdhive', name: '影巢/HDHive', enabled: true },
      { id: 'ayclub', name: 'AYClub', enabled: true },
    ],
    weights: ['resolutions', 'colors', 'audios', 'extensions', 'sizes', 'groups'],
    resolution_items: [
      { name: '4K', enabled: true },
      { name: '1080P', enabled: true },
      { name: '720P及以下', enabled: false },
    ],
    color_items: [
      { name: 'DV&HDR', enabled: true },
      { name: 'DV', enabled: true },
      { name: 'HDR10', enabled: true },
      { name: 'HDR', enabled: true },
      { name: 'HLG', enabled: true },
      { name: 'SDR', enabled: true },
    ],
    audio_items: [
      { name: 'TRUEHD', enabled: true },
      { name: 'DTS-HD MA', enabled: true },
      { name: 'DTS-X', enabled: true },
      { name: 'DTS-HD', enabled: true },
      { name: 'DTS', enabled: true },
      { name: 'EAC3', enabled: true },
      { name: 'AC3', enabled: true },
      { name: 'FLAC', enabled: true },
      { name: 'AAC', enabled: true },
    ],
    extension_items: [
      { name: 'MKV', enabled: true },
      { name: 'MP4', enabled: true },
      { name: 'M2TS', enabled: true },
      { name: 'TS', enabled: false },
      { name: 'ISO', enabled: false },
      { name: 'RMVB', enabled: false },
      { name: 'AVI', enabled: false },
      { name: 'MOV', enabled: false },
      { name: 'WMV', enabled: false },
      { name: '小众格式', enabled: false },
    ],
    size_items: [
      { name: '由大到小', enabled: true },
      { name: '40G以上', min: 40, max: 115, enabled: true },
      { name: '20-40G', min: 20, max: 40, enabled: true },
      { name: '10-20G', min: 10, max: 20, enabled: true },
      { name: '5-10G', min: 5, max: 10, enabled: true },
      { name: '0-5G', min: 0, max: 5, enabled: true },
      { name: '115G以上', min: 115, max: 999999, enabled: false },
      { name: '小于_MB', is_custom_size: true, value: 0, enabled: false },
    ],
    group_items: [
      { name: '特效', enabled: true },
      { name: '双语', enabled: true },
      { name: '简繁', enabled: true },
      { name: '中字', enabled: true },
      { name: '杜比', enabled: true },
      { name: '高码', enabled: true },
      { name: '收藏', enabled: true },
      { name: '国粤', enabled: true },
    ],
    block_filters: [
      { id: 'bf-1', tags: ['枪版'] },
      { id: 'bf-2', tags: ['偷拍'] },
      { id: 'bf-3', tags: ['CAM'] },
      { id: 'bf-4', tags: ['TS清晰'] },
      { id: 'bf-5', tags: ['HC字幕'] },
      { id: 'bf-6', tags: ['博彩'] },
    ],
    ignored_texts: [],
    cutoff_score: 85,
    min_score: 0,
    default_target_cid: '',
    default_target_name: '',
    prefer_flowlink: true,
  };
}

function syncDraftSubSourcesWithChannels(rd, channels) {
  const chList = Array.isArray(channels) ? channels : [];
  const activeTg = [];
  const seenNorm = new Set();
  for (const ch of chList) {
    if (ch.enabled === false) continue;
    const rawKey = ch._dirtyInput !== undefined ? ch._dirtyInput : (ch.identifier || ch.name || '');
    const norm = cleanTGInputId(rawKey).toLowerCase();
    if (!norm || seenNorm.has(norm)) continue;
    seenNorm.add(norm);
    const disp = (ch.name || ch.identifier || norm).trim();
    activeTg.push({ id: `telegram:${norm}`, name: disp });
  }

  const activeTgMap = new Map(activeTg.map((x) => [x.id, x.name]));
  const prevList = Array.isArray(rd.subscription_sources) ? rd.subscription_sources : [];
  const legacyTg = prevList.find((s) => String(s.id || '').toLowerCase() === 'telegram');
  const legacyTgEnabled = legacyTg ? legacyTg.enabled !== false : true;
  const baseDefs = new Map([
    ['shield', '私有神盾'],
    ['hdhive', '影巢/HDHive'],
    ['ayclub', 'AYClub'],
  ]);

  const merged = [];
  const inserted = new Set();

  for (const existing of prevList) {
    const idLow = String(existing.id || '').trim().toLowerCase();
    if (!idLow) continue;
    if (idLow === 'telegram') {
      if (activeTg.length === 0 && !inserted.has('telegram')) {
        inserted.add('telegram');
        merged.push({ id: 'telegram', name: 'Telegram', enabled: existing.enabled !== false });
      } else {
        for (const tgItem of activeTg) {
          if (!inserted.has(tgItem.id)) {
            inserted.add(tgItem.id);
            merged.push({ id: tgItem.id, name: tgItem.name, enabled: existing.enabled !== false });
          }
        }
      }
    } else if (idLow.startsWith('telegram:')) {
      if (activeTgMap.has(idLow) && !inserted.has(idLow)) {
        inserted.add(idLow);
        merged.push({ id: idLow, name: activeTgMap.get(idLow), enabled: existing.enabled !== false });
      }
    } else if (baseDefs.has(idLow) && !inserted.has(idLow)) {
      inserted.add(idLow);
      merged.push({ id: idLow, name: baseDefs.get(idLow), enabled: existing.enabled !== false });
    }
  }

  if (!inserted.has('shield')) {
    inserted.add('shield');
    merged.unshift({ id: 'shield', name: '私有神盾', enabled: true });
  }

  if (activeTg.length === 0) {
    if (!inserted.has('telegram')) {
      inserted.add('telegram');
      merged.push({ id: 'telegram', name: 'Telegram', enabled: legacyTgEnabled });
    }
  } else {
    for (const tgItem of activeTg) {
      if (!inserted.has(tgItem.id)) {
        inserted.add(tgItem.id);
        merged.push({ id: tgItem.id, name: tgItem.name, enabled: legacyTgEnabled });
      }
    }
  }

  for (const [bid, bname] of [['hdhive', '影巢/HDHive'], ['ayclub', 'AYClub']]) {
    if (!inserted.has(bid)) {
      inserted.add(bid);
      merged.push({ id: bid, name: bname, enabled: true });
    }
  }

  rd.subscription_sources = merged;
}

function ensureRulesDraft() {
  if (state.rulesDraft) {
    const curChs = state.channelDraft?.telegram?.channels || state.boot?.state?.telegram?.channels || [];
    syncDraftSubSourcesWithChannels(state.rulesDraft, curChs);
    return state.rulesDraft;
  }
  const r = state.boot?.state?.rules || {};
  const def = buildDefaultRulesDraft();
  const cloneArr = (arr, fb) => (Array.isArray(arr) && arr.length > 0 ? JSON.parse(JSON.stringify(arr)) : JSON.parse(JSON.stringify(fb)));
  const rd = {
    resource_types: cloneArr(r.resource_types, def.resource_types),
    subscription_sources: cloneArr(r.subscription_sources, def.subscription_sources),
    weights: cloneArr(r.weights, def.weights),
    resolution_items: cloneArr(r.resolution_items, def.resolution_items),
    color_items: cloneArr(r.color_items, def.color_items),
    audio_items: cloneArr(r.audio_items, def.audio_items),
    extension_items: cloneArr(r.extension_items, def.extension_items),
    size_items: cloneArr(r.size_items, def.size_items),
    group_items: Array.isArray(r.group_items) ? JSON.parse(JSON.stringify(r.group_items)) : JSON.parse(JSON.stringify(def.group_items)),
    block_filters: Array.isArray(r.block_filters) ? JSON.parse(JSON.stringify(r.block_filters)) : JSON.parse(JSON.stringify(def.block_filters)),
    ignored_texts: Array.isArray(r.ignored_texts) ? [...r.ignored_texts] : [],
    cutoff_score: r.cutoff_score ?? 85,
    min_score: r.min_score ?? 0,
    default_target_cid: r.default_target_cid || '',
    default_target_name: r.default_target_name || '',
    prefer_flowlink: r.prefer_flowlink !== false,
  };
  if (!rd.size_items.some((x) => x.is_custom_size)) {
    rd.size_items.push({ name: '小于_MB', is_custom_size: true, value: 0, enabled: false });
  }
  const curChs = state.channelDraft?.telegram?.channels || state.boot?.state?.telegram?.channels || [];
  syncDraftSubSourcesWithChannels(rd, curChs);
  state.rulesDraft = rd;
  return rd;
}

function syncRulesTopDOM(root, rd) {
  const cidInp = root.querySelector('#rl-cid');
  const minInp = root.querySelector('#rl-min');
  const flInp = root.querySelector('#rl-flowlink');
  if (cidInp) {
    const nextCid = cidInp.value.trim();
    if (nextCid !== (rd.default_target_cid || '').trim()) {
      rd.default_target_cid = nextCid;
      rd.default_target_name = resolveKnownCidName(nextCid);
    }
  }
  if (minInp) rd.min_score = Number(minInp.value || 0);
  if (flInp) rd.prefer_flowlink = flInp.checked;
}

// NextFind 1:1 smooth gradient opacity calculator across all enabled weighted items (1.00 -> 0.25)
function buildWeightStyleMap(rd) {
  const enabledKeys = [];
  for (const catKey of rd.weights) {
    const meta = WEIGHT_CAT_META[catKey];
    if (!meta) continue;
    const list = rd[meta.field] || [];
    list.forEach((item, idx) => {
      if (item.enabled && !item.is_custom_size) {
        enabledKeys.push(`${catKey}:${idx}`);
      }
    });
  }
  const total = enabledKeys.length;
  const map = new Map();
  enabledKeys.forEach((key, pos) => {
    const opacity = total <= 1 ? 1.0 : 1.0 - (pos / (total - 1)) * (1.0 - 0.25);
    map.set(key, `background-color: rgba(37, 99, 235, ${opacity.toFixed(3)}); color: #ffffff;`);
  });
  return map;
}

function openBlockFilterModal(rd, root) {
  const modalRoot = document.getElementById('modal-root');
  modalRoot.classList.remove('hidden');
  const presetRows = [
    { label: '资源/分辨率', items: ['115', 'ED2K', 'MAGNET', '4K', '1080P', '720P及以下'] },
    { label: '色彩/片源', items: ['DV&HDR', 'DV', 'HDR10', 'HDR', 'HLG', 'SDR', 'REMUX', 'BluRay', 'WEB-DL'] },
    { label: '流媒体平台 (国际)', items: STREAM_PLATFORMS_INTL },
    { label: '流媒体平台 (国内/亚洲)', items: STREAM_PLATFORMS_ASIA },
    { label: '音轨/后缀', items: ['TRUEHD', 'DTS-HD MA', 'EAC3', 'AC3', 'AAC', 'MKV', 'MP4', 'TS', 'ISO', 'RMVB'] },
    { label: '常见低质词', items: ['枪版', '偷拍', 'CAM', 'HC字幕', '韩版', '博彩', '水印', '尝鲜'] },
  ];
  let selectedTags = [];

  function renderModalContent() {
    modalRoot.innerHTML = `
      <div class="mf-modal" style="width:min(560px, 95vw)">
        <div class="mf-modal-head">
          <strong>添加屏蔽过滤规则（支持多条件「且」组合）</strong>
          <button type="button" class="mf-btn sm" id="bf-close">关闭</button>
        </div>
        <div class="mf-modal-body">
          <div class="mf-card-sub" style="margin-bottom:12px">
            点选 1 个标签即单条件一票否决（如 <code>枪版</code>）；同时点选多个标签则构成 <strong>且 (AND)</strong> 组合拦截（如点选 <code>4K</code> + <code>AAC</code>，仅当资源同时为 4K 且音轨为 AAC 时才屏蔽）。
          </div>
          ${presetRows.map((row) => `
            <div style="margin-bottom:10px">
              <div style="font-size:11.5px;color:var(--mf-muted);margin-bottom:5px">${esc(row.label)}</div>
              <div style="display:flex;flex-wrap:wrap;gap:6px">
                ${row.items.map((tag) => `
                  <button type="button" class="nf-preset-tag ${selectedTags.includes(tag) ? 'selected' : ''}" data-bf-preset="${esc(tag)}">${esc(tag)}</button>
                `).join('')}
              </div>
            </div>
          `).join('')}
          <div class="mf-row" style="margin-top:12px">
            <input id="bf-custom-inp" class="mf-input grow" placeholder="输入自定义屏蔽词（也支持直接填：4K 且 MP4），回车加入组合" />
            <button type="button" class="mf-btn sm" id="bf-add-custom">+ 加入条件</button>
          </div>
          <div style="margin-top:12px;padding:10px 12px;border-radius:8px;background:var(--mf-surface-2);border:1px solid var(--mf-border)">
            <div style="font-size:11.5px;color:var(--mf-muted);margin-bottom:6px">当前待添加屏蔽规则预览：</div>
            ${selectedTags.length === 0
              ? `<span style="font-size:12px;color:var(--mf-muted)">尚未选择条件（请点选上方标签或输入自定义词）</span>`
              : `<span class="nf-chip blocked">
                   <span>✕</span>
                   <span>${selectedTags.map((t) => esc(t)).join('<span class="nf-chip-and">且</span>')}</span>
                 </span>
                 <button type="button" class="mf-btn sm" style="margin-left:8px" id="bf-clear-sel">清空重选</button>`}
          </div>
        </div>
        <div class="mf-modal-foot">
          <button type="button" class="mf-btn" id="bf-cancel">取消</button>
          <button type="button" class="mf-btn primary" id="bf-confirm">确认添加屏蔽规则</button>
        </div>
      </div>
    `;

    const close = () => modalRoot.classList.add('hidden');
    modalRoot.querySelector('#bf-close').onclick = close;
    modalRoot.querySelector('#bf-cancel').onclick = close;
    const clearBtn = modalRoot.querySelector('#bf-clear-sel');
    if (clearBtn) clearBtn.onclick = () => { selectedTags = []; renderModalContent(); };

    modalRoot.querySelectorAll('[data-bf-preset]').forEach((btn) => {
      btn.onclick = () => {
        const t = btn.dataset.bfPreset;
        if (selectedTags.includes(t)) {
          selectedTags = selectedTags.filter((x) => x !== t);
        } else {
          selectedTags.push(t);
        }
        renderModalContent();
      };
    });

    const pushCustomInput = () => {
      const raw = modalRoot.querySelector('#bf-custom-inp').value.trim();
      if (!raw) return;
      raw.split(/\s*(?:且|\+|&&)\s*/).map((s) => s.trim()).filter(Boolean).forEach((part) => {
        if (!selectedTags.includes(part)) selectedTags.push(part);
      });
      renderModalContent();
    };
    modalRoot.querySelector('#bf-add-custom').onclick = pushCustomInput;
    modalRoot.querySelector('#bf-custom-inp').onkeydown = (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        pushCustomInput();
      }
    };

    modalRoot.querySelector('#bf-confirm').onclick = () => {
      const inpVal = modalRoot.querySelector('#bf-custom-inp').value.trim();
      if (inpVal) {
        inpVal.split(/\s*(?:且|\+|&&)\s*/).map((s) => s.trim()).filter(Boolean).forEach((part) => {
          if (!selectedTags.includes(part)) selectedTags.push(part);
        });
      }
      if (selectedTags.length === 0) {
        showToast('请至少选择或输入 1 个屏蔽条件', 'error');
        return;
      }
      rd.block_filters.push({
        id: `bf-${Date.now().toString(36)}`,
        tags: [...selectedTags],
      });
      close();
      renderRulesTab(root);
    };
  }

  renderModalContent();
}

function renderRulesTab(root) {
  const st = state.boot.state;
  const rd = ensureRulesDraft();
  const shares = st.shield_shares || [];
  const tIn = state.ruleTestInput;
  const tOut = state.ruleTestOut;
  const styleMap = buildWeightStyleMap(rd);

  root.innerHTML = `
    <div class="mf-header">
      <div>
        <h2>资源权重、落点分流与私有神盾</h2>
        <p>按住 <code>⠿</code> 可上下拖动大类维度或左右拖动规格芯片调整权重优先级（颜色越深权重越高，点击 <code>✓ / ✕</code> 切换准入或一票屏蔽）</p>
      </div>
      <div class="mf-actions">
        <button type="button" class="mf-btn" id="btn-reset-weights">恢复默认权重</button>
        <button type="button" class="mf-btn primary" id="btn-save-rules">保存资源权重与落点</button>
      </div>
    </div>

    <div class="mf-card">
      <div class="mf-card-head">
        <div>
          <div class="mf-card-title">📂 115 转存落点与下游分流模式</div>
          <div class="mf-card-sub">配合 <strong>MediWash 权重洗版</strong> 时，取消勾选「整包直投宿主 FlowLink」即可将 115 分享 / ED2K / Magnet 统一通过原生接口纯接收至中转目录 CID（留空自动跟随宿主转存监控目录）</div>
        </div>
      </div>
      <div class="mf-row" style="align-items:flex-start">
        <div class="mf-field grow" style="margin-bottom:0">
          ${(() => {
            const hs = state.boot?.host_status || {};
            const curCid = (rd.default_target_cid || '').trim();
            const curDirName = (rd.default_target_name || '').trim() || resolveKnownCidName(curCid);
            const autoFollowLabel = !curCid && rd.prefer_flowlink === false && hs.transfer_source_cid
              ? `${hs.transfer_source_folder || '转存监控'} (${hs.transfer_source_cid})`
              : '';
            return `
              <label style="display:flex;align-items:center;gap:8px;flex-wrap:wrap">
                <span>115 默认接收目录 CID（115 分享接收、仅转缺集与 ED2K/Magnet 原生离线共用）</span>
                ${curCid
                  ? `<span id="rl-cid-badge" class="mf-cid-badge">📂 ${esc(curDirName ? `${curDirName} (${curCid})` : `CID: ${curCid}`)}</span>`
                  : (autoFollowLabel
                    ? `<span id="rl-cid-badge" class="mf-cid-badge">📂 自动跟随：${esc(autoFollowLabel)}</span>`
                    : `<span id="rl-cid-badge" class="mf-cid-badge hidden"></span>`)}
              </label>
              <div class="mf-row">
                <input id="rl-cid" class="mf-input grow" value="${esc(curCid)}" placeholder="${hs.transfer_source_cid ? `留空在非直投模式下自动跟随宿主转存监控 (${esc(hs.transfer_source_folder || hs.transfer_source_cid)})` : '点右侧「浏览 115 目录」选择待整理中转文件夹 CID'}" />
                ${hs.transfer_source_cid ? `<button type="button" class="mf-btn sm" id="btn-fill-transfer-src" title="填入 Mediary 主程序配置的转存整理监控源目录 CID">同步转存监控 (${esc(hs.transfer_source_folder || 'CID')})</button>` : ''}
                <button type="button" class="mf-btn sm" id="btn-pick-cid">浏览 115 目录</button>
              </div>
              <span id="rl-cid-name-hint" style="font-size:11.5px;color:var(--mf-muted)">
                ${curCid
                  ? (curDirName
                    ? `当前接收目录：<strong>${esc(curDirName)}</strong> (${esc(curCid)})`
                    : `当前填写 CID：${esc(curCid)}（点击右上角「保存资源权重与落点」将自动解析 115 目录名）`)
                  : (rd.prefer_flowlink !== false
                    ? '当前整包直投宿主 FlowLink 自动整理；勾选部分单集精准接收时建议指定接收目录'
                    : (hs.transfer_source_cid
                      ? `当前留空，自动跟随宿主转存监控：<strong>${esc(hs.transfer_source_folder || '转存监控')}</strong> (${esc(hs.transfer_source_cid)})`
                      : '请点击右侧「浏览 115 目录」选择待整理中转文件夹'))}
              </span>
            `;
          })()}
        </div>
        <div class="mf-field" style="width:140px;margin-bottom:0">
          <label>最低准入分数</label>
          <input id="rl-min" type="number" class="mf-input" value="${rd.min_score ?? 0}" />
        </div>
      </div>
      <div class="mf-row" style="margin-top:10px">
        <label class="mf-row"><input type="checkbox" id="rl-flowlink" ${rd.prefer_flowlink !== false ? 'checked' : ''} /> 整包直投宿主 FlowLink 自动整理（关闭则 115 分享走 <code>share/receive</code>、ED2K/Magnet 走 <code>open115/offline</code> 纯落入中转目录 CID，交由下游 MediWash 守门归档）</label>
      </div>
    </div>

    <!-- NextFind 1:1 资源权重卡片 -->
    <div class="mf-card" style="padding:20px 22px">
      <div class="mf-card-head" style="margin-bottom:16px">
        <div class="nf-tg-head-title">
          <div class="nf-tg-icon-box">
            <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1"><path d="M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6" stroke-linecap="round" stroke-linejoin="round"/></svg>
          </div>
          <span>资源权重</span>
        </div>
        <div class="mf-card-sub">上下拖动左侧维度名改变大类权重，左右拖动芯片改变同级先后顺序</div>
      </div>

      <div class="nf-weight-rows">
        <!-- Row 1: 资源类型 -->
        <div class="nf-weight-row">
          <div class="nf-weight-label" style="padding-left:16px">资源类型</div>
          <div class="nf-chip-list">
            ${rd.resource_types.map((rt, idx) => `
              <div class="nf-chip draggable ${rt.enabled ? 'solid-blue' : 'blocked'}" draggable="true" data-chip-list="resource_types" data-chip-idx="${idx}">
                <span class="nf-chip-grip">⠿</span>
                <button type="button" class="nf-chip-toggle" data-chip-toggle="resource_types:${idx}" title="${rt.enabled ? '已启用（点击屏蔽此资源类型）' : '已屏蔽（点击启用）'}">${rt.enabled ? '✓' : '✕'}</button>
                <span>${esc(rt.name)}</span>
              </div>
            `).join('')}
          </div>
        </div>

        <!-- Row 2: 允许订阅 -->
        <div class="nf-weight-row">
          <div class="nf-weight-label" style="padding-left:16px">允许订阅</div>
          <div class="nf-chip-list">
            ${rd.subscription_sources.map((ss, idx) => `
              <div class="nf-chip draggable ${ss.enabled ? 'solid-blue' : 'blocked'}" draggable="true" data-chip-list="subscription_sources" data-chip-idx="${idx}">
                <span class="nf-chip-grip">⠿</span>
                <button type="button" class="nf-chip-toggle" data-chip-toggle="subscription_sources:${idx}" title="${ss.enabled ? '允许自动订阅追更（点击设为仅手动搜源）' : '仅手动搜源（点击允许自动追更）'}">${ss.enabled ? '✓' : '✕'}</button>
                <span>${esc(ss.name)}</span>
              </div>
            `).join('')}
          </div>
        </div>

        <!-- Draggable 6 Weight Categories -->
        ${rd.weights.map((catKey, catIdx) => {
          const meta = WEIGHT_CAT_META[catKey];
          if (!meta) return '';
          const list = rd[meta.field] || [];
          return `
            <div class="nf-weight-row" data-weight-cat-row="${catIdx}">
              <div class="nf-weight-label">
                <span class="nf-cat-drag" draggable="true" data-weight-cat-drag="${catIdx}" title="按住上下拖动调整「${esc(meta.label)}」维度整体权重">⠿</span>
                <span>${esc(meta.label)}</span>
              </div>
              <div class="nf-chip-list">
                ${list.map((item, idx) => {
                  if (item.is_custom_size) {
                    return `
                      <div class="nf-chip blocked" title="当资源体积大于 0 且小于设定 MB 时直接拦截（填 0 表示不限制）">
                        <span style="font-weight:700">✕</span>
                        <span>小于</span>
                        <input type="number" min="0" class="nf-size-inline-input" data-custom-mb-idx="${idx}" value="${Number(item.value || 0)}" placeholder="0" />
                        <span>MB</span>
                      </div>
                    `;
                  }
                  const gradStyle = item.enabled ? (styleMap.get(`${catKey}:${idx}`) || 'background-color:#2563eb;color:#fff;') : '';
                  const isGroup = catKey === 'groups';
                  return `
                    <div class="nf-chip draggable ${item.enabled ? '' : 'blocked'}" style="${gradStyle}" draggable="true" data-chip-list="${meta.field}" data-chip-idx="${idx}">
                      <span class="nf-chip-grip">⠿</span>
                      <button type="button" class="nf-chip-toggle" data-chip-toggle="${meta.field}:${idx}" title="${item.enabled ? '点击切换为屏蔽项' : '已屏蔽（点击恢复加分）'}">${item.enabled ? '✓' : '✕'}</button>
                      <span>${esc(item.name)}</span>
                      ${isGroup ? `<button type="button" class="nf-chip-del" data-del-group-kw="${idx}" title="删除关键词">×</button>` : ''}
                    </div>
                  `;
                }).join('')}

                ${catKey === 'groups'
                  ? (state.addingGroupKw
                    ? `<div class="nf-inline-add-box">
                         <input id="inp-new-group-kw" class="nf-inline-add-input" value="${esc(state.newGroupKw)}" placeholder="输入关键词/流媒体/压制组" />
                         <button type="button" class="mf-btn sm primary" style="height:26px;padding:0 8px" id="btn-confirm-group-kw">✓ 确认</button>
                         <button type="button" class="mf-btn sm" style="height:26px;padding:0 7px" id="btn-cancel-group-kw">×</button>
                       </div>
                       <div class="nf-quick-stream-bar">
                         <span class="nf-quick-stream-label">快捷点选流媒体平台：</span>
                         ${STREAM_PLATFORMS_ALL.map((p) => {
                           const exists = rd.group_items.some((x) => x.name.toLowerCase() === p.toLowerCase());
                           return `<button type="button" class="nf-quick-stream-btn ${exists ? 'added' : ''}" data-quick-group-kw="${esc(p)}" ${exists ? 'disabled' : ''}>${exists ? '✓ ' : '+ '}${esc(p)}</button>`;
                         }).join('')}
                       </div>`
                    : `<button type="button" class="nf-add-chip-btn" id="btn-show-add-group">+ 添加</button>`)
                  : ''}
              </div>
            </div>
          `;
        }).join('')}

        <hr class="nf-weight-divider" />

        <!-- Row 7: 屏蔽过滤 (支持多标签「且」组合) -->
        <div class="nf-weight-row">
          <div class="nf-weight-label" style="padding-left:16px">屏蔽过滤</div>
          <div class="nf-chip-list">
            ${rd.block_filters.map((grp, idx) => `
              <div class="nf-chip blocked">
                <span style="font-weight:700">✕</span>
                <span>${(grp.tags || []).map((t) => esc(t)).join('<span class="nf-chip-and">且</span>')}</span>
                <button type="button" class="nf-chip-del" data-del-bf="${idx}" title="移除此屏蔽规则">×</button>
              </div>
            `).join('')}
            <button type="button" class="nf-add-chip-btn danger" id="btn-add-block-filter">+ 添加</button>
          </div>
        </div>

        <!-- Row 8: 忽略文本 -->
        <div class="nf-weight-row">
          <div class="nf-weight-label" style="padding-left:16px">忽略文本</div>
          <div class="nf-chip-list">
            ${rd.ignored_texts.map((txt, idx) => `
              <div class="nf-chip blocked">
                <span>⊘</span>
                <span>${esc(txt)}</span>
                <button type="button" class="nf-chip-del" data-del-ig-txt="${idx}" title="移除忽略词">×</button>
              </div>
            `).join('')}
            ${state.addingIgnoredTxt
              ? `<div class="nf-inline-add-box">
                   <input id="inp-new-ig-txt" class="nf-inline-add-input" value="${esc(state.newIgnoredTxt)}" placeholder="输入需剔除的广告词" />
                   <button type="button" class="mf-btn sm primary" style="height:26px;padding:0 8px" id="btn-confirm-ig-txt">✓ 确认</button>
                   <button type="button" class="mf-btn sm" style="height:26px;padding:0 7px" id="btn-cancel-ig-txt">×</button>
                 </div>`
              : `<button type="button" class="nf-add-chip-btn danger" id="btn-show-add-ig-txt">+ 添加</button>`}
          </div>
        </div>
      </div>
    </div>

    <div class="mf-card">
      <div class="mf-card-head">
        <div>
          <div class="mf-card-title">🛡️ 私有 115 分享神盾索引库（SQLite 已收录 ${st.shield_files_total || 0} 个媒体文件）</div>
          <div class="mf-card-sub">支持大合集目录断点续扫，收录后在「聚合搜片」与原生搜索中离线 0 延迟命中，并按 0.1% 体积容差比对媒体库</div>
        </div>
      </div>
      <form id="shield-add-form" class="mf-row" style="margin-bottom:12px">
        <input id="sh-title" class="mf-input" style="width:160px" placeholder="合集备注名(选填)" />
        <input id="sh-url" class="mf-input grow" placeholder="粘贴 115 分享链接 https://115.com/s/xxx?password=yyyy" required />
        <button type="submit" class="mf-btn primary">+ 添加并开始索引</button>
      </form>
      ${shares.length === 0
        ? `<div class="mf-empty">暂未登记私有神盾分享链接</div>`
        : `<table class="mf-table">
            <thead><tr><th>分享名称</th><th>分享码</th><th>状态</th><th>已扫目录 / 待扫</th><th>收录视频/字幕</th><th>操作</th></tr></thead>
            <tbody>
              ${shares.map((s) => `
                <tr>
                  <td><strong>${esc(s.title)}</strong>${s.last_error ? `<div style="color:var(--mf-danger);font-size:11.5px">${esc(s.last_error)}</div>` : ''}</td>
                  <td>${esc(s.share_code)}</td>
                  <td><span class="mf-pill ${s.status === 'completed' ? 'success' : s.status === 'error' ? 'danger' : 'warn'}">${esc(s.status)}</span></td>
                  <td>${(s.processed_cids || []).length} / ${(s.pending_cids || []).length}</td>
                  <td><strong>${s.parsed_files || 0}</strong></td>
                  <td class="mf-actions">
                    ${(s.pending_cids || []).length > 0 ? `<button class="mf-btn sm primary" data-step-sh="${esc(s.id)}">继续解析一批</button>` : ''}
                    <button class="mf-btn sm danger" data-del-sh="${esc(s.id)}">删除</button>
                  </td>
                </tr>
              `).join('')}
            </tbody>
          </table>`}
    </div>

    <div class="mf-card">
      <div class="mf-card-head">
        <div>
          <div class="mf-card-title">实时标题/季集解析与权重打分测试器</div>
          <div class="mf-card-sub">粘贴任意 TG 消息文本、文件名或分享链接，实时验证忽略文本净化、纯片名、SxxExx 季集识别与当前权重得分（请先保存上方修改再测试）</div>
        </div>
        <button class="mf-btn sm primary" id="btn-run-test">立即解析打分</button>
      </div>
      <textarea id="test-text" class="mf-textarea" rows="3">${esc(tIn.text)}</textarea>
      <div class="mf-row" style="margin-top:8px">
        <input id="test-size" class="mf-input" style="width:130px" placeholder="体积 如 48GB" value="${esc(tIn.size_str)}" />
        <select id="test-mtype" class="mf-select" style="width:120px">
          <option value="tv" ${tIn.media_type === 'tv' ? 'selected' : ''}>剧集 (TV)</option>
          <option value="movie" ${tIn.media_type === 'movie' ? 'selected' : ''}>电影 (Movie)</option>
        </select>
      </div>
      ${tOut ? `
        <div class="mf-code" style="margin-top:10px">
          <div><strong>提取纯片名：</strong>${esc(tOut.pure_title || '-')} · <strong>年份：</strong>${esc(tOut.year || '-')} · <strong>TMDB：</strong>${esc(tOut.tmdb_id || '-')}</div>
          <div><strong>识别季集范围：</strong>${esc(tOut.parsed_episodes || '无')} · <strong>链接数：</strong>${(tOut.links || []).length} 条</div>
          <div><strong>规格属性：</strong>分辨率=${esc(tOut.attributes?.resolution || '-')} | 流媒体平台=${esc(tOut.attributes?.web_source || '-')} | 片源=${esc(tOut.attributes?.remux || '-')} | 色域=${esc(tOut.attributes?.color || '-')} | 编码=${esc(tOut.video_encode || '-')} | 音频=${esc(tOut.attributes?.audio || '-')} | 字幕=${esc(tOut.attributes?.subtitle || '-')} | 压制组=${esc(tOut.attributes?.group || '-')} | 后缀=${esc(tOut.attributes?.ext || '-')}</div>
          <div><strong>权重打分结果：</strong>${tOut.score?.accepted ? '✅ 准入通过' : '❌ 已拦截'} · 总分 <strong>${tOut.score?.score}</strong> (${esc((tOut.score?.reasons || []).join(' · '))})</div>
        </div>
      ` : ''}
    </div>
  `;

  const fillSrcBtn = root.querySelector('#btn-fill-transfer-src');
  if (fillSrcBtn) {
    fillSrcBtn.onclick = () => {
      syncRulesTopDOM(root, rd);
      rd.default_target_cid = state.boot?.host_status?.transfer_source_cid || '';
      rd.default_target_name = state.boot?.host_status?.transfer_source_folder || '';
      renderRulesTab(root);
    };
  }

  const rlCidInp = root.querySelector('#rl-cid');
  if (rlCidInp) {
    rlCidInp.oninput = () => {
      const val = rlCidInp.value.trim();
      const nm = val === (rd.default_target_cid || '').trim()
        ? (rd.default_target_name || resolveKnownCidName(val))
        : resolveKnownCidName(val);
      const badge = root.querySelector('#rl-cid-badge');
      const hint = root.querySelector('#rl-cid-name-hint');
      if (badge) {
        if (val) {
          badge.textContent = `📂 ${nm ? `${nm} (${val})` : `CID: ${val}`}`;
          badge.classList.remove('hidden');
        } else {
          badge.classList.add('hidden');
        }
      }
      if (hint) {
        hint.innerHTML = val
          ? (nm
            ? `当前接收目录：<strong>${esc(nm)}</strong> (${esc(val)})`
            : `当前填写 CID：${esc(val)}（点击右上角「保存资源权重与落点」将自动解析 115 目录名）`)
          : '已清空指定 CID，保存后按下方分流模式自动处理';
      }
    };
  }

  root.querySelector('#btn-pick-cid').onclick = () => {
    syncRulesTopDOM(root, rd);
    open115FolderPicker((cid, dirName) => {
      rd.default_target_cid = cid;
      rd.default_target_name = dirName || '';
      renderRulesTab(root);
    });
  };

  root.querySelector('#btn-reset-weights').onclick = () => {
    syncRulesTopDOM(root, rd);
    const keepCid = rd.default_target_cid;
    const keepName = rd.default_target_name;
    const keepFl = rd.prefer_flowlink;
    const keepMin = rd.min_score;
    state.rulesDraft = buildDefaultRulesDraft();
    state.rulesDraft.default_target_cid = keepCid;
    state.rulesDraft.default_target_name = keepName;
    state.rulesDraft.prefer_flowlink = keepFl;
    state.rulesDraft.min_score = keepMin;
    const curChs = state.channelDraft?.telegram?.channels || state.boot?.state?.telegram?.channels || [];
    syncDraftSubSourcesWithChannels(state.rulesDraft, curChs);
    renderRulesTab(root);
    showToast('已恢复默认权重布局，点击右上角「保存资源权重与落点」生效', 'info');
  };

  root.querySelector('#btn-save-rules').onclick = () => {
    syncRulesTopDOM(root, rd);
    callAction('ui_save_rules', rd);
  };

  // Toggle chip enabled / blocked
  root.querySelectorAll('[data-chip-toggle]').forEach((btn) => {
    btn.onclick = (e) => {
      e.stopPropagation();
      syncRulesTopDOM(root, rd);
      const [listName, idxStr] = btn.dataset.chipToggle.split(':');
      const arr = rd[listName];
      const item = arr?.[Number(idxStr)];
      if (!item) return;
      item.enabled = !item.enabled;
      renderRulesTab(root);
    };
  });

  // Custom min-size MB input
  root.querySelectorAll('[data-custom-mb-idx]').forEach((inp) => {
    inp.oninput = () => {
      const item = rd.size_items[Number(inp.dataset.customMbIdx)];
      if (item) item.value = Math.max(0, Number(inp.value || 0));
    };
  });

  // Chip horizontal drag-and-drop within each list
  root.querySelectorAll('[data-chip-list]').forEach((chipEl) => {
    const listName = chipEl.dataset.chipList;
    const idx = Number(chipEl.dataset.chipIdx);
    chipEl.ondragstart = (e) => {
      e.stopPropagation();
      syncRulesTopDOM(root, rd);
      state.dragRuleChip = { listName, idx };
      chipEl.classList.add('dragging');
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData('text/plain', `${listName}:${idx}`);
    };
    chipEl.ondragend = () => {
      state.dragRuleChip = null;
      root.querySelectorAll('.nf-chip').forEach((el) => el.classList.remove('dragging', 'drag-over'));
    };
    chipEl.ondragover = (e) => {
      if (!state.dragRuleChip || state.dragRuleChip.listName !== listName || state.dragRuleChip.idx === idx) return;
      e.preventDefault();
      e.stopPropagation();
      chipEl.classList.add('drag-over');
    };
    chipEl.ondragleave = () => chipEl.classList.remove('drag-over');
    chipEl.ondrop = (e) => {
      if (!state.dragRuleChip || state.dragRuleChip.listName !== listName) return;
      e.preventDefault();
      e.stopPropagation();
      chipEl.classList.remove('drag-over');
      const from = state.dragRuleChip.idx;
      state.dragRuleChip = null;
      if (from === idx) return;
      const arr = rd[listName];
      if (!Array.isArray(arr)) return;
      const [moved] = arr.splice(from, 1);
      arr.splice(idx, 0, moved);
      renderRulesTab(root);
    };
  });

  // Category row vertical drag-and-drop (`rd.weights`)
  root.querySelectorAll('[data-weight-cat-drag]').forEach((handle) => {
    const catIdx = Number(handle.dataset.weightCatDrag);
    const rowEl = root.querySelector(`[data-weight-cat-row="${catIdx}"]`);
    handle.ondragstart = (e) => {
      e.stopPropagation();
      syncRulesTopDOM(root, rd);
      state.dragWeightCat = catIdx;
      if (rowEl) rowEl.classList.add('cat-dragging');
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData('text/plain', `cat:${catIdx}`);
    };
    handle.ondragend = () => {
      state.dragWeightCat = null;
      root.querySelectorAll('.nf-weight-row').forEach((r) => r.classList.remove('cat-dragging', 'cat-drag-over'));
    };
  });

  root.querySelectorAll('[data-weight-cat-row]').forEach((rowEl) => {
    const targetIdx = Number(rowEl.dataset.weightCatRow);
    rowEl.ondragover = (e) => {
      if (state.dragWeightCat === null || state.dragWeightCat === targetIdx) return;
      e.preventDefault();
      rowEl.classList.add('cat-drag-over');
    };
    rowEl.ondragleave = () => rowEl.classList.remove('cat-drag-over');
    rowEl.ondrop = (e) => {
      if (state.dragWeightCat === null) return;
      e.preventDefault();
      rowEl.classList.remove('cat-drag-over');
      const from = state.dragWeightCat;
      state.dragWeightCat = null;
      if (from === targetIdx) return;
      const [movedCat] = rd.weights.splice(from, 1);
      rd.weights.splice(targetIdx, 0, movedCat);
      renderRulesTab(root);
    };
  });

  // Group keyword add & delete
  root.querySelectorAll('[data-del-group-kw]').forEach((btn) => {
    btn.onclick = (e) => {
      e.stopPropagation();
      syncRulesTopDOM(root, rd);
      rd.group_items.splice(Number(btn.dataset.delGroupKw), 1);
      renderRulesTab(root);
    };
  });
  const showAddGroupBtn = root.querySelector('#btn-show-add-group');
  if (showAddGroupBtn) {
    showAddGroupBtn.onclick = () => {
      syncRulesTopDOM(root, rd);
      state.addingGroupKw = true;
      state.newGroupKw = '';
      renderRulesTab(root);
      root.querySelector('#inp-new-group-kw')?.focus();
    };
  }
  const confirmGroupKw = () => {
    syncRulesTopDOM(root, rd);
    const val = (root.querySelector('#inp-new-group-kw')?.value || '').trim();
    if (val && !rd.group_items.some((x) => x.name.toLowerCase() === val.toLowerCase())) {
      rd.group_items.push({ name: val, enabled: true });
    }
    state.addingGroupKw = false;
    state.newGroupKw = '';
    renderRulesTab(root);
  };
  const btnConfirmGroup = root.querySelector('#btn-confirm-group-kw');
  if (btnConfirmGroup) btnConfirmGroup.onclick = confirmGroupKw;
  const btnCancelGroup = root.querySelector('#btn-cancel-group-kw');
  if (btnCancelGroup) {
    btnCancelGroup.onclick = () => {
      syncRulesTopDOM(root, rd);
      state.addingGroupKw = false;
      state.newGroupKw = '';
      renderRulesTab(root);
    };
  }
  const inpGroup = root.querySelector('#inp-new-group-kw');
  if (inpGroup) {
    inpGroup.oninput = () => { state.newGroupKw = inpGroup.value; };
    inpGroup.onkeydown = (e) => {
      if (e.key === 'Enter') { e.preventDefault(); confirmGroupKw(); }
      if (e.key === 'Escape') { state.addingGroupKw = false; renderRulesTab(root); }
    };
  }
  root.querySelectorAll('[data-quick-group-kw]').forEach((btn) => {
    btn.onclick = () => {
      syncRulesTopDOM(root, rd);
      const val = (btn.dataset.quickGroupKw || '').trim();
      if (val && !rd.group_items.some((x) => x.name.toLowerCase() === val.toLowerCase())) {
        rd.group_items.push({ name: val, enabled: true });
      }
      renderRulesTab(root);
    };
  });

  // Block Filters (`且` combinations) add & delete
  root.querySelector('#btn-add-block-filter').onclick = () => {
    syncRulesTopDOM(root, rd);
    openBlockFilterModal(rd, root);
  };
  root.querySelectorAll('[data-del-bf]').forEach((btn) => {
    btn.onclick = (e) => {
      e.stopPropagation();
      syncRulesTopDOM(root, rd);
      rd.block_filters.splice(Number(btn.dataset.delBf), 1);
      renderRulesTab(root);
    };
  });

  // Ignored texts add & delete
  root.querySelectorAll('[data-del-ig-txt]').forEach((btn) => {
    btn.onclick = (e) => {
      e.stopPropagation();
      syncRulesTopDOM(root, rd);
      rd.ignored_texts.splice(Number(btn.dataset.delIgTxt), 1);
      renderRulesTab(root);
    };
  });
  const showAddIgBtn = root.querySelector('#btn-show-add-ig-txt');
  if (showAddIgBtn) {
    showAddIgBtn.onclick = () => {
      syncRulesTopDOM(root, rd);
      state.addingIgnoredTxt = true;
      state.newIgnoredTxt = '';
      renderRulesTab(root);
      root.querySelector('#inp-new-ig-txt')?.focus();
    };
  }
  const confirmIgTxt = () => {
    syncRulesTopDOM(root, rd);
    const val = (root.querySelector('#inp-new-ig-txt')?.value || '').trim();
    if (val && !rd.ignored_texts.includes(val)) {
      rd.ignored_texts.push(val);
    }
    state.addingIgnoredTxt = false;
    state.newIgnoredTxt = '';
    renderRulesTab(root);
  };
  const btnConfirmIg = root.querySelector('#btn-confirm-ig-txt');
  if (btnConfirmIg) btnConfirmIg.onclick = confirmIgTxt;
  const btnCancelIg = root.querySelector('#btn-cancel-ig-txt');
  if (btnCancelIg) {
    btnCancelIg.onclick = () => {
      syncRulesTopDOM(root, rd);
      state.addingIgnoredTxt = false;
      state.newIgnoredTxt = '';
      renderRulesTab(root);
    };
  }
  const inpIg = root.querySelector('#inp-new-ig-txt');
  if (inpIg) {
    inpIg.oninput = () => { state.newIgnoredTxt = inpIg.value; };
    inpIg.onkeydown = (e) => {
      if (e.key === 'Enter') { e.preventDefault(); confirmIgTxt(); }
      if (e.key === 'Escape') { state.addingIgnoredTxt = false; renderRulesTab(root); }
    };
  }

  // Private Shield Indexer & Rule Tester
  root.querySelector('#shield-add-form').onsubmit = (e) => {
    e.preventDefault();
    callAction('ui_add_shield_share', {
      title: root.querySelector('#sh-title').value.trim(),
      share_url: root.querySelector('#sh-url').value.trim(),
      run_now: true,
    });
  };
  root.querySelectorAll('[data-step-sh]').forEach((b) => {
    b.onclick = () => callAction('ui_step_shield_share', { id: b.dataset.stepSh, max_dirs: 15 });
  });
  root.querySelectorAll('[data-del-sh]').forEach((b) => {
    b.onclick = () => callAction('ui_delete_shield_share', { id: b.dataset.delSh });
  });

  root.querySelector('#btn-run-test').onclick = async () => {
    syncRulesTopDOM(root, rd);
    state.ruleTestInput.text = root.querySelector('#test-text').value;
    state.ruleTestInput.size_str = root.querySelector('#test-size').value.trim();
    state.ruleTestInput.media_type = root.querySelector('#test-mtype').value;
    state.ruleTestOut = await bridge.action('ui_test_rule', state.ruleTestInput);
    render();
  };
}

/* ==================== TAB 5: 转存历史 (Separated Transfer History vs Task Logs + Filter + Unlock & Clear) ==================== */
function buildHistoryFilterPayload() {
  return {
    record_type: state.histType,
    source: state.histSource,
    action_group: state.histType === 'transfer' ? state.histAction : 'all',
    status: state.histStatus,
    keyword: state.histKeyword.trim(),
    limit: 300,
  };
}

async function loadHistory(notifyMsg = '') {
  state.histLoading = true;
  render();
  try {
    const res = await bridge.action('ui_history_list', buildHistoryFilterPayload());
    state.histData = res;
    if (state.boot?.state) {
      state.boot.state.transfer_history_total = res.transfer_total ?? 0;
      state.boot.state.task_history_total = res.task_total ?? 0;
      state.boot.state.dedupe_locks_total = res.dedupe_locks_total ?? 0;
    }
    if (notifyMsg) showToast(notifyMsg, 'success');
  } catch (err) {
    showToast(err.message || String(err), 'error');
  } finally {
    state.histLoading = false;
    render();
  }
}

function resolveKnownCidName(cid) {
  const c = String(cid || '').trim();
  if (!c || c === '0') return '';
  const rules = state.boot?.state?.rules || {};
  const draft = state.rulesDraft || {};
  if (draft.default_target_cid && draft.default_target_cid.trim() === c && draft.default_target_name) {
    return draft.default_target_name.trim();
  }
  if (rules.default_target_cid && rules.default_target_cid.trim() === c && rules.default_target_name) {
    return rules.default_target_name.trim();
  }
  const hs = state.boot?.host_status || {};
  if (hs.transfer_source_cid === c && hs.transfer_source_folder) return hs.transfer_source_folder;
  if (hs.target_cid === c && hs.target_folder) return hs.target_folder;
  if (hs.offline_folder_cid === c && hs.offline_folder) return hs.offline_folder;
  return '';
}

function humanizeCidInText(raw) {
  const text = String(raw || '').trim();
  if (!text) return '';
  return text.replace(/(?:「([^」]+)」\s*)?\(CID:(\d+)\)/g, (full, existingName, cid) => {
    if (existingName) return `「${existingName}」(CID:${cid})`;
    const nm = resolveKnownCidName(cid);
    return nm ? `「${nm}」(CID:${cid})` : full;
  });
}

function fmtCompactDetailCell(it) {
  const raw = String(it?.detail || '').trim();
  if (!raw) return `<span style="color:var(--mf-muted)">-</span>`;

  const fullDetail = humanizeCidInText(raw);
  const cidMatch = raw.match(/\(CID:(\d+)\)/);
  const dirMatch = fullDetail.match(/「([^」]+)」/);
  const dirName = dirMatch ? dirMatch[1] : (cidMatch ? resolveKnownCidName(cidMatch[1]) : '');
  const dirSuffix = dirName ? ` → ${dirName}` : '';

  let short = '';
  let m;
  if ((m = raw.match(/精准选集接收\s*(\d+)\s*个文件/))) {
    short = `选集 ${m[1]} 项${dirSuffix || ' → 中转目录'}`;
  } else if ((m = raw.match(/精准选集转存\s*(\d+)\s*个文件/))) {
    short = `选集 ${m[1]} 项 → FlowLink`;
  } else if (raw.includes('整包接收至')) {
    short = `整包接收${dirSuffix || ' → 中转目录'}`;
  } else if ((m = raw.match(/批量提交\s*(\d+)\s*个\s*ED2K/i))) {
    short = `ED2K 离线 ${m[1]} 项${dirSuffix}`;
  } else if ((m = raw.match(/已提交\s*(\d+)\s*个\s*ED2K/i))) {
    short = `ED2K 转存 ${m[1]} 项`;
  } else if (raw.includes('115 离线下载提交') || raw.includes('115 离线目录')) {
    short = `磁力离线${dirSuffix}`;
  } else if (raw.includes('已接收至 115 目录')) {
    short = `已接收${dirSuffix || '至 115'}`;
  } else if (raw.includes('审核中')) {
    short = '115 审核中待重试';
  } else if (raw.includes('连接影巢中转服务失败')) {
    short = '连接影巢中转失败';
  } else if ((m = raw.match(/剩余额度\s*(\d+)/))) {
    short = `授权正常 · 余 ${m[1]}`;
  } else if ((m = raw.match(/追更命中\s*(\d+\/\d+)/))) {
    short = `巡检完成 · 命中 ${m[1]}`;
  } else {
    short = fullDetail.replace(/，等待下游守门整理/g, '').trim();
    if (short.length > 18) short = `${short.slice(0, 17)}…`;
  }

  const tipLines = [fullDetail];
  if (it?.channel_name && it.channel_name !== '手动直链') {
    tipLines.push(`来源频道：${it.channel_name}`);
  }
  if (it?.slug) {
    tipLines.push(`资源链接：${it.slug}`);
  }
  if (it?.updated_at) {
    tipLines.push(`记录时间：${it.updated_at}`);
  }

  return `
    <span class="mf-detail-cell" data-tip="${esc(tipLines.join('\n'))}">
      <span class="mf-detail-short">${esc(short)}</span>
      <span class="mf-info-icon" aria-hidden="true">i</span>
    </span>
  `;
}

function cleanHistoryTitleText(raw) {
  return String(raw || '')
    .replace(/^[\s📺🎬🎞️🎥📽️🔥✨📌🔔💿📀🆕]+/u, '')
    .replace(/^(?:(?:名称|片名|标题|剧名|影视|电影|剧集|电视剧|连续剧|动漫|动画|综艺|纪录片|短剧)[：:\s]+)+/u, '')
    .trim() || '-';
}

function fmtShortHistoryTime(raw) {
  const s = String(raw || '').trim();
  const m = s.match(/^\d{4}-(\d{2}-\d{2}\s+\d{2}:\d{2})/);
  return m ? m[1] : (s || '-');
}

function fmtHistorySourceBadge(it) {
  const ch = (it.channel_name || '').trim();
  const src = (it.source || '').trim();
  if (ch && ch !== '手动直链') {
    const shortCh = ch.replace(/^TG\s*·\s*/i, '').trim() || ch;
    return `<span class="mf-pill info mf-pill-clip" data-tip="${esc(`${ch} (${src})`)}" style="max-width:112px">${esc(shortCh)}</span>`;
  }
  const map = {
    telegram: 'Telegram',
    hdhive: '影巢/HDHive',
    ayclub: 'AYClub',
    shield: '私有神盾',
    rss: 'RSS 订阅',
    manual: '手动直链',
    system: '系统巡检',
    '115': '115 分享',
    ed2k: 'ED2K 离线',
    magnet: '磁力离线',
  };
  return `<span class="mf-pill mf-pill-clip" style="max-width:112px">${esc(map[src.toLowerCase()] || src || '-')}</span>`;
}

function fmtHistoryStatusPill(it) {
  const st = String(it.status || '').toLowerCase();
  const tone = st === 'success' ? 'success' : st === 'reviewing' ? 'warn' : 'danger';
  const rawAct = String(it.action || st || '-').trim();
  const shortMap = {
    '订阅自动追更': '自动追更',
    'TG全量频道自动接收': 'TG全量',
    'TG入新频道自动进货': 'TG入新',
    'TG洗版频道高分升级': 'TG洗版',
    '原生资源搜索一键转存': '原生搜索',
    '预览弹窗整包 FlowLink 转存': '整包转存',
    '预览弹窗选集转存': '选集转存',
    '待审队列自动重试': '待审重试',
    '历史记录重新转存': '历史重转',
  };
  const label = shortMap[rawAct] || (rawAct.length > 6 ? `${rawAct.slice(0, 5)}…` : rawAct);
  return `<span class="mf-pill ${tone}" data-tip="${esc(`触发方式：${rawAct}`)}">${esc(label)}</span>`;
}

function renderHistoryTab(root) {
  const hd = state.histData || {
    items: [],
    transfer_total: state.boot?.state?.transfer_history_total || 0,
    task_total: state.boot?.state?.task_history_total || 0,
    filtered_total: 0,
    dedupe_locks_total: state.boot?.state?.dedupe_locks_total || 0,
  };
  const items = hd.items || [];
  const isTransfer = state.histType === 'transfer';
  const hasActiveFilter =
    state.histSource !== 'all' ||
    (isTransfer && state.histAction !== 'all') ||
    state.histStatus !== 'all' ||
    !!state.histKeyword.trim();

  root.innerHTML = `
    <div class="mf-header">
      <div>
        <h2>转存历史与任务记录管理</h2>
        <p>影视转存与后台签到/巡检日志分离管理；删除转存记录时默认同步解除 <code>transferred_slugs</code> 去重锁（当前锁数：${hd.dedupe_locks_total || 0}），方便误删后重新追更或转存</p>
      </div>
      <div class="mf-actions">
        <button type="button" class="mf-btn" id="btn-hist-refresh" ${state.histLoading ? 'disabled' : ''}>刷新列表</button>
      </div>
    </div>

    <div class="mf-card">
      <div class="mf-card-head" style="margin-bottom:12px">
        <div class="mf-cat-tabs" style="margin-bottom:0">
          <button type="button" class="mf-cat-btn ${isTransfer ? 'active' : ''}" data-htype="transfer">
            🎬 影视转存历史 (${Number(hd.transfer_total || 0)})
          </button>
          <button type="button" class="mf-cat-btn ${!isTransfer ? 'active' : ''}" data-htype="task">
            ⚙️ 签到与任务记录 (${Number(hd.task_total || 0)})
          </button>
        </div>

        <div class="mf-actions">
          ${isTransfer ? `
            <label class="mf-row" style="font-size:12px;color:var(--mf-muted);margin-right:4px" title="勾选后删除历史记录时会一并移除对应的去重记录，使同一链接允许再次自动或手动转存">
              <input type="checkbox" id="chk-hist-unlock" ${state.histUnlockDedupe ? 'checked' : ''} />
              <span>删除时同步解除去重锁</span>
            </label>
            <button type="button" class="mf-btn sm primary" id="btn-hist-retry-sel" ${state.histLoading ? 'disabled' : ''}>
              重新转存勾选 (${state.histSelected.size})
            </button>
          ` : ''}
          <button type="button" class="mf-btn sm danger" id="btn-hist-del-sel" ${state.histLoading ? 'disabled' : ''}>
            删除勾选 (${state.histSelected.size})
          </button>
          <button type="button" class="mf-btn sm warn" id="btn-hist-clear-filtered" ${state.histLoading ? 'disabled' : ''}>
            ${hasActiveFilter ? `清空当前筛选 (${Number(hd.filtered_total || 0)})` : `清空全部${isTransfer ? '转存历史' : '任务记录'} (${Number(hd.filtered_total || 0)})`}
          </button>
        </div>
      </div>

      <form id="hist-filter-form" class="mf-row" style="margin-bottom:12px">
        <select id="sel-hist-src" class="mf-select" style="width:146px">
          <option value="all" ${state.histSource === 'all' ? 'selected' : ''}>全部来源渠道</option>
          <option value="telegram" ${state.histSource === 'telegram' ? 'selected' : ''}>Telegram 频道</option>
          <option value="hdhive" ${state.histSource === 'hdhive' ? 'selected' : ''}>影巢 / HDHive</option>
          <option value="ayclub" ${state.histSource === 'ayclub' ? 'selected' : ''}>AYClub</option>
          <option value="shield" ${state.histSource === 'shield' ? 'selected' : ''}>私有神盾</option>
          <option value="rss" ${state.histSource === 'rss' ? 'selected' : ''}>RSS 订阅</option>
          ${isTransfer
            ? `<option value="manual" ${state.histSource === 'manual' ? 'selected' : ''}>手动直链 / 115</option>`
            : `<option value="system" ${state.histSource === 'system' ? 'selected' : ''}>系统巡检</option>`}
        </select>

        ${isTransfer ? `
          <select id="sel-hist-act" class="mf-select" style="width:154px">
            <option value="all" ${state.histAction === 'all' ? 'selected' : ''}>全部触发方式</option>
            <option value="manual" ${state.histAction === 'manual' ? 'selected' : ''}>手动与搜索转存</option>
            <option value="chase" ${state.histAction === 'chase' ? 'selected' : ''}>订阅自动追更</option>
            <option value="tg_auto" ${state.histAction === 'tg_auto' ? 'selected' : ''}>TG 入新 / 全量</option>
            <option value="review" ${state.histAction === 'review' ? 'selected' : ''}>待审暂存与重试</option>
            <option value="rss" ${state.histAction === 'rss' ? 'selected' : ''}>RSS 自动转存</option>
          </select>
        ` : ''}

        <select id="sel-hist-st" class="mf-select" style="width:128px">
          <option value="all" ${state.histStatus === 'all' ? 'selected' : ''}>全部状态</option>
          <option value="success" ${state.histStatus === 'success' ? 'selected' : ''}>成功</option>
          ${isTransfer ? `<option value="reviewing" ${state.histStatus === 'reviewing' ? 'selected' : ''}>审核中</option>` : ''}
          <option value="error" ${state.histStatus === 'error' ? 'selected' : ''}>失败 / 告警</option>
        </select>

        <input id="inp-hist-kw" class="mf-input grow" value="${esc(state.histKeyword)}" placeholder="${isTransfer ? '搜索片名、TMDB ID、TG 频道名、115 分享码或结果说明…' : '搜索签到或巡检摘要关键词…'}" />
        <button type="submit" class="mf-btn sm primary" ${state.histLoading ? 'disabled' : ''}>筛选查找</button>
        ${hasActiveFilter ? `<button type="button" class="mf-btn sm" id="btn-hist-reset">重置筛选</button>` : ''}
      </form>

      ${state.histLoading && !state.histData
        ? `<div class="mf-empty">正在加载历史记录…</div>`
        : items.length === 0
          ? `<div class="mf-empty">${hasActiveFilter ? '当前筛选条件下暂无匹配的记录' : (isTransfer ? '暂无影视转存历史记录' : '暂无签到或后台任务记录')}</div>`
          : `<div class="mf-hist-table-wrap">
              <table class="mf-hist-table">
                <colgroup>
                  <col style="width:32px" />
                  <col style="width:124px" />
                  <col />
                  ${isTransfer ? `<col style="width:94px" />` : ''}
                  <col style="width:88px" />
                  <col style="width:154px" />
                  <col style="width:92px" />
                  <col style="width:100px" />
                </colgroup>
                <thead>
                  <tr>
                    <th><input type="checkbox" id="chk-hist-all" ${items.length > 0 && items.every((it) => state.histSelected.has(it.seq)) ? 'checked' : ''} /></th>
                    <th>来源 / 频道</th>
                    <th>${isTransfer ? '影视标题 / 关联标识' : '任务名称'}</th>
                    ${isTransfer ? `<th>季集</th>` : ''}
                    <th>触发动作</th>
                    <th>执行说明</th>
                    <th>时间</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  ${items.map((it) => {
                    const checked = state.histSelected.has(it.seq) ? 'checked' : '';
                    const canRetry = isTransfer && !!(it.slug || it.tmdb_id);
                    const subLink = it.slug || it.clean_slug || '';
                    const subParts = [];
                    if (it.tmdb_id) subParts.push(`TMDB:${it.tmdb_id}`);
                    if (it.clean_slug) subParts.push(it.clean_slug);
                    else if (subLink) subParts.push(subLink);
                    const subMeta = subParts.join(' · ');
                    const cleanTitle = isTransfer ? cleanHistoryTitleText(it.title) : (it.title || '-');
                    return `
                      <tr>
                        <td><input type="checkbox" class="js-chk-hist" data-seq="${it.seq}" ${checked} /></td>
                        <td style="overflow:hidden;white-space:nowrap">${fmtHistorySourceBadge(it)}</td>
                        <td style="overflow:hidden">
                          <div style="font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" data-tip="${esc(it.title || '')}">${esc(cleanTitle)}</div>
                          ${subMeta ? `<div style="font-size:11.2px;color:var(--mf-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap" data-tip="${esc(subLink || subMeta)}">${esc(subMeta)}</div>` : ''}
                        </td>
                        ${isTransfer ? `
                          <td style="font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" ${it.episodes ? `data-tip="${esc(it.episodes)}"` : ''}>
                            ${esc(it.episodes || '-')}
                          </td>
                        ` : ''}
                        <td style="white-space:nowrap">${fmtHistoryStatusPill(it)}</td>
                        <td style="overflow:hidden;white-space:nowrap">${fmtCompactDetailCell(it)}</td>
                        <td style="font-size:11.8px;color:var(--mf-muted);white-space:nowrap" data-tip="${esc(it.updated_at || '')}">${esc(fmtShortHistoryTime(it.updated_at))}</td>
                        <td style="white-space:nowrap">
                          <div class="mf-actions" style="flex-wrap:nowrap;gap:5px">
                            ${canRetry ? `<button type="button" class="mf-btn sm primary" style="padding:3px 7px;font-size:11.5px" data-hist-retry="${it.seq}">重转</button>` : ''}
                            <button type="button" class="mf-btn sm danger" style="padding:3px 7px;font-size:11.5px" data-hist-del="${it.seq}">删除</button>
                          </div>
                        </td>
                      </tr>
                    `;
                  }).join('')}
                </tbody>
              </table>
            </div>`}
    </div>
  `;

  root.querySelector('#btn-hist-refresh').onclick = () => loadHistory();

  root.querySelectorAll('[data-htype]').forEach((btn) => {
    btn.onclick = () => {
      state.histType = btn.dataset.htype;
      state.histSource = 'all';
      state.histAction = 'all';
      state.histStatus = 'all';
      state.histSelected.clear();
      loadHistory();
    };
  });

  const unlockChk = root.querySelector('#chk-hist-unlock');
  if (unlockChk) {
    unlockChk.onchange = () => { state.histUnlockDedupe = unlockChk.checked; };
  }

  const applyFilter = () => {
    state.histSource = root.querySelector('#sel-hist-src').value;
    const actEl = root.querySelector('#sel-hist-act');
    if (actEl) state.histAction = actEl.value;
    state.histStatus = root.querySelector('#sel-hist-st').value;
    state.histKeyword = root.querySelector('#inp-hist-kw').value.trim();
    state.histSelected.clear();
    loadHistory();
  };

  root.querySelector('#hist-filter-form').onsubmit = (e) => {
    e.preventDefault();
    applyFilter();
  };
  root.querySelector('#sel-hist-src').onchange = applyFilter;
  const actSel = root.querySelector('#sel-hist-act');
  if (actSel) actSel.onchange = applyFilter;
  root.querySelector('#sel-hist-st').onchange = applyFilter;

  const resetBtn = root.querySelector('#btn-hist-reset');
  if (resetBtn) {
    resetBtn.onclick = () => {
      state.histSource = 'all';
      state.histAction = 'all';
      state.histStatus = 'all';
      state.histKeyword = '';
      state.histSelected.clear();
      loadHistory();
    };
  }

  const chkAll = root.querySelector('#chk-hist-all');
  if (chkAll) {
    chkAll.onchange = () => {
      if (chkAll.checked) items.forEach((it) => state.histSelected.add(it.seq));
      else state.histSelected.clear();
      renderHistoryTab(root);
    };
  }
  root.querySelectorAll('.js-chk-hist').forEach((chk) => {
    chk.onchange = () => {
      const seq = Number(chk.dataset.seq);
      if (chk.checked) state.histSelected.add(seq);
      else state.histSelected.delete(seq);
      renderHistoryTab(root);
    };
  });

  const doDelete = async (seqs, clearFiltered) => {
    state.histLoading = true;
    renderHistoryTab(root);
    try {
      const res = await bridge.action('ui_history_delete', {
        ...buildHistoryFilterPayload(),
        seqs,
        clear_filtered: clearFiltered,
        unlock_dedupe: state.histUnlockDedupe,
      });
      state.histSelected.clear();
      state.histData = res;
      if (state.boot?.state) {
        state.boot.state.transfer_history_total = res.transfer_total ?? 0;
        state.boot.state.task_history_total = res.task_total ?? 0;
        state.boot.state.dedupe_locks_total = res.dedupe_locks_total ?? 0;
      }
      showToast(res.notice || `已删除 ${res.deleted || 0} 条记录`, 'success');
    } catch (err) {
      showToast(err.message || String(err), 'error');
    } finally {
      state.histLoading = false;
      render();
    }
  };

  const doRetry = async (seqs) => {
    state.histLoading = true;
    showToast('正在解除去重锁并重新提交转存…', 'info');
    renderHistoryTab(root);
    try {
      const res = await bridge.action('ui_history_retry', {
        ...buildHistoryFilterPayload(),
        seqs,
      });
      state.histSelected.clear();
      state.histData = res;
      if (state.boot?.state) {
        state.boot.state.transfer_history_total = res.transfer_total ?? 0;
        state.boot.state.task_history_total = res.task_total ?? 0;
        state.boot.state.dedupe_locks_total = res.dedupe_locks_total ?? 0;
      }
      showToast(res.notice || '重新转存完成', 'success');
    } catch (err) {
      showToast(err.message || String(err), 'error');
    } finally {
      state.histLoading = false;
      render();
    }
  };

  root.querySelectorAll('[data-hist-del]').forEach((btn) => {
    btn.onclick = () => doDelete([Number(btn.dataset.histDel)], false);
  });
  root.querySelectorAll('[data-hist-retry]').forEach((btn) => {
    btn.onclick = () => doRetry([Number(btn.dataset.histRetry)]);
  });

  const retrySelBtn = root.querySelector('#btn-hist-retry-sel');
  if (retrySelBtn) {
    retrySelBtn.onclick = () => {
      const seqs = Array.from(state.histSelected);
      if (!seqs.length) return showToast('请先勾选要重新转存的记录', 'error');
      doRetry(seqs);
    };
  }

  root.querySelector('#btn-hist-del-sel').onclick = () => {
    const seqs = Array.from(state.histSelected);
    if (!seqs.length) return showToast('请先勾选要删除的记录', 'error');
    doDelete(seqs, false);
  };

  root.querySelector('#btn-hist-clear-filtered').onclick = () => {
    const cnt = Number(hd.filtered_total || 0);
    if (!cnt && !(isTransfer && !hasActiveFilter && Number(hd.dedupe_locks_total || 0) > 0)) {
      return showToast('当前筛选下没有可清空的记录', 'info');
    }
    doDelete([], true);
  };
}

function isDirectLinkText(q) {
  const low = q.toLowerCase();
  return low.startsWith('hdhive://') || low.startsWith('movie:hdhive://') || low.startsWith('tv:hdhive://') ||
    low.startsWith('relay://') || low.startsWith('radar://') || low.startsWith('nextfind://') ||
    low.startsWith('ed2k://') || low.startsWith('magnet:?') || low.includes('115.com/s/') || low.includes('115cdn.com/s/') || low.includes('anxia.com/s/');
}

function bindGlobalFloatingTooltip() {
  const tipEl = document.getElementById('mf-floating-tip');
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

  document.addEventListener('scroll', () => {
    activeTarget = null;
    tipEl.classList.add('hidden');
  }, true);
}

async function init() {
  bindGlobalFloatingTooltip();
  bridge.getTheme().catch(() => {});
  try {
    const res = await bridge.action('ui_bootstrap', {});
    state.boot = res;
    updateSidebarStatus();
    render();
    // Preload discover movies/TV silently using saved/default 8-row filters
    loadTMDB(false);
  } catch (err) {
    document.getElementById('view').innerHTML = `<div class="mf-empty" style="color:var(--mf-danger)">初始化失败：${esc(err.message || err)}</div>`;
  }
}

init();

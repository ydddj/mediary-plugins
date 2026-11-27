import { createMediaryBridge, downloadTextFile } from './bridge.js';

const bridge = createMediaryBridge();

const state = {
  tab: 'overview',
  data: null,
  historyQuery: '',
  userQuery: '',
  qrPollTimer: null,
  sandboxResult: null,
};

const el = (id) => document.getElementById(id);

function esc(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

function fmtBytes(bytes) {
  const n = Number(bytes || 0);
  if (!n || n <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(i >= 3 ? 2 : 1)} ${units[i]}`;
}

function fmtAgo(ts) {
  const n = Number(ts || 0);
  if (!n) return '-';
  const diff = Math.max(0, Math.floor(Date.now() / 1000) - n);
  if (diff < 60) return `${diff}秒前`;
  if (diff < 3600) return `${Math.floor(diff / 60)}分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}小时前`;
  return `${Math.floor(diff / 86400)}天前`;
}

let toastTimer = null;
function toast(msg, kind = 'info') {
  const box = el('mn-toast');
  box.textContent = msg;
  box.className = `mn-toast ${kind}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => box.classList.add('hidden'), 4200);
}

function openModal(title, html, onMount) {
  stopQrTimer();
  el('mn-modal-title').textContent = title;
  el('mn-modal-body').innerHTML = html;
  el('mn-modal-backdrop').classList.remove('hidden');
  if (onMount) onMount(el('mn-modal-body'));
}

function closeModal() {
  stopQrTimer();
  el('mn-modal-backdrop').classList.add('hidden');
  el('mn-modal-body').innerHTML = '';
}

function stopQrTimer() {
  if (state.qrPollTimer) {
    clearInterval(state.qrPollTimer);
    state.qrPollTimer = null;
  }
}

async function runAction(name, payload = {}, okMsg = '') {
  try {
    const res = await bridge.action(name, payload);
    if (res && res.bootstrap) {
      state.data = res.bootstrap;
    } else if (res && res.engine && res.lb_accounts) {
      state.data = res;
    }
    updateChrome();
    render();
    if (okMsg || res?.notice) toast(okMsg || res.notice, 'success');
    return res;
  } catch (err) {
    toast(err.message || String(err), 'error');
    throw err;
  }
}

function updateChrome() {
  const d = state.data;
  if (!d) return;
  el('mn-version-label').textContent = `v${d.version || '2.1.5'} · 原生 302 钩子`;
  el('badge-sessions').textContent = (d.active_sessions || []).length;
  el('badge-accounts').textContent = (d.source_accounts || []).length + (d.lb_accounts || []).length;
  el('badge-users').textContent = (d.users || []).length;
  el('badge-risk').textContent = (d.ua_stats || []).filter((x) => x.is_banned).length;
  el('badge-cache').textContent = d.stats?.cached_links ?? 0;
  el('badge-history').textContent = (d.history || []).length;

  const hs = d.host_status || {};
  const st = d.settings || {};
  const modePill = el('mn-mode-pill');
  if (hs.plugin_enabled === false || st['$enabled'] === false) {
    modePill.textContent = '插件当前未启用';
    modePill.className = 'mn-status-pill warn';
  } else if (hs.playback_hook_approved) {
    modePill.textContent = hs.socket_ready ? '原生 302 钩子 · 已接管 (:8118)' : '钩子已批准 · 等待进程就绪';
    modePill.className = hs.socket_ready ? 'mn-status-pill ok' : 'mn-status-pill warn';
  } else {
    modePill.textContent = '待在插件配置中批准 302 钩子';
    modePill.className = 'mn-status-pill warn';
  }

  const scopePill = el('mn-scope-pill');
  if (scopePill) {
    if (hs.media_service_read_ready && hs.media_service_write_ready) {
      scopePill.textContent = '媒体服务权限: 读写就绪';
      scopePill.className = 'mn-status-pill ok';
    } else if (hs.media_service_read_ready) {
      scopePill.textContent = '媒体服务权限: 只读 (缺强踢权限)';
      scopePill.className = 'mn-status-pill warn';
    } else {
      scopePill.textContent = '待授予 media-service 权限';
      scopePill.className = 'mn-status-pill warn';
    }
  }

  const lbs = d.lb_accounts || [];
  const okLbs = lbs.filter((a) => a.enabled && a.valid && !a.in_cooldown).length;
  const poolPill = el('mn-pool-pill');
  poolPill.textContent = `可用负载号: ${okLbs} / ${lbs.length}`;
  poolPill.className = okLbs > 0 ? 'mn-status-pill ok' : 'mn-status-pill warn';

  el('mn-sync-note').textContent = d.last_migrated_from
    ? `最近导入: ${d.last_migrated_from}`
    : (d.last_tick_result || 'Mediary 3.4.8+ 原生桥接');
}

function render() {
  const view = el('mn-view');
  if (!state.data) {
    view.innerHTML = `<div class="mn-card">正在加载 MediNext 工作台数据...</div>`;
    return;
  }
  switch (state.tab) {
    case 'overview': return renderOverview(view);
    case 'accounts': return renderAccounts(view);
    case 'users': return renderUsers(view);
    case 'risk': return renderRisk(view);
    case 'settings': return renderSettings(view);
    case 'history': return renderHistory(view);
  }
}

// ==================== 1. Overview & Live Sessions & Sandbox ====================
function renderOverview(view) {
  const d = state.data;
  const s = d.stats || {};
  const hs = d.host_status || {};
  const d1 = s.d1 || {};
  const d7 = s.d7 || {};
  const hitRate = d1.redirects > 0 ? Math.round((d1.cache_hits / d1.redirects) * 100) : 0;
  const sessions = d.active_sessions || [];
  const needApproval = !hs.playback_hook_approved || hs.pending_scope_approval || !hs.media_service_read_ready;

  view.innerHTML = `
    <div class="mn-header">
      <div>
        <h2>概览与实时在线会话</h2>
        <p>Mediary 3.4.6+ 原生 Unix Socket 播放钩子（零额外端口，播放器直连宿主 8118 端口即可享受多号负载均衡、用户秒传与观影风控）</p>
      </div>
      <div class="mn-actions">
        <button class="mn-btn" id="btn-refresh">刷新状态</button>
        <button class="mn-btn" id="btn-run-tick">立即执行配额/到期巡检</button>
        <button class="mn-btn primary" id="btn-open-sandbox">⚡ 302 调度实测沙盒</button>
      </div>
    </div>

    ${needApproval ? `
      <div class="mn-card" style="border-color:rgba(245,158,11,0.45)">
        <div class="mn-card-title" style="color:#fbbf24">⚠️ 待完成宿主权限或播放钩子审批</div>
        <div class="mn-card-sub" style="margin-top:4px">
          请返回 Mediary「插件中心 → MediNext → 配置」，勾选授予 <code>media-service:read</code>、<code>media-service:write</code> 权限并开启「批准播放决策钩子」后点击保存（插件升级后宿主会自动重置播放钩子审批以确保安全）。
        </div>
      </div>
    ` : ''}

    <div class="mn-kpis">
      <div class="mn-kpi">
        <div class="mn-kpi-label">当前并发流</div>
        <div class="mn-kpi-val">${s.active_streams ?? 0}</div>
        <div class="mn-kpi-sub">近 6 分钟活跃播放设备</div>
      </div>
      <div class="mn-kpi">
        <div class="mn-kpi-label">24h 播放调度</div>
        <div class="mn-kpi-val">${d1.total ?? 0}</div>
        <div class="mn-kpi-sub">缓存命中 ${hitRate}% · 预热 ${d1.prewarms ?? 0} · 拦截 ${d1.denies ?? 0}</div>
      </div>
      <div class="mn-kpi">
        <div class="mn-kpi-label">有效直链缓存 / 秒传池</div>
        <div class="mn-kpi-val">${s.cached_links ?? 0} <span style="font-size:14px;font-weight:500;color:var(--mn-muted)">/ ${s.sha1_pool_count ?? 0}</span></div>
        <div class="mn-kpi-sub">TTL ${d.engine?.cache_ttl_secs || 1800}s 自动续期</div>
      </div>
      <div class="mn-kpi">
        <div class="mn-kpi-label">7 日调度流量</div>
        <div class="mn-kpi-val">${fmtBytes(d7.traffic_bytes || 0)}</div>
        <div class="mn-kpi-sub">7 日共出流 ${d7.redirects ?? 0} 次</div>
      </div>
    </div>

    ${state.sandboxResult ? `
      <div class="mn-card">
        <div class="mn-card-head">
          <div>
            <h3 class="mn-card-title">最近沙盒实测结果</h3>
            <div class="mn-card-sub">状态: <b>${esc(state.sandboxResult.status)}</b> · 模式: ${esc(state.sandboxResult.mode_label)} · 账号: ${esc(state.sandboxResult.provider || '-')} · 耗时: ${state.sandboxResult.elapsed_ms} ms</div>
          </div>
          <button class="mn-btn ghost sm" id="btn-clear-sandbox">收起</button>
        </div>
        <div class="mn-code-box">${esc(state.sandboxResult.url || state.sandboxResult.fallback_video_url || state.sandboxResult.reason)}</div>
      </div>
    ` : ''}

    <div class="mn-card">
      <div class="mn-card-head">
        <div>
          <h3 class="mn-card-title">实时活跃播放会话 (${sessions.length})</h3>
          <div class="mn-card-sub">实时联动宿主 active-playbacks 会话表；强踢将向播放器发送 WebSocket Stop 并吊销会话票据 12 小时</div>
        </div>
      </div>
      ${sessions.length === 0 ? `<div class="mn-card-sub">当前暂无活跃播放流</div>` : `
        <div class="mn-table-wrap">
          <table class="mn-table mn-mobile-cards">
            <thead>
              <tr><th>用户</th><th>播放内容</th><th>客户端 / 设备</th><th>出流账号</th><th>IP</th><th>最近心跳</th><th>操作</th></tr>
            </thead>
            <tbody>
              ${sessions.map((s) => `
                <tr>
                  <td class="mn-td-primary" data-label="用户">
                    <div class="mn-cell-val"><b>${esc(s.username)}</b><span class="mn-mobile-only"> · ${esc(s.item_name || s.item_id || '-')}</span></div>
                  </td>
                  <td class="mn-mobile-hide" data-label="播放内容"><div class="mn-cell-val">${esc(s.item_name || s.item_id || '-')}</div></td>
                  <td data-label="客户端 / 设备"><div class="mn-cell-val">${esc(s.client || s.ua)} <span class="mn-tag">${esc(s.device_name || s.device_id || '-')}</span></div></td>
                  <td data-label="出流账号"><div class="mn-cell-val"><span class="mn-tag primary">${esc(s.provider || '-')}</span></div></td>
                  <td data-label="IP"><div class="mn-cell-val">${esc(s.ip || '-')}</div></td>
                  <td data-label="最近心跳"><div class="mn-cell-val">${fmtAgo(s.last_seen_at)}</div></td>
                  <td class="mn-td-actions" data-label="操作"><div class="mn-actions"><button class="mn-btn danger sm" data-kick="${esc(s.session_key)}">强踢下线</button></div></td>
                </tr>
              `).join('')}
            </tbody>
          </table>
        </div>
      `}
    </div>

    <div class="mn-card">
      <div class="mn-card-head">
        <div>
          <h3 class="mn-card-title">近 7 日观影活跃榜</h3>
          <div class="mn-card-sub">统计直链签发次数与累计媒体体积</div>
        </div>
      </div>
      ${(s.top_users_7d || []).length === 0 ? `<div class="mn-card-sub">暂无近 7 日播放数据</div>` : `
        <div class="mn-table-wrap">
          <table class="mn-table mn-mobile-cards">
            <thead><tr><th>排名</th><th>用户</th><th>7日播放次数</th><th>累计媒体流量</th><th>活跃指数</th></tr></thead>
            <tbody>
              ${(s.top_users_7d || []).map((u, idx) => `
                <tr>
                  <td class="mn-mobile-hide" data-label="排名"><div class="mn-cell-val">#${idx + 1}</div></td>
                  <td class="mn-td-primary" data-label="用户"><div class="mn-cell-val"><span class="mn-mobile-only" style="color:var(--mn-muted);margin-right:6px">#${idx + 1}</span><b>${esc(u.username)}</b></div></td>
                  <td data-label="7日播放次数"><div class="mn-cell-val">${u.plays_7d}</div></td>
                  <td data-label="累计媒体流量"><div class="mn-cell-val">${fmtBytes(u.traffic_bytes_7d)}</div></td>
                  <td data-label="活跃指数"><div class="mn-cell-val"><span class="mn-tag ok">${u.activity_score}</span></div></td>
                </tr>
              `).join('')}
            </tbody>
          </table>
        </div>
      `}
    </div>
  `;

  el('btn-refresh').onclick = () => runAction('ui_bootstrap', {}, '已刷新最新状态');
  el('btn-run-tick').onclick = () => runAction('tick', {});
  el('btn-open-sandbox').onclick = openSandboxModal;
  if (el('btn-clear-sandbox')) {
    el('btn-clear-sandbox').onclick = () => { state.sandboxResult = null; render(); };
  }
  view.querySelectorAll('[data-kick]').forEach((b) => {
    b.onclick = () => runAction('ui_kick_session', { session_key: b.dataset.kick });
  });
}

function openSandboxModal() {
  const users = state.data?.users || [];
  openModal('302 播放链路实测沙盒', `
    <div style="display:flex;flex-direction:column;gap:11px">
      <div class="mn-field">
        <label>115 提取码 (pickcode) 或 Mediary 条目 ID</label>
        <input class="mn-input" id="sb-pickcode" placeholder="例如: ay4u93dndfz6ehj39" />
      </div>
      <div class="mn-grid-2">
        <div class="mn-field">
          <label>模拟观影用户</label>
          <input class="mn-input" id="sb-user" list="sb-user-list" value="${esc(users[0]?.username || 'ydddj')}" />
          <datalist id="sb-user-list">${users.map((u) => `<option value="${esc(u.username)}"></option>`).join('')}</datalist>
        </div>
        <div class="mn-field">
          <label>模拟客户端 User-Agent</label>
          <input class="mn-input" id="sb-ua" value="Infuse-Direct/8.0" />
        </div>
      </div>
      <label class="mn-check"><input type="checkbox" id="sb-bypass" checked /> 跳过 1800s 缓存强制实测选号/秒传</label>
      <label class="mn-check"><input type="checkbox" id="sb-prefetch" /> 模拟详情页小号负载预热 (prefetch 模式：仅预秒传+预签缓存，不占并发不发通知)</label>
      <div class="mn-modal-foot">
        <button class="mn-btn primary" id="sb-submit">开始实测解析</button>
      </div>
    </div>
  `, (box) => {
    box.querySelector('#sb-submit').onclick = async () => {
      const val = box.querySelector('#sb-pickcode').value.trim();
      if (!val) return toast('请填写 pickcode 或条目 ID', 'error');
      const isItemId = /^[0-9a-f]{32}$/i.test(val) || val.length >= 24;
      const res = await runAction('ui_test_resolve', {
        request: {
          pick_code: isItemId ? '' : val,
          item_id: isItemId ? val : '',
          username: box.querySelector('#sb-user').value.trim(),
          user_agent: box.querySelector('#sb-ua').value.trim(),
          device_id: 'sandbox-probe-01',
          client: 'Sandbox',
          client_ip: '127.0.0.1',
          bypass_cache: box.querySelector('#sb-bypass').checked,
          prefetch: box.querySelector('#sb-prefetch').checked,
        },
      });
      state.sandboxResult = res.decision;
      closeModal();
      render();
    };
  });
}

// ==================== 2. 115 Account Pool & QR Code Login ====================
function renderAccounts(view) {
  const srcs = state.data.source_accounts || [];
  const lbs = state.data.lb_accounts || [];

  view.innerHTML = `
    <div class="mn-header">
      <div>
        <h2>115 源盘主号与负载均衡号池</h2>
        <p>源盘主号提供原始媒体元数据与秒传 Range 校验；负载均衡号自动接收秒传至缓存目录并分流直链并发</p>
      </div>
      <div class="mn-actions">
        <button class="mn-btn" id="btn-health-all">探活全部账号</button>
        <button class="mn-btn success" id="btn-qr-add">📱 115 扫码新增/更新账号</button>
        <button class="mn-btn primary" id="btn-manual-add">+ 手动添加账号</button>
      </div>
    </div>

    <div class="mn-card">
      <div class="mn-card-head">
        <div>
          <h3 class="mn-card-title">可选独立源盘主号 (${srcs.length})</h3>
          <div class="mn-card-sub">可选配置：不填时源盘直连、小文件直出与秒传 range-sha1 分段校验均自动复用 Mediary 宿主内置 115 主号；仅在需覆盖特定源盘账号时添加</div>
        </div>
      </div>
      ${srcs.length === 0 ? `<div class="mn-card-sub">当前未单独配置源盘主号（默认自动复用 Mediary 宿主内置 115 授权，无需重复绑号）</div>` : renderAccountTable(srcs, 'source')}
    </div>

    <div class="mn-card">
      <div class="mn-card-head">
        <div>
          <h3 class="mn-card-title">多号负载均衡池 (${lbs.length})</h3>
          <div class="mn-card-sub">按「活跃会话数 × 100w + 调用计数 - 用户/设备亲和偏置」自动分配；遇 405 自动冷却隔离并故障转移</div>
        </div>
      </div>
      ${lbs.length === 0 ? `<div class="mn-card-sub">尚未添加负载均衡号</div>` : renderAccountTable(lbs, 'lb')}
    </div>
  `;

  el('btn-health-all').onclick = () => runAction('health_check', {});
  el('btn-qr-add').onclick = () => openQrModal('lb', '');
  el('btn-manual-add').onclick = () => openAccountModal({ role: 'lb', enabled: true, cache_path: '/Next', quota_enabled: true, quota_gb: 5120, app: 'tv' }, true);

  view.querySelectorAll('[data-probe]').forEach((b) => {
    b.onclick = () => runAction('ui_probe_account', { name: b.dataset.probe, role: b.dataset.role });
  });
  view.querySelectorAll('[data-edit-acc]').forEach((b) => {
    const list = b.dataset.role === 'source' ? srcs : lbs;
    const item = list.find((x) => x.name === b.dataset.editAcc);
    if (item) b.onclick = () => openAccountModal(item, false);
  });
  view.querySelectorAll('[data-qr-acc]').forEach((b) => {
    b.onclick = () => openQrModal(b.dataset.role, b.dataset.qrAcc);
  });
  view.querySelectorAll('[data-reset-cd]').forEach((b) => {
    b.onclick = () => runAction('ui_reset_cooldown', { name: b.dataset.resetCd }, '已解除 405 冷却状态');
  });
  view.querySelectorAll('[data-clean-lb]').forEach((b) => {
    b.onclick = () => {
      if (confirm(`确定立即清理账号 [${b.dataset.cleanLb}] 的历史转存缓存并清空 115 回收站吗？`)) {
        runAction('ui_clean_quota', { name: b.dataset.cleanLb, force_all: true });
      }
    };
  });
  view.querySelectorAll('[data-del-acc]').forEach((b) => {
    b.onclick = () => {
      if (confirm(`确定删除账号 [${b.dataset.delAcc}] 吗？`)) {
        runAction('ui_delete_account', { name: b.dataset.delAcc, role: b.dataset.role }, '账号已删除');
      }
    };
  });
}

function renderAccountTable(list, role) {
  const uidCounts = {};
  list.forEach((a) => {
    if (a.uid) uidCounts[a.uid] = (uidCounts[a.uid] || 0) + 1;
  });
  return `
    <div class="mn-table-wrap">
      <table class="mn-table mn-mobile-cards">
        <thead>
          <tr>
            <th>名称 / UID</th>
            <th>状态</th>
            ${role === 'lb' ? `<th>缓存目录</th><th>转存配额占用</th><th>并发 / 累计调度</th>` : `<th>终端 / 凭证</th><th>网盘容量 / 备注</th>`}
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          ${list.map((a) => {
            const limitBytes = (a.quota_gb || 0) * 1024 * 1024 * 1024;
            const pct = limitBytes > 0 ? Math.min(100, Math.round(((a.quota_usage_bytes || 0) / limitBytes) * 100)) : 0;
            const sharedUid = a.uid && uidCounts[a.uid] > 1;
            return `
              <tr>
                <td class="mn-td-primary" data-label="名称 / UID">
                  <div class="mn-cell-val">
                    <b>${esc(a.name)}</b> ${sharedUid ? `<span class="mn-tag primary" title="与池内其它终端条目共享同一物理 115 账号秒传池与配额">多端同号</span>` : ''}
                    <div class="mn-card-sub">UID: ${esc(a.uid || '待探活')}${role === 'lb' && a.drive_info ? ` · ${esc(a.drive_info)}` : ''}</div>
                  </div>
                </td>
                <td data-label="状态">
                  <div class="mn-cell-val">
                    ${!a.enabled ? `<span class="mn-tag">已停用</span>`
                      : a.in_cooldown ? `<span class="mn-tag warn">405冷却 (${Math.ceil(a.cooldown_remaining_secs / 60)}m)</span>`
                      : a.valid ? `<span class="mn-tag ok">正常</span>`
                      : `<span class="mn-tag danger">凭证失效</span>`}
                  </div>
                </td>
                ${role === 'lb' ? `
                  <td data-label="缓存目录"><div class="mn-cell-val"><code>${esc(a.cache_path || '/Next')}</code></div></td>
                  <td data-label="转存配额占用">
                    <div class="mn-cell-val">
                      <div>${fmtBytes(a.quota_usage_bytes)} / ${a.quota_enabled && a.quota_gb ? `${(a.quota_gb / 1024).toFixed(1)} TB` : '不限'}</div>
                      ${a.quota_enabled && a.quota_gb ? `<div class="mn-progress"><div class="mn-progress-bar" style="width:${pct}%"></div></div>` : ''}
                    </div>
                  </td>
                  <td data-label="并发 / 累计调度"><div class="mn-cell-val">活跃 <b>${a.active_sessions || 0}</b> · 累计 ${a.call_count || 0}</div></td>
                ` : `
                  <td data-label="终端 / 凭证"><div class="mn-cell-val"><span class="mn-tag">${esc(a.app || 'tv')}</span> ${esc(a.cookie_masked || 'OpenAPI')}</div></td>
                  <td data-label="网盘容量 / 备注"><div class="mn-cell-val">${esc(a.drive_info || '-')}</div></td>
                `}
                <td class="mn-td-actions" data-label="操作">
                  <div class="mn-actions">
                    <button class="mn-btn sm" data-probe="${esc(a.name)}" data-role="${role}">探活</button>
                    <button class="mn-btn sm" data-qr-acc="${esc(a.name)}" data-role="${role}">扫码重绑</button>
                    <button class="mn-btn sm" data-edit-acc="${esc(a.name)}" data-role="${role}">编辑</button>
                    ${a.in_cooldown ? `<button class="mn-btn success sm" data-reset-cd="${esc(a.name)}">解除冷却</button>` : ''}
                    ${role === 'lb' ? `<button class="mn-btn sm" data-clean-lb="${esc(a.name)}">清缓存</button>` : ''}
                    <button class="mn-btn danger sm" data-del-acc="${esc(a.name)}" data-role="${role}">删除</button>
                  </div>
                </td>
              </tr>
            `;
          }).join('')}
        </tbody>
      </table>
    </div>
  `;
}

function openAccountModal(acc, isNew) {
  openModal(isNew ? '添加 115 账号' : `编辑账号: ${acc.name}`, `
    <div style="display:flex;flex-direction:column;gap:10px">
      <div class="mn-grid-2">
        <div class="mn-field">
          <label>账号角色</label>
          <select class="mn-select" id="ac-role" ${!isNew ? 'disabled' : ''}>
            <option value="lb" ${acc.role === 'lb' ? 'selected' : ''}>负载均衡号 (接收秒传出流)</option>
            <option value="source" ${acc.role === 'source' ? 'selected' : ''}>源盘主号 (媒体库原盘)</option>
          </select>
        </div>
        <div class="mn-field">
          <label>账号标识名称</label>
          <input class="mn-input" id="ac-name" value="${esc(acc.name || '')}" placeholder="如: 115-divingcat(安卓TV)" />
        </div>
      </div>
      <div class="mn-field">
        <label>115 Cookie（留空或保留掩码则不修改原保存值）</label>
        <textarea class="mn-textarea" id="ac-cookie" placeholder="UID=...; CID=...; SEID=...">${esc(acc.cookie_masked || '')}</textarea>
      </div>
      <div class="mn-grid-3">
        <div class="mn-field">
          <label>缓存目录 (仅负载号)</label>
          <input class="mn-input" id="ac-path" value="${esc(acc.cache_path || '/Next')}" />
        </div>
        <div class="mn-field">
          <label>转存配额上限 (GB, 5120=5TB)</label>
          <input type="number" class="mn-input" id="ac-quota" value="${acc.quota_gb ?? 5120}" />
        </div>
        <div class="mn-field">
          <label>回收站安全码 (默认000000)</label>
          <input type="password" class="mn-input" id="ac-rb" placeholder="${acc.has_recycle_code ? '****** (已设置)' : '000000'}" />
        </div>
      </div>
      <div class="mn-field">
        <label>独立代理地址 (可选，如 http://10.0.0.252:7892)</label>
        <input class="mn-input" id="ac-proxy" value="${esc(acc.proxy_url || '')}" />
      </div>
      <div class="mn-actions">
        <label class="mn-check"><input type="checkbox" id="ac-enabled" ${acc.enabled !== false ? 'checked' : ''} /> 启用该账号</label>
        <label class="mn-check"><input type="checkbox" id="ac-qenabled" ${acc.quota_enabled !== false ? 'checked' : ''} /> 启用超配额自动清理</label>
      </div>
      <div class="mn-modal-foot">
        <button class="mn-btn primary" id="ac-save">保存账号</button>
      </div>
    </div>
  `, (box) => {
    box.querySelector('#ac-save').onclick = async () => {
      const rbVal = box.querySelector('#ac-rb').value.trim();
      await runAction('ui_save_account', {
        original_name: isNew ? '' : acc.name,
        account: {
          name: box.querySelector('#ac-name').value.trim(),
          role: box.querySelector('#ac-role').value,
          cookies: box.querySelector('#ac-cookie').value.trim(),
          cache_path: box.querySelector('#ac-path').value.trim(),
          quota_gb: Number(box.querySelector('#ac-quota').value || 0),
          quota_enabled: box.querySelector('#ac-qenabled').checked,
          recycle_bin_code: rbVal || (acc.has_recycle_code ? '******' : ''),
          proxy_url: box.querySelector('#ac-proxy').value.trim(),
          enabled: box.querySelector('#ac-enabled').checked,
        },
      }, '账号已保存');
      closeModal();
    };
  });
}

function openQrModal(defaultKind = 'lb', defaultName = '') {
  openModal('115 扫码登录与自动绑号', `
    <div style="display:flex;flex-direction:column;gap:11px">
      <div class="mn-grid-3">
        <div class="mn-field">
          <label>绑定目标类型</label>
          <select class="mn-select" id="qr-kind">
            <option value="lb" ${defaultKind === 'lb' ? 'selected' : ''}>负载均衡号</option>
            <option value="source" ${defaultKind === 'source' ? 'selected' : ''}>源盘主号</option>
            <option value="user" ${defaultKind === 'user' ? 'selected' : ''}>指定观影用户</option>
          </select>
        </div>
        <div class="mn-field">
          <label>账号名称 / 用户名</label>
          <input class="mn-input" id="qr-name" value="${esc(defaultName)}" placeholder="如: 115-lb3 或 jerry" />
        </div>
        <div class="mn-field">
          <label>登录终端通道</label>
          <select class="mn-select" id="qr-app">
            <option value="tv">安卓电视 (tv · 推荐防顶号)</option>
            <option value="android">安卓手机 (android)</option>
            <option value="alipaymini">支付宝小程序 (alipaymini)</option>
            <option value="wechatmini">微信小程序 (wechatmini)</option>
            <option value="qandroid">安卓平板 (qandroid)</option>
            <option value="ios">iOS 客户端 (ios)</option>
            <option value="web">网页版 (web)</option>
            <option value="clouddrive">115 OpenAPI (clouddrive)</option>
          </select>
        </div>
      </div>
      <div class="mn-qr-box" id="qr-stage">
        <div class="mn-card-sub">填写上方绑定名称后，点击「获取二维码」开始扫码</div>
        <button class="mn-btn primary" id="qr-start-btn">获取 115 登录二维码</button>
      </div>
    </div>
  `, (box) => {
    const startQr = async () => {
      stopQrTimer();
      const bindKind = box.querySelector('#qr-kind').value;
      const bindName = box.querySelector('#qr-name').value.trim();
      const app = box.querySelector('#qr-app').value;
      if (!bindName) return toast('请先输入要绑定的账号名称或用户名', 'error');

      const stage = box.querySelector('#qr-stage');
      stage.innerHTML = `<div class="mn-card-sub">正在请求 115 二维码...</div>`;
      try {
        const qr = await bridge.action('ui_qr_start', { app });
        stage.innerHTML = `
          ${qr.image_base64 ? `<img src="${qr.image_base64}" alt="115 QR" />` : `<div class="mn-code-box">${esc(qr.qrcode_text)}</div>`}
          <div id="qr-status-text" style="font-weight:600">请使用手机 115 App 扫码登录 (${esc(app)})</div>
          <button class="mn-btn sm" id="qr-refresh-btn">刷新二维码</button>
        `;
        stage.querySelector('#qr-refresh-btn').onclick = startQr;

        state.qrPollTimer = setInterval(async () => {
          try {
            const res = await bridge.action('ui_qr_poll', {
              uid: qr.uid,
              time: qr.time,
              sign: qr.sign,
              app: qr.app,
              bind_kind: bindKind,
              bind_name: bindName,
            });
            const p = res.poll || {};
            const stEl = box.querySelector('#qr-status-text');
            if (stEl && p.message) stEl.textContent = p.message;
            if (p.status === 'success') {
              stopQrTimer();
              if (res.bootstrap) state.data = res.bootstrap;
              updateChrome();
              closeModal();
              render();
              toast(`扫码绑定成功：[${bindName}] UID=${p.p115_uid || ''}`, 'success');
            } else if (p.status === 'expired') {
              stopQrTimer();
            }
          } catch (_) {}
        }, 2200);
      } catch (e) {
        stage.innerHTML = `<div style="color:var(--mn-danger)">${esc(e.message)}</div><button class="mn-btn sm" id="qr-retry">重试</button>`;
        stage.querySelector('#qr-retry').onclick = startQr;
      }
    };
    box.querySelector('#qr-start-btn').onclick = startQr;
  });
}

// ==================== 3. Users & Policy Templates ====================
function buildUsersListHtml(users) {
  if (users.length === 0) return `<div class="mn-card-sub">暂无匹配用户</div>`;
  return `
    <div class="mn-table-wrap">
      <table class="mn-table mn-mobile-cards">
        <thead>
          <tr><th>用户名</th><th>状态</th><th>路由模式</th><th>模板</th><th>并发 / 设备</th><th>到期日</th><th>专属 115</th><th>操作</th></tr>
        </thead>
        <tbody>
          ${users.map((u) => `
            <tr>
              <td class="mn-td-primary" data-label="用户名">
                <div class="mn-cell-val">
                  <b>${esc(u.username)}</b> ${u.is_admin ? `<span class="mn-tag primary">管理员</span>` : ''}
                  ${u.remark ? `<div class="mn-card-sub">${esc(u.remark)}</div>` : ''}
                </div>
              </td>
              <td data-label="状态">
                <div class="mn-cell-val">
                  ${u.is_banned ? `<span class="mn-tag danger">已封禁</span>`
                    : u.is_expired ? `<span class="mn-tag warn">已到期</span>`
                    : `<span class="mn-tag ok">正常</span>`}
                </div>
              </td>
              <td data-label="路由模式"><div class="mn-cell-val">${u.route_mode === 'direct' ? '源盘直连' : u.route_mode === 'own_cookie' ? '独立115秒传' : u.route_mode === 'load_balance' ? '多号负载均衡' : '跟随模板'}</div></td>
              <td data-label="策略模板"><div class="mn-cell-val"><span class="mn-tag">${esc(u.template || '-')}</span></div></td>
              <td data-label="并发 / 设备"><div class="mn-cell-val">流 ${u.active_streams} · 设备 ${u.device_count}</div></td>
              <td data-label="到期日"><div class="mn-cell-val">${esc(u.expiry_date || '永久')}</div></td>
              <td data-label="专属 115"><div class="mn-cell-val">${u.has_own_cookie ? `<span class="mn-tag ok">UID:${esc(u.p115_uid || '已绑')}</span>` : `<span class="mn-card-sub">未绑</span>`}</div></td>
              <td class="mn-td-actions" data-label="操作">
                <div class="mn-actions">
                  <button class="mn-btn sm" data-qr-user="${esc(u.username)}">扫码绑115</button>
                  <button class="mn-btn sm" data-edit-user="${esc(u.username)}">编辑</button>
                  <button class="mn-btn danger sm" data-del-user="${esc(u.username)}">删除</button>
                </div>
              </td>
            </tr>
          `).join('')}
        </tbody>
      </table>
    </div>
  `;
}

function renderUsers(view) {
  const tpls = state.data.templates || [];
  const filterUsers = () => {
    const q = state.userQuery.trim().toLowerCase();
    return (state.data.users || []).filter(
      (u) => !q || u.username.toLowerCase().includes(q) || (u.remark || '').toLowerCase().includes(q)
    );
  };
  const users = filterUsers();

  view.innerHTML = `
    <div class="mn-header">
      <div>
        <h2>观影用户策略与模板管理</h2>
        <p>管理并发流上限、设备数上限、账号有效期及路由分流模式（多号负载均衡 / 自有 115 秒传 / 源盘直连）</p>
      </div>
      <div class="mn-actions">
        <button class="mn-btn" id="btn-sync-users">🔄 从宿主媒体服务同步用户</button>
        <button class="mn-btn" id="btn-add-tpl">+ 新增策略模板</button>
        <button class="mn-btn primary" id="btn-add-user">+ 添加用户策略</button>
      </div>
    </div>

    <div class="mn-card">
      <div class="mn-card-head">
        <div>
          <h3 class="mn-card-title">策略模板 (${tpls.length})</h3>
          <div class="mn-card-sub">用户路由设为「跟随模板」时自动继承模板的并发、设备数与默认出流通道</div>
        </div>
      </div>
      <div class="mn-grid-3">
        ${tpls.map((t) => `
          <div class="mn-kpi" style="display:flex;flex-direction:column;gap:6px">
            <div style="display:flex;justify-content:space-between;align-items:center;gap:8px">
              <b>${esc(t.name)}</b>
              <span class="mn-tag primary">${t.default_route === 'direct' ? '源盘直连' : t.default_route === 'own_cookie' ? '独立秒传' : '负载均衡'}</span>
            </div>
            <div class="mn-card-sub">最大并发流: <b>${t.max_streams || '不限'}</b> · 最大设备数: <b>${t.max_devices || '不限'}</b></div>
            <div class="mn-actions">
              <button class="mn-btn sm" data-edit-tpl="${esc(t.name)}">编辑</button>
              ${tpls.length > 1 ? `<button class="mn-btn danger sm" data-del-tpl="${esc(t.name)}">删除</button>` : ''}
            </div>
          </div>
        `).join('')}
      </div>
    </div>

    <div class="mn-card">
      <div class="mn-card-head">
        <div>
          <h3 class="mn-card-title" id="users-count-title">观影用户列表 (${users.length})</h3>
        </div>
        <input class="mn-input mn-search-input" id="user-search" type="search" placeholder="搜索用户名 / 备注..." value="${esc(state.userQuery)}" />
      </div>
      <div id="users-list-wrap">${buildUsersListHtml(users)}</div>
    </div>
  `;

  const bindUserActions = (root) => {
    root.querySelectorAll('[data-qr-user]').forEach((b) => {
      b.onclick = () => openQrModal('user', b.dataset.qrUser);
    });
    root.querySelectorAll('[data-edit-user]').forEach((b) => {
      const u = (state.data.users || []).find((x) => x.username === b.dataset.editUser);
      if (u) b.onclick = () => openUserModal(u);
    });
    root.querySelectorAll('[data-del-user]').forEach((b) => {
      b.onclick = () => {
        if (confirm(`确定移除用户 [${b.dataset.delUser}] 的策略记录吗？`)) {
          runAction('ui_delete_user', { username: b.dataset.delUser }, '已移除');
        }
      };
    });
  };

  el('btn-sync-users').onclick = () => runAction('ui_sync_users', {});
  el('btn-add-tpl').onclick = () => openTemplateModal({ name: '', max_streams: 3, max_devices: 5, default_route: 'load_balance' });
  el('btn-add-user').onclick = () => openUserModal({ template: tpls[0]?.name || 'vvvvvip', route_mode: 'template', cache_path: '/EmbyCache' });
  el('user-search').oninput = (e) => {
    state.userQuery = e.target.value;
    const filtered = filterUsers();
    const titleEl = el('users-count-title');
    if (titleEl) titleEl.textContent = `观影用户列表 (${filtered.length})`;
    const wrap = el('users-list-wrap');
    if (wrap) {
      wrap.innerHTML = buildUsersListHtml(filtered);
      bindUserActions(wrap);
    }
  };

  view.querySelectorAll('[data-edit-tpl]').forEach((b) => {
    const t = tpls.find((x) => x.name === b.dataset.editTpl);
    if (t) b.onclick = () => openTemplateModal(t);
  });
  view.querySelectorAll('[data-del-tpl]').forEach((b) => {
    b.onclick = () => runAction('ui_delete_template', { name: b.dataset.delTpl }, '模板已删除');
  });
  bindUserActions(el('users-list-wrap'));
}

function openTemplateModal(tpl) {
  openModal(tpl.name ? `编辑策略模板: ${tpl.name}` : '新增策略模板', `
    <div style="display:flex;flex-direction:column;gap:10px">
      <div class="mn-field">
        <label>模板名称</label>
        <input class="mn-input" id="tp-name" value="${esc(tpl.name || '')}" placeholder="如: vvvvvip" />
      </div>
      <div class="mn-grid-3">
        <div class="mn-field">
          <label>最大并发流数 (0=不限)</label>
          <input type="number" class="mn-input" id="tp-streams" value="${tpl.max_streams ?? 3}" />
        </div>
        <div class="mn-field">
          <label>最大绑定设备数 (0=不限)</label>
          <input type="number" class="mn-input" id="tp-devs" value="${tpl.max_devices ?? 5}" />
        </div>
        <div class="mn-field">
          <label>默认调度通道</label>
          <select class="mn-select" id="tp-route">
            <option value="load_balance" ${tpl.default_route === 'load_balance' ? 'selected' : ''}>多号负载均衡</option>
            <option value="own_cookie" ${tpl.default_route === 'own_cookie' ? 'selected' : ''}>独立115秒传</option>
            <option value="direct" ${tpl.default_route === 'direct' ? 'selected' : ''}>源盘直连</option>
          </select>
        </div>
      </div>
      <div class="mn-modal-foot">
        <button class="mn-btn primary" id="tp-save">保存模板</button>
      </div>
    </div>
  `, (box) => {
    box.querySelector('#tp-save').onclick = async () => {
      await runAction('ui_save_template', {
        template: {
          name: box.querySelector('#tp-name').value.trim(),
          max_streams: Number(box.querySelector('#tp-streams').value || 0),
          max_devices: Number(box.querySelector('#tp-devs').value || 0),
          default_route: box.querySelector('#tp-route').value,
          quota_gb: 0,
        },
      }, '策略模板已保存');
      closeModal();
    };
  });
}

function openUserModal(u) {
  const tpls = state.data.templates || [];
  openModal(u.username ? `编辑观影用户: ${u.username}` : '添加观影用户策略', `
    <div style="display:flex;flex-direction:column;gap:10px">
      <div class="mn-grid-2">
        <div class="mn-field">
          <label>用户名 (与 Mediary/Emby 一致)</label>
          <input class="mn-input" id="us-name" value="${esc(u.username || '')}" />
        </div>
        <div class="mn-field">
          <label>Emby / 宿主 User ID (可选)</label>
          <input class="mn-input" id="us-id" value="${esc(u.emby_id || '')}" />
        </div>
      </div>
      <div class="mn-grid-3">
        <div class="mn-field">
          <label>路由模式</label>
          <select class="mn-select" id="us-route">
            <option value="template" ${u.route_mode === 'template' ? 'selected' : ''}>跟随策略模板</option>
            <option value="load_balance" ${u.route_mode === 'load_balance' ? 'selected' : ''}>多号负载均衡</option>
            <option value="own_cookie" ${u.route_mode === 'own_cookie' ? 'selected' : ''}>用户独立115秒传</option>
            <option value="direct" ${u.route_mode === 'direct' ? 'selected' : ''}>源盘主号直连</option>
          </select>
        </div>
        <div class="mn-field">
          <label>所属策略模板</label>
          <select class="mn-select" id="us-tpl">
            ${tpls.map((t) => `<option value="${esc(t.name)}" ${u.template === t.name ? 'selected' : ''}>${esc(t.name)}</option>`).join('')}
          </select>
        </div>
        <div class="mn-field">
          <label>到期日期 (留空永久)</label>
          <input type="date" class="mn-input" id="us-exp" value="${esc(u.expiry_date || '')}" />
        </div>
      </div>
      <div class="mn-grid-2">
        <div class="mn-field">
          <label>专属秒传目录 (绑自有 Cookie 时)</label>
          <input class="mn-input" id="us-cpath" value="${esc(u.cache_path || '/EmbyCache')}" />
        </div>
        <div class="mn-field">
          <label>备注</label>
          <input class="mn-input" id="us-remark" value="${esc(u.remark || '')}" />
        </div>
      </div>
      <div class="mn-field">
        <label>用户专属 115 Cookie (可选，留空保留原值)</label>
        <textarea class="mn-textarea" id="us-cookie" style="min-height:68px" placeholder="UID=...; CID=...; SEID=...">${esc(u.cookie_masked || '')}</textarea>
      </div>
      <div class="mn-actions">
        <label class="mn-check"><input type="checkbox" id="us-admin" ${u.is_admin ? 'checked' : ''} /> 管理员豁免风控限制</label>
        <label class="mn-check"><input type="checkbox" id="us-ban" ${u.is_banned ? 'checked' : ''} /> 封禁禁止播放</label>
        ${u.has_own_cookie ? `<label class="mn-check"><input type="checkbox" id="us-clear-ck" /> 清除已绑定的独立 Cookie</label>` : ''}
      </div>
      <div class="mn-modal-foot">
        <button class="mn-btn primary" id="us-save">保存用户策略</button>
      </div>
    </div>
  `, (box) => {
    box.querySelector('#us-save').onclick = async () => {
      await runAction('ui_save_user', {
        clear_cookie: box.querySelector('#us-clear-ck')?.checked || false,
        user: {
          username: box.querySelector('#us-name').value.trim(),
          emby_id: box.querySelector('#us-id').value.trim(),
          route_mode: box.querySelector('#us-route').value,
          template: box.querySelector('#us-tpl').value,
          expiry_date: box.querySelector('#us-exp').value.trim(),
          cache_path: box.querySelector('#us-cpath').value.trim(),
          remark: box.querySelector('#us-remark').value.trim(),
          cookies: box.querySelector('#us-cookie').value.trim(),
          is_admin: box.querySelector('#us-admin').checked,
          is_banned: box.querySelector('#us-ban').checked,
        },
      }, '用户策略已保存');
      closeModal();
    };
  });
}

// ==================== 4. Playback Risk & Anti-Scrape ====================
function renderRisk(view) {
  const r = state.data.risk || {};
  const uas = state.data.ua_stats || [];
  const devs = state.data.known_devices || [];

  view.innerHTML = `
    <div class="mn-header">
      <div>
        <h2>播放风控、UA 防爬与设备管控</h2>
        <p>实时拦截非法下载器/爬虫 UA、限制账号短时间跨多 IP 共享及超额设备接入</p>
      </div>
      <div class="mn-actions">
        <button class="mn-btn primary" id="btn-save-risk">保存风控规则</button>
      </div>
    </div>

    <div class="mn-card">
      <h3 class="mn-card-title" style="margin-bottom:10px">全局风控策略</h3>
      <div class="mn-grid-3">
        <div class="mn-field">
          <label>客户端 UA 校验模式</label>
          <select class="mn-select" id="rk-mode">
            <option value="blacklist" ${r.ua_mode !== 'whitelist' ? 'selected' : ''}>黑名单模式 (拦截匹配词)</option>
            <option value="whitelist" ${r.ua_mode === 'whitelist' ? 'selected' : ''}>严格白名单模式 (仅放行匹配词)</option>
          </select>
        </div>
        <div class="mn-field">
          <label>跨 IP 检测时间窗 (分钟)</label>
          <input type="number" class="mn-input" id="rk-ip-win" value="${r.ip_lock_window_mins ?? 30}" />
        </div>
        <div class="mn-field">
          <label>时间窗内允许不同 IP 数</label>
          <input type="number" class="mn-input" id="rk-ip-max" value="${r.ip_lock_max_ips ?? 3}" />
        </div>
      </div>
      <div class="mn-grid-2" style="margin-top:10px">
        <div class="mn-field">
          <label>UA 黑名单关键词 (逗号分隔)</label>
          <input class="mn-input" id="rk-ban-kw" value="${esc((r.banned_ua_keywords || []).join(', '))}" />
        </div>
        <div class="mn-field">
          <label>UA 白名单关键词 (逗号分隔)</label>
          <input class="mn-input" id="rk-allow-kw" value="${esc((r.allowed_ua_keywords || []).join(', '))}" />
        </div>
      </div>
      <div class="mn-actions" style="margin-top:10px">
        <label class="mn-check"><input type="checkbox" id="rk-dev" ${r.enforce_device_limit !== false ? 'checked' : ''} /> 启用模板设备数量上限拦截</label>
        <label class="mn-check"><input type="checkbox" id="rk-ip" ${r.ip_lock_enabled ? 'checked' : ''} /> 启用短时间跨多 IP 异常锁</label>
        <label class="mn-check"><input type="checkbox" id="rk-exp" ${r.auto_disable_expired !== false ? 'checked' : ''} /> 到期自动请求宿主停用账号</label>
      </div>
    </div>

    <div class="mn-grid-2">
      <div class="mn-card">
        <div class="mn-card-head">
          <div>
            <h3 class="mn-card-title">客户端 UA 实时统计 (${uas.length})</h3>
            <div class="mn-card-sub">支持对可疑 UA 一键拉黑或解除封禁</div>
          </div>
        </div>
        ${uas.length === 0 ? `<div class="mn-card-sub">暂无 UA 请求统计</div>` : `
          <div class="mn-table-wrap">
            <table class="mn-table mn-mobile-cards">
              <thead><tr><th>UA 标识</th><th>次数</th><th>最近用户</th><th>状态</th><th>操作</th></tr></thead>
              <tbody>
                ${uas.map((u) => `
                  <tr>
                    <td class="mn-td-primary" data-label="UA 标识"><div class="mn-cell-val"><code>${esc(u.ua_name)}</code></div></td>
                    <td data-label="请求次数"><div class="mn-cell-val">${u.hit_count}</div></td>
                    <td data-label="最近用户"><div class="mn-cell-val">${esc(u.last_username || '-')}</div></td>
                    <td data-label="状态"><div class="mn-cell-val">${u.is_banned ? `<span class="mn-tag danger">已拉黑</span>` : `<span class="mn-tag ok">放行</span>`}</div></td>
                    <td class="mn-td-actions" data-label="操作">
                      <div class="mn-actions">
                        <button class="mn-btn ${u.is_banned ? 'success' : 'danger'} sm" data-ua="${esc(u.ua_name)}" data-ban="${!u.is_banned}">
                          ${u.is_banned ? '解封' : '拉黑'}
                        </button>
                      </div>
                    </td>
                  </tr>
                `).join('')}
              </tbody>
            </table>
          </div>
        `}
      </div>

      <div class="mn-card">
        <div class="mn-card-head">
          <div>
            <h3 class="mn-card-title">已登记播放设备 (${devs.length})</h3>
            <div class="mn-card-sub">可解绑旧设备以释放名额，或封禁异常设备</div>
          </div>
        </div>
        ${devs.length === 0 ? `<div class="mn-card-sub">暂无设备记录</div>` : `
          <div class="mn-table-wrap">
            <table class="mn-table mn-mobile-cards">
              <thead><tr><th>用户</th><th>设备 / 客户端</th><th>最近活跃</th><th>操作</th></tr></thead>
              <tbody>
                ${devs.slice(0, 40).map((dv) => `
                  <tr>
                    <td class="mn-td-primary" data-label="用户">
                      <div class="mn-cell-val"><b>${esc(dv.username)}</b>${dv.is_banned ? ` <span class="mn-tag danger">已封禁</span>` : ''}</div>
                    </td>
                    <td data-label="设备 / 客户端"><div class="mn-cell-val">${esc(dv.device_name || dv.device_id)} <span class="mn-card-sub">(${esc(dv.client)})</span></div></td>
                    <td data-label="最近活跃"><div class="mn-cell-val">${fmtAgo(dv.last_seen_at)}</div></td>
                    <td class="mn-td-actions" data-label="操作">
                      <div class="mn-actions">
                        <button class="mn-btn ${dv.is_banned ? 'success' : 'danger'} sm" data-dev-ban="${esc(dv.device_id)}" data-user="${esc(dv.username)}" data-val="${!dv.is_banned}">
                          ${dv.is_banned ? '解封' : '封禁'}
                        </button>
                        <button class="mn-btn ghost sm" data-dev-rm="${esc(dv.device_id)}" data-user="${esc(dv.username)}">释放</button>
                      </div>
                    </td>
                  </tr>
                `).join('')}
              </tbody>
            </table>
          </div>
        `}
      </div>
    </div>
  `;

  el('btn-save-risk').onclick = () => {
    const splitCsv = (s) => s.split(',').map((x) => x.trim()).filter(Boolean);
    runAction('ui_save_risk', {
      risk: {
        ua_mode: el('rk-mode').value,
        ip_lock_window_mins: Number(el('rk-ip-win').value || 30),
        ip_lock_max_ips: Number(el('rk-ip-max').value || 3),
        banned_ua_keywords: splitCsv(el('rk-ban-kw').value),
        allowed_ua_keywords: splitCsv(el('rk-allow-kw').value),
        enforce_device_limit: el('rk-dev').checked,
        ip_lock_enabled: el('rk-ip').checked,
        auto_disable_expired: el('rk-exp').checked,
      },
    }, '风控规则已保存');
  };

  view.querySelectorAll('[data-ua]').forEach((b) => {
    b.onclick = () => runAction('ui_toggle_ua_ban', { ua_name: b.dataset.ua, banned: b.dataset.ban === 'true' });
  });
  view.querySelectorAll('[data-dev-ban]').forEach((b) => {
    b.onclick = () => runAction('ui_toggle_device_ban', {
      username: b.dataset.user,
      device_id: b.dataset.devBan,
      banned: b.dataset.val === 'true',
    });
  });
  view.querySelectorAll('[data-dev-rm]').forEach((b) => {
    b.onclick = () => runAction('ui_toggle_device_ban', {
      username: b.dataset.user,
      device_id: b.dataset.devRm,
      banned: false,
      remove: true,
    }, '设备名额已释放');
  });
}

// ==================== 5. Engine Settings, Cache & NextEmby Migration ====================
function renderSettings(view) {
  const e = state.data.engine || {};
  const s = state.data.stats || {};

  view.innerHTML = `
    <div class="mn-header">
      <div>
        <h2>全局调度参数、缓存维护与一键迁移</h2>
        <p>无缝导入原 NextEmby 数据库，管理直链缓存 TTL、小文件直出阈值与容错回源策略</p>
      </div>
      <div class="mn-actions">
        <button class="mn-btn" id="btn-export">导出 MediNext 配置</button>
        <button class="mn-btn primary" id="btn-save-engine">保存全局参数</button>
      </div>
    </div>

    <div class="mn-card">
      <div class="mn-card-head">
        <div>
          <h3 class="mn-card-title">从 NextEmby 一键无损迁移</h3>
          <div class="mn-card-sub">支持直接上传 NextEmby 的 <code>config.db</code> SQLite 文件或 <code>.bak</code> 备份文件，自动导入源盘主号、负载均衡池、观影用户、到期日与策略模板</div>
        </div>
        <button class="mn-btn success" id="btn-upload-nextemby">📂 选择 config.db / .bak 文件导入</button>
      </div>
      ${state.data.last_migrated_from ? `<div class="mn-tag ok">已导入记录：${esc(state.data.last_migrated_from)}</div>` : ''}
    </div>

    <div class="mn-card">
      <h3 class="mn-card-title" style="margin-bottom:10px">原生 302 调度引擎参数</h3>
      <div class="mn-grid-3">
        <div class="mn-field">
          <label>直链缓存有效期 (秒, 默认 1800)</label>
          <input type="number" class="mn-input" id="eg-ttl" value="${e.cache_ttl_secs ?? 1800}" />
        </div>
        <div class="mn-field">
          <label>小文件直连阈值 (MB, 0=关闭)</label>
          <input type="number" class="mn-input" id="eg-small" value="${e.small_file_direct_mb ?? 200}" />
        </div>
        <div class="mn-field">
          <label>405 风控自动冷却时长 (秒)</label>
          <input type="number" class="mn-input" id="eg-cd405" value="${e.risk_405_cooldown_secs ?? 1800}" />
        </div>
        <div class="mn-field">
          <label>同接口 UID 最小间隔 (毫秒, 建议 200-300)</label>
          <input type="number" class="mn-input" id="eg-cool" value="${e.p115_cooldown_ms ?? 250}" />
        </div>
        <div class="mn-field">
          <label>单用户每日最大秒传数 (0=不限)</label>
          <input type="number" class="mn-input" id="eg-daily" value="${e.max_daily_transfers ?? 0}" />
        </div>
        <div class="mn-field">
          <label>未纳管访客默认通道</label>
          <select class="mn-select" id="eg-guest">
            <option value="load_balance" ${e.guest_route !== 'direct' ? 'selected' : ''}>多号负载均衡</option>
            <option value="direct" ${e.guest_route === 'direct' ? 'selected' : ''}>源盘直连</option>
          </select>
        </div>
      </div>
      <div class="mn-field" style="margin-top:10px">
        <label>115 全局代理地址 (可选，留空自动继承宿主环境代理)</label>
        <input class="mn-input" id="eg-proxy" value="${esc(e.p115_proxy_url || '')}" placeholder="如 http://10.0.0.252:7892" />
      </div>
      <div class="mn-grid-3" style="margin-top:10px">
        <div class="mn-field">
          <label>到期拦截提示视频直链 (fallback_video_url)</label>
          <input class="mn-input" id="eg-v-exp" value="${esc(e.expired_video_url || '')}" placeholder="可选 http/https MP4 直链" />
        </div>
        <div class="mn-field">
          <label>并发超限提示视频直链 (fallback_video_url)</label>
          <input class="mn-input" id="eg-v-conc" value="${esc(e.concurrency_video_url || '')}" placeholder="可选 http/https MP4 直链" />
        </div>
        <div class="mn-field">
          <label>封禁/防爬提示视频直链 (fallback_video_url)</label>
          <input class="mn-input" id="eg-v-ban" value="${esc(e.banned_video_url || '')}" placeholder="可选 http/https MP4 直链" />
        </div>
      </div>
      <div class="mn-actions" style="margin-top:10px">
        <label class="mn-check"><input type="checkbox" id="eg-fb" ${e.fallback_on_fail !== false ? 'checked' : ''} /> 秒传/负载失败时允许自动回退宿主原生 302</label>
        <label class="mn-check"><input type="checkbox" id="eg-fbg" ${e.fallback_for_guest !== false ? 'checked' : ''} /> 未绑 Cookie 用户允许回退宿主</label>
      </div>
    </div>

    <div class="mn-grid-2">
      <div class="mn-card">
        <h3 class="mn-card-title" style="margin-bottom:6px">⚡ 小号负载智能预热（Mediary 3.4.8+ 原生对接）</h3>
        <div class="mn-card-sub" style="margin-bottom:10px">
          <b>双开关说明</b>：在 Mediary <code>v3.4.8+</code> 中，需<b>同时开启</b>主程序「<b>STRM 工具 → 详情页直链预热</b>」（总闸）与下方插件开关才会触发详情页、多版本切换与片尾 <code>/prefetch</code> 转发；当 MediNext 播放钩子已启用并批准时，开启宿主总闸会<b>自动跳过宿主主号内置预热</b>，全程仅使用小号负载池且不消耗主号配额。
        </div>
        <div style="display:flex;flex-direction:column;gap:8px">
          <label class="mn-check"><input type="checkbox" id="eg-pre-next" ${e.enable_next_ep_prewarm !== false ? 'checked' : ''} /> <b>剧集播放时后台自动预秒传下一集</b>（首帧定向秒传至当前同一负载号并预签同 UA 直链，切集零等待）</label>
          <label class="mn-check"><input type="checkbox" id="eg-pre-detail" ${e.enable_detail_prefetch !== false ? 'checked' : ''} /> <b>详情页 / 多版本切换 / 片尾下一集小号预热</b>（接收 <code>/prefetch</code> 转发：支持详情页停留、<code>PlaybackInfo</code> 多版本切换即时预热、片尾 <code>Progress</code> 临期直链 &lt;300s 自动续签及跨入口缓存复用）</label>
          <div class="mn-card-sub" style="margin-top:2px">停留等待时长与 30 秒冷却合并已由 Mediary 3.4.8+ 宿主统一控制，插件侧零额外阻塞延迟（独立 4s 超时且不计入 /resolve 熔断）。</div>
        </div>
      </div>

      <div class="mn-card">
        <div class="mn-card-head">
          <div>
            <h3 class="mn-card-title">🔔 主程序通知中心联动</h3>
            <div class="mn-card-sub">通过宿主 <code>POST /plugin/notifications</code> 推送至企业微信 / Telegram / Bark（同会话 30 分钟自动去重，拖动进度条不刷屏）</div>
          </div>
          <button class="mn-btn sm" id="btn-test-notify">发送测试通知</button>
        </div>
        <div style="display:flex;flex-direction:column;gap:8px;margin-top:8px">
          <label class="mn-check"><input type="checkbox" id="eg-nt-risk" ${e.notify_risk_intercept !== false ? 'checked' : ''} /> <b>推送风控拦截告警通知</b>（UA 黑名单拦截、并发/设备超限、异地 IP 锁、账号到期拦截）</label>
          <label class="mn-check"><input type="checkbox" id="eg-nt-play" ${e.notify_playback === true ? 'checked' : ''} /> 推送插件额外播放调度文本通知（默认关闭，推荐直接使用 Mediary 主程序自带的剧照卡片播放通知以免重复）</label>
        </div>
      </div>
    </div>

    <div class="mn-card">
      <div class="mn-card-head">
        <div>
          <h3 class="mn-card-title">缓存池维护</h3>
          <div class="mn-card-sub">当前有效直链缓存: <b>${s.cached_links ?? 0}</b> 条 · 跨号秒传 SHA1 映射池: <b>${s.sha1_pool_count ?? 0}</b> 条</div>
        </div>
        <div class="mn-actions">
          <button class="mn-btn" id="btn-clr-links">清空 1800s 直链缓存</button>
          <button class="mn-btn danger" id="btn-clr-all">清空直链与秒传映射池</button>
        </div>
      </div>
    </div>
  `;

  el('btn-save-engine').onclick = () => {
    runAction('ui_save_engine', {
      engine: {
        p115_proxy_url: el('eg-proxy').value.trim(),
        cache_ttl_secs: Number(el('eg-ttl').value || 1800),
        negative_cache_secs: 60,
        small_file_direct_mb: Number(el('eg-small').value || 0),
        risk_405_cooldown_secs: Number(el('eg-cd405').value || 1800),
        p115_cooldown_ms: Number(el('eg-cool').value || 250),
        max_daily_transfers: Number(el('eg-daily').value || 0),
        guest_route: el('eg-guest').value,
        expired_video_url: el('eg-v-exp').value.trim(),
        concurrency_video_url: el('eg-v-conc').value.trim(),
        banned_video_url: el('eg-v-ban').value.trim(),
        fallback_on_fail: el('eg-fb').checked,
        fallback_for_guest: el('eg-fbg').checked,
        enable_next_ep_prewarm: el('eg-pre-next').checked,
        enable_detail_prefetch: el('eg-pre-detail').checked,
        detail_prefetch_delay_ms: 0,
        notify_playback: el('eg-nt-play').checked,
        notify_risk_intercept: el('eg-nt-risk').checked,
      },
    }, '全局调度参数已保存');
  };

  el('btn-test-notify').onclick = () => runAction('ui_test_notify', {});

  el('btn-export').onclick = async () => {
    const res = await bridge.action('ui_export_config', { include_secrets: false });
    downloadTextFile(res.filename, res.content);
    toast('已导出脱敏配置文件', 'success');
  };

  el('btn-upload-nextemby').onclick = () => {
    const fileInput = el('mn-hidden-file');
    fileInput.value = '';
    fileInput.onchange = () => {
      const f = fileInput.files?.[0];
      if (!f) return;
      const reader = new FileReader();
      reader.onload = async () => {
        const res = await runAction('ui_import_nextemby', {
          filename: f.name,
          file_base64: String(reader.result || ''),
        });
        const sm = res.summary || {};
        toast(`导入成功：主号 ${sm.imported_sources} 个、负载号 ${sm.imported_lbs} 个、用户 ${sm.imported_users} 人`, 'success');
      };
      reader.readAsDataURL(f);
    };
    fileInput.click();
  };

  el('btn-clr-links').onclick = () => runAction('ui_clear_cache', { target: 'links' }, '直链缓存已清空');
  el('btn-clr-all').onclick = () => {
    if (confirm('确定同时清空直链缓存和 SHA1 秒传映射池吗？')) {
      runAction('ui_clear_cache', { target: 'all' }, '缓存池已全部重置');
    }
  };
}

// ==================== 6. Playback Dispatch History ====================
function buildHistoryListHtml(list) {
  if (list.length === 0) return `<div class="mn-card-sub">暂无播放调度记录</div>`;
  return `
    <div class="mn-table-wrap">
      <table class="mn-table mn-mobile-cards">
        <thead>
          <tr><th>时间</th><th>用户</th><th>媒体文件</th><th>通道</th><th>出流账号</th><th>客户端 / IP</th><th>结果</th><th>说明</th></tr>
        </thead>
        <tbody>
          ${list.map((h) => `
            <tr>
              <td data-label="时间" style="white-space:nowrap"><div class="mn-cell-val">${esc(h.updated_at)}</div></td>
              <td data-label="用户"><div class="mn-cell-val"><b>${esc(h.username)}</b></div></td>
              <td class="mn-td-primary" data-label="媒体文件">
                <div class="mn-cell-val">
                  <b>${esc(h.item_name || h.item_id || '-')}</b>
                  <span class="mn-mobile-only" style="margin-left:6px">
                    <span class="mn-tag ${h.status === 'redirect' ? 'ok' : h.status === 'deny' ? 'danger' : h.status === 'prewarm' ? 'primary' : 'warn'}">
                      ${h.cache_hit ? '302 (缓存)' : h.status === 'prewarm' ? '已预热' : esc(h.status)}
                    </span>
                  </span>
                </div>
              </td>
              <td data-label="通道"><div class="mn-cell-val"><span class="mn-tag">${esc(h.mode_label)}</span></div></td>
              <td data-label="出流账号"><div class="mn-cell-val">${esc(h.provider || '-')}</div></td>
              <td data-label="客户端 / IP"><div class="mn-cell-val">${esc(h.ua)} <span class="mn-card-sub">${esc(h.ip)}</span></div></td>
              <td class="mn-mobile-hide" data-label="结果">
                <div class="mn-cell-val">
                  <span class="mn-tag ${h.status === 'redirect' ? 'ok' : h.status === 'deny' ? 'danger' : h.status === 'prewarm' ? 'primary' : 'warn'}">
                    ${h.cache_hit ? '302 (缓存)' : h.status === 'prewarm' ? '已预热' : esc(h.status)}
                  </span>
                </div>
              </td>
              <td data-label="说明"><div class="mn-cell-val">${esc(h.detail)}</div></td>
            </tr>
          `).join('')}
        </tbody>
      </table>
    </div>
  `;
}

function renderHistory(view) {
  const filterHistory = () => {
    const q = state.historyQuery.trim().toLowerCase();
    return (state.data.history || []).filter(
      (h) =>
        !q ||
        (h.username || '').toLowerCase().includes(q) ||
        (h.item_name || '').toLowerCase().includes(q) ||
        (h.provider || '').toLowerCase().includes(q) ||
        (h.detail || '').toLowerCase().includes(q)
    );
  };
  const list = filterHistory();

  view.innerHTML = `
    <div class="mn-header">
      <div>
        <h2>播放调度与风控日志</h2>
        <p>展示每一次 302 直链签发、缓存续期、跨号秒传、回源兜底与风控拦截明细</p>
      </div>
      <div class="mn-actions">
        <input class="mn-input mn-search-input" id="hist-q" type="search" placeholder="筛选用户 / 片名 / 账号..." value="${esc(state.historyQuery)}" />
        <button class="mn-btn danger" id="btn-clr-hist">清空记录</button>
      </div>
    </div>

    <div class="mn-card" id="history-list-wrap">${buildHistoryListHtml(list)}</div>
  `;

  el('hist-q').oninput = (e) => {
    state.historyQuery = e.target.value;
    const wrap = el('history-list-wrap');
    if (wrap) wrap.innerHTML = buildHistoryListHtml(filterHistory());
  };
  el('btn-clr-hist').onclick = () => {
    if (confirm('确定清空全部历史记录吗？')) runAction('ui_clear_history', {}, '历史记录已清空');
  };
}

// ==================== Bootstrap ====================
document.querySelectorAll('#mn-nav .mn-nav-btn').forEach((btn) => {
  btn.onclick = () => {
    document.querySelectorAll('#mn-nav .mn-nav-btn').forEach((b) => b.classList.remove('active'));
    btn.classList.add('active');
    const nav = el('mn-nav');
    if (nav && nav.scrollWidth > nav.clientWidth) {
      const targetLeft = Math.max(0, btn.offsetLeft - (nav.clientWidth - btn.offsetWidth) / 2);
      nav.scrollTo({ left: targetLeft, behavior: 'smooth' });
    }
    state.tab = btn.dataset.tab;
    render();
    const mainEl = document.querySelector('.mn-main');
    if (mainEl) mainEl.scrollTop = 0;
  };
});

el('mn-modal-close').onclick = closeModal;
el('mn-modal-backdrop').onclick = (e) => {
  if (e.target === el('mn-modal-backdrop')) closeModal();
};

(async function init() {
  await bridge.getTheme();
  try {
    state.data = await bridge.action('ui_bootstrap', {});
    updateChrome();
    render();
  } catch (err) {
    el('mn-view').innerHTML = `<div class="mn-card" style="color:var(--mn-danger)">加载失败: ${esc(err.message)}</div>`;
  }
})();

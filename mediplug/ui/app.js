import { copyToClipboard, createMediaryBridge, downloadTextFile } from './bridge.js';

const bridge = createMediaryBridge();

const state = {
  tab: 'sources',
  data: null,
  busy: false,
  storeFilter: 'all',
  storeQuery: '',
  logQuery: '',
  lastInspection: null,
  builderItems: [],
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
  const units = ['B', 'KB', 'MB', 'GB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(i >= 2 ? 2 : 1)} ${units[i]}`;
}

function fmtAgo(ts) {
  const n = Number(ts || 0);
  if (!n) return '未同步';
  const diff = Math.max(0, Math.floor(Date.now() / 1000) - n);
  if (diff < 60) return `${diff}秒前`;
  if (diff < 3600) return `${Math.floor(diff / 60)}分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}小时前`;
  return `${Math.floor(diff / 86400)}天前`;
}

let toastTimer = null;
function toast(msg, kind = 'info') {
  const box = el('mp-toast');
  box.textContent = msg;
  box.className = `mp-toast ${kind}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => box.classList.add('hidden'), 4600);
}

function openModal(title, html, onMount) {
  el('mp-modal-title').textContent = title;
  el('mp-modal-body').innerHTML = html;
  el('mp-modal-backdrop').classList.remove('hidden');
  if (onMount) onMount(el('mp-modal-body'));
}

function closeModal() {
  el('mp-modal-backdrop').classList.add('hidden');
  el('mp-modal-body').innerHTML = '';
}

function confirmModal(title, message, confirmLabel = '确认执行', danger = false, onConfirm) {
  openModal(
    title,
    `
      <div style="font-size:13.5px;line-height:1.6;margin-bottom:16px">${message}</div>
      <div class="mp-actions" style="justify-content:flex-end">
        <button type="button" class="mp-btn" id="modal-cancel-btn">取消</button>
        <button type="button" class="mp-btn ${danger ? 'danger' : 'primary'}" id="modal-ok-btn">${esc(confirmLabel)}</button>
      </div>
    `,
    (root) => {
      root.querySelector('#modal-cancel-btn').onclick = closeModal;
      root.querySelector('#modal-ok-btn').onclick = async () => {
        closeModal();
        await onConfirm();
      };
    }
  );
}

async function runAction(name, payload = {}, okMsg = '') {
  state.busy = true;
  try {
    const res = await bridge.action(name, payload);
    if (res && res.bootstrap) {
      state.data = res.bootstrap;
    } else if (res && res.host_status && res.engine) {
      state.data = res;
    }
    updateChrome();
    render();
    if (okMsg || res?.notice) toast(okMsg || res.notice, 'success');
    return res;
  } catch (err) {
    toast(err.message || String(err), 'error');
    throw err;
  } finally {
    state.busy = false;
  }
}

function updateChrome() {
  const d = state.data;
  if (!d) return;
  const hs = d.host_status || {};
  const st = d.settings || {};

  el('mp-version-label').textContent = `v${d.version || '1.0.0'} · 插件源桥接`;
  el('badge-sources').textContent = (d.host_sources || []).length;
  el('badge-github').textContent = (d.github_repos || []).length;
  el('badge-store').textContent = (d.store?.plugins || []).length;
  el('badge-builder').textContent = state.builderItems.length || (d.hosted_entries || []).length;
  el('badge-logs').textContent = (d.logs || []).length;

  const daemonPill = el('mp-daemon-pill');
  if (hs.plugin_enabled === false || st['$enabled'] === false) {
    daemonPill.textContent = '插件当前未启用';
    daemonPill.className = 'mp-status-pill warn';
  } else if (hs.daemon_ready) {
    daemonPill.textContent = `本地 Catalog 服务: 127.0.0.1:${hs.active_port}`;
    daemonPill.className = 'mp-status-pill ok';
  } else {
    daemonPill.textContent = `待启动本地服务 (:${d.engine?.listen_port || 18119})`;
    daemonPill.className = 'mp-status-pill warn';
  }

  const authPill = el('mp-auth-pill');
  if (hs.store_auth_ready) {
    authPill.textContent = '宿主源接口: 鉴权已就绪';
    authPill.className = 'mp-status-pill ok';
  } else {
    authPill.textContent = '待配置管理员 API Token';
    authPill.className = 'mp-status-pill warn';
  }

  const bridgePill = el('mp-bridge-pill');
  const bridgedCount = (d.unified_catalog?.plugins || []).length;
  if (hs.bridge_registered) {
    bridgePill.textContent = `已接入插件中心 (${bridgedCount} 款转换插件)`;
    bridgePill.className = 'mp-status-pill ok';
  } else {
    bridgePill.textContent = `本地待转换插件: ${bridgedCount} 款`;
    bridgePill.className = bridgedCount > 0 ? 'mp-status-pill warn' : 'mp-status-pill';
  }

  el('mp-sync-note').textContent = d.last_sync_summary || 'Mediary 3.4.6+ 原生多源聚合';
}

function renderAuthBanner(hs, engine) {
  if (hs.store_auth_ready) return '';
  return `
    <div class="mp-card" style="border-color:rgba(245,158,11,0.5)">
      <div class="mp-card-head">
        <div>
          <div class="mp-card-title" style="color:var(--mp-warn)">🔑 配置 Mediary 管理员 API Token 以管理插件源</div>
          <div class="mp-card-sub">
            Mediary v3.4.6 的 <code>/api/plugin-store/sources</code> 接口要求使用管理员 API Token 鉴权。请打开宿主「<b>设置 → 安全</b>」复制 API Token 粘贴在下方保存：
          </div>
        </div>
      </div>
      <div style="display:flex;gap:8px;flex-wrap:wrap;align-items:center">
        <input type="password" class="mp-input" id="quick-admin-token" style="flex:1;min-width:240px" placeholder="粘贴 Mediary 管理员 API Token..." />
        <button type="button" class="mp-btn primary" id="btn-save-quick-token">保存并连接插件源接口</button>
      </div>
      ${hs.store_auth_error ? `<div class="mp-card-sub" style="margin-top:6px;color:var(--mp-danger)">${esc(hs.store_auth_error)}</div>` : ''}
    </div>
  `;
}

function bindQuickTokenSave(view) {
  const btn = view.querySelector('#btn-save-quick-token');
  if (!btn) return;
  btn.onclick = async () => {
    const val = view.querySelector('#quick-admin-token')?.value?.trim() || '';
    if (!val) {
      toast('请先粘贴管理员 API Token', 'error');
      return;
    }
    const eng = { ...(state.data?.engine || {}), admin_api_token: val };
    await runAction('ui_save_engine', { engine: eng }, '管理员 API Token 已保存');
  };
}

function render() {
  const view = el('mp-view');
  if (!state.data) {
    view.innerHTML = `<div class="mp-card">正在加载 MediPlug 工作台数据...</div>`;
    return;
  }
  switch (state.tab) {
    case 'sources': return renderSourcesTab(view);
    case 'github': return renderGithubTab(view);
    case 'store': return renderStoreTab(view);
    case 'builder': return renderBuilderTab(view);
    case 'settings': return renderSettingsTab(view);
  }
}

// ==================== 1. Host Plugin Sources Management ====================
function renderSourcesTab(view) {
  const d = state.data;
  const hs = d.host_status || {};
  const sources = d.host_sources || [];
  const store = d.store || {};
  const bridgedCount = (d.unified_catalog?.plugins || []).length;

  view.innerHTML = `
    <div class="mp-header">
      <div>
        <h2>宿主插件源管理（/api/plugin-store/sources）</h2>
        <p>免敲 curl 命令可视化增删启停 Mediary 第三方 catalog.json 插件源，接入后直接在宿主「插件 → 插件中心」卡片安装与热升级</p>
      </div>
      <div class="mp-actions">
        <button type="button" class="mp-btn" id="btn-refresh-sources">刷新列表</button>
        <button type="button" class="mp-btn success" id="btn-register-bridge" ${!hs.store_auth_ready ? 'disabled' : ''}>
          ${hs.bridge_registered ? '重新同步本地桥接源' : '⚡ 一键注册本地 GitHub 桥接源'}
        </button>
        <button type="button" class="mp-btn primary" id="btn-add-source" ${!hs.store_auth_ready ? 'disabled' : ''}>+ 添加标准 catalog.json 源</button>
      </div>
    </div>

    ${renderAuthBanner(hs, d.engine)}

    <div class="mp-kpis">
      <div class="mp-kpi">
        <div class="mp-kpi-label">宿主已注册插件源</div>
        <div class="mp-kpi-val">${sources.length}</div>
        <div class="mp-kpi-sub">含官方内置保护源 official</div>
      </div>
      <div class="mp-kpi">
        <div class="mp-kpi-label">聚合可见插件总数</div>
        <div class="mp-kpi-val">${(store.plugins || []).length}</div>
        <div class="mp-kpi-sub">可更新 ${store.updates_available || 0} 款 · 冲突 ${(store.conflicts || []).length} 项</div>
      </div>
      <div class="mp-kpi">
        <div class="mp-kpi-label">本地 GitHub 转换服务</div>
        <div class="mp-kpi-val">${hs.daemon_ready ? '在线' : '待启用'}</div>
        <div class="mp-kpi-sub">${esc(hs.bridge_catalog_url || '')}</div>
      </div>
      <div class="mp-kpi">
        <div class="mp-kpi-label">无 catalog.json 已桥接</div>
        <div class="mp-kpi-val">${bridgedCount} <span style="font-size:13px;font-weight:500;color:var(--mp-muted)">款插件</span></div>
        <div class="mp-kpi-sub">来自 ${(d.github_repos || []).length} 个 GitHub 仓库</div>
      </div>
    </div>

    <div class="mp-card">
      <div class="mp-card-head">
        <div>
          <h3 class="mp-card-title">已注册插件源列表</h3>
          <div class="mp-card-sub">官方内置源 <code>official</code> 命名空间受宿主保护；第三方源支持 <code>http://</code> 与 <code>https://</code> 标准 catalog.json 地址</div>
        </div>
        <div class="mp-actions">
          <button type="button" class="mp-btn sm" id="btn-copy-bridge-url">复制本地桥接 URL</button>
        </div>
      </div>

      <div class="mp-table-wrap">
        <table class="mp-table">
          <thead>
            <tr>
              <th>源名称 / ID</th>
              <th>类型</th>
              <th>Catalog 索引地址</th>
              <th>状态</th>
              <th style="text-align:right">操作</th>
            </tr>
          </thead>
          <tbody>
            ${sources.length === 0 ? `
              <tr>
                <td colspan="5" style="text-align:center;padding:24px;color:var(--mp-muted)">
                  ${hs.store_auth_ready ? '暂无插件源记录' : '完成上方管理员 API Token 配置后即可读取并管理宿主插件源'}
                </td>
              </tr>
            ` : sources.map((s) => `
              <tr>
                <td>
                  <div style="font-weight:600">${esc(s.name)}</div>
                  <div style="font-size:11.5px;color:var(--mp-muted)"><code>${esc(s.id)}</code></div>
                </td>
                <td>
                  ${s.builtin
                    ? '<span class="mp-tag primary">官方内置</span>'
                    : s.is_mediplug_bridge
                      ? '<span class="mp-tag ok">MediPlug 本地桥接</span>'
                      : '<span class="mp-tag">第三方直连源</span>'}
                </td>
                <td>
                  <div style="max-width:440px;word-break:break-all;font-family:monospace;font-size:12px">${esc(s.url || '(宿主默认官方源)')}</div>
                  ${s.error ? `<div style="font-size:11.5px;color:var(--mp-danger);margin-top:3px">错误: ${esc(s.error)}</div>` : ''}
                </td>
                <td>
                  ${s.enabled ? '<span class="mp-tag ok">已启用</span>' : '<span class="mp-tag warn">已停用</span>'}
                </td>
                <td style="text-align:right;white-space:nowrap">
                  ${s.builtin ? `
                    <span style="font-size:12px;color:var(--mp-muted)">宿主保护</span>
                  ` : `
                    <button type="button" class="mp-btn sm" data-act="toggle-src" data-id="${esc(s.id)}" data-enabled="${s.enabled ? '0' : '1'}">
                      ${s.enabled ? '停用' : '启用'}
                    </button>
                    <button type="button" class="mp-btn sm" data-act="edit-src" data-id="${esc(s.id)}">编辑</button>
                    <button type="button" class="mp-btn sm danger" data-act="del-src" data-id="${esc(s.id)}" data-name="${esc(s.name)}">删除</button>
                  `}
                </td>
              </tr>
            `).join('')}
          </tbody>
        </table>
      </div>
    </div>
  `;

  bindQuickTokenSave(view);

  view.querySelector('#btn-refresh-sources').onclick = () => runAction('ui_bootstrap', {}, '已刷新宿主插件源状态');
  view.querySelector('#btn-copy-bridge-url').onclick = async () => {
    await copyToClipboard(hs.bridge_catalog_url || 'http://127.0.0.1:18119/catalog.json');
    toast('已复制本地桥接 catalog.json 地址到剪贴板', 'success');
  };
  view.querySelector('#btn-register-bridge').onclick = () =>
    runAction('ui_register_local_bridge', {}, '已将本地桥接源注册至宿主插件中心');

  view.querySelector('#btn-add-source').onclick = () => openHostSourceModal(null);

  view.querySelectorAll('[data-act="toggle-src"]').forEach((btn) => {
    btn.onclick = () => {
      const id = btn.dataset.id;
      const enabled = btn.dataset.enabled === '1';
      runAction('ui_toggle_host_source', { id, enabled });
    };
  });

  view.querySelectorAll('[data-act="edit-src"]').forEach((btn) => {
    btn.onclick = () => {
      const target = sources.find((x) => x.id === btn.dataset.id);
      if (target) openHostSourceModal(target);
    };
  });

  view.querySelectorAll('[data-act="del-src"]').forEach((btn) => {
    btn.onclick = () => {
      const id = btn.dataset.id;
      const name = btn.dataset.name;
      confirmModal(
        '删除第三方插件源',
        `确定要从宿主插件中心移除插件源「<b>${esc(name)}</b> (<code>${esc(id)}</code>)」吗？（不会卸载已安装的插件）`,
        '确认删除',
        true,
        () => runAction('ui_delete_host_source', { id })
      );
    };
  });
}

function openHostSourceModal(existing) {
  const isEdit = Boolean(existing);
  openModal(
    isEdit ? `编辑插件源: ${existing.id}` : '添加标准 catalog.json 插件源',
    `
      <div style="display:flex;flex-direction:column;gap:12px">
        <div class="mp-field">
          <label>插件源 ID（英文小写、数字与连字符，不可为 official）</label>
          <input type="text" class="mp-input" id="src-id" value="${esc(existing?.id || '')}" ${isEdit ? 'readonly' : ''} placeholder="例如: my-community-store" />
        </div>
        <div class="mp-field">
          <label>显示名称</label>
          <input type="text" class="mp-input" id="src-name" value="${esc(existing?.name || '')}" placeholder="例如: 社区第三方插件源" />
        </div>
        <div class="mp-field">
          <label>标准 catalog.json 地址（支持 https:// 或 http://）</label>
          <input type="text" class="mp-input" id="src-url" value="${esc(existing?.url || '')}" placeholder="https://raw.githubusercontent.com/yourname/mediary-plugins/main/catalog.json" />
        </div>
        <label class="mp-check">
          <input type="checkbox" id="src-enabled" ${existing?.enabled !== false ? 'checked' : ''} />
          <span>注册后立即启用该插件源</span>
        </label>
        <div class="mp-card-sub">
          提示：如果对方 GitHub 仓库<b>没有编写 catalog.json</b>（仅在 Releases 发布 .tar.gz 包），请切换到左侧「<b>GitHub 仓库桥接</b>」页直接填入仓库地址自动转换！
        </div>
        <div class="mp-actions" style="justify-content:flex-end;margin-top:4px">
          <button type="button" class="mp-btn" id="src-cancel">取消</button>
          <button type="button" class="mp-btn primary" id="src-save">保存并注册到宿主</button>
        </div>
      </div>
    `,
    (root) => {
      root.querySelector('#src-cancel').onclick = closeModal;
      root.querySelector('#src-save').onclick = async () => {
        const id = root.querySelector('#src-id').value.trim();
        const name = root.querySelector('#src-name').value.trim();
        const url = root.querySelector('#src-url').value.trim();
        const enabled = root.querySelector('#src-enabled').checked;
        if (!id || !url) {
          toast('请完整填写插件源 ID 与 catalog.json URL', 'error');
          return;
        }
        await runAction('ui_save_host_source', { source: { id, name, url, enabled } });
        closeModal();
      };
    }
  );
}

// ==================== 2. GitHub Release Repository Auto-Bridge ====================
function renderGithubTab(view) {
  const d = state.data;
  const hs = d.host_status || {};
  const repos = d.github_repos || [];

  view.innerHTML = `
    <div class="mp-header">
      <div>
        <h2>无 catalog.json 的 GitHub Release 仓库自动桥接</h2>
        <p>只需填入任意 GitHub 仓库地址，MediPlug 自动解析其 Releases 资产、核验 sha256 与包内 plugin.json 权限，生成本地标准 catalog.json 并注册进原生「插件中心」</p>
      </div>
      <div class="mp-actions">
        <button type="button" class="mp-btn" id="btn-preview-unified">预览本地 catalog.json</button>
        <button type="button" class="mp-btn" id="btn-sync-all-gh" ${repos.length === 0 ? 'disabled' : ''}>🔄 立即同步全部仓库</button>
        <button type="button" class="mp-btn primary" id="btn-add-gh">+ 接入 GitHub 仓库</button>
      </div>
    </div>

    ${!hs.daemon_ready ? `
      <div class="mp-card" style="border-color:rgba(245,158,11,0.5)">
        <div class="mp-card-title" style="color:var(--mp-warn)">⚠️ 本地 Catalog HTTP 常驻服务尚未运行</div>
        <div class="mp-card-sub">
          请前往 Mediary「已安装 → MediPlug → 配置」开启「<b>启用插件</b>」并保存，宿主将自动拉起本地 <code>${esc(hs.bridge_catalog_url)}</code> 服务供插件中心拉取。
        </div>
      </div>
    ` : ''}

    <div class="mp-card">
      <div class="mp-card-head">
        <div>
          <h3 class="mp-card-title">快速接入普通 GitHub 仓库</h3>
          <div class="mp-card-sub">支持单插件仓库（如 <code>https://github.com/owner/mediwash</code>）或多插件合集 Release 仓库</div>
        </div>
      </div>
      <div style="display:flex;gap:8px;flex-wrap:wrap;align-items:center">
        <input type="text" class="mp-input" id="quick-gh-repo" style="flex:2;min-width:250px" placeholder="输入 https://github.com/owner/repo 或 owner/repo" />
        <select class="mp-select" id="quick-gh-mode" style="flex:1;min-width:190px;max-width:240px">
          <option value="unified">合并入统一桥接源 (/catalog.json)</option>
          <option value="dedicated">注册为独立插件源 (/catalog/id.json)</option>
        </select>
        <button type="button" class="mp-btn primary" id="btn-quick-gh-add">⚡ 解析并接入插件中心</button>
      </div>
    </div>

    <div class="mp-card">
      <div class="mp-card-head">
        <div>
          <h3 class="mp-card-title">已桥接的 GitHub Release 仓库 (${repos.length})</h3>
          <div class="mp-card-sub">定时任务会自动检测新 Release 并更新本地索引，使宿主「插件中心」自动亮起「可更新」按钮</div>
        </div>
      </div>

      <div class="mp-table-wrap">
        <table class="mp-table">
          <thead>
            <tr>
              <th>GitHub 仓库</th>
              <th>注册方式</th>
              <th>最新 Release</th>
              <th>已转换插件与架构</th>
              <th>同步状态</th>
              <th style="text-align:right">操作</th>
            </tr>
          </thead>
          <tbody>
            ${repos.length === 0 ? `
              <tr>
                <td colspan="6" style="text-align:center;padding:26px;color:var(--mp-muted)">
                  尚未添加 GitHub Release 仓库，在上方输入任意插件的 GitHub 仓库地址即可一键转换接入
                </td>
              </tr>
            ` : repos.map((r) => {
              const plugins = r.plugins || [];
              return `
                <tr>
                  <td>
                    <div style="font-weight:600">${esc(r.name || r.repo)}</div>
                    <div style="font-size:11.5px;color:var(--mp-muted)"><code>${esc(r.repo)}</code> · ID: <code>${esc(r.id)}</code></div>
                  </td>
                  <td>
                    ${r.register_mode === 'dedicated'
                      ? `<span class="mp-tag primary">独立源 (/catalog/${esc(r.id)}.json)</span>`
                      : '<span class="mp-tag ok">统一聚合源 (/catalog.json)</span>'}
                  </td>
                  <td>
                    <span class="mp-tag">${esc(r.last_release_tag || (r.pinned_tag ? `锁定 ${r.pinned_tag}` : '待同步'))}</span>
                  </td>
                  <td>
                    ${plugins.length === 0 ? '<span style="color:var(--mp-muted)">-</span>' : plugins.map((p) => {
                      const archs = Object.keys(p.artifacts || {}).join(', ');
                      return `<div style="margin-bottom:3px"><b>${esc(p.id)}</b> <span class="mp-tag ok">v${esc(p.version)}</span> <span style="font-size:11px;color:var(--mp-muted)">[${esc(archs)} · ${p.permissions?.length || 0} 权限]</span></div>`;
                    }).join('')}
                  </td>
                  <td>
                    ${r.last_error
                      ? `<span class="mp-tag danger">失败</span><div style="font-size:11px;color:var(--mp-danger);max-width:220px;margin-top:2px">${esc(r.last_error)}</div>`
                      : `<span class="mp-tag ok">${r.enabled ? '正常' : '已停用'}</span><div style="font-size:11px;color:var(--mp-muted);margin-top:2px">${fmtAgo(r.last_synced_at)}</div>`}
                  </td>
                  <td style="text-align:right;white-space:nowrap">
                    <button type="button" class="mp-btn sm" data-act="sync-gh" data-id="${esc(r.id)}">同步</button>
                    <button type="button" class="mp-btn sm" data-act="json-gh" data-id="${esc(r.id)}">JSON</button>
                    <button type="button" class="mp-btn sm" data-act="edit-gh" data-id="${esc(r.id)}">编辑</button>
                    <button type="button" class="mp-btn sm danger" data-act="del-gh" data-id="${esc(r.id)}" data-repo="${esc(r.repo)}">移除</button>
                  </td>
                </tr>
              `;
            }).join('')}
          </tbody>
        </table>
      </div>
    </div>
  `;

  view.querySelector('#btn-quick-gh-add').onclick = async () => {
    const repo = view.querySelector('#quick-gh-repo').value.trim();
    const register_mode = view.querySelector('#quick-gh-mode').value;
    if (!repo) {
      toast('请输入 GitHub 仓库地址（如 https://github.com/owner/repo）', 'error');
      return;
    }
    await runAction('ui_save_github_repo', {
      repo: { repo, register_mode, enabled: true },
      sync_now: true,
    });
  };

  view.querySelector('#btn-add-gh').onclick = () => openGithubRepoModal(null);
  view.querySelector('#btn-sync-all-gh').onclick = () => runAction('ui_sync_github_repo', { id: 'all' });
  view.querySelector('#btn-preview-unified').onclick = () => previewCatalogModal('');

  view.querySelectorAll('[data-act="sync-gh"]').forEach((btn) => {
    btn.onclick = () => runAction('ui_sync_github_repo', { id: btn.dataset.id });
  });
  view.querySelectorAll('[data-act="json-gh"]').forEach((btn) => {
    btn.onclick = () => previewCatalogModal(btn.dataset.id);
  });
  view.querySelectorAll('[data-act="edit-gh"]').forEach((btn) => {
    btn.onclick = () => {
      const target = repos.find((x) => x.id === btn.dataset.id);
      if (target) openGithubRepoModal(target);
    };
  });
  view.querySelectorAll('[data-act="del-gh"]').forEach((btn) => {
    btn.onclick = () => {
      confirmModal(
        '移除 GitHub 转换仓库',
        `确定移除仓库 <code>${esc(btn.dataset.repo)}</code> 的桥接转换吗？`,
        '确认移除',
        true,
        () => runAction('ui_delete_github_repo', { id: btn.dataset.id, remove_host_source: true })
      );
    };
  });
}

function openGithubRepoModal(existing) {
  const isEdit = Boolean(existing);
  openModal(
    isEdit ? `编辑 GitHub 桥接仓: ${existing.repo}` : '接入 GitHub Release 仓库',
    `
      <div style="display:flex;flex-direction:column;gap:12px">
        <div class="mp-field">
          <label>GitHub 仓库地址（支持完整 URL 或 owner/repo）</label>
          <input type="text" class="mp-input" id="gh-repo" value="${esc(existing?.repo || '')}" placeholder="https://github.com/owner/repo" />
        </div>
        <div class="mp-grid-2">
          <div class="mp-field">
            <label>显示名称（选填）</label>
            <input type="text" class="mp-input" id="gh-name" value="${esc(existing?.name || '')}" placeholder="留空默认使用 owner/repo" />
          </div>
          <div class="mp-field">
            <label>自定义源 ID（选填）</label>
            <input type="text" class="mp-input" id="gh-id" value="${esc(existing?.id || '')}" ${isEdit ? 'readonly' : ''} placeholder="留空自动生成 gh-owner-repo" />
          </div>
        </div>
        <div class="mp-grid-2">
          <div class="mp-field">
            <label>锁定特定 Tag（选填，留空跟随最新 Release）</label>
            <input type="text" class="mp-input" id="gh-tag" value="${esc(existing?.pinned_tag || '')}" placeholder="如 v2.3.4（留空自动取最新版）" />
          </div>
          <div class="mp-field">
            <label>资产文件名过滤词（选填）</label>
            <input type="text" class="mp-input" id="gh-kw" value="${esc(existing?.asset_keyword || '')}" placeholder="多产物仓库可填前缀过滤" />
          </div>
        </div>
        <div class="mp-field">
          <label>注册至宿主「插件中心」方式</label>
          <select class="mp-select" id="gh-mode">
            <option value="unified" ${existing?.register_mode !== 'dedicated' ? 'selected' : ''}>合并入统一桥接源 (/catalog.json)</option>
            <option value="dedicated" ${existing?.register_mode === 'dedicated' ? 'selected' : ''}>作为独立插件源注册 (/catalog/&lt;id&gt;.json)</option>
          </select>
        </div>
        <div style="display:flex;gap:18px;flex-wrap:wrap">
          <label class="mp-check">
            <input type="checkbox" id="gh-pre" ${existing?.include_prerelease ? 'checked' : ''} />
            <span>包含 Pre-release 预发布版本</span>
          </label>
          <label class="mp-check">
            <input type="checkbox" id="gh-en" ${existing?.enabled !== false ? 'checked' : ''} />
            <span>启用自动同步转换</span>
          </label>
        </div>
        <div class="mp-actions" style="justify-content:flex-end;margin-top:4px">
          <button type="button" class="mp-btn" id="gh-cancel">取消</button>
          <button type="button" class="mp-btn primary" id="gh-save">保存并立即解析 Release</button>
        </div>
      </div>
    `,
    (root) => {
      root.querySelector('#gh-cancel').onclick = closeModal;
      root.querySelector('#gh-save').onclick = async () => {
        const repo = root.querySelector('#gh-repo').value.trim();
        if (!repo) {
          toast('请填写 GitHub 仓库地址', 'error');
          return;
        }
        await runAction('ui_save_github_repo', {
          repo: {
            id: root.querySelector('#gh-id').value.trim(),
            repo,
            name: root.querySelector('#gh-name').value.trim(),
            pinned_tag: root.querySelector('#gh-tag').value.trim(),
            asset_keyword: root.querySelector('#gh-kw').value.trim(),
            register_mode: root.querySelector('#gh-mode').value,
            include_prerelease: root.querySelector('#gh-pre').checked,
            enabled: root.querySelector('#gh-en').checked,
          },
          sync_now: true,
        });
        closeModal();
      };
    }
  );
}

async function previewCatalogModal(repoId) {
  const res = await bridge.action('ui_preview_local_catalog', { repo_id: repoId });
  openModal(
    repoId ? `转换生成的 catalog.json (${repoId})` : '本地聚合 catalog.json 实时预览',
    `
      <div style="display:flex;flex-direction:column;gap:10px">
        <textarea class="mp-textarea" style="min-height:300px" readonly>${esc(res.content)}</textarea>
        <div class="mp-actions" style="justify-content:flex-end">
          <button type="button" class="mp-btn" id="cat-copy">复制 JSON</button>
          <button type="button" class="mp-btn primary" id="cat-dl">下载 ${esc(res.filename)}</button>
        </div>
      </div>
    `,
    (root) => {
      root.querySelector('#cat-copy').onclick = async () => {
        await copyToClipboard(res.content);
        toast('已复制 catalog.json 内容', 'success');
      };
      root.querySelector('#cat-dl').onclick = () => {
        downloadTextFile(res.filename, res.content);
      };
    }
  );
}

// ==================== 3. Merged Plugin Store Explorer ====================
function renderStoreTab(view) {
  const d = state.data;
  const hs = d.host_status || {};
  const store = d.store || {};
  const allPlugins = store.plugins || [];
  const conflicts = store.conflicts || [];
  const q = state.storeQuery.trim().toLowerCase();

  const filtered = allPlugins.filter((p) => {
    if (state.storeFilter === 'updates' && !p.update_available) return false;
    if (state.storeFilter === 'third_party' && p.official) return false;
    if (state.storeFilter === 'official' && !p.official) return false;
    if (state.storeFilter === 'installed' && !p.installed) return false;
    if (!q) return true;
    return `${p.id} ${p.name} ${p.description} ${p.author} ${p.source_name}`.toLowerCase().includes(q);
  });

  view.innerHTML = `
    <div class="mp-header">
      <div>
        <h2>原生「插件中心」聚合结果预览</h2>
        <p>实时展示宿主 <code>GET /api/plugin-store</code> 合并官方源与全部第三方源后的插件卡片、可更新状态与命名冲突拦截记录</p>
      </div>
      <div class="mp-actions">
        <button type="button" class="mp-btn" id="btn-refresh-store">刷新插件中心</button>
      </div>
    </div>

    ${renderAuthBanner(hs, d.engine)}

    ${conflicts.length > 0 ? `
      <div class="mp-card" style="border-color:rgba(239,68,68,0.45)">
        <div class="mp-card-title" style="color:var(--mp-danger)">🛡️ 宿主检测到插件 ID 命名冲突 (${conflicts.length} 项已被安全拦截)</div>
        <div class="mp-card-sub" style="margin-bottom:8px">官方源命名空间受宿主保护，第三方源不可与官方插件重名：</div>
        <div class="mp-code-box">${esc(JSON.stringify(conflicts, null, 2))}</div>
      </div>
    ` : ''}

    <div class="mp-card">
      <div style="display:flex;gap:8px;flex-wrap:wrap;align-items:center">
        <input type="text" class="mp-input" id="store-search" style="flex:1;min-width:220px" value="${esc(state.storeQuery)}" placeholder="搜索插件名称、ID、作者或来源..." />
        <select class="mp-select" id="store-filter" style="width:auto;min-width:160px">
          <option value="all" ${state.storeFilter === 'all' ? 'selected' : ''}>全部插件 (${allPlugins.length})</option>
          <option value="third_party" ${state.storeFilter === 'third_party' ? 'selected' : ''}>第三方源插件</option>
          <option value="updates" ${state.storeFilter === 'updates' ? 'selected' : ''}>有可更新版本 (${store.updates_available || 0})</option>
          <option value="installed" ${state.storeFilter === 'installed' ? 'selected' : ''}>已安装插件</option>
          <option value="official" ${state.storeFilter === 'official' ? 'selected' : ''}>官方内置源</option>
        </select>
      </div>
    </div>

    ${filtered.length === 0 ? `
      <div class="mp-card" style="text-align:center;padding:32px;color:var(--mp-muted)">
        ${hs.store_auth_ready ? '当前筛选条件下暂无匹配的插件' : '配置管理员 API Token 后即可预览聚合插件商店'}
      </div>
    ` : `
      <div class="mp-store-grid">
        ${filtered.map((p) => `
          <div class="mp-plugin-card">
            <div>
              <div style="display:flex;justify-content:space-between;gap:8px;align-items:flex-start">
                <div>
                  <div style="font-weight:700;font-size:14.5px">${esc(p.name)}</div>
                  <div style="font-size:11.5px;color:var(--mp-muted)">
                    <code>${esc(p.id)}</code> · v${esc(p.version)}
                    ${p.installed ? ` · 已装 v${esc(p.installed_version || p.version)}` : ''}
                  </div>
                </div>
                ${p.official
                  ? '<span class="mp-tag primary">官方</span>'
                  : `<span class="mp-tag ok">${esc(p.source_name || '第三方')}</span>`}
              </div>
              <div class="mp-plugin-desc" style="margin-top:8px">${esc(p.description || '暂无简介')}</div>
            </div>

            <div style="display:flex;justify-content:space-between;align-items:center;gap:8px;padding-top:8px;border-top:1px solid var(--mp-border)">
              <div style="font-size:11.5px;color:var(--mp-muted)">
                ${p.permissions?.length || 0} 项权限${p.min_mediary_version ? ` · >= v${esc(p.min_mediary_version)}` : ''}
              </div>
              <div class="mp-actions">
                ${p.update_available ? `
                  <button type="button" class="mp-btn sm primary" data-act="install-plug" data-id="${esc(p.id)}">⬆️ 热升级至 v${esc(p.version)}</button>
                ` : p.installed ? `
                  <span class="mp-tag ok">${p.enabled ? '已安装·启用' : '已安装·停用'}</span>
                ` : `
                  <button type="button" class="mp-btn sm primary" data-act="install-plug" data-id="${esc(p.id)}">一键安装</button>
                `}
              </div>
            </div>
          </div>
        `).join('')}
      </div>
    `}
  `;

  bindQuickTokenSave(view);
  view.querySelector('#btn-refresh-store').onclick = () => runAction('ui_bootstrap', {}, '已刷新插件中心聚合数据');
  view.querySelector('#store-search').oninput = (e) => {
    state.storeQuery = e.target.value;
    renderStoreTab(view);
    const input = view.querySelector('#store-search');
    input.focus();
    input.setSelectionRange(input.value.length, input.value.length);
  };
  view.querySelector('#store-filter').onchange = (e) => {
    state.storeFilter = e.target.value;
    renderStoreTab(view);
  };
  view.querySelectorAll('[data-act="install-plug"]').forEach((btn) => {
    btn.onclick = () => {
      const pid = btn.dataset.id;
      confirmModal(
        '调用宿主安装 / 热升级插件',
        `确定调用宿主原生安装器下载校验 SHA256 并热安装插件 <code>${esc(pid)}</code> 吗？（首次安装默认保持停用状态，安装后可在宿主「已安装」页确认权限启用）`,
        '立即安装',
        false,
        () => runAction('ui_install_store_plugin', { plugin_id: pid })
      );
    };
  });
}

// ==================== 4. Package Validator & Publisher catalog.json Builder ====================
function renderBuilderTab(view) {
  const d = state.data;
  const hosted = d.hosted_entries || [];
  const ins = state.lastInspection;
  const builderDoc = {
    schema_version: 1,
    plugins: state.builderItems.length > 0 ? state.builderItems : hosted,
  };
  const builderJson = JSON.stringify(builderDoc, null, 2);

  view.innerHTML = `
    <div class="mp-header">
      <div>
        <h2>插件 .tar.gz 规范校验与标准 catalog.json 生成器</h2>
        <p>按宿主 v3.4.6 严格标准校验打包产物（根目录 plugin.json、plugin 0755 权限、严禁含 config.json 或 data/、解压 &lt;= 250MB），自动生成精确 sha256 与 permissions 索引</p>
      </div>
      <div class="mp-actions">
        <button type="button" class="mp-btn primary" id="btn-pick-tgz">📦 选择本地 .tar.gz 包校验</button>
      </div>
    </div>

    <div class="mp-card">
      <div class="mp-card-head">
        <div>
          <h3 class="mp-card-title">校验配置（自动填充 Release 下载 URL）</h3>
          <div class="mp-card-sub">可选填入准备发布的 GitHub Release 下载链接，校验通过后自动拼入生成的 catalog.json</div>
        </div>
      </div>
      <div class="mp-grid-2">
        <div class="mp-field">
          <label>GitHub Release 资产下载 URL（选填）</label>
          <input type="text" class="mp-input" id="builder-dl-url" placeholder="https://github.com/yourname/mediwash/releases/download/v3.1.3/mediwash-3.1.3-linux-amd64.tar.gz" />
        </div>
        <div class="mp-field">
          <label>或输入容器可访问的本地 .tar.gz 路径校验（选填）</label>
          <div style="display:flex;gap:8px">
            <input type="text" class="mp-input" id="builder-local-path" placeholder="/path/to/plugin-1.0.0-linux-amd64.tar.gz" />
            <button type="button" class="mp-btn" id="btn-inspect-path">路径校验</button>
          </div>
        </div>
      </div>
      <div style="margin-top:10px">
        <label class="mp-check">
          <input type="checkbox" id="builder-host-check" />
          <span>校验通过后同时挂载至 MediPlug 本地 <code>/catalog.json</code> 聚合源</span>
        </label>
      </div>
    </div>

    ${ins ? `
      <div class="mp-card" style="border-color:${ins.valid_for_mediary_346 ? 'rgba(34,197,94,0.5)' : 'rgba(239,68,68,0.55)'}">
        <div class="mp-card-head">
          <div>
            <h3 class="mp-card-title">
              ${ins.valid_for_mediary_346 ? '✅ 通过 Mediary v3.4.6 安装器规范校验' : '❌ 未通过 Mediary v3.4.6 安装器规范校验'}
              — <code>${esc(ins.filename)}</code>
            </h3>
            <div class="mp-card-sub">
              架构: <b>${esc(ins.arch)}</b> · 压缩包大小 (size): <b>${ins.compressed_size}</b> (${fmtBytes(ins.compressed_size)}) · 解压体积: ${fmtBytes(ins.unpacked_size)}
            </div>
          </div>
        </div>
        <div class="mp-code-box" style="margin-bottom:10px">sha256: ${esc(ins.sha256)}</div>
        ${(ins.issues || []).length > 0 ? `
          <div style="color:var(--mp-danger);font-size:12.5px">
            ${ins.issues.map((x) => `<div>• ${esc(x)}</div>`).join('')}
          </div>
        ` : `
          <div style="font-size:12.5px;color:var(--mp-success)">
            ✓ 根目录包含 plugin.json (<code>${esc(ins.manifest?.id)}@${esc(ins.manifest?.version)}</code>) &nbsp;|&nbsp;
            ✓ 可执行文件 plugin (0755) &nbsp;|&nbsp;
            ✓ 无 config.json 或 data/ 污染 &nbsp;|&nbsp;
            ✓ 同步 ${ins.manifest?.requested_scopes?.length || 0} 项 requested_scopes 权限
          </div>
        `}
      </div>
    ` : ''}

    <div class="mp-card">
      <div class="mp-card-head">
        <div>
          <h3 class="mp-card-title">标准 catalog.json 索引生成结果（可累计添加多个插件如 medifind / mediwash / medinext / mediplug）</h3>
          <div class="mp-card-sub">当前已合并 ${builderDoc.plugins.length} 款插件（本地挂载条目: ${hosted.length} 款）</div>
        </div>
        <div class="mp-actions">
          <button type="button" class="mp-btn sm" id="btn-clear-builder">清空列表</button>
          <button type="button" class="mp-btn sm" id="btn-copy-builder">复制 catalog.json</button>
          <button type="button" class="mp-btn sm primary" id="btn-dl-builder">下载 catalog.json</button>
        </div>
      </div>
      <textarea class="mp-textarea" style="min-height:260px" readonly>${esc(builderJson)}</textarea>
    </div>
  `;

  const mergeBuilderEntry = (entry) => {
    if (!entry || !entry.id) return;
    const idx = state.builderItems.findIndex((x) => x.id === entry.id);
    if (idx >= 0) {
      state.builderItems[idx] = {
        ...entry,
        artifacts: { ...state.builderItems[idx].artifacts, ...entry.artifacts },
      };
    } else {
      state.builderItems.push(entry);
    }
  };

  view.querySelector('#btn-pick-tgz').onclick = () => {
    const fileInput = el('mp-hidden-archive');
    fileInput.value = '';
    fileInput.onchange = async () => {
      const file = fileInput.files?.[0];
      if (!file) return;
      const dlUrl = view.querySelector('#builder-dl-url').value.trim();
      const addHosted = view.querySelector('#builder-host-check').checked;
      const b64 = await new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(String(reader.result || ''));
        reader.onerror = () => reject(new Error('读取本地文件失败'));
        reader.readAsDataURL(file);
      });
      const res = await runAction('ui_inspect_archive', {
        filename: file.name,
        file_base64: b64,
        download_url: dlUrl,
        add_to_hosted: addHosted,
      });
      state.lastInspection = res.inspection;
      if (res.inspection?.valid_for_mediary_346 && res.inspection.catalog_entry) {
        mergeBuilderEntry(res.inspection.catalog_entry);
        toast(`校验通过：已将 ${res.inspection.catalog_entry.id} 加入 catalog.json`, 'success');
      } else {
        toast('压缩包存在规范问题，请查看校验报告', 'error');
      }
      updateChrome();
      renderBuilderTab(view);
    };
    fileInput.click();
  };

  view.querySelector('#btn-inspect-path').onclick = async () => {
    const localPath = view.querySelector('#builder-local-path').value.trim();
    if (!localPath) {
      toast('请输入 .tar.gz 本地路径', 'error');
      return;
    }
    const dlUrl = view.querySelector('#builder-dl-url').value.trim();
    const addHosted = view.querySelector('#builder-host-check').checked;
    const fname = localPath.split('/').pop() || 'plugin-linux-amd64.tar.gz';
    const res = await runAction('ui_inspect_archive', {
      filename: fname,
      local_path: localPath,
      download_url: dlUrl,
      add_to_hosted: addHosted,
    });
    state.lastInspection = res.inspection;
    if (res.inspection?.valid_for_mediary_346 && res.inspection.catalog_entry) {
      mergeBuilderEntry(res.inspection.catalog_entry);
      toast(`校验通过：已加入 ${res.inspection.catalog_entry.id}`, 'success');
    }
    updateChrome();
    renderBuilderTab(view);
  };

  view.querySelector('#btn-clear-builder').onclick = async () => {
    state.builderItems = [];
    state.lastInspection = null;
    if ((state.data?.hosted_entries || []).length > 0) {
      await runAction('ui_delete_hosted_entry', { clear_all: true });
    } else {
      updateChrome();
      renderBuilderTab(view);
    }
  };
  view.querySelector('#btn-copy-builder').onclick = async () => {
    await copyToClipboard(builderJson);
    toast('已复制标准 catalog.json 到剪贴板', 'success');
  };
  view.querySelector('#btn-dl-builder').onclick = () => {
    downloadTextFile('catalog.json', builderJson);
  };
}

// ==================== 5. Global Settings, Backup & Activity Logs ====================
function renderSettingsTab(view) {
  const d = state.data;
  const eng = d.engine || {};
  const logs = (d.logs || []).filter((l) => {
    const q = state.logQuery.trim().toLowerCase();
    if (!q) return true;
    return `${l.title} ${l.source} ${l.version} ${l.action} ${l.detail}`.toLowerCase().includes(q);
  });

  view.innerHTML = `
    <div class="mp-header">
      <div>
        <h2>全局设置、网络加速与操作记录</h2>
        <p>配置宿主管理员 API Token、本地 Catalog HTTP 监听端口、GitHub 访问令牌与下载镜像加速</p>
      </div>
      <div class="mp-actions">
        <button type="button" class="mp-btn" id="btn-test-notify">🔔 发送测试通知</button>
        <button type="button" class="mp-btn" id="btn-export-cfg">导出配置备份</button>
        <button type="button" class="mp-btn" id="btn-import-cfg">导入配置</button>
      </div>
    </div>

    <div class="mp-card">
      <div class="mp-card-head">
        <div>
          <h3 class="mp-card-title">鉴权、本地桥接服务与 GitHub 网络设置</h3>
          <div class="mp-card-sub">敏感令牌已脱敏展示，留空或保持 <code>******</code> 保存不会覆盖已有密钥</div>
        </div>
      </div>

      <div class="mp-grid-2">
        <div class="mp-field">
          <label>Mediary 管理员 API Token（设置 → 安全 查看）</label>
          <input type="password" class="mp-input" id="eng-admin-token" value="${esc(eng.admin_api_token || '')}" placeholder="用于读写 /api/plugin-store/sources" />
        </div>
        <div class="mp-field">
          <label>GitHub Personal Access Token（选填，提升至 5000 次/小时）</label>
          <input type="password" class="mp-input" id="eng-gh-token" value="${esc(eng.github_token || '')}" placeholder="ghp_...（防止 GitHub API 60 次/h 匿名限流）" />
        </div>
        <div class="mp-field">
          <label>本地 Catalog HTTP 监听地址</label>
          <select class="mp-select" id="eng-host">
            <option value="127.0.0.1" ${eng.listen_host !== '0.0.0.0' ? 'selected' : ''}>127.0.0.1（本机回环推荐，最安全）</option>
            <option value="0.0.0.0" ${eng.listen_host === '0.0.0.0' ? 'selected' : ''}>0.0.0.0（允许跨容器 / 局域网拉取）</option>
          </select>
        </div>
        <div class="mp-field">
          <label>本地 Catalog HTTP 监听端口（默认 18119）</label>
          <input type="number" class="mp-input" id="eng-port" value="${esc(eng.listen_port || 18119)}" min="1024" max="65535" />
        </div>
        <div class="mp-field">
          <label>统一桥接源 ID / 显示名称</label>
          <div style="display:flex;gap:8px">
            <input type="text" class="mp-input" id="eng-src-id" value="${esc(eng.unified_source_id || 'mediplug-github-bridge')}" placeholder="mediplug-github-bridge" />
            <input type="text" class="mp-input" id="eng-src-name" value="${esc(eng.unified_source_name || 'MediPlug GitHub Release 聚合源')}" placeholder="显示名称" />
          </div>
        </div>
        <div class="mp-field">
          <label>自定义外部访问前缀（选填，默认自动 http://127.0.0.1:&lt;port&gt;）</label>
          <input type="text" class="mp-input" id="eng-pub-url" value="${esc(eng.public_base_url || '')}" placeholder="留空默认使用本机回环地址" />
        </div>
        <div class="mp-field">
          <label>HTTP / SOCKS5 代理（选填，留空自动复用宿主代理设置）</label>
          <input type="text" class="mp-input" id="eng-proxy" value="${esc(eng.proxy_url || '')}" placeholder="例如 http://127.0.0.1:7890" />
        </div>
        <div class="mp-field">
          <label>GitHub Release 下载镜像加速前缀（选填）</label>
          <input type="text" class="mp-input" id="eng-mirror" value="${esc(eng.github_mirror || '')}" placeholder="例如 https://ghfast.top（不影响原包 sha256 校验）" />
        </div>
      </div>

      <div style="display:flex;justify-content:space-between;align-items:center;flex-wrap:wrap;gap:12px;margin-top:14px">
        <label class="mp-check">
          <input type="checkbox" id="eng-deep" ${eng.deep_inspect_archive !== false ? 'checked' : ''} />
          <span>深度流式核验 Release 压缩包内 <code>plugin.json</code> 权限与真实 <code>sha256</code>（强烈推荐开启）</span>
        </label>
        <button type="button" class="mp-btn primary" id="btn-save-eng">保存全局设置</button>
      </div>
    </div>

    <div class="mp-card">
      <div class="mp-card-head">
        <div>
          <h3 class="mp-card-title">同步与操作历史记录 (${logs.length})</h3>
        </div>
        <div class="mp-actions">
          <input type="text" class="mp-input" id="log-search" style="width:200px" value="${esc(state.logQuery)}" placeholder="过滤日志..." />
          <button type="button" class="mp-btn sm" id="btn-clear-logs">清空日志</button>
          <button type="button" class="mp-btn sm danger" id="btn-clear-cache">清空日志与校验缓存</button>
        </div>
      </div>

      <div class="mp-table-wrap">
        <table class="mp-table">
          <thead>
            <tr>
              <th>目标 / 仓库</th>
              <th>分类</th>
              <th>版本 / 标识</th>
              <th>状态</th>
              <th>详情说明</th>
              <th>时间</th>
            </tr>
          </thead>
          <tbody>
            ${logs.length === 0 ? `
              <tr><td colspan="6" style="text-align:center;padding:20px;color:var(--mp-muted)">暂无记录</td></tr>
            ` : logs.map((l) => `
              <tr>
                <td style="font-weight:600">${esc(l.title)}</td>
                <td>${esc(l.source)}</td>
                <td><code>${esc(l.version)}</code></td>
                <td>
                  <span class="mp-tag ${l.status === 'success' ? 'ok' : 'danger'}">${esc(l.status)}</span>
                </td>
                <td style="max-width:360px;word-break:break-all">${esc(l.detail)}</td>
                <td style="white-space:nowrap;color:var(--mp-muted)">${esc(l.updated_at)}</td>
              </tr>
            `).join('')}
          </tbody>
        </table>
      </div>
    </div>
  `;

  view.querySelector('#btn-save-eng').onclick = () => {
    const next = {
      admin_api_token: view.querySelector('#eng-admin-token').value.trim(),
      github_token: view.querySelector('#eng-gh-token').value.trim(),
      listen_host: view.querySelector('#eng-host').value,
      listen_port: Number(view.querySelector('#eng-port').value || 18119),
      unified_source_id: view.querySelector('#eng-src-id').value.trim(),
      unified_source_name: view.querySelector('#eng-src-name').value.trim(),
      public_base_url: view.querySelector('#eng-pub-url').value.trim(),
      proxy_url: view.querySelector('#eng-proxy').value.trim(),
      github_mirror: view.querySelector('#eng-mirror').value.trim(),
      deep_inspect_archive: view.querySelector('#eng-deep').checked,
    };
    runAction('ui_save_engine', { engine: next });
  };

  view.querySelector('#btn-test-notify').onclick = () => runAction('ui_test_notify', {});
  view.querySelector('#btn-export-cfg').onclick = async () => {
    const res = await bridge.action('ui_export_config', { include_secrets: false });
    downloadTextFile(res.filename, res.content);
    toast('已导出脱敏配置备份 JSON', 'success');
  };
  view.querySelector('#btn-import-cfg').onclick = () => {
    const inp = el('mp-hidden-config');
    inp.value = '';
    inp.onchange = async () => {
      const f = inp.files?.[0];
      if (!f) return;
      const content = await f.text();
      await runAction('ui_import_config', { content });
    };
    inp.click();
  };

  view.querySelector('#log-search').oninput = (e) => {
    state.logQuery = e.target.value;
    renderSettingsTab(view);
    const input = view.querySelector('#log-search');
    input.focus();
    input.setSelectionRange(input.value.length, input.value.length);
  };
  view.querySelector('#btn-clear-logs').onclick = () => runAction('ui_clear_logs', { clear_cache: false });
  view.querySelector('#btn-clear-cache').onclick = () => runAction('ui_clear_logs', { clear_cache: true });
}

// ==================== Init ====================
function initNav() {
  el('mp-nav').addEventListener('click', (e) => {
    const btn = e.target.closest('[data-tab]');
    if (!btn) return;
    state.tab = btn.dataset.tab;
    el('mp-nav').querySelectorAll('.mp-nav-btn').forEach((b) => {
      b.classList.toggle('active', b.dataset.tab === state.tab);
    });
    render();
  });
  el('mp-modal-close').onclick = closeModal;
  el('mp-modal-backdrop').addEventListener('click', (e) => {
    if (e.target === el('mp-modal-backdrop')) closeModal();
  });
}

async function init() {
  initNav();
  await bridge.getTheme();
  await runAction('ui_bootstrap', {});
}

init();

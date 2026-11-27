// Mediary v3.3.2+ Sandbox iframe <-> Host postMessage Bridge
function normalizeColorToken(val) {
  if (!val || typeof val !== 'string') return '';
  const trimmed = val.trim();
  if (!trimmed) return '';
  if (/^\d+(?:\.\d+)?\s+\d+(?:\.\d+)?%\s+\d+(?:\.\d+)?%(?:\s*\/\s*[\d.%]+)?$/.test(trimmed)) {
    return `hsl(${trimmed})`;
  }
  return trimmed;
}

export function applyHostTheme(themePayload) {
  if (!themePayload || typeof themePayload !== 'object') return;
  const root = document.documentElement;
  if (themePayload.theme === 'light' || themePayload.theme === 'dark') {
    root.dataset.theme = themePayload.theme;
  }
  const tokens = themePayload.tokens || {};
  const map = {
    '--bg-main': '--mp-bg',
    '--bg-sidebar': '--mp-sidebar-bg',
    '--bg-card': '--mp-surface',
    '--bg-input': '--mp-input-bg',
    '--bg-card-hover': '--mp-hover',
    '--text-main': '--mp-text',
    '--text-muted': '--mp-muted',
    '--primary': '--mp-primary',
    '--danger': '--mp-danger',
    '--warning': '--mp-warn',
    '--success': '--mp-success',
  };
  for (const [hostKey, localKey] of Object.entries(map)) {
    const color = normalizeColorToken(tokens[hostKey]);
    if (color) root.style.setProperty(localKey, color);
  }
  const borderColor = normalizeColorToken(tokens['--border-color'] || tokens['--border']);
  if (borderColor && !borderColor.includes('var(')) {
    root.style.setProperty('--mp-border', borderColor);
  }
}

export function createMediaryBridge() {
  const pending = new Map();
  let seq = 0;
  const listeners = new Set();

  if (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches) {
    if (!document.documentElement.dataset.theme) {
      document.documentElement.dataset.theme = 'light';
    }
  } else if (!document.documentElement.dataset.theme) {
    document.documentElement.dataset.theme = 'dark';
  }

  window.addEventListener('message', (e) => {
    const m = e.data;
    if (!m || m.source !== 'mediary-host') return;
    if (m.type === 'init' || m.type === 'theme') {
      applyHostTheme(m);
      listeners.forEach((fn) => fn(m));
    } else if (m.type === 'response' && pending.has(m.request_id)) {
      const { resolve, reject } = pending.get(m.request_id);
      pending.delete(m.request_id);
      if (m.ok) resolve(m.data);
      else reject(new Error(m.error || '宿主桥接调用失败'));
    }
  });

  const call = (method, payload = {}, action) =>
    new Promise((resolve, reject) => {
      const request_id = `mp_${++seq}_${Date.now()}`;
      const timer = setTimeout(() => {
        if (pending.has(request_id)) {
          pending.delete(request_id);
          reject(new Error(`请求超时 (${action || method})`));
        }
      }, 95000);
      pending.set(request_id, {
        resolve: (d) => { clearTimeout(timer); resolve(d); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
      const msg = { source: 'mediary-plugin-ui', request_id, method, payload };
      if (action) msg.action = action;
      window.parent.postMessage(msg, '*');
    });

  return {
    call,
    action: (name, payload = {}) => call('action', payload, name),
    getConfig: () => call('get_config', {}),
    saveConfig: (settings) => call('save_config', { settings }),
    getTheme: async () => {
      try {
        const res = await call('get_theme', {});
        applyHostTheme(res);
        return res;
      } catch (_) {
        return null;
      }
    },
    notify: (message) => call('notify', { message }),
    close: () => call('close', {}),
    onHostEvent: (fn) => { listeners.add(fn); return () => listeners.delete(fn); },
  };
}

export function downloadTextFile(filename, content, mime = 'application/json;charset=utf-8') {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1500);
}

export async function copyToClipboard(text) {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (_) {
      // Fallback below
    }
  }
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  const ok = document.execCommand('copy');
  ta.remove();
  return ok;
}

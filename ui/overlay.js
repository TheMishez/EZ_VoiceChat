'use strict';
// In-game overlay: who's talking in your channel. Click-through; the app positions the window.
const T = window.__TAURI__;
const invoke = (c, a) => T.core.invoke(c, a);
const listen = (e, cb) => T.event.listen(e, (x) => cb(x.payload));
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const initials = (n) => (String(n || '?').match(/[A-Za-z0-9]/g) || ['?']).slice(0, 2).join('').toUpperCase();
const MIC_OFF = '<svg viewBox="0 0 24 24"><path d="M3 3l18 18M9 9v2a3 3 0 0 0 5.1 2.1M15 9.3V6a3 3 0 0 0-5.9-.8M5 11a7 7 0 0 0 11.5 5.4M12 18v3"/></svg>';

let cfg = { corner: 'tl', show: 'talking', scale: 1, opacity: 0.9 };
let view = null;
let talking = new Set();
let meTalking = false;
let self = { mute: false, deaf: false };
const pics = {};
const wanted = new Set();
let picTimer = null;
function pic(u) {
  if (!u.avatar) return '';
  if (pics[u.avatar]) return `<img src="${pics[u.avatar]}" alt="">`;
  if (!wanted.has(u.avatar)) {
    wanted.add(u.avatar);
    clearTimeout(picTimer);
    picTimer = setTimeout(async () => {
      Object.assign(pics, await invoke('vc_avatars', { hashes: [...wanted] }));
      wanted.clear();
      render();
    }, 50);
  }
  return '';
}

function render() {
  const list = document.getElementById('list');
  list.className = cfg.corner;
  list.style.transform = `scale(${cfg.scale})`;
  list.style.opacity = cfg.opacity;
  const rows = [];
  if (self.deaf || self.mute) rows.push(`<div class="r warn">${MIC_OFF}<span>${self.deaf ? 'Deafened' : 'Mic muted'}</span></div>`);
  if (view && view.me != null && view.users[view.me]) {
    const ch = view.users[view.me].channel;
    const people = Object.values(view.users)
      .filter((u) => u.channel === ch)
      .map((u) => ({ u, me: u.id === view.me, on: u.id === view.me ? meTalking : talking.has(u.id) }))
      .filter((p) => cfg.show === 'channel' || p.on)
      .sort((a, b) => (b.on - a.on) || a.u.name.localeCompare(b.u.name));
    for (const p of people) {
      const muted = p.u.self_mute || p.u.muted || p.u.self_deaf || p.u.deafened;
      rows.push(`<div class="r ${p.on ? 'talking' : 'idle'} ${p.me ? 'me' : ''}"><span class="av">${pic(p.u) || esc(initials(p.u.name))}</span><span class="n">${esc(p.u.name)}</span>${muted ? MIC_OFF : ''}</div>`);
    }
  }
  list.innerHTML = rows.join('');
}

(async () => {
  cfg = { ...cfg, ...(await invoke('ov_get')) };
  const s = await invoke('vc_state');
  view = s.view;
  self = s.self;
  render();
  listen('ov-config', (c) => { cfg = { ...cfg, ...c }; render(); });
  listen('vc-state', (v) => { view = v; render(); });
  listen('vc-self', (x) => { self = x; render(); });
  listen('vc-talking', (t) => { talking = new Set(t.ids); meTalking = t.me; render(); });
})();

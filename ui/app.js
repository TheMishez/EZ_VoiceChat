'use strict';
/* EZ VoiceChat page: shows the server, channels, who's talking and chat. All voice and
   networking runs in the Rust backend; this page only displays it and sends commands. */
const T = window.__TAURI__;
const invoke = (c, a) => T.core.invoke(c, a);
const listen = (e, cb) => T.event.listen(e, (x) => cb(x.payload));

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const uid = () => Math.random().toString(36).slice(2, 10);

const I = {
  plus: '<path d="M12 5v14M5 12h14"/>',
  mic: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>',
  micOff: '<path d="M3 3l18 18"/><path d="M9 9v2a3 3 0 0 0 5.1 2.1M15 9.3V6a3 3 0 0 0-5.9-.8"/><path d="M5 11a7 7 0 0 0 11.5 5.4M19 11a7 7 0 0 1-.6 2.8M12 18v3"/>',
  head: '<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="4" height="6" rx="1.5"/><rect x="17" y="14" width="4" height="6" rx="1.5"/>',
  headOff: '<path d="M3 3l18 18"/><path d="M4 15v-3a8 8 0 0 1 12.3-6.7M19.6 9.4A8 8 0 0 1 20 12v3"/><rect x="3" y="14" width="4" height="6" rx="1.5"/><rect x="17" y="14" width="4" height="6" rx="1.5"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
  hash: '<path d="M5 9h14M5 15h14M10 3L8 21M16 3l-2 18"/>',
  lock: '<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>'
};
const icon = (n) => `<svg class="i" viewBox="0 0 24 24">${I[n] || ''}</svg>`;

let view = null;
let me = { mute: false, deaf: false };
let talking = new Set();
let meTalking = false;
let bookmarks = [];
let profile = { nickname: '' };
let activeBm = null;

/* ---------------- toasts / modal / menu ---------------- */
function toast(msg, kind = '') {
  const t = document.createElement('div');
  t.className = 'toast ' + kind;
  t.textContent = msg;
  $('#toasts').appendChild(t);
  setTimeout(() => t.remove(), 3500);
}
function modal(html, { wide = false, onClose } = {}) {
  const back = document.createElement('div');
  back.className = 'back';
  back.innerHTML = `<div class="modal ${wide ? 'wide' : ''}">${html}</div>`;
  $('#modal-root').appendChild(back);
  const close = () => { back.remove(); document.removeEventListener('keydown', onKey, true); onClose && onClose(); };
  const onKey = (e) => { if (e.key === 'Escape' && !recording && back === $('#modal-root').lastElementChild) close(); };
  document.addEventListener('keydown', onKey, true);
  back.addEventListener('mousedown', (e) => { if (e.target === back) close(); });
  $$('[data-close]', back).forEach((b) => (b.onclick = close));
  return { el: back.firstElementChild, close };
}
function menu(x, y, html, bind) {
  const m = $('#menu');
  m.innerHTML = html;
  m.hidden = false;
  const r = m.getBoundingClientRect();
  m.style.left = Math.min(x, innerWidth - r.width - 8) + 'px';
  m.style.top = Math.min(y, innerHeight - r.height - 8) + 'px';
  bind(m);
}
document.addEventListener('mousedown', (e) => { if (!e.target.closest('#menu')) $('#menu').hidden = true; });

/* ---------------- bookmarks ---------------- */
async function loadStore() {
  bookmarks = (await invoke('vc_store_get', { name: 'bookmarks' })) || [];
  profile = (await invoke('vc_store_get', { name: 'profile' })) || { nickname: '' };
  if (!Array.isArray(bookmarks)) bookmarks = [];
}
const saveBookmarks = () => invoke('vc_store_set', { name: 'bookmarks', data: bookmarks });

function renderBookmarks() {
  const host = $('#bookmarks');
  host.innerHTML = bookmarks.map((b) => `
    <div class="bm ${activeBm === b.id ? 'on' : ''}" data-id="${b.id}">
      <span class="ico ${b.kind}">${b.kind === 'ts3' ? 'TS3' : 'MUM'}</span>
      <div><b>${esc(b.name || b.host)}</b><small>${esc(b.host)}${b.port ? ':' + b.port : ''}</small></div>
    </div>`).join('') || '<div class="empty-note">No servers yet. Click + to add one.</div>';
}

function editServer(bm) {
  const b = bm ? { ...bm } : { id: uid(), name: '', kind: 'mumble', host: '', port: '', nickname: profile.nickname || '', password: '' };
  const m = modal(`
    <h3>${bm ? 'Edit server' : 'Add a server'}</h3>
    <div class="field"><label>Server type</label>
      <div class="seg" id="f-kind"><button data-k="mumble">Mumble</button><button data-k="ts3">TeamSpeak 3</button></div></div>
    <div class="field"><label>Name (optional)</label><input class="input" id="f-name" value="${esc(b.name)}" placeholder="My team"></div>
    <div class="two">
      <div class="field"><label>Address</label><input class="input" id="f-host" value="${esc(b.host)}" placeholder="voice.example.com"></div>
      <div class="field"><label>Port</label><input class="input" id="f-port" value="${esc(b.port)}" placeholder=""></div>
    </div>
    <div class="field"><label>Nickname</label><input class="input" id="f-nick" value="${esc(b.nickname)}" maxlength="64"></div>
    <div class="field"><label>Server password (if it has one)</label><input class="input" id="f-pass" type="password" value="${esc(b.password)}"></div>
    <p class="small" id="f-note"></p>
    <div class="actions">
      ${bm ? '<button class="btn danger ghost" id="f-del">Delete</button><div class="flex"></div>' : ''}
      <button class="btn ghost" data-close>Cancel</button>
      <button class="btn" id="f-save">Save</button>
      <button class="btn primary" id="f-go">Save &amp; connect</button>
    </div>`);
  const kindSeg = $('#f-kind', m.el);
  const setKind = (k) => {
    b.kind = k;
    $$('button', kindSeg).forEach((x) => x.classList.toggle('on', x.dataset.k === k));
    $('#f-port', m.el).placeholder = k === 'ts3' ? '9987' : '64738';
    $('#f-note', m.el).textContent = k === 'ts3'
      ? 'TeamSpeak 3 support uses an open-source, unofficial implementation of the TeamSpeak protocol. TeamSpeak 6 servers are not supported.'
      : 'Works with any Mumble server, including your own EZ server.';
  };
  kindSeg.onclick = (e) => { const x = e.target.closest('[data-k]'); if (x) setKind(x.dataset.k); };
  setKind(b.kind);
  const read = () => {
    b.name = $('#f-name', m.el).value.trim();
    b.host = $('#f-host', m.el).value.trim();
    b.port = $('#f-port', m.el).value.trim().replace(/\D/g, '');
    b.nickname = $('#f-nick', m.el).value.trim();
    b.password = $('#f-pass', m.el).value;
    // "host:port" typed in one box
    const hp = /^\[?([^\]]+?)\]?:(\d{2,5})$/.exec(b.host);
    if (hp && !b.port) { b.host = hp[1]; b.port = hp[2]; }
    if (!b.host) { toast('Enter the server address', 'err'); return false; }
    if (!b.nickname) { toast('Pick a nickname', 'err'); return false; }
    return true;
  };
  const store = () => {
    const i = bookmarks.findIndex((x) => x.id === b.id);
    if (i >= 0) bookmarks[i] = b; else bookmarks.push(b);
    profile.nickname = b.nickname;
    invoke('vc_store_set', { name: 'profile', data: profile });
    saveBookmarks();
    renderBookmarks();
  };
  $('#f-save', m.el).onclick = () => { if (read()) { store(); m.close(); } };
  $('#f-go', m.el).onclick = () => { if (read()) { store(); m.close(); connect(b); } };
  const del = $('#f-del', m.el);
  if (del) del.onclick = () => { bookmarks = bookmarks.filter((x) => x.id !== b.id); saveBookmarks(); renderBookmarks(); m.close(); };
  $('#f-host', m.el).focus();
}

let lastConnect = null;
async function connect(b, trust) {
  activeBm = b.id;
  lastConnect = b;
  renderBookmarks();
  $('#messages').innerHTML = '';
  try {
    await invoke('vc_connect', { kind: b.kind, host: b.host, port: b.port ? Number(b.port) : null, nickname: b.nickname, password: b.password || null, trust: trust || null });
  } catch (e) {
    toast(String(e), 'err');
  }
}

/* ---------------- server view ---------------- */
function initials(n) { return (String(n || '?').match(/[A-Za-z0-9]/g) || ['?']).slice(0, 2).join('').toUpperCase(); }

function renderView() {
  const on = !!view;
  $('#server-head').hidden = !on;
  $('#tree').hidden = !on;
  $('#welcome').hidden = on;
  $('#chat-input').disabled = !on;
  if (!on) {
    $('#me-name').textContent = profile.nickname || 'Not connected';
    $('#me-sub').textContent = 'Not connected';
    $('#me-avatar').textContent = initials(profile.nickname);
    $('#chat-where').textContent = '';
    renderWelcome();
    return;
  }
  $('#server-name').textContent = view.server_name || view.host;
  const ping = view.ping_ms != null ? `${Math.round(view.ping_ms)} ms` : '…';
  const users = Object.keys(view.users).length;
  $('#server-meta').innerHTML = [
    `<span class="tag">${view.kind === 'ts3' ? 'TeamSpeak 3' : 'Mumble'}</span>`,
    `<span class="tag">${esc(view.host)}</span>`,
    `<span class="tag ${view.ping_ms != null && view.ping_ms < 60 ? 'good' : ''}">ping ${ping}</span>`,
    view.kind === 'mumble' ? `<span class="tag ${view.udp ? 'good' : 'warn'}">${view.udp ? 'UDP voice' : 'TCP voice (UDP blocked?)'}</span>` : '',
    `<span class="tag">${users} online</span>`,
    `<span class="tag" id="lat-tag"></span>`
  ].join('');
  paintLatency();
  const mine = view.users[view.me];
  $('#me-name').textContent = mine ? mine.name : profile.nickname;
  $('#me-avatar').textContent = initials(mine ? mine.name : profile.nickname);
  const myChan = mine ? view.channels[mine.channel] : null;
  $('#me-sub').textContent = myChan ? `In ${myChan.name || 'Root'}` : 'Connected';
  $('#chat-where').textContent = myChan ? '#' + (myChan.name || 'Root') : '';
  renderTree();
}

function renderTree() {
  const chans = Object.values(view.channels);
  const users = Object.values(view.users);
  const kids = new Map();
  for (const c of chans) {
    const p = c.parent == null ? 'root' : c.parent;
    if (!kids.has(p)) kids.set(p, []);
    kids.get(p).push(c);
  }
  for (const list of kids.values()) list.sort((a, b) => a.position - b.position || a.name.localeCompare(b.name));
  const byChan = new Map();
  for (const u of users) {
    if (!byChan.has(u.channel)) byChan.set(u.channel, []);
    byChan.get(u.channel).push(u);
  }
  for (const list of byChan.values()) list.sort((a, b) => a.name.localeCompare(b.name));
  const mineCh = view.users[view.me] ? view.users[view.me].channel : null;
  const userHtml = (u) => `
    <div class="user ${u.id === view.me ? 'me' : ''} ${talking.has(u.id) || (u.id === view.me && meTalking) ? 'talking' : ''}" data-user="${u.id}">
      <span class="dot">${esc(initials(u.name))}</span><span class="name">${esc(u.name)}</span>
      <span class="st">${u.self_mute || u.muted ? icon('micOff') : ''}${u.self_deaf || u.deafened ? icon('headOff') : ''}</span>
    </div>`;
  const chanHtml = (c, depth) => {
    const children = kids.get(c.id) || [];
    const us = byChan.get(c.id) || [];
    return `
      <div class="ch ${c.id === mineCh ? 'mine' : ''}" data-chan="${c.id}" title="${esc(c.description || '')}">${icon('hash')}<span>${esc(c.name || view.server_name || 'Root')}</span>${c.id !== mineCh ? '<span class="hint">double-click to join</span>' : ''}</div>
      ${us.length || children.length ? `<div class="kids">${us.map(userHtml).join('')}${children.map((k) => chanHtml(k, depth + 1)).join('')}</div>` : ''}`;
  };
  $('#tree').innerHTML = (kids.get('root') || []).map((c) => chanHtml(c, 0)).join('');
}

function paintTalking() {
  $$('#tree .user').forEach((el) => {
    const id = Number(el.dataset.user);
    el.classList.toggle('talking', talking.has(id) || (view && id === view.me && meTalking));
  });
  $('#me-avatar').classList.toggle('talking', meTalking);
}

function renderWelcome() {
  $('#welcome').innerHTML = `
    <div class="card">
      <svg class="big-mark" viewBox="24 58 222 124"><g fill="currentColor" transform="translate(120 120) skewX(-11) translate(-120 -120)"><rect x="40" y="65" width="22" height="110"/><rect x="40" y="65" width="70" height="22"/><rect x="40" y="109" width="54" height="22"/><rect x="40" y="153" width="70" height="22"/><rect x="122" y="65" width="78" height="22"/><rect x="122" y="153" width="78" height="22"/></g></svg>
      <h2>Voice chat with almost no delay</h2>
      <p>Join any <b>Mumble</b> or <b>TeamSpeak 3</b> server. Opus voice, noise suppression, push-to-talk that never touches your game's input, and it keeps running in the tray.</p>
      <div class="row"><button class="btn primary" id="w-add">${icon('plus')}Add a server</button>
      <button class="btn ghost" id="w-host">Host your own EZ server</button></div>
    </div>`;
  $('#w-add').onclick = () => editServer(null);
  $('#w-host').onclick = () => invoke('app_open_external', { url: 'https://github.com/TheMishez/EZ_VoiceChat/tree/main/server' });
}

/* ---------------- voice delay readout ---------------- */
let lat = null;
// Mouth-to-ear estimate for your voice reaching a teammate with a similar connection: your
// device buffer + noise suppression + Opus frame + network (your ping covers you->server and
// server->them) + their jitter buffer + their output buffer. Device buffers, ping and the jitter
// buffer are measured live; the codec and noise suppression costs are fixed.
function paintLatency() {
  const el = $('#lat-tag');
  if (!el || !lat) return;
  const r = (x) => Math.round(x * 10) / 10;
  const ts = lat.kind === 'ts3';
  const jitter = lat.jitter != null ? lat.jitter : (ts ? 40 : 15);
  const ping = lat.ping != null ? lat.ping : 0;
  const parts = [
    ['Mic buffer', lat.input, lat.inputLow ? 'low-latency' : 'standard'],
    ['Noise suppression', lat.ns, ''],
    ['Opus frame + lookahead', lat.frame + lat.codec, ''],
    ['Network (ping)', ping, lat.ping == null ? 'not measured yet' : ''],
    ['Jitter buffer', jitter, ts ? 'TS3 library buffer, estimated' : 'adaptive'],
    ['Output buffer', lat.output, lat.outputLow ? 'low-latency' : 'standard']
  ];
  const total = parts.reduce((a, p) => a + p[1], 0);
  el.textContent = `voice delay ≈ ${Math.round(total)} ms`;
  el.className = 'tag ' + (total < 70 ? 'good' : total < 120 ? '' : 'warn');
  el.title = parts.map((p) => `${p[0]}: ${r(p[1])} ms${p[2] ? ' (' + p[2] + ')' : ''}`).join('\n') + '\n\nEstimate for a teammate with a connection like yours.';
}
setInterval(async () => {
  if (!view) return;
  try { lat = await invoke('vc_latency'); paintLatency(); } catch (_) {}
}, 1000);

/* ---------------- status / chat ---------------- */
function setStatus(st) {
  const pill = $('#status');
  const phase = st.phase || 'idle';
  pill.className = 'pill ' + phase;
  const text = st.message || { idle: 'Not connected', connected: 'Connected', disconnected: 'Disconnected' }[phase] || phase;
  $('span', pill).textContent = text;
  pill.title = text;
  if (phase === 'error') toast(text, 'err');
}

function addChat(m) {
  const host = $('#messages');
  const el = document.createElement('div');
  el.className = 'msg ' + m.kind;
  const t = new Date(m.time).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  el.innerHTML = (m.from ? `<b>${esc(m.from)}</b>` : '') + `<span></span><time>${t}</time>`;
  el.querySelector('span').textContent = m.text;
  host.appendChild(el);
  while (host.children.length > 400) host.firstChild.remove();
  host.scrollTop = host.scrollHeight;
}

/* ---------------- self mute / deafen ---------------- */
function renderSelf() {
  const bm = $('#btn-mute');
  const bd = $('#btn-deaf');
  bm.innerHTML = icon(me.mute ? 'micOff' : 'mic');
  bd.innerHTML = icon(me.deaf ? 'headOff' : 'head');
  bm.classList.toggle('off', me.mute);
  bd.classList.toggle('off', me.deaf);
  bm.title = me.mute ? 'Unmute microphone' : 'Mute microphone';
  bd.title = me.deaf ? 'Undeafen' : 'Deafen';
}

/* ---------------- settings ---------------- */
let recording = false;
async function openSettings() {
  const [st, dev] = await Promise.all([invoke('vc_get_settings'), invoke('vc_devices')]);
  const opt = (list, cur, def) => `<option value="">System default${def ? ' (' + esc(def) + ')' : ''}</option>` + list.map((d) => `<option ${d === cur ? 'selected' : ''}>${esc(d)}</option>`).join('');
  const keyLabel = (k) => !k ? 'Click to bind' : k.replace(/^Key/, '').replace(/^Digit/, '').replace(/^Mouse(\d)$/, 'Mouse $1');
  const m = modal(`
    <h3>Audio settings</h3>
    <div class="section">Devices</div>
    <div class="field"><label>Microphone</label><select class="select" id="s-in">${opt(dev.inputs, st.input_device, dev.defaultInput)}</select></div>
    <div class="field"><label>Speakers / headphones</label><select class="select" id="s-out">${opt(dev.outputs, st.output_device, dev.defaultOutput)}</select></div>
    <div class="section">Transmit</div>
    <div class="seg" id="s-mode"><button data-m="vad">Voice activation</button><button data-m="ptt">Push-to-talk</button><button data-m="continuous">Always on</button></div>
    <div id="s-vad" class="field" style="margin-top:12px"><label>Sensitivity <span class="small" id="s-thr-v"></span></label>
      <div class="meter" id="s-meter"><i></i><b></b></div>
      <input type="range" id="s-thr" min="-70" max="-10" step="1" value="${st.vad_threshold_db}">
      <span class="small">Speak normally: the bar should pass the white line. Background noise should stay below it.</span></div>
    <div id="s-ptt" class="field" style="margin-top:12px"><label>Push-to-talk key</label>
      <div class="row"><button class="key-btn" id="s-key">${esc(keyLabel(st.ptt_key))}</button>
      <span class="small">Release delay</span><input class="input" style="width:90px" id="s-rel" type="number" min="0" max="1000" step="10" value="${st.ptt_release_ms}"><span class="small">ms</span></div>
      <span class="small">Works in every game without adding input lag: the key still reaches the game. Mouse 3–5 work too.</span></div>
    <div class="section">Processing</div>
    <label class="switch"><input type="checkbox" id="s-ns" ${st.noise_suppression ? 'checked' : ''}><span></span>Noise suppression (RNNoise)</label>
    <div class="field"><label>Microphone volume <span class="small" id="s-gain-v"></span></label><input type="range" id="s-gain" min="0" max="3" step="0.05" value="${st.input_gain}"></div>
    <div class="field"><label>Output volume <span class="small" id="s-vol-v"></span></label><input type="range" id="s-vol" min="0" max="2" step="0.05" value="${st.output_volume}"></div>
    <div class="section">Quality</div>
    <label class="switch"><input type="checkbox" id="s-ll" ${st.low_latency ? 'checked' : ''}><span></span>Low-latency audio mode</label>
    <p class="small" id="s-ll-info" style="margin:2px 0 12px">Uses the smallest audio buffers Windows allows. Turn off if you hear crackling.</p>
    <div class="field"><label>Latency (Opus frame size)</label><div class="seg" id="s-frame"><button data-f="10">10 ms (lowest delay)</button><button data-f="20">20 ms (more robust)</button></div></div>
    <div class="field"><label>Voice quality (bitrate)</label><select class="select" id="s-br">${[32000, 48000, 64000, 96000, 128000].map((b) => `<option value="${b}" ${b === st.bitrate ? 'selected' : ''}>${b / 1000} kbps${b === 64000 ? ' (recommended)' : ''}</option>`).join('')}</select></div>
    <label class="switch"><input type="checkbox" id="s-test"><span></span>Mic test (hear yourself)</label>
    <div class="section">Local test server</div>
    <div id="srv"></div>
    <div class="actions"><button class="btn primary" data-close>Done</button></div>`, { wide: true, onClose: () => { clearInterval(meterTimer); invoke('vc_mic_test', { on: false }); } });
  const el = m.el;
  const s = { ...st };
  const save = () => invoke('vc_set_settings', { settings: s });
  const setMode = (v) => {
    s.mode = v;
    $$('#s-mode button', el).forEach((b) => b.classList.toggle('on', b.dataset.m === v));
    $('#s-vad', el).hidden = v !== 'vad';
    $('#s-ptt', el).hidden = v !== 'ptt';
  };
  setMode(s.mode);
  $('#s-mode', el).onclick = (e) => { const b = e.target.closest('[data-m]'); if (b) { setMode(b.dataset.m); save(); } };
  const setFrame = (v) => { s.frame_ms = v; $$('#s-frame button', el).forEach((b) => b.classList.toggle('on', Number(b.dataset.f) === v)); };
  setFrame(s.frame_ms);
  $('#s-frame', el).onclick = (e) => { const b = e.target.closest('[data-f]'); if (b) { setFrame(Number(b.dataset.f)); save(); } };
  $('#s-in', el).onchange = (e) => { s.input_device = e.target.value; save(); };
  $('#s-out', el).onchange = (e) => { s.output_device = e.target.value; save(); };
  const pct = (v) => Math.round(v * 100) + '%';
  const thrLine = () => { $('#s-meter b', el).style.left = ((s.vad_threshold_db + 70) / 70 * 100) + '%'; $('#s-thr-v', el).textContent = s.vad_threshold_db + ' dB'; };
  thrLine();
  $('#s-thr', el).oninput = (e) => { s.vad_threshold_db = Number(e.target.value); thrLine(); save(); };
  $('#s-gain-v', el).textContent = pct(s.input_gain);
  $('#s-vol-v', el).textContent = pct(s.output_volume);
  $('#s-gain', el).oninput = (e) => { s.input_gain = Number(e.target.value); $('#s-gain-v', el).textContent = pct(s.input_gain); save(); };
  $('#s-vol', el).oninput = (e) => { s.output_volume = Number(e.target.value); $('#s-vol-v', el).textContent = pct(s.output_volume); save(); };
  $('#s-ns', el).onchange = (e) => { s.noise_suppression = e.target.checked; save(); };
  const llInfo = async () => {
    const l = await invoke('vc_latency');
    const f = (x, low) => `${Math.round(x * 10) / 10} ms${low ? '' : ' (standard)'}`;
    $('#s-ll-info', el).textContent = `Mic buffer ${f(l.input, l.inputLow)} · Output buffer ${f(l.output, l.outputLow)}. Turn off if you hear crackling.`;
  };
  llInfo();
  $('#s-ll', el).onchange = async (e) => { s.low_latency = e.target.checked; await save(); setTimeout(llInfo, 800); };
  $('#s-br', el).onchange = (e) => { s.bitrate = Number(e.target.value); save(); toast('Applies the next time you connect'); };
  $('#s-rel', el).onchange = (e) => { s.ptt_release_ms = Math.max(0, Math.min(1000, Number(e.target.value) || 0)); save(); };
  $('#s-test', el).onchange = (e) => invoke('vc_mic_test', { on: e.target.checked });
  // Push-to-talk key: record the next key or mouse button (3 = middle, 4/5 = side buttons).
  $('#s-key', el).onclick = () => {
    const b = $('#s-key', el);
    recording = true;
    b.classList.add('rec');
    b.textContent = 'Press a key or mouse button… (Esc cancels)';
    const done = (code) => {
      document.removeEventListener('keydown', onKey, true);
      document.removeEventListener('mousedown', onMouse, true);
      setTimeout(() => (recording = false), 100);
      b.classList.remove('rec');
      if (code) { s.ptt_key = code; save(); }
      b.textContent = keyLabel(s.ptt_key);
    };
    const onKey = (e) => { e.preventDefault(); e.stopPropagation(); done(e.code === 'Escape' ? null : e.code); };
    const onMouse = (e) => {
      const n = { 1: 'Mouse3', 3: 'Mouse4', 4: 'Mouse5' }[e.button];
      if (!n) return;
      e.preventDefault(); e.stopPropagation(); done(n);
    };
    document.addEventListener('keydown', onKey, true);
    document.addEventListener('mousedown', onMouse, true);
  };
  serverSection($('#srv', el));
  const meterTimer = setInterval(async () => {
    if (!el.isConnected) return;
    const l = await invoke('vc_levels');
    const p = Math.max(0, Math.min(100, ((l.db + 70) / 70) * 100));
    $('#s-meter i', el).style.width = p + '%';
    $('#s-meter', el).classList.toggle('open', l.gate);
  }, 60);
}

/* ---------------- local test server ---------------- */
const SERVER_WARNING = `
  <h3>Before you host a server on this PC</h3>
  <p class="small">The local server is meant for <b>testing and quick sessions</b>. It works, but it is not the safest way to host voice chat. Here's why:</p>
  <ul class="warn-list">
    <li><b>Everyone who joins sees your IP address.</b> Someone could use it to DDoS your internet connection (it happens a lot in competitive games) or roughly find where you live.</li>
    <li><b>Letting people in from outside your home means port forwarding.</b> That opens a direct path from the internet to your gaming PC, instead of to a rented server with nothing personal on it.</li>
    <li><b>It's a small, new server with no security audit.</b> It has no accounts, admin tools, bans or permissions. A password is the only thing keeping strangers out, so set one.</li>
    <li><b>Its certificate is self-signed.</b> People joining get a "trust this server?" prompt and can't check it's really you the first time.</li>
    <li><b>It only runs while your PC and the app are on</b>, and it uses your upload bandwidth and a little CPU while you play.</li>
  </ul>
  <p class="small">For everyday team voice, the <b>EZ server on a Linode</b> is the safer setup: your IP stays hidden and nothing on your PC is exposed.</p>
  <label class="switch"><input type="checkbox" id="t-ok"><span></span>I understand these risks and want to use the local server anyway</label>
  <div class="actions"><button class="btn ghost" id="t-linode">Set up an EZ server instead</button><div class="flex"></div><button class="btn ghost" data-close>Cancel</button><button class="btn primary" id="t-accept" disabled>Accept &amp; continue</button></div>`;

function askServerTerms() {
  return new Promise((resolve) => {
    let ok = false;
    const m = modal(SERVER_WARNING, { wide: true, onClose: () => resolve(ok) });
    $('#t-ok', m.el).onchange = (e) => ($('#t-accept', m.el).disabled = !e.target.checked);
    $('#t-linode', m.el).onclick = () => invoke('app_open_external', { url: 'https://github.com/TheMishez/EZ_VoiceChat/tree/main/server' });
    $('#t-accept', m.el).onclick = async () => { await invoke('srv_accept_terms'); ok = true; m.close(); };
  });
}

async function serverSection(host) {
  let info = await invoke('srv_get');
  const s = { ...info.settings };
  const render = () => {
    const st = info.status;
    const on = st.running;
    const who = st.users && st.users.length ? ` · ${st.users.length} connected: ${st.users.map(esc).join(', ')}` : ' · nobody connected yet';
    const where = on ? (st.lan ? `Others on your network join <b>${esc(st.lanIp || 'your PC\'s IP')}:${st.port}</b> (Mumble). For people outside your home you'd also need to forward port ${st.port} TCP+UDP on your router.` : `Only this PC can join (127.0.0.1:${st.port}).`) : '';
    host.innerHTML = `
      <p class="small" style="margin:0 0 10px">Run a small Mumble server inside the app to test voice or host a quick session. ${info.accepted ? '' : '<b>Not recommended for regular use</b>: you\'ll see why before it starts.'}</p>
      <div class="row" style="margin-bottom:12px"><span class="pill ${on ? 'connected' : 'idle'}"><i></i><span>${on ? 'Running on port ' + st.port + who : 'Off'}</span></span></div>
      ${on ? `<p class="small" style="margin:0 0 12px">${where}</p>` : ''}
      <div class="two">
        <div class="field"><label>Server name</label><input class="input" id="v-name" value="${esc(s.name)}" maxlength="60" ${on ? 'disabled' : ''}></div>
        <div class="field"><label>Port</label><input class="input" id="v-port" value="${s.port}" ${on ? 'disabled' : ''}></div>
      </div>
      <div class="field"><label>Who can join</label>
        <div class="seg" id="v-lan"><button data-l="0" ${on ? 'disabled' : ''}>This PC only (testing)</button><button data-l="1" ${on ? 'disabled' : ''}>My network</button></div></div>
      <p class="small" id="v-lan-note" style="margin:-6px 0 12px"></p>
      <div class="field"><label>Join password ${s.lan ? '(strongly recommended)' : '(optional)'}</label><input class="input" id="v-pass" type="password" value="${esc(s.password)}" ${on ? 'disabled' : ''}></div>
      <div class="row">
        ${on ? '<button class="btn primary" id="v-join">Join my server</button><button class="btn danger" id="v-stop">Stop server</button>' : '<button class="btn primary" id="v-start">Start server</button>'}
      </div>`;
    $$('#v-lan button', host).forEach((b) => b.classList.toggle('on', Number(b.dataset.l) === (s.lan ? 1 : 0)));
    $('#v-lan-note', host).textContent = s.lan ? 'Your IP address will be visible to everyone who joins. Windows may ask to allow EZ VoiceChat through the firewall.' : '';
    if (on) {
      $('#v-stop', host).onclick = async () => {
        if (view && (view.host === '127.0.0.1' || view.host === 'localhost')) await invoke('vc_disconnect');
        await invoke('srv_stop');
        info = await invoke('srv_get');
        render();
      };
      $('#v-join', host).onclick = () => {
        connect({ id: 'local', kind: 'mumble', host: '127.0.0.1', port: String(st.port), nickname: profile.nickname || 'Host', password: s.password });
        toast('Joining your local server');
      };
      return;
    }
    const read = () => {
      s.name = $('#v-name', host).value;
      s.port = Math.max(1024, Math.min(65535, Number($('#v-port', host).value.replace(/\D/g, '')) || 64738));
      s.password = $('#v-pass', host).value;
    };
    $('#v-lan', host).onclick = (e) => { const b = e.target.closest('[data-l]'); if (!b) return; read(); s.lan = b.dataset.l === '1'; invoke('srv_save', { settings: s }); render(); };
    $('#v-start', host).onclick = async () => {
      read();
      if (!info.accepted) {
        const ok = await askServerTerms();
        if (!ok) return;
        info.accepted = true;
      }
      if (s.lan && !s.password && !confirm('Start without a password? Anyone on your network could join.')) return;
      try {
        info.status = await invoke('srv_start', { settings: s });
        toast('Local server started');
      } catch (e) {
        toast(String(e), 'err');
      }
      render();
    };
  };
  render();
  serverRefresh = async () => {
    if (!host.isConnected) return;
    info = await invoke('srv_get');
    if (info.status.running) render();
  };
}
let serverRefresh = null;

/* ---------------- wiring ---------------- */
function bind() {
  $('#add-server').innerHTML = icon('plus');
  $('#btn-settings').innerHTML = icon('gear');
  $('#add-server').onclick = () => editServer(null);
  $('#btn-settings').onclick = openSettings;
  $('#btn-mute').onclick = () => invoke('vc_set_self', { mute: !me.mute, deaf: me.deaf && me.mute });
  $('#btn-deaf').onclick = () => invoke('vc_set_self', { mute: !me.deaf, deaf: !me.deaf });
  $('#btn-disconnect').onclick = () => { activeBm = null; renderBookmarks(); invoke('vc_disconnect'); };
  $$('[data-win]').forEach((b) => (b.onclick = () => invoke('win_' + b.dataset.win)));

  $('#bookmarks').onclick = (e) => {
    const el = e.target.closest('[data-id]');
    if (!el) return;
    const b = bookmarks.find((x) => x.id === el.dataset.id);
    if (b) connect(b);
  };
  $('#bookmarks').oncontextmenu = (e) => {
    const el = e.target.closest('[data-id]');
    if (!el) return;
    e.preventDefault();
    const b = bookmarks.find((x) => x.id === el.dataset.id);
    menu(e.clientX, e.clientY, '<button data-a="connect">Connect</button><button data-a="edit">Edit…</button><button data-a="del" class="danger">Delete</button>', (m) => {
      m.onclick = (ev) => {
        const a = ev.target.closest('[data-a]')?.dataset.a;
        m.hidden = true;
        if (a === 'connect') connect(b);
        else if (a === 'edit') editServer(b);
        else if (a === 'del') { bookmarks = bookmarks.filter((x) => x.id !== b.id); saveBookmarks(); renderBookmarks(); }
      };
    });
  };
  $('#tree').ondblclick = (e) => {
    const c = e.target.closest('[data-chan]');
    if (c) invoke('vc_join', { channel: Number(c.dataset.chan) });
  };
  $('#tree').oncontextmenu = (e) => {
    const u = e.target.closest('[data-user]');
    const c = e.target.closest('[data-chan]');
    if (c) {
      e.preventDefault();
      menu(e.clientX, e.clientY, '<button data-a="join">Join channel</button>', (m) => {
        m.onclick = () => { m.hidden = true; invoke('vc_join', { channel: Number(c.dataset.chan) }); };
      });
      return;
    }
    if (!u || !view) return;
    e.preventDefault();
    const id = Number(u.dataset.user);
    if (id === view.me) return;
    const cur = userVolumes[id] ?? 1;
    menu(e.clientX, e.clientY, `<div class="vol">Volume for ${esc(view.users[id]?.name || '')}: <b id="mv">${Math.round(cur * 100)}%</b><input type="range" min="0" max="3" step="0.05" value="${cur}" id="mvr"></div>`, (m) => {
      $('#mvr', m).oninput = (ev) => {
        const v = Number(ev.target.value);
        userVolumes[id] = v;
        $('#mv', m).textContent = Math.round(v * 100) + '%';
        invoke('vc_user_volume', { user: id, volume: v });
      };
    });
  };
  $('#chat-form').onsubmit = (e) => {
    e.preventDefault();
    const inp = $('#chat-input');
    const t = inp.value.trim();
    if (!t) return;
    invoke('vc_chat', { text: t });
    inp.value = '';
  };
}
const userVolumes = {};

async function boot() {
  bind();
  await loadStore();
  renderBookmarks();
  const s = await invoke('vc_state');
  view = s.view;
  me = s.self;
  setStatus(s.status || {});
  renderSelf();
  renderView();
  listen('vc-state', (v) => { view = v; renderView(); });
  listen('vc-status', (st) => {
    setStatus(st);
    if (st.phase === 'disconnected' || st.phase === 'error') { if (!view) { activeBm = null; renderBookmarks(); } }
  });
  listen('vc-chat', addChat);
  listen('srv-users', () => serverRefresh && serverRefresh());
  listen('vc-self', (x) => { me = x; renderSelf(); });
  listen('vc-talking', (t) => { talking = new Set(t.ids); meTalking = t.me; paintTalking(); });
  listen('vc-audio', (a) => {
    if (!a.input) toast('No microphone found. Check Settings.', 'err');
    if (!a.output) toast('No speakers/headphones found. Check Settings.', 'err');
  });
  listen('vc-cert', (c) => {
    const m = modal(`
      <h3>Server certificate changed</h3>
      <p class="small">The certificate of <b>${esc(c.host)}:${c.port}</b> is different from the last time you connected. This is normal if the server was reinstalled, but it can also mean someone is intercepting the connection.</p>
      <p class="small">Before: <code>${esc(c.previous)}</code><br>Now: <code>${esc(c.fingerprint)}</code></p>
      <div class="actions"><button class="btn ghost" data-close>Cancel</button><button class="btn primary" id="c-ok">Trust and connect</button></div>`);
    $('#c-ok', m.el).onclick = () => { m.close(); if (lastConnect) connect(lastConnect, c.fingerprint); };
  });
}
boot().catch((e) => { document.body.insertAdjacentHTML('beforeend', `<pre style="color:#f66">${esc(e.stack || e)}</pre>`); });

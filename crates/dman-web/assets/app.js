// dman web UI: progressive enhancements on top of htmx. Every core flow also works
// without this file (plain forms). No inline code: loaded under a strict CSP.
(() => {
'use strict';
const d = document, root = d.documentElement;
root.classList.add('js');
const meta = n => (d.querySelector(`meta[name="${n}"]`) || {}).content || '';
const base = meta('dman-base'), csrf = meta('csrf-token');
const $ = (s, el) => (el || d).querySelector(s), $$ = (s, el) => [...(el || d).querySelectorAll(s)];
const mk = (tag, cls, text) => { const e = d.createElement(tag); if (cls) e.className = cls; if (text) e.textContent = text; return e; };
const toasts = () => $('#toasts');

if (/[?&](added|done|undo)=/.test(location.search)) {
const u = new URL(location.href);
['added', 'done', 'undo'].forEach(k => u.searchParams.delete(k));
history.replaceState(null, '', u.pathname + u.search + u.hash);
}

// ---- toasts: newest four stay; hover pauses the timer
function toastEl(level, text) {
const t = mk('div', 'toast ' + level), x = mk('button', 'x', '×');
t.setAttribute('role', level === 'error' ? 'alert' : 'status');
x.type = 'button'; x.dataset.dismiss = ''; x.setAttribute('aria-label', 'Dismiss');
t.append(mk('span', 'tdot'), mk('span', 'tmsg', text), x);
toasts().append(t); armToasts();
}
const addHtml = h => { toasts().insertAdjacentHTML('beforeend', h); armToasts(); };
const dismiss = t => { if (!t) return; t.classList.add('out'); setTimeout(() => t.remove(), 260); };
function armToasts() {
const all = $$('#toasts .toast');
all.slice(0, Math.max(0, all.length - 4)).forEach(t => t.remove());
all.filter(t => !t.dataset.armed).forEach(t => {
t.dataset.armed = 1;
const ms = t.matches('.error') ? 12000 : $('.undoform,.tact', t) ? 9000 : 5000;
let h;
const arm = () => { h = setTimeout(() => dismiss(t), ms); };
t.onmouseenter = () => clearTimeout(h);
t.onmouseleave = arm;
arm();
});
}

// ---- theme: saved per browser, system by default
function theme(v) {
try { v === 'system' ? localStorage.removeItem('dman-theme') : localStorage.setItem('dman-theme', v); } catch (e) {}
v === 'system' ? root.removeAttribute('data-theme') : root.setAttribute('data-theme', v);
syncTheme();
}
const syncTheme = () => { const c = root.getAttribute('data-theme') || 'system'; $$('[data-theme-set]').forEach(b => b.setAttribute('aria-pressed', b.dataset.themeSet === c)); };

// ---- selection and row menus survive htmx swaps
const picked = new Set(), fresh = new Map();
let openMenu = null;
function syncSelection() {
const boxes = $$('.sel'), all = $('#selall'), sc = $('#selcount');
boxes.forEach(c => { c.checked = picked.has(c.value); });
if (sc) sc.textContent = picked.size + ' selected';
if (all) all.checked = boxes.length > 0 && boxes.every(c => c.checked);
}
function restore() {
syncSelection(); syncTheme();
const now = Date.now();
fresh.forEach((until, id) => {
if (until < now) return fresh.delete(id);
const li = d.getElementById('job-' + id);
if (li) li.classList.add('fresh');
});
const m = openMenu && $('#' + openMenu + ' details.menu');
if (m) m.open = true;
armToasts();
}
const clearSelection = () => { picked.clear(); syncSelection(); };
const closeMenus = except => $$('details.menu[open]').forEach(o => { if (o !== except) o.open = false; });

// ---- link kind badges under the paste fields
const VIDEO = /(^|\.)(youtube\.com|youtu\.be|vimeo\.com|twitch\.tv|dailymotion\.com|tiktok\.com|soundcloud\.com|x\.com|twitter\.com)$/i;
function kindOf(l) {
if (/^magnet:\?/i.test(l)) return ['magnet', 'Magnet'];
const m = /^(https?|ftp|sftp):\/\/([^/?#:]+)([^?#]*)/i.exec(l);
if (!m) return ['bad', 'Not a link'];
if (/\.torrent$/i.test(m[3])) return ['torrent', 'Torrent'];
if (/\.(metalink|meta4)$/i.test(m[3])) return ['torrent', 'Metalink'];
if (/^https?$/i.test(m[1]) && VIDEO.test(m[2])) return ['video', 'Video'];
return ['link', m[1].toUpperCase()];
}
function kinds(t) {
const out = d.getElementById(t.dataset.kinds), seen = new Map();
if (!out) return;
t.value.split(/\s+/).filter(Boolean).slice(0, 200).forEach(l => {
const k = kindOf(l), s = seen.get(k[1]) || { k, n: 0 };
s.n++; seen.set(k[1], s);
});
out.textContent = '';
seen.forEach(s => out.append(mk('span', 'kind ' + s.k[0], (s.n > 1 ? s.n + ' × ' : '') + s.k[1])));
}

// ---- events
d.addEventListener('input', e => { if (e.target.dataset && e.target.dataset.kinds) kinds(e.target); });
d.addEventListener('change', e => {
const t = e.target;
if (t.matches('.sel')) { t.checked ? picked.add(t.value) : picked.delete(t.value); syncSelection(); }
else if (t.id === 'selall') { $$('.sel').forEach(c => t.checked ? picked.add(c.value) : picked.delete(c.value)); syncSelection(); }
else if (t.type === 'file' && t.closest('#paste') && t.files.length) { upload(t.files); t.value = ''; }
else if (t.id === 'files') $('#files-help').textContent = [...t.files].map(f => f.name).join(', ');
});
d.addEventListener('toggle', e => {
const m = e.target;
if (!m.matches || !m.matches('details.menu')) return;
const li = m.closest('.job');
if (m.open) { openMenu = li && li.id; closeMenus(m); const f = $('.mi', m); if (f) f.focus(); }
else if (li && openMenu === li.id) openMenu = null;
}, true);
d.addEventListener('click', e => {
const t = e.target, on = s => t.closest(s);
if (on('[data-dismiss]')) return dismiss(on('.toast'));
if (!on('details.menu')) closeMenus();
if (on('[data-theme-set]')) return theme(on('[data-theme-set]').dataset.themeSet);
if (on('[data-cmdk]')) return openCmd();
if (on('[role=tab]')) { e.preventDefault(); return selectTab(on('[role=tab]'), true); }
const close = on('[data-close]');
if (close && close.closest('dialog')) { e.preventDefault(); return close.closest('dialog').close(); }
const pick = on('[data-pick]'), dir = $('#dir');
if (pick && dir) { dir.value = pick.dataset.pick; dir.focus(); }
if (pick || on('[data-unpick]')) return void ($('#fs-picker').innerHTML = '');
const chk = on('[data-check]');
if (chk) return $$('input[name=select]', chk.closest('.pick')).forEach(c => { c.checked = chk.dataset.check === 'all'; });
if (on('[data-clear-sel]')) clearSelection();
});
const typing = t => /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName);
d.addEventListener('keydown', e => {
const t = e.target, k = e.key;
if ((e.metaKey || e.ctrlKey) && k.toLowerCase() === 'k') { e.preventDefault(); return openCmd(); }
if (k === 'Escape') { const o = $('details.menu[open]'); if (o) { o.open = false; $('summary', o).focus(); } }
if (k === '/' && !typing(t) && $('#filter [name=q]')) { e.preventDefault(); return $('#filter [name=q]').focus(); }
// Arrow keys move between rows, menu items and tabs.
if (k === 'ArrowDown' || k === 'ArrowUp') {
const list = t.closest('.pop') ? $$('.mi', t.closest('.pop')) : t.matches('.jname') ? $$('.jname') : null;
const next = list && list[list.indexOf(t) + (k === 'ArrowDown' ? 1 : -1)];
if (list) e.preventDefault();
if (next) next.focus();
if (list) return;
}
if ((k === 'ArrowRight' || k === 'ArrowLeft') && t.getAttribute('role') === 'tab') {
const tabs = $$('[role=tab]'), n = tabs[tabs.indexOf(t) + (k === 'ArrowRight' ? 1 : -1)];
if (n) { e.preventDefault(); selectTab(n, true); n.focus(); }
return;
}
if (k === 'Enter' && !e.shiftKey && !e.isComposing && t.id === 'paste-in') { e.preventDefault(); if (t.value.trim()) t.form.requestSubmit(); }
});

// ---- detail tabs (without JavaScript the sections are simply stacked)
function selectTab(tab, push) {
const id = tab.dataset.tab, p = $('.panels');
if (!p || !id) return;
p.dataset.tab = id;
$$('[role=tab]').forEach(a => { a.setAttribute('aria-selected', a === tab); a.tabIndex = a === tab ? 0 : -1; });
if (push) history.replaceState(null, '', '#' + id);
}
const tabs0 = $$('[role=tab]'), want = tabs0.find(a => a.dataset.tab === location.hash.slice(1)) || tabs0[0];
if (want) selectTab(want, false);

// ---- live speed sparkline, sampled once a second from the stats bar
const pts = [];
function spark() {
const sv = $('#spark'), b = $('#stats [data-down]');
if (!sv || !b) return;
pts.push(+b.dataset.down || 0);
if (pts.length > 60) pts.shift();
const max = Math.max(1, ...pts) * 1.3, w = 120 / Math.max(1, pts.length - 1), y = v => (30 - v / max * 27).toFixed(1);
let line = pts.map((v, i) => (i ? 'L' : 'M') + (i * w).toFixed(1) + ' ' + y(v)).join('');
if (pts.length === 1) line += 'L120 ' + y(pts[0]);
$('.sl', sv).setAttribute('d', line);
$('.sa', sv).setAttribute('d', line + 'L120 32L0 32Z');
}

// ---- command menu (Ctrl/Cmd-K)
const cmd = $('#cmdk'), cq = $('#cmdk-q'), cl = $('#cmdk-list');
let items = [], sel = 0;
function post(url, fields) {
const f = mk('form'); f.method = 'post'; f.action = base + url; f.hidden = true;
Object.entries({ ...fields, _csrf: csrf }).forEach(([k, v]) => { const i = mk('input'); i.type = 'hidden'; i.name = k; i.value = v; f.append(i); });
d.body.append(f); f.submit();
}
const act = url => () => fetch(base + url, { method: 'POST', credentials: 'same-origin', headers: { 'X-CSRF-Token': csrf, 'HX-Request': 'true' } })
.then(r => r.text()).then(addHtml).catch(() => toastEl('error', 'Cannot reach dman.'));
const go = p => () => { location.href = base + p; };
const COMMANDS = [['Go to', 'dl', 'Downloads', go('/')], ['Go to', 'plus', 'Add downloads', go('/add')], ['Go to', 'search', 'Search', go('/search')],
['Go to', 'sliders', 'Settings', go('/settings')], ['Actions', 'pause', 'Pause all downloads', act('/queue/pause-all')],
['Actions', 'play', 'Resume all downloads', act('/queue/resume-all')], ['Actions', 'x', 'Show removed downloads', go('/?removed=1')],
['Theme', 'sun', 'Light theme', () => theme('light')], ['Theme', 'moon', 'Dark theme', () => theme('dark')], ['Theme', 'monitor', 'System theme', () => theme('system')]];
function build() {
const q = cq.value.trim(), ql = q.toLowerCase(), list = [], has = s => s.toLowerCase().includes(ql);
const add = (g, icon, label, run, hint) => list.push({ g, icon, label, run, hint });
// A pasted link is added; otherwise matching commands and downloads come first,
// then searching for the text.
if (/^(magnet:\?|(https?|s?ftp):\/\/)/i.test(q)) {
add('Add', 'plus', 'Add this link', () => post('/add', { uris: q, from: 'downloads' }), kindOf(q)[1]);
return list;
}
COMMANDS.filter(c => has(c[2])).forEach(c => add(...c));
if (q) {
$$('.jname').filter(a => has(a.textContent)).forEach(a => add('Downloads', 'file', a.textContent, go(a.getAttribute('href').slice(base.length))));
add('Search', 'search', `Search torrents for “${q}”`, () => post('/search', { q }));
add('Search', 'dl', `Filter downloads by “${q}”`, go('/?q=' + encodeURIComponent(q)));
}
return list.slice(0, 40);
}
function sprite() {
const u = d.querySelector('svg.i use'), h = u && u.getAttribute('href');
return h ? h.split('#')[0] : base + '/static/icons.svg';
}
function icon(name) {
const ns = 'http://www.w3.org/2000/svg', svg = d.createElementNS(ns, 'svg'), use = d.createElementNS(ns, 'use');
svg.setAttribute('class', 'i'); svg.setAttribute('aria-hidden', 'true');
use.setAttribute('href', sprite() + '#' + name); svg.append(use);
return svg;
}
function renderCmd() {
items = build(); sel = Math.min(sel, Math.max(0, items.length - 1));
cl.textContent = '';
let g = '';
if (!items.length) cl.append(mk('li', 'none', 'Nothing found.'));
items.forEach((it, i) => {
if (it.g !== g) { g = it.g; const h = mk('li', 'grp', g); h.setAttribute('role', 'presentation'); cl.append(h); }
const li = mk('li'); li.id = 'cmd-' + i; li.setAttribute('role', 'option');
li.append(icon(it.icon), mk('span', '', it.label));
if (it.hint) li.append(mk('span', 'hint', it.hint));
li.onmousemove = () => { if (sel !== i) { sel = i; mark(); } };
li.onclick = () => run(i);
cl.append(li);
});
mark();
}
function mark() {
$$('[role=option]', cl).forEach(li => li.setAttribute('aria-selected', li.id === 'cmd-' + sel));
const cur = d.getElementById('cmd-' + sel);
if (cur) { cq.setAttribute('aria-activedescendant', cur.id); cur.scrollIntoView({ block: 'nearest' }); }
}
function run(i) { const it = items[i]; if (it) { cmd.close(); it.run(); } }
function openCmd() {
if (!cmd || cmd.open) return;
closeMenus(); cq.value = ''; sel = 0; renderCmd(); cmd.showModal(); cq.focus();
}
if (cmd) {
cq.oninput = () => { sel = 0; renderCmd(); };
cq.onkeydown = e => {
const n = Math.max(1, items.length);
if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); sel = (sel + (e.key === 'ArrowDown' ? 1 : n - 1)) % n; mark(); }
else if (e.key === 'Enter') { e.preventDefault(); run(sel); }
};
cmd.onclick = e => { if (e.target === cmd) cmd.close(); };
}

// ---- htmx glue
const modal = $('#modal');
d.addEventListener('htmx:load', restore);
d.addEventListener('htmx:afterSwap', e => {
if (e.detail.target && e.detail.target.id === 'modal-body' && modal && !modal.open) {
openMenu = null; closeMenus(); modal.showModal();
const f = $('#modal-body button[type=submit]');
if (f) f.focus();
}
});
d.addEventListener('dman-close', () => { if (modal && modal.open) modal.close(); });
d.addEventListener('dman-clear-sel', clearSelection);
const refresh = () => { if (window.htmx && $('#jobs')) htmx.trigger('#jobs', 'sse:jobs-changed'); };
d.addEventListener('dman-restored', refresh);
// A video link is probed before it is added: say so when that takes a moment.
d.addEventListener('submit', e => {
const s = e.target.querySelector && e.target.querySelector('#add-checking');
if (s && e.target.querySelector('[name=as_video]:checked')) setTimeout(() => { s.hidden = false; }, 900);
});
d.addEventListener('dman-added', e => {
((e.detail && e.detail.ids) || []).forEach(id => fresh.set(String(id), Date.now() + 4000));
const p = $('#paste');
if (p) { p.reset(); kinds($('#paste-in')); $('#paste-in').focus(); }
});
if (modal) modal.addEventListener('close', () => { $('#modal-body').innerHTML = ''; });
d.addEventListener('htmx:sendError', () => toastEl('error', 'Cannot reach dman. Check that the daemon is running, then try again.'));

// ---- drop .torrent files anywhere: the hidden #dropform posts them through htmx
let depth = 0;
const hasFiles = e => e.dataTransfer && [...e.dataTransfer.types].includes('Files');
const upload = files => { const f = $('#dropform'); if (!f) return; $('input[type=file]', f).files = files; f.requestSubmit(); };
d.addEventListener('dragenter', e => { if (hasFiles(e)) { depth++; root.classList.add('drag'); } });
d.addEventListener('dragleave', () => { if (--depth <= 0) { depth = 0; root.classList.remove('drag'); } });
d.addEventListener('dragover', e => { if (hasFiles(e)) e.preventDefault(); });
d.addEventListener('drop', e => {
if (!hasFiles(e)) return;
e.preventDefault(); depth = 0; root.classList.remove('drag');
upload(e.dataTransfer.files);
});

const ck = $('.cmdbtn kbd');
if (ck && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent)) ck.textContent = '⌘K';
armToasts(); syncSelection(); syncTheme(); spark();
setInterval(spark, 1000);
$$('[data-kinds]').forEach(kinds);
})();

// dman web UI: small progressive enhancements on top of htmx. Every core flow also
// works without this file (plain forms). No inline code: loaded under a strict CSP.
(function(){
'use strict';
var d = document, root = d.documentElement;
root.classList.add('js');
var meta = function(n){ var m = d.querySelector('meta[name="' + n + '"]'); return m ? m.content : ''; };
var base = meta('dman-base'), csrf = meta('csrf-token');
var $ = function(s, el){ return (el || d).querySelector(s); };
var $$ = function(s, el){ return Array.prototype.slice.call((el || d).querySelectorAll(s)); };

if (/[?&](added|done|undo)=/.test(location.search)){
var u = new URL(location.href);
u.searchParams.delete('added'); u.searchParams.delete('done'); u.searchParams.delete('undo');
history.replaceState(null, '', u.pathname + u.search + u.hash);
}

function toastEl(level, text){
var t = d.createElement('div'), s = d.createElement('span'), x = d.createElement('button');
t.className = 'toast ' + level; t.setAttribute('role', level === 'error' ? 'alert' : 'status');
s.className = 'tmsg'; s.textContent = text;
x.className = 'x'; x.type = 'button'; x.setAttribute('data-dismiss', ''); x.setAttribute('aria-label', 'Dismiss'); x.textContent = '×';
t.appendChild(s); t.appendChild(x); $('#toasts').appendChild(t); armToasts();
}
function dismiss(t){ t.classList.add('out'); setTimeout(function(){ t.remove(); }, 300); }
function armToasts(){
var all = $$('#toasts .toast');
all.slice(0, Math.max(0, all.length - 5)).forEach(function(t){ t.remove(); });
all.forEach(function(t){
if (t.dataset.armed) return;
t.dataset.armed = '1';
setTimeout(function(){ dismiss(t); }, t.classList.contains('error') ? 12000 : t.querySelector('.undoform') ? 8000 : 5000);
});
}

var picked = new Set(), fresh = new Map(), openMenu = null;
function syncSelection(){
$$('.sel').forEach(function(c){ c.checked = picked.has(c.value); });
var n = picked.size, sc = $('#selcount');
if (sc) sc.textContent = n + ' selected';
var all = $('#selall'), boxes = $$('.sel');
if (all) all.checked = boxes.length > 0 && boxes.every(function(c){ return c.checked; });
}
function restore(el){
syncSelection();
var now = Date.now();
fresh.forEach(function(until, id){
if (until < now){ fresh.delete(id); return; }
var li = d.getElementById('job-' + id);
if (li) li.classList.add('fresh');
});
if (openMenu){ var m = $('#' + openMenu + ' details.menu'); if (m) m.open = true; }
armToasts();
}
function clearSelection(){ picked.clear(); syncSelection(); }

d.addEventListener('change', function(e){
var t = e.target;
if (t.classList.contains('sel')){ t.checked ? picked.add(t.value) : picked.delete(t.value); syncSelection(); }
else if (t.id === 'selall'){
$$('.sel').forEach(function(c){ t.checked ? picked.add(c.value) : picked.delete(c.value); });
syncSelection();
}else if (t.type === 'file' && t.closest('#paste') && t.files.length){
t.closest('form').requestSubmit();
}
});

d.addEventListener('toggle', function(e){
var m = e.target;
if (!m.matches || !m.matches('details.menu')) return;
var li = m.closest('.job');
if (m.open){
openMenu = li ? li.id : null;
$$('details.menu[open]').forEach(function(o){ if (o !== m) o.open = false; });
}else if (li && openMenu === li.id) openMenu = null;
}, true);

d.addEventListener('click', function(e){
var t = e.target;
var x = t.closest('[data-dismiss]');
if (x){ dismiss(x.closest('.toast')); return; }
if (!t.closest('details.menu')) $$('details.menu[open]').forEach(function(o){ o.open = false; });
var close = t.closest('[data-close]');
if (close && close.closest('dialog')){ e.preventDefault(); close.closest('dialog').close(); return; }
var pick = t.closest('[data-pick]');
if (pick){ var dir = $('#dir'); if (dir){ dir.value = pick.getAttribute('data-pick'); dir.focus(); } $('#fs-picker').innerHTML = ''; return; }
if (t.closest('[data-unpick]')){ $('#fs-picker').innerHTML = ''; return; }
var chk = t.closest('[data-check]');
if (chk){ var on = chk.getAttribute('data-check') === 'all'; $$('input[name=select]', chk.closest('.pick')).forEach(function(c){ c.checked = on; }); return; }
if (t.closest('[data-clear-sel]')) clearSelection();
});

d.addEventListener('keydown', function(e){
var t = e.target;
if (e.key === 'Escape') $$('details.menu[open]').forEach(function(o){ o.open = false; });
if (e.key !== 'Enter' || e.shiftKey || e.isComposing) return;
if (t.id === 'paste-in'){ e.preventDefault(); if (t.value.trim()) t.form.requestSubmit(); }
});

d.addEventListener('htmx:load', function(e){ restore(e.target); });
d.addEventListener('htmx:afterSwap', function(e){
var m = $('#modal');
if (e.detail.target && e.detail.target.id === 'modal-body' && m && !m.open){
openMenu = null; $$('details.menu[open]').forEach(function(o){ o.open = false; });
m.showModal();
var f = $('#modal-body button[type=submit]');
if (f) f.focus();
}
});
d.addEventListener('dman-close', function(){ var m = $('#modal'); if (m && m.open) m.close(); });
d.addEventListener('dman-clear-sel', clearSelection);
d.addEventListener('dman-restored', function(){ if (window.htmx && $('#jobs')) htmx.trigger('#jobs', 'sse:jobs-changed'); });
// A video link is probed before it is added: say so when that takes a moment.
d.addEventListener('submit', function(e){
var f = e.target, s = f && f.querySelector && f.querySelector('#add-checking');
if (s && f.querySelector('[name=as_video]:checked')) setTimeout(function(){ s.hidden = false; }, 900);
});
d.addEventListener('dman-added', function(e){
var ids = (e.detail && e.detail.ids) || [];
ids.forEach(function(id){ fresh.set(String(id), Date.now() + 4000); });
var p = $('#paste');
if (p){ p.reset(); $('#paste-in').focus(); }
});
var modal = $('#modal');
if (modal) modal.addEventListener('close', function(){ $('#modal-body').innerHTML = ''; });
d.addEventListener('htmx:sendError', function(){ toastEl('error', 'Cannot reach dman. Check that the daemon is running, then try again.'); });

var depth = 0;
function hasFiles(e){ return e.dataTransfer && Array.prototype.indexOf.call(e.dataTransfer.types, 'Files') >= 0; }
d.addEventListener('dragenter', function(e){ if (hasFiles(e)){ depth++; root.classList.add('drag'); } });
d.addEventListener('dragleave', function(){ if (--depth <= 0){ depth = 0; root.classList.remove('drag'); } });
d.addEventListener('dragover', function(e){ if (hasFiles(e)) e.preventDefault(); });
d.addEventListener('drop', function(e){
if (!hasFiles(e)) return;
e.preventDefault(); depth = 0; root.classList.remove('drag');
var files = Array.prototype.filter.call(e.dataTransfer.files, function(f){ return /\.(torrent|metalink|meta4)$/i.test(f.name); });
if (!files.length){ toastEl('error', 'Only .torrent and .metalink files can be dropped. Paste links into the box instead.'); return; }
var fd = new FormData();
fd.append('_csrf', csrf); fd.append('from', 'drop');
files.forEach(function(f){ fd.append('file', f, f.name); });
fetch(base + '/add', { method: 'POST', body: fd, credentials: 'same-origin', headers: { 'X-CSRF-Token': csrf, 'HX-Request': 'true' } })
.then(function(r){
return r.text().then(function(html){
var ids = r.headers.get('X-Dman-Added') || '';
if (ids && !$('#jobs')){ location.href = base + '/?added=' + encodeURIComponent(ids); return; }
if (r.ok){ $('#toasts').insertAdjacentHTML('beforeend', html); armToasts(); }else toastEl('error', 'Upload failed (' + r.status + ').');
ids.split(',').forEach(function(id){ if (id) fresh.set(id, Date.now() + 4000); });
if (ids && window.htmx) htmx.trigger('#jobs', 'sse:jobs-changed');
});
})
.catch(function(){ toastEl('error', 'Upload failed: cannot reach dman.'); });
});

armToasts();
syncSelection();
})();

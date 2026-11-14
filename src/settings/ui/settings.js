'use strict';

const byId = (id) => document.getElementById(id);
const root = byId('settings');
const content = byId('content');
const pages = [...content.querySelectorAll('.page')];
const nav = [...document.querySelectorAll('nav [data-page]')];
const dialog = byId('dialog');
const dialogBody = byId('dialog-body');
const dialogError = byId('dialog-error');
const dialogSubmit = byId('dialog-submit');
const bridge = window.chrome.webview;
let sceneId, state, setup = false, onboarding = false, visited = 'scenes';
let pending, pendingFocus, sequence = 0, commitDialog;
let invalidLauncher = false;

const iconPaths = {
  layers: 'M12 2 2 7l10 5 10-5-10-5ZM2 12l10 5 10-5M2 17l10 5 10-5',
  panel: 'M3 4h18v16H3zM3 9h18m-14-2h.01m3 0h.01',
  monitor: 'M3 4h18v12H3zM8 21h8m-4-5v5',
  keyboard: 'M2 6h20v12H2zM6 10h.01M10 10h.01m4 0h.01m4 0h.01M6 14h.01M10 14h4m4 0h.01',
  archive: 'M3 4h18v4H3zM5 8v12h14V8m-10 4h6',
  plus: 'M12 5v14M5 12h14',
  check: 'm5 12 4 4L19 6',
  sliders: 'M4 5h8m4 0h4M4 12h3m4 0h9M4 19h11m4 0h1M12 3v4m-5 3v4m8 3v4',
  eye: 'M2 12c5-8 15-8 20 0-5 8-15 8-20 0Zm13 0a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z',
  refresh: 'M20 11a8 8 0 1 0-2.1 6.4M20 5v6h-6',
  restore: 'M3 8h4V4M3 8a9 9 0 1 1-.2 7M12 7v5l4 2',
  code: 'm8 6-6 6 6 6m8-12 6 6-6 6m-3-15-2 18',
  globe: 'M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0ZM3 12h18m-9-9c5 5 5 13 0 18m0-18c-5 5-5 13 0 18',
  chart: 'M3 3v18h18M7 14l4-4 3 2 6-7',
  terminal: 'M3 4h18v16H3zm4 5 3 3-3 3m6 0h4',
  folder: 'M2 6h8l2 3h10v11H2z',
  file: 'M5 3h9l5 5v13H5zM14 3v6h5',
  mail: 'M3 5h18v14H3zm0 1 9 7 9-7',
  chat: 'M3 4h18v13H9l-6 4z',
  star: 'm12 3 2.8 5.6 6.2.9-4.5 4.4 1 6.2-5.5-2.9-5.5 2.9 1-6.2L3 9.5l6.2-.9z',
  grip: 'M9 7h.01m6 0h.01M9 12h.01m6 0h.01M9 17h.01m6 0h.01',
  more: 'M5 12h.01m7 0h.01m7 0h.01',
};

function icon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  const path = document.createElementNS(svg.namespaceURI, 'path');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', '1.65');
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('stroke-linejoin', 'round');
  svg.setAttribute('aria-hidden', 'true');
  path.setAttribute('d', iconPaths[name] || iconPaths.layers);
  svg.append(path);
  return svg;
}

function el(tag, className, ...children) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  node.append(...children.filter((item) => item != null));
  return node;
}

function button(label, action, { css = '', glyph, edit = false, disabled = false, aria, focus } = {}) {
  const btn = el('button', css, glyph ? icon(glyph) : null, label);
  btn.type = 'button';
  btn.addEventListener('click', action);
  btn.disabled = disabled || (edit && state?.read_only);
  if (edit) btn.dataset.edit = '';
  if (aria) btn.setAttribute('aria-label', aria);
  if (focus) btn.dataset.focus = focus;
  return btn;
}

function field(label, control, hint) {
  return el('label', 'field', el('span', '', label), control, hint ? el('small', '', hint) : null);
}
function input(value = '', name = '') {
  const node = el('input');
  node.type = 'text';
  node.name = name;
  node.value = value;
  return node;
}
function check(label, checked, detail, type = 'checkbox') {
  const box = el('input');
  box.type = type;
  box.checked = checked;
  const text = el('span', 'identity', el('strong', '', label), detail ? el('small', '', detail) : null);
  return [el('label', 'choice', box, text), box];
}
function select(items, value) {
  const node = el('select');
  setOptions(node, items, value);
  return node;
}
function setOptions(node, items, value) {
  node.replaceChildren(...items.map(([id, name]) => new Option(name, id)));
  node.value = value;
}
function badge(name, neutral = false) {
  return el('span', 'icon-box' + (neutral ? ' neutral' : ''), icon(name));
}
function appIcon(name) {
  if (/terminal/i.test(name)) return 'terminal';
  if (/code/i.test(name)) return 'code';
  if (/edge|chrome|firefox/i.test(name)) return 'globe';
  if (/explorer|エクスプローラー/i.test(name)) return 'folder';
  if (/outlook|mail/i.test(name)) return 'mail';
  return 'panel';
}
function rowIdentity(app, title, glyph = appIcon(app)) {
  return el('div', 'inline', badge(glyph, true), el('span', 'identity',
    el('strong', '', app), title ? el('small', '', title) : null));
}

function request(action, values = {}) {
  if (pending) return Promise.reject(new Error('前の操作が完了するまでお待ちください。'));
  const id = ++sequence;
  return new Promise((resolve, reject) => {
    pending = { id, resolve, reject, focus: root.contains(document.activeElement) ? document.activeElement : null };
    root.setAttribute('aria-busy', 'true');
    root.inert = true;
    bridge.postMessage({ id, action, ...values });
  });
}

bridge.addEventListener('message', ({ data }) => {
  if (data.kind === 'close') { void closeSettings(); return; }
  if (data.kind === 'page') {
    if (state && !dialog.open) go(data.page, true);
    return;
  }
  if (data.kind === 'warning') { showMessage(data.message); return; }
  if (data.kind !== 'reply' || !pending || data.id !== pending.id) return;
  const { resolve, reject, focus } = pending;
  pending = undefined;
  root.setAttribute('aria-busy', 'false');
  root.inert = false;
  pendingFocus = focus?.dataset.focus || focus;
  focus?.focus({ preventScroll: true });
  if (!data.ok) { reject(new Error(data.error)); return; }
  if (data.state) state = data.state;
  resolve(data.extra);
});

function showMessage(text = '', info = false) {
  const message = byId('message');
  message.textContent = text;
  message.hidden = !text;
  message.toggleAttribute('data-info', info);
}
function showDialogError(text = '') {
  dialogError.textContent = text;
  dialogError.hidden = !text;
}
async function act(action, values, after) {
  try {
    if (pending) return;
    const extra = await request(action, values);
    after?.(extra);
    redraw();
  } catch (error) {
    showMessage(error.message);
  }
}

function go(page, focus = true) {
  if (!pages.some((node) => node.id === page)) return;
  if (visited !== page) {
    pages.forEach((node) => { node.hidden = node.id !== page; });
    nav.forEach((node) => {
      if (node.dataset.page === page) node.setAttribute('aria-current', 'page');
      else node.removeAttribute('aria-current');
    });
    content.scrollTop = 0;
    visited = page;
  }
  if (focus) byId(page + '-title').focus({ preventScroll: true });
  pendingFocus = undefined;
}

function redraw() {
  if (!state) return;
  const focusKey = document.activeElement?.dataset.focus;
  sceneId = state.scenes.some((scene) => scene.id === sceneId) ? sceneId : state.scenes[0]?.id;
  renderScenes();
  renderLauncher();
  renderBehavior();
  renderShortcuts();
  renderFooter();
  root.querySelectorAll('[data-edit]').forEach((node) => { node.disabled = state.read_only; });
  const restore = focusKey || pendingFocus;
  pendingFocus = undefined;
  if (restore && !dialog.open) {
    const control = typeof restore === 'string'
      ? [...root.querySelectorAll('[data-focus]')].find((node) => node.dataset.focus === restore) : restore;
    control?.focus({ preventScroll: true });
  }
}

function renderFooter() {
  const dirty = state.dirty || invalidLauncher;
  const text = state.read_only ? '読み取り専用：新しいバージョンの設定です'
    : dirty ? '未保存の変更があります' : '保存済み';
  const status = byId('status');
  status.toggleAttribute('data-dirty', dirty);
  if (status.textContent !== text) status.textContent = text;
  byId('save').disabled = !dirty || state.read_only || invalidLauncher;
  byId('discard').disabled = !dirty || state.read_only;
}

function selected() { return state.scenes.find((scene) => scene.id === sceneId); }
function scopeLabel() {
  const { policy, monitor } = targetValue();
  if (policy === 'all_monitors') return 'すべての画面';
  if (policy === 'launcher_monitor') return '固定バーのある画面';
  if (!monitor) return '未設定';
  return state.monitors.find((m) => m.id === monitor)
    ? '画面 ' + state.monitors.find((m) => m.id === monitor).number : '未接続の画面';
}
function monitorSelected(id) {
  return state.target.monitor_policy === 'all_monitors' || state.effective_monitor === id;
}
function targetValue() { return { policy: state.target.monitor_policy, monitor: state.target.monitor_id }; }

function renderScenes() {
  byId('first-run').hidden = !onboarding || state.read_only;
  byId('target-summary').textContent = '操作する画面：' + scopeLabel();
  byId('mini-monitors').replaceChildren(...state.monitors.map((m) => el('span', monitorSelected(m.id) ? 'active' : '', String(m.number))));
  byId('scene-list').replaceChildren(...state.scenes.map((scene) => {
    const selected = scene.id === sceneId;
    const count = state.bindings.filter((binding) => binding.scenes.includes(scene.id)).length;
    const item = button('', () => { sceneId = scene.id; redraw(); }, { focus: 'scene:' + scene.id });
    item.setAttribute('aria-pressed', String(selected));
    item.replaceChildren(badge(scene.icon, !selected), el('span', 'identity', el('strong', '', scene.name), el('small', '', count + ' ウィンドウ')),
      selected ? el('span', 'check', icon('check')) : el('span'));
    return item;
  }));
  const scene = selected();
  byId('scene-editor').hidden = !scene;
  if (scene) {
    const bindings = state.bindings.filter((binding) => binding.scenes.includes(scene.id));
    byId('scene-windows').textContent = scene.name + 'のウィンドウ';
    byId('scene-bindings').replaceChildren(...(bindings.length
      ? bindings.map((binding) => bindingRow(binding, scene)) : [el('li', 'empty', 'ウィンドウはまだありません。開いているウィンドウから追加できます。')]));
  }
  byId('all-bindings').replaceChildren(...(state.bindings.length
    ? state.bindings.map((binding) => bindingRow(binding)) : [el('li', 'empty', '登録済みのウィンドウはありません。')]));
}

function bindingRow(binding, scene) {
  const description = [binding.running ? binding.title : 'ウィンドウが見つかりません', binding.monitor_label].filter(Boolean).join(' · ');
  const status = [...new Set([
    ...(!binding.enabled ? ['無効'] : []),
    ...(binding.always ? ['常に表示'] : []),
    ...(binding.scenes.length > 1 ? ['他のシーンと共有'] : []),
    ...binding.warnings,
  ])];
  const controls = el('div', 'actions',
    ...status.map((text) => el('span', 'tag', text)),
    button(binding.ambiguous.length ? '候補を確認' : '詳細', () => openBinding(binding), {
      css: 'quiet', aria: binding.app + ' の詳細', focus: 'binding:' + (scene?.id || '-') + ':' + binding.id,
    }),
    scene ? button('外す', () => act('remove_from_scene', { scene: scene.id, id: binding.id }, () => {
      if (!byId('add-windows').disabled) queueMicrotask(() => byId('add-windows').focus({ preventScroll: true }));
    }), { css: 'quiet', edit: true, aria: binding.app + ' をこのシーンから外す' }) : null);
  return el('li', 'row labelled-row', rowIdentity(binding.app, description), controls);
}

const launcherFields = ['show_bar','ctrl_double_tap','orientation','cursor_popup_position','button_size_dip',
  'grip_size_dip','visible_slots','always_on_top','snap_to_edges','wheel_wrap'];
function renderLauncher() {
  const cfg = state.launcher;
  launcherFields.forEach((id) => {
    const node = byId(id);
    if (node.type === 'checkbox') node.checked = cfg[id];
    else if (!(invalidLauncher && node.type === 'number')) node.value = cfg[id];
  });
  byId('tray-hint').hidden = cfg.show_bar || cfg.ctrl_double_tap;
  const preview = byId('bar-preview');
  if (!cfg.show_bar) { preview.replaceChildren(el('small', '', '固定バーは非表示です。')); return; }
  const scenes = state.bar_scenes;
  const bar = el('div', 'bar' + (cfg.orientation === 'vertical' ? ' vertical' : ''),
    el('span', 'grip', icon('grip')),
    ...scenes.map((scene) => {
      const node = el('span', 'slot', icon(scene.icon));
      node.title = scene.name;
      return node;
    }),
    state.overflow ? el('span', 'slot', icon('more')) : null);
  bar.style.setProperty('--slot', (Math.min(49, Math.max(24, cfg.button_size_dip))) + 'px');
  bar.setAttribute('role', 'img');
  bar.setAttribute('aria-label', ['固定バー ' + (cfg.orientation === 'vertical' ? '縦向き' : '横向き'), ...scenes.map((s) => s.name), ...(state.overflow ? ['その他のシーン'] : [])].join('、'));
  preview.replaceChildren(bar);
}

function renderBehavior() {
  const all = state.target.monitor_policy === 'all_monitors';
  const options = [['all_monitors', 'すべての画面'], ['launcher_monitor', '固定バーのある画面']];
  const fixed = state.target.monitor_policy === 'fixed';
  const value = fixed ? 'fixed:' + (state.target.monitor_id || '') : state.target.monitor_policy;
  if (fixed && !state.monitors.some((m) => m.id === state.target.monitor_id)) {
    options.push([value, state.target.monitor_id ? '未接続の画面（現在の設定）' : '未設定']);
  }
  options.push(...state.monitors.map((m) => ['fixed:' + m.id, '画面 ' + m.number + ' だけ' + (m.name ? ' — ' + m.name : '')]));
  setOptions(byId('target'), options, value);
  byId('monitor-hint').textContent = !all && !state.monitors.some((m) => m.id === state.effective_monitor)
    ? '対象の画面が見つかりません。接続を確認するか、別の画面を選んでください。'
    : '切替時は「' + scopeLabel() + '」にある登録済みのウィンドウを操作します。';
  byId('setup-next-row').hidden = !setup;
  byId('restore').checked = state.launcher.restore_all_on_switch;
  byId('focus').checked = state.launcher.focus_on_switch;
  byId('autostart').checked = state.autostart;
  byId('verbose').checked = state.verbose;

  renderMonitors();
}

function renderMonitors() {
  const map = byId('monitor-map');
  const monitors = state.monitors;
  if (!monitors.length) { map.replaceChildren(el('small', '', '接続されている画面はありません。')); return; }
  const bounds = monitors.map(({rect}) => rect);
  const minX = Math.min(...bounds.map((r) => r.left)), minY = Math.min(...bounds.map((r) => r.top));
  const width = Math.max(1, Math.max(...bounds.map((r) => r.right)) - minX);
  const height = Math.max(1, Math.max(...bounds.map((r) => r.bottom)) - minY);
  const maxWidth = map.clientWidth || 230;
  const scale = Math.min(maxWidth / width, 116 / height);
  map.replaceChildren(...monitors.map((m) => {
    const rect = m.rect;
    const label = '画面 ' + m.number;
    const primary = m.primary ? '（メイン）' : '';
    const item = button(label, () => void act('target', { policy: 'fixed', monitor: m.id }), {
      edit: true, aria: label + primary + (m.name ? ' ' + m.name : ''), focus: 'monitor:' + m.id,
    });
    item.setAttribute('aria-pressed', String(monitorSelected(m.id)));
    item.title = label + primary + (m.name ? ' — ' + m.name : '');
    item.style.left = (rect.left - minX) * scale + 'px';
    item.style.top = (rect.top - minY) * scale + 'px';
    item.style.width = Math.max(1, (rect.right - rect.left) * scale - 3) + 'px';
    item.style.height = Math.max(1, (rect.bottom - rect.top) * scale - 3) + 'px';
    return item;
  }));
  map.style.height = Math.max(70, height * scale) + 'px';
}

const kinds = [['folder','フォルダー'],['file','ファイル'],['website','サイト']];
const keys = [
  ...Array.from({length: 26}, (_, i) => [65 + i, String.fromCharCode(65 + i)]),
  ...Array.from({length: 10}, (_, i) => [48 + i, String(i)]),
  ...Array.from({length: 24}, (_, i) => [112 + i, 'F' + (i + 1)]),
  [8,'Backspace'],[9,'Tab'],[13,'Enter'],[19,'Pause'],[20,'Caps Lock'],[27,'Esc'],[32,'Space'],[33,'Page Up'],[34,'Page Down'],
  [35,'End'],[36,'Home'],[37,'←'],[38,'↑'],[39,'→'],[40,'↓'],[44,'Print Screen'],[45,'Insert'],[46,'Delete'],
  ...Array.from({length: 10}, (_, i) => [96 + i, 'テンキー ' + i]),
  [106,'テンキー *'],[107,'テンキー +'],[109,'テンキー −'],[110,'テンキー .'],[111,'テンキー /'],
  [186,';'],[187,'='],[188,','],[189,'-'],[190,'.'],[191,'/'],[192,'`'],[219,'['],[220,'\\'],[221,']'],[222,"'"],
];
const mods = [[2,'Ctrl'],[1,'Alt'],[4,'Shift'],[8,'Win']];
function keyName(key) {
  return keys.find(([code]) => code === key)?.[1] || 'キー ' + key.toString(16).toUpperCase();
}
function hotkeyLabel(hotkey) {
  return hotkey ? [...mods.filter(([mask]) => mask & hotkey.mods).map(([, name]) => name), keyName(hotkey.key)].join(' + ') : '未設定';
}
function renderShortcuts() {
  byId('hotkeys').replaceChildren(...(state.scenes.length ? state.scenes.map((scene) =>
    el('li', 'row labelled-row', rowIdentity(scene.name, null, scene.icon), el('div', 'actions',
      el('kbd', 'tag', hotkeyLabel(scene.hotkey)),
      button('設定', () => openHotkey(scene), { css: 'quiet', edit: true, aria: scene.name + 'に切り替えるキーを設定', focus: 'hk:' + scene.id }))))
    : [el('li', 'empty', 'シーンを作ると、切替キーを設定できます。')]));
  byId('shortcut-list').replaceChildren(...(state.shortcuts.length ? state.shortcuts.map((item, index) =>
    el('li', 'row labelled-row', rowIdentity(item.name, item.target, item.kind === 'website' ? 'globe' : item.kind),
      el('div', 'actions', el('span', 'tag', kinds.find(([id]) => id === item.kind)?.[1]),
        button('編集', () => openShortcut(index), { css: 'quiet', edit: true, aria: item.name + ' を編集', focus: 'shortcut:' + index }))))
    : [el('li', 'empty', 'フォルダー、ファイル、サイトをここから追加できます。')]));
}

function openDialog(title, caption, content, submitLabel, onSubmit, { focus } = {}) {
  commitDialog = onSubmit;
  byId('dialog-title').textContent = title;
  byId('dialog-caption').textContent = caption || '';
  byId('dialog-caption').hidden = !caption;
  dialogBody.replaceChildren(...(Array.isArray(content) ? content : [content]));
  byId('dialog-cancel').textContent = onSubmit ? 'キャンセル' : '閉じる';
  dialogSubmit.hidden = !onSubmit;
  dialogSubmit.disabled = false;
  dialogSubmit.textContent = submitLabel || '';
  showDialogError();
  if (!dialog.open) dialog.showModal();
  dialogBody.scrollTop = 0;
  (focus || byId('dialog-cancel')).focus({ preventScroll: true });
}

function dismiss() {
  dialog.close();
  commitDialog = undefined;
}

function confirm(title, caption, label, callback) {
  openDialog(title, caption, [], label, callback);
}

function runDialogAction(handler) {
  return async () => {
    try {
      if (pending) return;
      showDialogError();
      await handler();
    } catch (error) { showDialogError(error.message); }
  };
}

async function applyDialog(action, args, next) {
  const extra = await request(action, args);
  dismiss();
  next?.(extra);
  redraw();
}

function openCreate() {
  if (pending || state.read_only) return;
  const name = input('', 'name');
  name.required = true;
  openDialog('シーンを作る', '例：開発、調べもの、会議', field('シーン名', name), '作成', () =>
    applyDialog('create_scene', { name: name.value }, ({ scene }) => {
      sceneId = scene;
      queueMicrotask(() => byId('add-windows').focus());
    }), { focus: name });
}

function openSceneDetails() {
  const scene = selected();
  if (!scene || pending) return;
  const name = input(scene.name);
  name.required = true;
  const options = state.icons.map(([id, name]) => [id, name]);
  options.push(['custom', 'ファイルから選ぶ']);
  const builtIn = state.icons.some(([id]) => id === scene.icon);
  const presets = select(options, builtIn ? scene.icon : 'custom');
  const path = input(builtIn ? '' : scene.icon);
  path.readOnly = true;
  const choose = button('ファイルを選ぶ…', runDialogAction(async () => {
    const result = await request('browse', { kind: 'image' });
    if (result?.path) { path.value = result.path; presets.value = 'custom'; imageField.hidden = false; }
  }), { edit: true });
  const imageField = el('div', 'stack', field('アイコンのファイル', path), el('div', 'actions', choose));
  imageField.hidden = builtIn;
  presets.addEventListener('change', () => { imageField.hidden = presets.value !== 'custom'; });
  const [pin, pinned] = check('固定バーに表示する', scene.pinned);
  const order = select(state.scenes.map((_, index) => [String(index), (index + 1) + ' 番目']), String(state.scenes.indexOf(scene)));
  const layouts = el('div', 'stack', el('h3', '', '保存したウィンドウ配置'),
    el('p', '', scene.layouts ? scene.layouts + ' 画面分の配置があります。' : '保存された配置はありません。'),
    scene.layouts ? el('div', 'actions', button('保存した配置を削除', () => confirm('保存した配置を削除しますか？',
      'シーンのウィンドウ登録はそのままです。', '配置を削除', () =>
        applyDialog('delete_layout', { id: scene.id })), { edit: true })) : null);
  const remove = el('div', 'actions', button('シーンを削除', () => confirm('「' + scene.name + '」を削除しますか？',
    'このシーンの切替キーと保存した配置も削除されます。ほかのシーンに共有したウィンドウは残ります。', 'シーンを削除',
    () => applyDialog('delete_scene', { id: scene.id })), { css: 'danger', edit: true }));
  for (const ctrl of [name, presets, pinned, order]) ctrl.disabled = state.read_only;
  openDialog('シーンの詳細設定', '', [field('シーン名', name), field('アイコン', presets), imageField, pin, field('表示の順番', order), layouts, remove],
    '編集に反映', state.read_only ? undefined : () => applyDialog('edit_scene', {
      id: scene.id, name: name.value, icon: presets.value === 'custom' ? path.value : presets.value,
      pinned: pinned.checked, order: Number(order.value),
    }), { focus: state.read_only ? undefined : name });
}

async function openWindowPicker() {
  if (!selected() || state.read_only || pending) return;
  try {
    await request('refresh');
    redraw();
    const scene = selected();
    if (!scene) return;
    const list = el('div');
    const matches = state.candidates;
    const chosen = [];
    for (const candidate of matches) {
      const already = candidate.scenes.includes(scene.id);
      const status = [candidate.title, candidate.monitor, already ? '追加済み' : candidate.blocked,
        candidate.ambiguous ? '登録済みウィンドウの候補です。先に登録の候補を確認してください' : null,
        !already && candidate.scenes.length ? '他のシーンと共有' : null, ...candidate.warnings];
      const [label, box] = check(candidate.app, false, [...new Set(status.filter(Boolean))].join(' · '));
      box.disabled = already || Boolean(candidate.blocked) || candidate.ambiguous;
      box.value = candidate.key;
      if (!box.disabled) chosen.push(box);
      list.append(label);
    }
    if (!matches.length) list.append(el('p', '', 'ウィンドウが見つかりません。登録したいアプリを開いてから、もう一度お試しください。'));
    list.addEventListener('change', () => { dialogSubmit.disabled = !chosen.some((box) => box.checked); });
    openDialog('開いているウィンドウから追加', '追加先：' + scene.name + '　／　操作範囲：' + scopeLabel(),
      list, '選んだウィンドウを追加', () => applyDialog('add_windows', {
        scene: scene.id, candidates: chosen.filter((box) => box.checked).map((box) => box.value),
      }, () => { onboarding = false; }), { focus: chosen[0] });
    dialogSubmit.disabled = true;
  } catch (error) { showMessage(error.message); }
}

function openBinding(binding) {
  if (pending) return;
  const readOnly = state.read_only;
  const checks = state.scenes.map((scene) => {
    const [label, box] = check(scene.name, binding.scenes.includes(scene.id));
    box.disabled = readOnly;
    return { label, box, id: scene.id };
  });
  const scenes = el('fieldset', 'rule-box', el('legend', '', '登録先のシーン'), ...checks.map(({ label }) => label));
  const [always, isAlways] = check('シーンを切り替えても常に表示する', binding.always);
  const [enabled, isEnabled] = check('このウィンドウの登録を有効にする', binding.enabled);
  const type = select([['any','ウィンドウ名を問わない'],['contains','指定した文字を含む'],
    ['prefix','指定した文字から始まる'],['exact','指定した名前と一致'],['regex','正規表現']], binding.rule.type);
  const value = input(binding.rule.value || '');
  const condition = field('一致させる文字', value);
  const monitorChoices = [['', '画面を問わない'], ...state.monitors.map((m) => [m.id, '画面 ' + m.number + (m.name ? ' — ' + m.name : '')])];
  if (binding.monitor && !state.monitors.some((m) => m.id === binding.monitor)) monitorChoices.push([binding.monitor, '未接続の画面（現在の設定）']);
  const monitor = select(monitorChoices, binding.monitor || '');
  const rules = el('details', '', el('summary', '', '再起動後に見分ける条件'), el('div', 'stack',
    field('ウィンドウ名の条件', type), condition, field('登録したウィンドウを探す画面', monitor),
    el('p', '', '名前には口座番号などが含まれる場合があります。必要な文字だけを設定してください。')));
  const updateRule = () => { condition.hidden = type.value === 'any'; value.required = !condition.hidden; };
  updateRule();
  type.addEventListener('change', updateRule);
  for (const ctrl of [type, value, monitor, isAlways, isEnabled]) ctrl.disabled = readOnly;
  const ambiguity = el('div', 'stack');
  if (binding.ambiguous.length) {
    ambiguity.append(el('p', 'notice', '一致するウィンドウが複数あります。確認できるまで、この登録は切替で操作しません。'));
    if (!readOnly) {
      ambiguity.append(button('対象のウィンドウを選ぶ', () => chooseCandidate(binding), { css: 'quiet' }));
    }
  }
  const remove = el('div', 'actions', button('すべてのシーンから登録を解除', () =>
    confirm('「' + binding.app + '」の登録を解除しますか？', '関連するすべてのシーンから外れます。', '登録を解除',
      () => applyDialog('delete_binding', { id: binding.id })), { edit: true, css: 'danger' }));
  openDialog(binding.app + ' の登録', binding.title || 'ウィンドウが見つかりません',
    [ambiguity, scenes, always, enabled, rules, remove], '編集に反映', readOnly ? undefined : () =>
      applyDialog('edit_binding', { id: binding.id, scenes: checks.filter(({box}) => box.checked).map(({id}) => id),
        always: isAlways.checked, enabled: isEnabled.checked, rule: { type: type.value, value: type.value === 'any' ? null : value.value },
        monitor: monitor.value || null }));
}

function chooseCandidate(binding) {
  const choices = state.candidates.filter((candidate) => binding.ambiguous.includes(candidate.key));
  const list = el('div');
  for (const candidate of choices) {
    const [label, option] = check(candidate.app, false, [candidate.title, candidate.monitor, candidate.blocked].filter(Boolean).join(' · '), 'radio');
    option.name = 'candidate';
    option.value = candidate.key;
    option.disabled = Boolean(candidate.blocked);
    list.append(label);
  }
  list.addEventListener('change', () => { dialogSubmit.disabled = !list.querySelector('input:checked'); });
  if (!choices.length) list.append(el('p', '', '状態を更新して対象ウィンドウを確認してください。'));
  openDialog('対象のウィンドウを確認', '実際に操作したいウィンドウを一つ選びます。',
    list, 'このウィンドウに決める', () => applyDialog('choose_window', {
      id: binding.id, candidate: list.querySelector('input:checked').value,
    }));
  dialogSubmit.disabled = true;
}

async function inspect() {
  const scene = selected();
  if (!scene || pending) return;
  try {
    const result = await request('inspect', { scene: scene.id });
    const group = (label, items) => el('div', 'stack', el('h3', '', label),
      items.length ? el('ul', 'rows', ...items.map(({ window, reason }) => el('li', '',
        rowIdentity(window.app, [window.title, window.monitor, reason].filter(Boolean).join(' · ')))))
        : el('p', '', '該当なし'));
    const warnings = result.warnings.map((warning) => el('p', 'notice', warning));
    openDialog('「' + scene.name + '」に切り替えると', '現在のウィンドウと編集中の設定からの見込みです。ここでは操作しません。', [
      ...warnings,
      group('表示する', result.restore.map((window) => ({ window }))),
      group('最小化する', result.minimize.map((window) => ({ window }))),
      group('表示状態を保つ', result.keep),
    ]);
  } catch (error) { showMessage(error.message); }
}

function openHotkey(scene) {
  const chosen = scene.hotkey;
  const options = [['0','設定しない'], ...keys.map(([key, label]) => [String(key), label])];
  if (chosen && !keys.some(([key]) => key === chosen.key)) options.push([String(chosen.key), keyName(chosen.key)]);
  const key = select(options, String(chosen?.key || 0));
  const modifiers = mods.map(([mask, name]) => {
    const [label, box] = check(name, !!(mask & (chosen?.mods ?? 0)));
    return { label, box, mask };
  });
  const values = () => ({
    scene: scene.id, modifiers: modifiers.reduce((sum, {box,mask}) => sum | (box.checked ? mask : 0), 0),
    key: Number(key.value) || null,
  });
  const warn = el('p', 'notice', '修飾キーなしでは、ほかのアプリのキー操作と重なる場合があります。');
  const controls = el('div', 'actions', ...modifiers.map(({label}) => label));
  function sync() { const data = values(); warn.hidden = !data.key || Boolean(data.modifiers); }
  controls.addEventListener('change', sync);
  key.addEventListener('change', sync);
  sync();
  openDialog('「' + scene.name + '」に切り替えるキー', 'Kiri を開いていないときも Windows 全体で有効です。',
    [field('キー', key), controls, warn], '編集に反映', () => {
      const data = values();
      if (data.key && !data.modifiers && (data.key !== chosen?.key || chosen?.mods)) {
        confirm('修飾キーを付けずに設定しますか？', '普段の入力や他のアプリのショートカットに影響することがあります。',
          'このキーに設定', () => applyDialog('set_hotkey', data));
        return;
      }
      return applyDialog('set_hotkey', data);
    }, { focus: key });
}

function openShortcut(index) {
  if (state.read_only || pending) return;
  const current = state.shortcuts[index];
  const name = input(current?.name || '');
  name.required = true;
  const kind = select(kinds, current?.kind || 'folder');
  const target = input(current?.target || '');
  target.required = true;
  const browse = button('参照…', runDialogAction(async () => {
    const result = await request('browse', { kind: kind.value });
    if (result?.path) target.value = result.path;
  }));
  kind.addEventListener('change', () => { browse.hidden = kind.value === 'website'; });
  browse.hidden = kind.value === 'website';
  const order = current ? select(state.shortcuts.map((_, i) => [String(i), (i + 1) + ' 番目']), String(index)) : null;
  const edit = current ? el('div', 'actions',
    button('削除', () => confirm('ショートカットを削除しますか？', current.name, '削除',
      () => applyDialog('delete_shortcut', { index })), { css: 'danger' })) : null;
  openDialog(current ? 'ショートカットを編集' : 'ショートカットを追加', 'カーソル付近に開くランチャーから呼び出せます。',
    [field('表示名', name), field('種類', kind), field('場所', target, 'フォルダーやファイルのパス、または http://・https:// の URL。'),
      el('div', 'actions', browse), order ? field('表示の順番', order) : null, edit].filter(Boolean),
    '編集に反映', () => applyDialog('edit_shortcut', { index: current ? index : null, order: order ? Number(order.value) : state.shortcuts.length, shortcut: {
      name: name.value, kind: kind.value, target: target.value,
    } }), { focus: name });
}

function saveEdits() {
  if (!state?.dirty || pending || invalidLauncher) return;
  const {show_bar, ctrl_double_tap} = state.launcher;
  showMessage();
  if (!show_bar && !ctrl_double_tap) {
    confirm('タスクトレイからだけ開きますか？', '固定バーと Ctrl の二度押しがオフになります。タスクトレイから設定を開き直せます。',
      '変更を保存', async () => {
        const result = await request('save', { tray_only_confirmed: true });
        dismiss();
        redraw();
        if (result.warning) showMessage(result.warning);
      });
  } else void act('save', { tray_only_confirmed: false }, (result) => {
    if (result.warning) showMessage(result.warning);
  });
}

async function closeSettings() {
  if (pending) return;
  if (state?.dirty || invalidLauncher || dialog.open) {
    confirm('保存していない変更を破棄しますか？', '閉じると、この画面での編集は失われます。',
      '破棄して閉じる', () => request('close', { discard: true }));
  } else await act('close', { discard: false });
}

function sendLauncher() {
  const controls = launcherFields.map(byId);
  invalidLauncher = controls.some((node) => node.type === 'number' && !node.checkValidity());
  renderFooter();
  if (invalidLauncher) { showMessage('固定バーの大きさや表示件数を、項目に表示された範囲で入力してください。'); return; }
  showMessage();
  const values = Object.fromEntries(controls.map((node) => [node.id, node.type === 'checkbox' ? node.checked
    : node.type === 'number' ? node.valueAsNumber : node.value]));
  void act('launcher', { values });
}

function sendBehavior() {
  void act('behavior', Object.fromEntries(['restore','focus','autostart','verbose'].map((id) => [id, byId(id).checked])));
}

async function restorePreview() {
  if (pending) return;
  try {
    const windows = await request('recovery_preview');
    const rows = windows.length ? el('ul','rows', ...windows.map((win) => el('li','', rowIdentity(win.app, win.title))))
      : el('p', '', 'いま復元できるウィンドウはありません。');
    openDialog('復元できるウィンドウ', 'Kiri が最小化したウィンドウだけを元に戻します。', rows,
      'ウィンドウを復元', windows.length ? () => applyDialog('restore', { candidates: windows.map((win) => win.key) },
        ({ restored }) => showMessage(restored + ' 件のウィンドウを復元しました。', true)) : undefined);
  } catch (error) { showMessage(error.message); }
}

function importSettings() {
  const load = async () => {
    const result = await request('import');
    if (!result?.imported) return;
    if (dialog.open) dismiss();
    invalidLauncher = false;
    onboarding = false;
    showMessage('設定を読み込みました。「変更を保存」で反映します。' +
      (result.warnings.length ? ' 未対応の設定項目が ' + result.warnings.length + ' 件あります。' : ''), true);
    redraw();
  };
  if (state.dirty || invalidLauncher) {
    confirm('編集中の内容を置き換えますか？', '読み込んだ設定は「変更を保存」で反映されます。',
      'ファイルを選ぶ', load);
  } else void load().catch((error) => showMessage(error.message));
}

document.querySelectorAll('[data-icon]').forEach((node) => node.prepend(icon(node.dataset.icon)));
nav.forEach((node) => node.addEventListener('click', () => { if (state) { go(node.dataset.page); if (visited === 'behavior') renderBehavior(); } }));
root.querySelectorAll('[data-link]').forEach((node) => node.addEventListener('click', () => { go(node.dataset.link); renderBehavior(); }));
byId('start-setup').addEventListener('click', () => { setup = true; go('behavior'); renderBehavior(); });
byId('setup-next').addEventListener('click', () => { setup = false; go('scenes'); redraw(); byId('create-scene').focus(); });
byId('create-scene').addEventListener('click', openCreate);
byId('add-windows').addEventListener('click', openWindowPicker);
byId('scene-details').addEventListener('click', openSceneDetails);
byId('inspect').addEventListener('click', inspect);
byId('refresh-bindings').addEventListener('click', () => act('refresh'));
byId('refresh-monitors').addEventListener('click', () => act('refresh'));
byId('target').addEventListener('change', (event) => {
  const value = event.target.value;
  if (value.startsWith('fixed:')) {
    const monitor = value.slice(6);
    if (monitor && state.monitors.some((m) => m.id === monitor)) void act('target', { policy: 'fixed', monitor });
  } else void act('target', { policy: value, monitor: null });
});
launcherFields.forEach((id) => byId(id).addEventListener('change', sendLauncher));
['restore','focus','autostart','verbose'].forEach((id) => byId(id).addEventListener('change', sendBehavior));
byId('reset-position').addEventListener('click', () => act('reset_position', {}, () => showMessage('保存すると固定バーの位置が初期化されます。', true)));
byId('new-shortcut').addEventListener('click', () => openShortcut());
byId('recovery-preview').addEventListener('click', restorePreview);
byId('export').addEventListener('click', () => act('export', {}, (result) => { if (result?.exported) showMessage('保存済みの設定を書き出しました。', true); }));
byId('import').addEventListener('click', importSettings);
byId('open-folder').addEventListener('click', () => act('open_folder'));
byId('save').addEventListener('click', saveEdits);
byId('discard').addEventListener('click', () => {
  if (pending) return;
  confirm('保存していない変更を破棄しますか？', '最後に保存した設定に戻ります。', '変更を破棄', () =>
    applyDialog('discard', {}, () => { invalidLauncher = false; showMessage(); }));
});
byId('dialog-cancel').addEventListener('click', () => { if (!pending) dismiss(); });
dialog.addEventListener('cancel', (event) => {
  event.preventDefault();
  if (!pending) dismiss();
});
byId('dialog-form').addEventListener('submit', (event) => {
  event.preventDefault();
  if (commitDialog && !pending && !dialogSubmit.disabled) void runDialogAction(commitDialog)();
});
document.addEventListener('keydown', (event) => {
  if (event.ctrlKey && !event.altKey && event.key.toLowerCase() === 's' && !dialog.open) {
    event.preventDefault();
    saveEdits();
  }
});
new ResizeObserver(() => { if (state && visited === 'behavior') renderMonitors(); }).observe(content);

const darkScheme = matchMedia('(prefers-color-scheme: dark)');
function syncAppearance() {
  bridge.postMessage({ id: 0, action: 'appearance', dark: darkScheme.matches });
}
darkScheme.addEventListener('change', syncAppearance);

request('ready').then((extra) => {
  onboarding = Boolean(extra.first_run);
  go(extra.page, true);
  redraw();
  syncAppearance();
}).catch((error) => showMessage(error.message));

import { element } from './ui.js';

// JSON chunks can end inside a string, an escape, or a Unicode escape.
// Recover only a string value from a bounded argument preview. This is
// presentation only: it never supplies arguments to an actual tool call.
export function partialString(raw, field) {
  const marker = new RegExp('(?:^|[,{])\\s*"' + field + '"\\s*:\\s*"');
  const match = marker.exec(raw);
  if (!match) return undefined;
  const start = match.index + match[0].length;
  let end = start;
  let escaped = false;
  for (; end < raw.length; end++) {
    const ch = raw[end];
    if (escaped) { escaped = false; continue; }
    if (ch === '\\') { escaped = true; continue; }
    if (ch === '"') {
      try { return JSON.parse(raw.slice(start - 1, end + 1)); }
      catch { return undefined; }
    }
  }
  const prefix = raw.slice(start, end);
  for (let cut = 0; cut <= Math.min(6, prefix.length); cut++) {
    try { return JSON.parse('"' + prefix.slice(0, prefix.length - cut) + '"'); }
    catch { /* the stream may end in an incomplete escape */ }
  }
  return undefined;
}

const format = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 });
const size = bytes => bytes < 1024 ? bytes + ' Б' : format.format(bytes / 1024) + ' КБ';

export function toolPreview(tool) {
  const raw = tool.preview || '';
  const target = tool.target || partialString(raw, 'path') || '';
  if (tool.name === 'write') {
    const code = partialString(raw, 'content');
    return { target, text: code ?? '', label: 'Содержимое файла', ready: code != null };
  }
  if (tool.name === 'edit') {
    const before = partialString(raw, 'oldText');
    const after = partialString(raw, 'newText');
    return { target, text: after ?? before ?? '', label: 'Первая замена · ' + (after != null ? 'Стало' : 'Было') };
  }
  return { target, text: raw, label: 'Аргументы' };
}

// The server decodes saved writes before truncating; legacy history falls
// back to its JSON preview. Never read the current file as a substitute.
export function savedWritePreview(tool) {
  if (tool.name !== 'write') return null;
  if (typeof tool.write_preview?.text === 'string') {
    return { target: tool.target || '', text: tool.write_preview.text, truncated: Boolean(tool.write_preview.truncated) };
  }
  const raw = tool.arguments || '';
  const clipped = raw.endsWith('\n… (вывод сокращён)');
  const source = clipped ? raw.slice(0, -'\n… (вывод сокращён)'.length) : raw;
  let args;
  try { args = JSON.parse(source); } catch { /* legacy preview can be clipped */ }
  const text = typeof args?.content === 'string' ? args.content : partialString(source, 'content');
  return text == null ? null : {
    target: tool.target || args?.path || partialString(source, 'path') || '',
    text, truncated: clipped || !args
  };
}

const clippedMarker = '\n… (вывод сокращён)';

export function savedToolView(tool) {
  let args = {};
  let invalid = false;
  try {
    const parsed = JSON.parse(tool.arguments || '{}');
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) args = parsed;
    else invalid = true;
  } catch { invalid = true; }
  const target = typeof args.path === 'string' ? args.path : tool.target || '';
  const result = typeof tool.result === 'string' ? tool.result : null;
  const clipped = result?.endsWith(clippedMarker) ?? false;
  const output = clipped ? result.slice(0, -clippedMarker.length) : result;
  const fields = [];
  const field = (label, value) => { if (value != null && value !== '') fields.push([label, String(value)]); };
  const positive = n => Number.isSafeInteger(n) && n > 0;
  const view = { target, fields, output, clipped, invalid, kind: 'generic', status: null, write: null, edit: null };
  const success = tool.status === 'ok';
  switch (tool.name) {
    case 'read':
      view.kind = 'read';
      if (positive(args.limit)) {
        const from = positive(args.offset) ? args.offset : 1;
        field('Запрошено', 'Строки ' + from + '–' + (from + args.limit - 1));
      } else if (positive(args.offset)) field('Запрошено', 'С строки ' + args.offset);
      break;
    case 'write':
      view.kind = 'write';
      view.write = savedWritePreview(tool);
      if (success && /^Updated [^\n]+$/.test(output || '')) view.status = 'Файл записан';
      break;
    case 'edit':
      view.kind = 'edit';
      view.edit = tool.edit_preview ?? (Array.isArray(args.edits) ? {
        total: args.edits.length,
        replacements: args.edits.filter(edit => typeof edit?.oldText === 'string' && typeof edit?.newText === 'string').map(edit => ({before:edit.oldText,after:edit.newText,truncated:false}))
      } : null);
      if (success && /^Updated [^\n]+$/.test(output || '')) view.status = 'Изменения сохранены';
      break;
    case 'mkdir':
      view.kind = 'directory';
      if (success && /^Directory ready: [^\n]+$/.test(output || '')) view.status = 'Каталог готов';
      break;
    case 'copy_skill_resource':
      view.kind = 'copy';
      field('Скилл', args.skill);
      field('Источник', args.source);
      if (success && /^Updated [^\n]+$/.test(output || '')) view.status = 'Ресурс скопирован';
      break;
    case 'ls':
      view.kind = 'list';
      field('Лимит', args.limit);
      break;
    case 'find':
    case 'grep':
      view.kind = 'search';
      field('Запрос', args.pattern);
      field('Папка', args.path);
      field('Фильтр', args.glob);
      if (args.ignoreCase === true) field('Регистр', 'Не учитывать');
      if (args.literal === true) field('Поиск', 'Точное совпадение');
      field('Контекст', args.context);
      field('Лимит', args.limit);
      break;
    case 'run_command':
    case 'start_background_command':
    case 'bash':
      view.kind = 'command';
      view.command = typeof args.command === 'string' ? args.command : target;
      break;
    case 'finish_task':
      view.kind = 'finish';
      field('Итог', args.status === 'completed' ? 'Завершено' : args.status === 'blocked' ? 'Заблокировано' : args.status);
      view.summary = typeof args.summary === 'string' ? args.summary : '';
      if (success && output === 'Outcome recorded.') view.status = 'Итог сохранён';
      break;
    default:
      for (const [key, value] of Object.entries(args)) {
        if (['string','number','boolean'].includes(typeof value)) field(key, value);
      }
  }
  return view;
}

function appendFields(body, fields) {
  if (!fields.length) return;
  const list = element('dl', '');
  list.className = 'tool-fields';
  for (const [label, value] of fields) list.append(element('dt', label), element('dd', value));
  body.append(list);
}

function appendOutput(body, title, output, clipped = false, error = false) {
  if (output == null) return;
  const block = element('div', '');
  block.className = 'tool-output';
  block.dataset.error = String(error);
  block.append(element('h4', title), element('pre', output || '(пусто)'));
  if (clipped) {
    const note = element('p', 'Показана часть результата.');
    note.className = 'tool-code-note';
    block.append(note);
  }
  body.append(block);
}

function appendReplacement(body, replacement, index) {
  const diff = element('div', '');
  diff.className = 'tool-diff';
  diff.append(element('h4', 'Замена ' + (index + 1)));
  for (const [side, title, text] of [['before','Было',replacement.before],['after','Стало',replacement.after]]) {
    const part = element('div', '');
    part.className = 'tool-diff-part';
    part.dataset.side = side;
    part.append(element('span', title));
    const pre = element('pre', '');
    pre.append(element('code', text || ''));
    part.append(pre);
    diff.append(part);
  }
  if (replacement.truncated) diff.append(element('p', 'Показана часть замены.'));
  body.append(diff);
}

export function renderSavedTool(body, tool, technicalDetail) {
  const view = savedToolView(tool);
  const error = tool.status === 'error';
  if (view.target && ['directory','copy','list'].includes(view.kind)) {
    const path = element('p', view.target);
    path.className = 'tool-path';
    body.append(path);
  }
  appendFields(body, view.fields);
  if (view.write) {
    const block = createToolCodeBlock();
    block.update(view.write);
    body.append(block.node);
  } else if (view.kind === 'read' && !error && tool.status === 'ok' && view.output != null &&
      view.output !== '[изображение]' && view.output !== '[нетекстовый результат]') {
    const block = createToolCodeBlock();
    block.update({target:view.target,text:view.output,truncated:view.clipped});
    body.append(block.node);
  } else if (view.edit) {
    const count = element('p', 'Замен: ' + view.edit.total);
    count.className = 'tool-inline-status';
    body.append(count);
    view.edit.replacements.forEach((replacement, index) => appendReplacement(body, replacement, index));
    if (view.edit.total > view.edit.replacements.length) {
      const more = element('p', 'Остальные замены сохранены в журнале.');
      more.className = 'tool-inline-status';
      body.append(more);
    }
  } else if (view.command) {
    appendOutput(body, 'Команда', view.command);
  }
  if (view.summary) appendOutput(body, 'Отчёт', view.summary);
  if (error) appendOutput(body, 'Ошибка', view.output, view.clipped, true);
  else if (view.status) {
    const status = element('p', view.status);
    status.className = 'tool-inline-status';
    status.dataset.success = 'true';
    body.append(status);
  } else if (!(view.kind === 'read' && tool.status === 'ok' && view.output != null &&
      view.output !== '[изображение]' && view.output !== '[нетекстовый результат]')) {
    const titles = {list:'Содержимое каталога',search:'Совпадения',command:'Вывод команды'};
    appendOutput(body, titles[view.kind] || 'Результат', view.output, view.clipped);
  }
  if (tool.status === 'pending') {
    const pending = element('p', 'Ожидает результата…');
    pending.className = 'tool-inline-status';
    body.append(pending);
  }
  const summary = element('summary', 'Технические детали');
  const technical = technicalDetail(summary);
  const populate = () => {
    if (!technical.open || technical.querySelector('.tool-technical-content')) return;
    const raw = element('div', '');
    raw.className = 'tool-technical-content';
    appendOutput(raw, 'Аргументы (сохранённый предпросмотр)', tool.arguments || '{}');
    if (tool.write_preview) appendOutput(raw, 'Содержимое (предпросмотр)', tool.write_preview.text, tool.write_preview.truncated);
    if (tool.result != null) appendOutput(raw, 'Результат (предпросмотр)', tool.result);
    technical.append(raw);
  };
  technical.addEventListener('toggle', populate);
  populate();
  body.append(technical);
}

export function codeLanguage(path) {
  const name = path.split(/[\\/]/).pop().toLowerCase();
  if (name === 'dockerfile') return 'Dockerfile';
  if (name === 'makefile') return 'Makefile';
  const ext = name.includes('.') ? name.split('.').pop() : '';
  const names = {js:'JavaScript',jsx:'JSX',mjs:'JavaScript',ts:'TypeScript',tsx:'TSX',html:'HTML',htm:'HTML',css:'CSS',scss:'SCSS',json:'JSON',rs:'Rust',py:'Python',kt:'Kotlin',kts:'Kotlin',java:'Java',swift:'Swift',cpp:'C++',cc:'C++',hpp:'C++',h:'C/C++',c:'C',cs:'C#',go:'Go',md:'Markdown',yaml:'YAML',yml:'YAML',toml:'TOML',xml:'XML',svg:'SVG',sh:'Shell',ps1:'PowerShell',sql:'SQL',gd:'GDScript'};
  return Object.hasOwn(names, ext) ? names[ext] : 'Текст';
}

export function createToolCodeBlock({live = false} = {}) {
  const node = element('div', '');
  node.className = 'tool-code-block';
  const header = element('div', '');
  header.className = 'tool-code-header';
  const name = element('span', '');
  name.className = 'tool-code-filename';
  const language = element('span', '');
  language.className = 'tool-code-language';
  header.append(name, language);
  const pre = element('pre', '');
  pre.dir = 'ltr';
  const code = element('code', '');
  pre.append(code);
  const note = element('p', '');
  note.className = 'tool-code-note';
  node.append(header, pre, note);
  function update(view, follow = false) {
    const target = view.target || '';
    const filename = target.split(/[\\/]/).pop() || 'Файл';
    setText(name, filename);
    name.title = target;
    setText(language, codeLanguage(target));
    const nearEnd = pre.scrollHeight - pre.scrollTop - pre.clientHeight < 24;
    if (setText(code, view.text) && follow && nearEnd) pre.scrollTop = pre.scrollHeight;
    note.hidden = !view.truncated && view.text !== '';
    setText(note, view.truncated ? (live ? 'Предпросмотр ограничен; генерация продолжается.' : 'Показано начало файла; содержимое вызова сокращено.') : live ? 'Ожидаем содержимое файла…' : 'Пустой файл');
  }
  return { node, update };
}

function setText(node, value) {
  if (node.textContent === value) return false;
  node.textContent = value;
  return true;
}

export function createLiveTools(container, changed) {
  let delegationId;
  let rows = new Map();
  let lastKey;
  function createRow() {
    const row = element('details', '');
    row.className = 'live-tool';
    row.open = true;
    const summary = element('summary', '');
    const icon = element('span', '');
    icon.className = 'live-tool-icon';
    icon.setAttribute('aria-hidden', 'true');
    const title = element('span', '');
    title.className = 'live-tool-title';
    const name = element('span', '');
    name.className = 'live-tool-name';
    const state = element('span', '');
    state.className = 'live-tool-state';
    title.append(name, state);
    const metrics = element('span', '');
    metrics.className = 'live-tool-metrics';
    const count = element('span', '');
    const speed = element('span', '');
    metrics.append(count, speed);
    const chevron = element('span', '');
    chevron.className = 'disclosure-chevron';
    chevron.setAttribute('aria-hidden', 'true');
    summary.append(icon, title, metrics, chevron);
    const body = element('div', '');
    body.className = 'live-tool-body';
    const label = element('p', '');
    const code = element('pre', '');
    code.dir = 'ltr';
    const note = element('p', 'Предпросмотр ограничен; генерация продолжается.');
    note.hidden = true;
    const fileCode = createToolCodeBlock({live: true});
    fileCode.node.hidden = true;
    body.append(fileCode.node, label, code, note);
    row.append(summary, body);
    return { row, name, state, count, speed, body, label, code, note, chevron, fileCode };
  }
  function render(delegation) {
    const [id, task] = delegation ?? [];
    const tools = task?.live_tools ?? [];
    const key = JSON.stringify([id, tools]);
    if (key === lastKey) return;
    lastKey = key;
    if (id !== delegationId) {
      delegationId = id;
      container.replaceChildren();
      rows.clear();
    }
    const next = new Map();
    for (const [index, tool] of tools.entries()) {
      const item = rows.get(tool.id) ?? createRow();
      const { row, name, state, count, speed, body, label, code, note, chevron, fileCode } = item;
      const view = toolPreview(tool);
      row.dataset.phase = tool.phase;
      const title = tool.name + (view.target ? ' · ' + view.target : '');
      setText(name, title);
      name.title = title;
      setText(state, tool.phase === 'generating' ? (tool.name === 'write' ? 'Генерация содержимого' : 'Генерация аргументов')
        : tool.phase === 'queued' ? 'Аргументы готовы · ожидает выполнения' : 'Выполнение инструмента');
      const elapsed = tool.generation_ms;
      setText(count, tool.argument_bytes > 0 ? size(tool.argument_bytes) + (elapsed != null ? ' · ' + format.format(elapsed / 1000) + ' с' : '') : '');
      const rate = elapsed >= 250 && tool.argument_bytes > 0 ? size(Math.round(tool.argument_bytes * 1000 / elapsed)) + '/с' : '';
      setText(speed, tool.phase === 'generating' ? rate : '');
      const write = tool.name === 'write';
      const edit = tool.name === 'edit';
      const show = (write || edit || Boolean(view.text)) && tool.phase !== 'executing';
      body.hidden = !show;
      chevron.hidden = !show;
      fileCode.node.hidden = !write || !show;
      label.hidden = code.hidden = !show || write;
      note.hidden = !show || write || !tool.preview_truncated;
      if (write && show) {
        fileCode.update({ ...view, truncated: tool.preview_truncated }, row.open);
      } else if (show) {
        setText(label, edit && !view.text ? 'Ожидаем содержимое замены…' : view.label);
        code.hidden = edit && !view.text;
        const nearEnd = code.scrollHeight - code.scrollTop - code.clientHeight < 24;
        if (setText(code, view.text) && row.open && nearEnd) code.scrollTop = code.scrollHeight;
      } else {
        setText(code, '');
      }
      next.set(tool.id, item);
      if (container.children[index] !== row) container.insertBefore(row, container.children[index] ?? null);
    }
    for (const [key, item] of rows) if (!next.has(key)) item.row.remove();
    rows = next;
    container.hidden = tools.length === 0;
    changed();
  }
  return { render };
}

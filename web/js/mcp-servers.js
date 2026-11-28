import { element, get, iconButton, settingsSwitch, trashIcon } from './ui.js';

const endpoint = '/settings/mcp-servers';
const normalizedName = name => name.replace(/-/g, '_').toLowerCase();

export function replaceServer(config, oldName, name, server) {
  if (!/^[A-Za-z0-9_-]+$/.test(name)) throw new Error('Имя: только латинские буквы, цифры, _ и -.');
  const entries = Object.entries(config.mcpServers).filter(([key]) => key !== oldName);
  if (entries.some(([key]) => normalizedName(key) === normalizedName(name))) {
    throw new Error('Сервер с таким именем уже существует.');
  }
  return { mcpServers: Object.fromEntries([...entries, [name, server]]) };
}

export function buildServer(fields) {
  const local = fields.transport === 'stdio';
  const server = { type: local ? 'stdio' : fields.httpType === 'http' ? 'http' : 'streamable-http',
    enabled: fields.enabled, exposure: fields.exposure };
  if (fields.description !== '') server.description = fields.description;
  if (fields.timeout !== '') {
    const timeout = Number(fields.timeout);
    if (!Number.isInteger(timeout) || timeout < 1 || timeout > 4294967295) throw new Error('Таймаут должен быть положительным целым числом.');
    server.timeout = timeout;
  }
  if (Object.keys(fields.tools).length) server.toolExposure = fields.tools;
  if (local) {
    if (!fields.command.trim()) throw new Error('Укажи команду запуска.');
    server.command = fields.command;
    if (fields.args.length) server.args = fields.args;
    if (fields.cwd !== '') server.cwd = fields.cwd;
    if (Object.keys(fields.env).length) server.env = fields.env;
  } else {
    const url = new URL(fields.url);
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || fields.url.includes('#')) {
      throw new Error('Укажи HTTP(S) URL без логина, пароля и фрагмента.');
    }
    server.url = fields.url;
    if (Object.keys(fields.headers).length) server.headers = fields.headers;
  }
  return server;
}

export function createMcpServers() {
  const list = get('mcp-servers-list');
  const status = get('mcp-servers-status');
  const add = get('mcp-servers-add');
  const reload = get('mcp-servers-reload');
  const jsonButton = get('mcp-servers-json');
  const dialog = get('mcp-server-dialog');
  const form = get('mcp-server-form');
  const rawDialog = get('mcp-json-dialog');
  const rawForm = get('mcp-json-form');
  const rawEditor = get('mcp-json-editor');
  const field = name => get('mcp-server-' + name);
  const local = field('local');
  const remote = field('remote');
  let online = false;
  let busy = false;
  let available = true;
  let config;
  let etag;
  let refreshPending = false;
  let editingName;
  let httpType;
  let dirty = false;
  let rawSaved = '';
  let listKey;

  function report(message = '', error = false, target = status) {
    target.textContent = message;
    target.dataset.error = String(error);
  }

  function renderList() {
    const entries = Object.entries(config?.mcpServers ?? {});
    const key = JSON.stringify(entries);
    if (key !== listKey) {
      listKey = key;
      list.replaceChildren(...entries.map(([name, server]) => {
        const row = element('li', '');
        row.className = 'settings-row';
        const info = element('div', '');
        info.className = 'settings-info';
        info.append(element('strong', name));
        const kind = element('span', server.url != null ? 'Streamable HTTP' : 'stdio · локальный');
        kind.className = 'mcp-server-type';
        info.append(kind);
        if (server.description) info.append(element('p', server.description));
        let address = server.command ?? '';
        if (server.url) {
          try { const url = new URL(server.url); address = url.origin + url.pathname; }
          catch { address = 'HTTP'; }
        }
        const endpointText = element('code', address);
        info.append(endpointText);
        const actions = element('div', '');
        actions.className = 'settings-actions';
        const toggle = settingsSwitch(name, server.enabled !== false);
        toggle.onclick = () => {
          if (!canEdit()) return;
          void persist(replaceServer(config, name, name, { ...server, enabled: server.enabled === false }));
        };
        const edit = iconButton('Редактировать ' + name, 'm15 5 4 4M4 20l4-1 13-13-4-4L4 15z');
        edit.onclick = () => openServer(name);
        const remove = iconButton('Удалить ' + name, trashIcon);
        remove.dataset.delete = '';
        remove.onclick = () => {
          if (!canEdit() || !window.confirm('Удалить MCP-сервер «' + name + '» из конфигурации Lyra?')) return;
          void persist({ mcpServers: Object.fromEntries(Object.entries(config.mcpServers).filter(([key]) => key !== name)) });
        };
        actions.append(toggle, edit, remove);
        row.append(info, actions);
        return row;
      }));
    }
    get('mcp-servers-empty').hidden = !config || entries.length > 0;
    get('mcp-servers-count').textContent = config
      ? 'Включено: ' + entries.filter(([, server]) => server.enabled !== false).length + ' из ' + entries.length
      : 'Нет данных';
  }

  function canEdit() { return online && available && !busy && config != null; }

  function render() {
    renderList();
    add.disabled = jsonButton.disabled = !canEdit();
    reload.disabled = !online || busy || !available;
    for (const button of list.querySelectorAll('button')) button.disabled = !canEdit();
    for (const node of [...form.elements, ...rawForm.elements]) node.disabled = !canEdit();
    if (canEdit()) updateTransport();
    field('cancel').disabled = get('mcp-json-cancel').disabled = busy;
    field('save').disabled = !canEdit();
    get('mcp-json-save').disabled = !canEdit() || rawEditor.value === rawSaved;
  }

  function updateTransport() {
    const isLocal = field('transport').value === 'stdio';
    local.hidden = !isLocal;
    remote.hidden = isLocal;
    local.disabled = !isLocal;
    remote.disabled = isLocal;
  }

  async function request(next, target = status) {
    busy = true;
    report(next ? 'Сохраняем…' : 'Загружаем…', false, target);
    render();
    try {
      const headers = { 'Content-Type': 'application/json' };
      if (etag) headers['If-Match'] = etag;
      const response = await fetch(endpoint, next ? { method: 'POST', headers, body: JSON.stringify(next) } : undefined);
      if (!response.ok) {
        if (response.status === 403) { available = false; config = undefined; }
        throw new Error(await response.text() || 'HTTP ' + response.status);
      }
      config = await response.json();
      etag = response.headers.get('ETag');
      report(next ? 'Сохранено.' : '', false, status);
      if (target !== status) report('', false, target);
      return true;
    } catch (error) {
      report(error.message, true, target);
      return false;
    } finally {
      busy = false;
      render();
      if (refreshPending) refresh();
    }
  }

  async function persist(next, target = status) {
    if (!canEdit()) return false;
    return request(next, target);
  }

  function dynamicRow(container, kind, key = '', value = '') {
    const row = element('div', '');
    row.className = 'mcp-dynamic-row';
    const input = (label, initial, type = 'text') => {
      const node = element('input', '');
      node.type = type;
      node.placeholder = label;
      node.setAttribute('aria-label', label);
      node.autocomplete = 'off';
      node.spellcheck = false;
      node.value = initial;
      return node;
    };
    if (kind === 'arg') row.append(input('Аргумент', key));
    else {
      row.dataset.pair = '';
      row.append(input(kind === 'tool' ? 'Имя инструмента' : 'Имя', key));
      if (kind === 'tool') {
        const select = element('select', '');
        select.setAttribute('aria-label', 'Доступность инструмента');
        for (const [v, label] of [['direct', 'Открыт'], ['hidden', 'Скрыт']]) {
          const option = element('option', label);
          option.value = v;
          select.append(option);
        }
        select.value = value || 'direct';
        row.append(select);
      } else {
        row.dataset.secret = '';
        const secret = input('Значение', value, 'password');
        const reveal = element('button', '◉');
        reveal.className = 'terminal-mode';
        reveal.type = 'button';
        reveal.title = 'Показать значение';
        reveal.setAttribute('aria-label', reveal.title);
        reveal.onclick = () => {
          secret.type = secret.type === 'password' ? 'text' : 'password';
          reveal.title = secret.type === 'password' ? 'Показать значение' : 'Скрыть значение';
          reveal.setAttribute('aria-label', reveal.title);
        };
        row.append(secret, reveal);
      }
    }
    const remove = element('button', '×');
    remove.type = 'button';
    remove.className = 'terminal-mode';
    remove.setAttribute('aria-label', 'Удалить строку');
    remove.onclick = () => { row.remove(); dirty = true; };
    row.append(remove);
    container.append(row);
    return row;
  }

  function readPairs(name) {
    const entries = [];
    const seen = new Set();
    for (const row of field(name).children) {
      const [key, value] = row.querySelectorAll('input, select');
      if (!key.value) throw new Error('Заполни имя или удали пустую строку.');
      const unique = name === 'headers' ? key.value.toLowerCase() : key.value;
      if (seen.has(unique)) throw new Error('Повторяющееся имя: ' + key.value);
      seen.add(unique);
      entries.push([key.value, value.value]);
    }
    return Object.fromEntries(entries);
  }

  function openServer(name) {
    if (!canEdit()) return;
    editingName = name;
    const server = name == null ? {} : config.mcpServers[name];
    httpType = server.type;
    field('title').textContent = name == null ? 'Добавить MCP-сервер' : 'Настройки MCP-сервера';
    field('name').value = name ?? '';
    field('description').value = server.description ?? '';
    field('transport').value = server.url != null ? 'streamable-http' : 'stdio';
    field('command').value = server.command ?? '';
    field('cwd').value = server.cwd ?? '';
    field('url').value = server.url ?? '';
    field('enabled').checked = server.enabled !== false;
    field('timeout').value = server.timeout ?? '';
    field('exposure').value = server.exposure ?? 'direct';
    for (const name of ['args', 'env', 'headers', 'tools']) field(name).replaceChildren();
    for (const arg of server.args ?? []) dynamicRow(field('args'), 'arg', arg);
    for (const [key, value] of Object.entries(server.env ?? {})) dynamicRow(field('env'), 'env', key, value);
    for (const [key, value] of Object.entries(server.headers ?? {})) dynamicRow(field('headers'), 'header', key, value);
    for (const [key, value] of Object.entries(server.toolExposure ?? {})) dynamicRow(field('tools'), 'tool', key, value);
    field('advanced').open = server.timeout != null || server.exposure === 'hidden' || Object.keys(server.toolExposure ?? {}).length > 0;
    report('', false, field('form-status'));
    dirty = false;
    render();
    dialog.showModal();
  }

  for (const [button, container, kind] of [['add-arg','args','arg'],['add-env','env','env'],['add-header','headers','header'],['add-tool','tools','tool']]) {
    field(button).onclick = () => {
      if (!canEdit()) return;
      dynamicRow(field(container), kind).querySelector('input')?.focus();
      dirty = true;
    };
  }
  form.oninput = () => { dirty = true; };
  field('transport').onchange = () => { dirty = true; updateTransport(); };
  form.onsubmit = async event => {
    event.preventDefault();
    if (!canEdit()) return;
    const target = field('form-status');
    try {
      const transport = field('transport').value;
      const isLocal = transport === 'stdio';
      const server = buildServer({
        transport, httpType, description: field('description').value,
        enabled: field('enabled').checked, timeout: field('timeout').value,
        exposure: field('exposure').value, tools: readPairs('tools'),
        command: field('command').value, cwd: field('cwd').value,
        args: [...field('args').querySelectorAll('input')].map(node => node.value),
        env: isLocal ? readPairs('env') : {}, headers: isLocal ? {} : readPairs('headers'),
        url: field('url').value,
      });
      const next = replaceServer(config, editingName, field('name').value.trim(), server);
      if (await persist(next, target)) { dirty = false; dialog.close(); }
    } catch (error) { report(error.message, true, target); }
  };

  function refresh() {
    if (!online || !available) return;
    if (busy || dialog.open || rawDialog.open) {
      refreshPending = true;
      return;
    }
    refreshPending = false;
    void request();
  }

  function cancelAllowed(changed) {
    return !busy && (!changed || window.confirm('Закрыть без сохранения изменений?'));
  }
  dialog.oncancel = event => { if (!cancelAllowed(dirty)) event.preventDefault(); };
  field('cancel').onclick = () => { if (cancelAllowed(dirty)) dialog.close(); };
  rawDialog.oncancel = event => { if (!cancelAllowed(rawEditor.value !== rawSaved)) event.preventDefault(); };
  get('mcp-json-cancel').onclick = () => { if (cancelAllowed(rawEditor.value !== rawSaved)) rawDialog.close(); };
  rawDialog.addEventListener('close', () => {
    rawEditor.value = rawSaved = '';
    if (refreshPending) refresh();
  });
  dialog.addEventListener('close', () => {
    dirty = false;
    field('env').replaceChildren();
    field('headers').replaceChildren();
    if (refreshPending) refresh();
  });
  add.onclick = () => openServer();
  reload.onclick = () => { if (online && !busy && available) void request(); };
  jsonButton.onclick = () => {
    if (!canEdit()) return;
    rawEditor.value = rawSaved = JSON.stringify(config, null, 2);
    report('', false, get('mcp-json-status'));
    render();
    rawDialog.showModal();
  };
  rawEditor.oninput = render;
  rawForm.onsubmit = async event => {
    event.preventDefault();
    if (!canEdit() || rawEditor.value === rawSaved) return;
    const target = get('mcp-json-status');
    try {
      const next = JSON.parse(rawEditor.value);
      if (!next || typeof next.mcpServers !== 'object' || !next.mcpServers || Array.isArray(next.mcpServers)) throw new Error('Ожидается объект с ключом mcpServers.');
      if (await persist(next, target)) { rawSaved = rawEditor.value; rawDialog.close(); }
    } catch (error) { report(error.message, true, target); }
  };

  return {
    refresh,
    setOnline(value) {
      const connected = value && !online;
      online = value;
      render();
      if (connected) refresh();
    },
  };
}

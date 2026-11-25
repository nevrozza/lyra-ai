const get = (id) => document.getElementById(id);
const runButton = get('run');
const pinModel = get('pin-model');
const unloadModel = get('unload-model');
const modelList = get('model-list');
const ramExpand = get('ram-expand');
const connection = get('connection');
const connectionLabel = document.createTextNode('Подключение');
connection.lastChild.replaceWith(connectionLabel);
const timeline = get('timeline');
const oneDecimal = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 });
const whole = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 0 });
const clock = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
const farFuture = 100 * 365 * 24 * 60 * 60 * 1000;
let stream;
let state;
let metrics;
let lastTimeline;
let pending = false;
let polling = false;
let metricsTimer;
let metricsRefreshPending = false;
let modelSubmitting;
let modelRequestError = '';
let selectingModel;
let modelRows = new Map();
let ramTimer;
let readingRam = false;
let detailedRam;

function element(tag, text) {
  const node = document.createElement(tag);
  node.textContent = text;
  return node;
}

function connectionState(value, label) {
  connection.dataset.state = value;
  connectionLabel.textContent = label;
  updateControls();
}

function updateControls() {
  const blocked = !state || connection.dataset.state !== 'online' || pending || selectingModel != null ||
    modelSubmitting != null || state.model_action != null || state.status === 'running';
  const installed = metrics?.installed_models?.includes(state?.model);
  runButton.disabled = blocked || !installed;
  pinModel.disabled = blocked || !installed || metrics?.models == null;
  unloadModel.disabled = blocked || !installed || !loadedModel(metrics?.models, state?.model);
  for (const { button } of modelRows.values()) button.disabled = blocked;
}

function duration(ms) {
  const seconds = Math.floor(Math.max(0, ms) / 1000);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}

function renderTime() {
  const started = state?.started_at;
  get('elapsed').textContent = started == null ? '—' : duration(state.elapsed_ms ?? (Date.now() - started));
}

function renderRun(next) {
  const justStarted = state?.status !== 'running' && next.status === 'running';
  const actionFinished = (modelSubmitting != null || state?.model_action != null) && next.model_action == null;
  state = next;
  if (selectingModel === next.model) selectingModel = undefined;
  pending = false;
  modelSubmitting = undefined;
  updateControls();
  runButton.textContent = next.status === 'running' ? 'Выполняется…' : 'Запустить проверку';
  const phase = get('phase');
  phase.dataset.status = next.status;
  if (phase.textContent !== next.phase) phase.textContent = next.phase;
  get('model-name').textContent = next.model ?? '';
  get('workspace-row').hidden = !next.workspace;
  get('workspace').textContent = next.workspace ?? '';
  get('result').hidden = !next.result;
  get('result').textContent = next.result ?? '';
  get('thinking').hidden = !next.thinking;
  if (get('trace').textContent !== next.thinking) get('trace').textContent = next.thinking;
  get('empty').hidden = next.timeline.length > 0;
  const timelineKey = JSON.stringify(next.timeline);
  if (lastTimeline !== timelineKey) {
    lastTimeline = timelineKey;
    timeline.replaceChildren(...next.timeline.map((entry) => {
      const row = element('li', '');
      row.dataset.kind = entry.kind;
      row.append(element('time', `+${duration(entry.at_ms)}`), element('span', entry.text));
      return row;
    }));
  }
  renderTime();
  renderModel(metrics?.models);
  if (justStarted || actionFinished) scheduleMetrics(0);
}

function connect() {
  stream = new EventSource('/events');
  stream.onopen = () => connectionState('online', 'Подключено');
  stream.onerror = () => connectionState('waiting', 'Переподключение');
  stream.onmessage = (event) => renderRun(JSON.parse(event.data));
}

runButton.addEventListener('click', async () => {
  pending = true;
  updateControls();
  try {
    const response = await postModel('/run', state.model);
    if (!response.ok) throw new Error(modelResponseError(response));
  } catch (error) {
    pending = false;
    updateControls();
    get('result').hidden = false;
    get('result').textContent = `Не удалось запустить проверку. ${error.message}`;
  }
});

pinModel.addEventListener('change', () => {
  const pinned = pinModel.checked;
  changeModel(pinned ? 'pin' : 'unpin', '/model/pin', { pinned });
});

unloadModel.addEventListener('click', () => changeModel('unload', '/model/unload'));

function postModel(path, selected, payload) {
  const options = { method: 'POST', headers: { 'X-Lyra-Model': selected ?? '' } };
  if (payload) {
    options.headers['Content-Type'] = 'application/json';
    options.body = JSON.stringify(payload);
  }
  return fetch(path, options);
}

function modelResponseError(response) {
  if (response.status === 409) return 'Модель изменилась или выполняется другая операция.';
  if (response.status === 404) return 'Модель больше не установлена.';
  if (response.status === 503) return 'Ollama недоступна.';
  return `Запрос завершился ошибкой (${response.status}).`;
}

async function selectModel(model) {
  if (model === state?.model) return;
  selectingModel = model;
  modelRequestError = '';
  renderModel(metrics?.models);
  try {
    const response = await postModel('/model/select', state?.model, { model });
    if (!response.ok) throw new Error(modelResponseError(response));
  } catch (error) {
    selectingModel = undefined;
    modelRequestError = error.message;
    renderModel(metrics?.models);
    scheduleMetrics(0);
  }
}

async function changeModel(action, path, payload) {
  modelSubmitting = action;
  modelRequestError = '';
  renderModel(metrics?.models);
  try {
    const response = await postModel(path, state.model, payload);
    if (!response.ok) throw new Error(modelResponseError(response));
  } catch (error) {
    modelSubmitting = undefined;
    modelRequestError = error.message;
    renderModel(metrics?.models);
  }
}

function gib(value) {
  return `${oneDecimal.format(value / 2 ** 30)} ГиБ`;
}

function memoryAmount(bytes) {
  return bytes >= 2 ** 30 ? gib(bytes) : `${whole.format(bytes / 2 ** 20)} МиБ`;
}

function memoryLabel(value) {
  return value ? `${oneDecimal.format(value.used / 2 ** 30)} / ${gib(value.total)}` : '—';
}

function memoryPercent(value) {
  return value?.total ? value.used / value.total * 100 : null;
}

function meter(node, value) {
  node.hidden = value == null;
  if (value != null) node.value = value;
}

function loadedModel(models, name) {
  return models?.find((model) => model.name === name || model.name === `${name}:latest` || `${model.name}:latest` === name);
}

function renderModelList(names, loaded) {
  const installed = names ?? [];
  if (installed.length !== modelRows.size || installed.some((name) => !modelRows.has(name))) {
    modelRows = new Map(installed.map((name) => {
      const row = element('li', '');
      const button = element('button', '');
      const status = element('span', '');
      button.type = 'button';
      button.append(element('span', name), status);
      button.addEventListener('click', () => selectModel(name));
      row.append(button);
      return [name, { row, button, status }];
    }));
    modelList.replaceChildren(...Array.from(modelRows.values(), ({ row }) => row));
  }
  for (const [name, { button, status }] of modelRows) {
    const selected = name === state?.model;
    const resident = loadedModel(loaded, name);
    button.setAttribute('aria-pressed', String(selected));
    status.textContent = selected && state?.model_working ? 'Работает' :
      resident ? 'В памяти' : loaded == null ? '—' : 'На диске';
  }
  modelList.hidden = installed.length === 0;
  const message = get('model-list-message');
  message.hidden = installed.length > 0;
  message.textContent = names === undefined ? 'Получаем список моделей…' :
    names === null ? 'Список Ollama недоступен.' : 'Локальных моделей пока нет. Добавьте модель через ollama pull.';
}

function renderModel(models) {
  const names = metrics?.installed_models;
  renderModelList(names, models);
  get('selected-model').hidden = !names?.includes(state?.model);
  const loaded = loadedModel(models, state?.model);
  const action = modelSubmitting ?? state?.model_action;
  const expiry = Date.parse(loaded?.expires_at);
  const remaining = expiry - Date.now();
  // Ollama represents indefinite retention with a deadline centuries ahead.
  const pinned = Boolean(loaded && (Number.isFinite(expiry) && expiry > 0 ? remaining > farFuture : state?.pinned));
  const dot = get('model-dot');
  const status = get('model-state');
  let mode = 'idle';
  let label = 'Модель выгружена';
  if (state?.model_working) {
    mode = 'running';
    label = 'Работает';
  } else if (models === null) {
    mode = 'unknown';
    label = 'Проверьте, запущена ли Ollama';
  } else if (action === 'unload') {
    mode = 'loading';
    label = 'Выгружается';
  } else if (!loaded && (state?.status === 'running' || action === 'pin')) {
    mode = 'loading';
    label = 'Загружается';
  } else if (loaded) {
    mode = 'resident';
    label = pinned ? 'В памяти · закреплена' : 'В памяти';
  } else if (models === undefined) {
    mode = 'unknown';
    label = 'Проверяем Ollama…';
  }
  dot.dataset.state = mode;
  dot.parentElement.title = label;
  status.classList.toggle('visually-hidden', mode === 'idle');
  if (status.textContent !== label) status.textContent = label;
  const countdown = get('model-countdown');
  countdown.textContent = mode === 'resident' && !pinned && remaining > 0 ? `· до выгрузки ≈ ${duration(remaining)}` : '';
  get('ollama').textContent = models === null ? 'Ollama недоступна' : 'Ollama';
  pinModel.checked = action === 'pin' || (action !== 'unpin' && pinned);
  const modelError = modelRequestError || state?.model_error;
  get('model-error').hidden = !modelError;
  get('model-error').textContent = modelError ?? '';
  get('model-details').hidden = !loaded;
  if (loaded) {
    get('context').textContent = loaded.context_length != null ? `${whole.format(loaded.context_length)} токенов` : '—';
    get('model-vram').textContent = loaded.size_vram != null ? gib(loaded.size_vram) : '—';
  }
  updateControls();
}

function gpuCard(gpu) {
  const node = element('div', '');
  node.className = 'gpu';
  node.append(element('strong', gpu.name));
  const entries = [
    ['GPU', gpu.load != null ? `${gpu.load}%` : '—', gpu.load],
    ['VRAM', memoryLabel(gpu.vram), memoryPercent(gpu.vram)],
    ['Температура', gpu.temperature != null ? `${gpu.temperature} °C` : '—', null],
  ];
  for (const [name, label, value] of entries) {
    const row = element('div', '');
    row.className = 'metric';
    row.append(element('span', name), element('output', label));
    if (value != null) {
      const bar = element('meter', '');
      bar.max = 100;
      bar.setAttribute('aria-label', `${gpu.name} ${name}`);
      meter(bar, value);
      row.append(bar);
    }
    node.append(row);
  }
  return node;
}

function renderMetrics(next) {
  metrics = next;
  get('sampled').textContent = clock.format(new Date());
  get('cpu-value').textContent = next.cpu_percent == null ? '—' : `${whole.format(next.cpu_percent)}%`;
  meter(get('cpu'), next.cpu_percent);
  renderRamTotal((ramExpand.open && detailedRam) || next.ram);
  get('gpus').replaceChildren(...next.gpus.map(gpuCard));
  get('no-gpu').hidden = next.gpus.length > 0;
  get('no-gpu').textContent = 'Данные NVIDIA GPU недоступны.';
  renderModel(next.models);
}

function renderRamTotal(ram) {
  get('ram-value').textContent = memoryLabel(ram);
  meter(get('ram'), memoryPercent(ram));
}

function scheduleMetrics(delay) {
  clearTimeout(metricsTimer);
  if (document.hidden) return;
  if (polling) {
    metricsRefreshPending ||= delay === 0;
  } else {
    metricsTimer = setTimeout(readMetrics, delay);
  }
}

async function readMetrics() {
  if (document.hidden) return;
  polling = true;
  try {
    const response = await fetch('/metrics');
    if (!response.ok) throw new Error();
    renderMetrics(await response.json());
  } catch {
    get('sampled').textContent = 'Нет связи';
  } finally {
    polling = false;
    const delay = metricsRefreshPending ? 0 : (state?.status === 'running' ? 5_000 : 15_000);
    metricsRefreshPending = false;
    scheduleMetrics(delay);
  }
}

function scheduleRam(delay) {
  clearTimeout(ramTimer);
  if (!document.hidden && ramExpand.open && !readingRam) ramTimer = setTimeout(readRam, delay);
}

async function readRam() {
  if (document.hidden || !ramExpand.open) return;
  readingRam = true;
  const list = get('ram-list');
  const message = get('ram-message');
  try {
    const response = await fetch('/ram');
    if (!response.ok) throw new Error();
    const usage = await response.json();
    if (!ramExpand.open || document.hidden) return;
    const apps = usage.applications;
    detailedRam = usage.physical;
    renderRamTotal(detailedRam ?? metrics?.ram);
    renderRamRow('ram-windows', usage.windows_bytes);
    renderRamRow('ram-other', usage.other_process_bytes || null);
    renderRamRow('ram-unattributed', usage.unattributed_bytes);
    list.replaceChildren(...(apps ?? []).map((app) => {
      const row = element('li', '');
      row.append(element('span', app.name), element('output', memoryAmount(app.bytes)));
      return row;
    }));
    message.hidden = Boolean(apps?.length);
    message.textContent = apps === null ? 'Данные по приложениям недоступны.' : 'Нет приложений от 300 МиБ.';
  } catch {
    detailedRam = undefined;
    renderRamTotal(metrics?.ram);
    for (const id of ['ram-windows', 'ram-other', 'ram-unattributed']) renderRamRow(id, null);
    list.replaceChildren();
    message.hidden = false;
    message.textContent = 'Не удалось получить список приложений.';
  } finally {
    readingRam = false;
    scheduleRam(5_000);
  }
}

function renderRamRow(id, bytes) {
  get(id).hidden = bytes == null;
  if (bytes != null) get(`${id}-value`).textContent = memoryAmount(bytes);
}

ramExpand.addEventListener('toggle', () => {
  if (!ramExpand.open) {
    detailedRam = undefined;
    renderRamTotal(metrics?.ram);
  }
  scheduleRam(0);
});

document.addEventListener('visibilitychange', () => {
  if (document.hidden) {
    stream?.close();
    stream = undefined;
    clearTimeout(metricsTimer);
    clearTimeout(ramTimer);
  } else {
    if (!stream) connect();
    scheduleMetrics(0);
    scheduleRam(0);
  }
});

setInterval(() => {
  if (document.hidden) return;
  if (state?.status === 'running') renderTime();
  if (get('model-countdown').textContent) renderModel(metrics?.models);
}, 1_000);

if (!document.hidden) {
  connect();
  scheduleMetrics(0);
}

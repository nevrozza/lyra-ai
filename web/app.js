const get = (id) => document.getElementById(id);
const runButton = get('run');
const pinModel = get('pin-model');
const unloadModel = get('unload-model');
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
  const blocked = !state || connection.dataset.state !== 'online' || modelSubmitting != null || state.model_action != null || state.status === 'running';
  runButton.disabled = blocked || pending;
  pinModel.disabled = blocked || pending || metrics?.models == null;
  unloadModel.disabled = blocked || pending || !loadedModel(metrics?.models);
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
  pending = false;
  modelSubmitting = undefined;
  updateControls();
  runButton.textContent = next.status === 'running' ? 'Выполняется…' : 'Запустить проверку';
  const phase = get('phase');
  phase.dataset.status = next.status;
  if (phase.textContent !== next.phase) phase.textContent = next.phase;
  get('model-name').textContent = next.model;
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
    const response = await fetch('/run', { method: 'POST' });
    if (!response.ok && response.status !== 409) throw new Error(`HTTP ${response.status}`);
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

async function changeModel(action, path, payload) {
  modelSubmitting = action;
  modelRequestError = '';
  renderModel(metrics?.models);
  try {
    const options = { method: 'POST' };
    if (payload) {
      options.headers = { 'Content-Type': 'application/json' };
      options.body = JSON.stringify(payload);
    }
    const response = await fetch(path, options);
    if (!response.ok) throw new Error(response.status === 409 ? 'Дождитесь завершения текущей операции.' : `Запрос завершился ошибкой (${response.status}).`);
  } catch (error) {
    modelSubmitting = undefined;
    modelRequestError = error.message;
    renderModel(metrics?.models);
  }
}

function gib(value) {
  return `${oneDecimal.format(value / 2 ** 30)} ГиБ`;
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

function loadedModel(models) {
  return models?.find((model) => model.name === state?.model || model.name === `${state?.model}:latest`);
}

function renderModel(models) {
  const loaded = loadedModel(models);
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
  get('ram-value').textContent = memoryLabel(next.ram);
  meter(get('cpu'), next.cpu_percent);
  meter(get('ram'), memoryPercent(next.ram));
  get('gpus').replaceChildren(...next.gpus.map(gpuCard));
  get('no-gpu').hidden = next.gpus.length > 0;
  get('no-gpu').textContent = 'Данные NVIDIA GPU недоступны.';
  renderModel(next.models);
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

document.addEventListener('visibilitychange', () => {
  if (document.hidden) {
    stream?.close();
    stream = undefined;
    clearTimeout(metricsTimer);
  } else {
    if (!stream) connect();
    scheduleMetrics(0);
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

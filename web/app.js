const get = (id) => document.getElementById(id);
const runButton = get('run');
const connection = get('connection');
const connectionLabel = document.createTextNode('Подключение');
connection.lastChild.replaceWith(connectionLabel);
const timeline = get('timeline');
const oneDecimal = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 });
const whole = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 0 });
const clock = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
let stream;
let state;
let metrics;
let lastTimeline;
let pending = false;
let polling = false;
let metricsTimer;

function element(tag, text) {
  const node = document.createElement(tag);
  node.textContent = text;
  return node;
}

function connectionState(value, label) {
  connection.dataset.state = value;
  connectionLabel.textContent = label;
  runButton.disabled = value !== 'online' || pending || state?.status === 'running';
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
  state = next;
  pending = false;
  runButton.disabled = next.status === 'running';
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
  if (metrics) renderModel(metrics.models);
  if (justStarted) scheduleMetrics(0);
}

function connect() {
  stream = new EventSource('/events');
  stream.onopen = () => connectionState('online', 'Подключено');
  stream.onerror = () => connectionState('waiting', 'Переподключение');
  stream.onmessage = (event) => renderRun(JSON.parse(event.data));
}

runButton.addEventListener('click', async () => {
  pending = true;
  runButton.disabled = true;
  try {
    const response = await fetch('/run', { method: 'POST' });
    if (!response.ok && response.status !== 409) throw new Error(`HTTP ${response.status}`);
  } catch (error) {
    pending = false;
    runButton.disabled = connection.dataset.state !== 'online';
    get('result').hidden = false;
    get('result').textContent = `Не удалось запустить проверку. ${error.message}`;
  }
});

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

function renderModel(models) {
  const loaded = models?.find((model) => model.name === state?.model || model.name === `${state?.model}:latest`);
  get('ollama').textContent = models === null ? 'Ollama недоступна' : 'Ollama';
  get('model-state').textContent = models === null ? 'Проверьте, запущена ли Ollama' : (loaded ? 'В памяти' : 'Ожидает загрузки');
  get('model-details').hidden = !loaded;
  if (loaded) {
    get('context').textContent = loaded.context_length != null ? `${whole.format(loaded.context_length)} токенов` : '—';
    get('model-vram').textContent = loaded.size_vram != null ? gib(loaded.size_vram) : '—';
  }
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
  if (!document.hidden && !polling) metricsTimer = setTimeout(readMetrics, delay);
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
    scheduleMetrics(state?.status === 'running' ? 5_000 : 15_000);
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
  if (!document.hidden && state?.status === 'running') renderTime();
}, 1_000);

if (!document.hidden) {
  connect();
  scheduleMetrics(0);
}

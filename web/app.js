const get = (id) => document.getElementById(id);
const runButton = get('run');
const pinModel = get('pin-model');
const unloadModel = get('unload-model');
const modelList = get('model-list');
const benchmarkRun = get('benchmark-run');
const description = get('model-description');
const saveDescription = get('save-description');
const contextSize = get('context-size');
const saveContext = get('save-context');
const ramExpand = get('ram-expand');
const connection = get('connection');
const connectionLabel = document.createTextNode('Подключение');
connection.lastChild.replaceWith(connectionLabel);
const timeline = get('timeline');
const oneDecimal = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 1 });
const whole = new Intl.NumberFormat('ru-RU', { maximumFractionDigits: 0 });
const clock = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
const benchmarkDate = new Intl.DateTimeFormat('ru-RU', { dateStyle: 'medium', timeStyle: 'short' });
const farFuture = 100 * 365 * 24 * 60 * 60 * 1000;
const contextPresets = [null, 2048, 4096, 8192, 16384, 32768, 65536, 131072];
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
let benchmarkRecord;
let benchmarkRead = 0;
let descriptionSaving = false;
let contextSaving = false;
let contextEstimateRequest = 0;
let contextBytesPerToken;
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
    modelSubmitting != null || state.model_action != null || state.benchmark_running || contextSaving || state.status === 'running';
  const installed = metrics?.installed_models?.includes(state?.model);
  runButton.disabled = blocked || !installed;
  pinModel.disabled = blocked || !installed || metrics?.models == null;
  unloadModel.disabled = blocked || !installed || !loadedModel(metrics?.models, state?.model);
  benchmarkRun.disabled = blocked || !installed || metrics?.models == null;
  description.disabled = !installed || !benchmarkRecord || descriptionSaving;
  saveDescription.disabled = description.disabled || description.value === benchmarkRecord?.description ||
    connection.dataset.state !== 'online';
  contextSize.disabled = blocked || !installed || !benchmarkRecord;
  saveContext.disabled = contextSize.disabled || contextPresets[contextSize.value] === benchmarkRecord?.context_tokens;
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
  const modelChanged = state?.model !== next.model;
  const benchmarkUpdated = state?.benchmark_revision !== next.benchmark_revision;
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
  if (modelChanged || benchmarkUpdated) readBenchmark(next.model, modelChanged);
  if (modelChanged) readContextEstimate(next.model);
  if (justStarted || actionFinished || benchmarkUpdated) scheduleMetrics(0);
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
benchmarkRun.addEventListener('click', () => changeModel('benchmark', '/model/benchmark'));
contextSize.addEventListener('input', () => {
  get('context-feedback').textContent = '';
  renderContextChoice();
  updateControls();
});
get('context-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (saveContext.disabled || !state?.model) return;
  const model = state.model;
  const tokens = contextPresets[contextSize.value];
  const feedback = get('context-feedback');
  contextSaving = true;
  feedback.textContent = '';
  updateControls();
  try {
    const response = await postModel('/model/context', model, { context_tokens: tokens });
    if (!response.ok) throw new Error(modelResponseError(response));
    if (state?.model === model) {
      if (benchmarkRecord) benchmarkRecord.context_tokens = tokens;
      feedback.textContent = 'Сохранено';
    }
  } catch (error) {
    if (state?.model === model) feedback.textContent = error.message;
  } finally {
    contextSaving = false;
    updateControls();
  }
});

function contextLabel(tokens) {
  return tokens == null ? 'Авто' : `${whole.format(tokens)} токенов`;
}

function renderContextChoice() {
  const tokens = contextPresets[contextSize.value];
  const estimated = tokens && contextBytesPerToken ? tokens * contextBytesPerToken : null;
  const label = contextLabel(tokens) + (estimated ? ` · ≈ ${memoryAmount(estimated)}` : '');
  get('context-choice').textContent = label;
  contextSize.setAttribute('aria-valuetext', label);
  get('context-note').textContent = estimated ?
    'Оценка для KV-кэша в FP16. Веса модели и рабочие буферы занимают память дополнительно.' :
    tokens && contextBytesPerToken === null ? 'Для этой модели оценка памяти недоступна.' :
    'Лимит действует при следующей загрузке.';
}

async function readContextEstimate(model) {
  const request = ++contextEstimateRequest;
  contextBytesPerToken = undefined;
  renderContextChoice();
  if (!model) return;
  let bytes = null;
  try {
    const response = await fetch(`/model/context/estimate?model=${encodeURIComponent(model)}`);
    if (response.ok) bytes = (await response.json()).bytes_per_token_f16;
  } catch {
    // The slider still works if Ollama can't provide model metadata.
  }
  if (request !== contextEstimateRequest) return;
  contextBytesPerToken = bytes;
  renderContextChoice();
}

description.addEventListener('input', updateControls);
get('model-description-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (saveDescription.disabled || !state?.model || !benchmarkRecord) return;
  const model = state.model;
  const text = description.value;
  descriptionSaving = true;
  modelRequestError = '';
  renderModel(metrics?.models);
  try {
    const response = await postModel('/model/description', model, { description: text });
    if (!response.ok) throw new Error(modelResponseError(response));
    if (state?.model === model && benchmarkRecord) benchmarkRecord.description = text;
  } catch (error) {
    if (state?.model === model) modelRequestError = `Описание не сохранено. ${error.message}`;
  } finally {
    descriptionSaving = false;
    renderModel(metrics?.models);
  }
});

async function readBenchmark(model, modelChanged) {
  const request = ++benchmarkRead;
  if (modelChanged) {
    benchmarkRecord = undefined;
    description.value = '';
    contextSize.value = '0';
    get('context-feedback').textContent = '';
    renderContextChoice();
    renderBenchmark('Загружаем результаты…');
  }
  if (!model) return;
  try {
    const response = await fetch(`/model/benchmark?model=${encodeURIComponent(model)}`);
    if (!response.ok) throw new Error();
    const record = await response.json();
    if (request !== benchmarkRead) return;
    if (description.value === (benchmarkRecord?.description ?? '')) description.value = record.description;
    if (contextPresets[contextSize.value] === (benchmarkRecord?.context_tokens ?? null)) {
      contextSize.value = String(Math.max(0, contextPresets.indexOf(record.context_tokens)));
      renderContextChoice();
    }
    benchmarkRecord = record;
    renderBenchmark();
  } catch {
    if (request === benchmarkRead) renderBenchmark('Не удалось прочитать результаты.');
  }
}

function renderBenchmark(message) {
  const result = benchmarkRecord?.latest;
  const cold = benchmarkRecord?.last_cold_load;
  const formattedDate = result && Date.parse(result.measured_at);
  get('benchmark-summary').textContent = message ?? (result ?
    `${result.was_loaded ? 'Модель уже была в памяти' : 'Модель загружалась'}${Number.isFinite(formattedDate) ? ` · ${benchmarkDate.format(formattedDate)}` : ''}` :
    'Замеров пока нет.');
  const facts = get('benchmark-results');
  facts.hidden = !result;
  if (result) {
    const elapsed = (ms) => ms < 1000 ? `${oneDecimal.format(ms)} мс` : `${oneDecimal.format(ms / 1000)} с`;
    const rate = (tokens) => tokens == null ? '—' : `${oneDecimal.format(tokens)} ток/с`;
    const rows = [
      ['Контекст прогона', contextLabel(result.context_tokens)],
      ['Загрузка в этом прогоне', elapsed(result.load_ms)],
      ['Последняя холодная загрузка', cold ? elapsed(cold.load_ms) : '—'],
      ['Скорость генерации', rate(result.generated_tokens_per_second)],
      ['Скорость обработки промпта', rate(result.prompt_tokens_per_second)],
      ['Токены · вход / выход', `${result.prompt_tokens} / ${result.generated_tokens}`],
      ['Общее время', elapsed(result.total_ms)],
    ];
    facts.replaceChildren(...rows.map(([label, value]) => {
      const row = document.createElement('div');
      row.append(element('dt', label), element('dd', value));
      return row;
    }));
  }
  updateControls();
}

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
  const matches = models?.filter((model) => model.name === name || model.name === `${name}:latest` || `${model.name}:latest` === name);
  const desired = name === state?.model ? benchmarkRecord?.context_tokens : null;
  return matches?.find((model) => desired != null && model.context_length === desired) ?? matches?.[0];
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
  const benchmarking = action === 'benchmark' || state?.benchmark_running;
  benchmarkRun.textContent = benchmarking ? 'Бенчмарк выполняется…' : 'Запустить бенчмарк';
  const expiry = Date.parse(loaded?.expires_at);
  const remaining = expiry - Date.now();
  // Ollama represents indefinite retention with a deadline centuries ahead.
  const pinned = Boolean(loaded && (Number.isFinite(expiry) && expiry > 0 ? remaining > farFuture : state?.pinned));
  const dot = get('model-dot');
  const status = get('model-state');
  let mode = 'idle';
  let label = 'Модель выгружена';
  if (benchmarking) {
    mode = 'running';
    label = 'Выполняется бенчмарк';
  } else if (state?.model_working) {
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

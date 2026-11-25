const get = (id) => document.getElementById(id);
const uiRevision = document.querySelector('meta[name="lyra-ui-revision"]').content;
const runButton = get('run');
const pinModel = get('pin-model');
const unloadModel = get('unload-model');
const modelList = get('model-list');
const benchmarkRun = get('benchmark-run');
const description = get('model-description');
const saveDescription = get('save-description');
const contextSize = get('context-size');
const saveContext = get('save-context');
const orchestratorMode = get('mode-orchestrator');
const ramExpand = get('ram-expand');
const ramDialog = get('ram-dialog');
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
const benchmarkStages = {
  preparing: 'Подготовка',
  loading_and_prefill: 'Загрузка и промпт',
  prefill: 'Обработка промпта',
  generating: 'Генерация',
  measuring_memory: 'Замер памяти',
  saving: 'Сохранение',
};
let stream;
let applicationClosed = false;
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
let inspectedModel;
let observedModel;
let benchmarkRecord;
let benchmarkRead = 0;
let descriptionSaving = false;
let contextSaving = false;
let contextEstimateRequest = 0;
let contextBytesPerToken;
let configuredAutoContext;
let waitingForMemorySample = false;
let memorySampleEpoch = 0;
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
  const blocked = applicationClosed || !state || connection.dataset.state !== 'online' || pending || selectingModel != null ||
    modelSubmitting != null || state.model_action != null || state.benchmark_running || contextSaving || state.status === 'running';
  const installed = metrics?.installed_models?.includes(observedModel);
  runButton.disabled = blocked || orchestratorMode.checked || !metrics?.installed_models?.includes(state?.model);
  pinModel.disabled = blocked || !installed || metrics?.models == null;
  unloadModel.disabled = blocked || !installed || !loadedModel(metrics?.models, observedModel);
  benchmarkRun.disabled = blocked || !installed || metrics?.models == null;
  description.readOnly = applicationClosed;
  description.disabled = !installed || !benchmarkRecord || descriptionSaving;
  const descriptionChanged = benchmarkRecord && description.value !== benchmarkRecord.description;
  saveDescription.hidden = !descriptionChanged;
  saveDescription.disabled = applicationClosed || description.disabled || !descriptionChanged ||
    connection.dataset.state !== 'online';
  contextSize.disabled = blocked || !installed || !benchmarkRecord;
  saveContext.disabled = contextSize.disabled || contextPresets[contextSize.value] === benchmarkRecord?.context_tokens;
  for (const { button } of modelRows.values()) button.disabled = applicationClosed || (!orchestratorMode.checked && blocked);
  get('model-mode').disabled = applicationClosed;
  ramExpand.disabled = applicationClosed;
}

get('model-mode').addEventListener('change', () => renderModel(metrics?.models));

function displayedModel() {
  if (!orchestratorMode.checked) return state?.model;
  const installed = metrics?.installed_models;
  if (installed?.includes(inspectedModel)) return inspectedModel;
  return installed?.includes(state?.model) ? state.model : installed?.[0];
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
  const memoryWasChanging = modelMemoryChanging();
  const justStarted = state?.status !== 'running' && next.status === 'running';
  const actionFinished = (modelSubmitting != null || state?.model_action != null) && next.model_action == null;
  const benchmarkUpdated = state?.benchmark_revision !== next.benchmark_revision;
  state = next;
  if (selectingModel === next.model) selectingModel = undefined;
  pending = false;
  modelSubmitting = undefined;
  if (memoryWasChanging && !modelMemoryChanging()) refreshMemoryWarning();
  updateControls();
  runButton.textContent = next.status === 'running' ? 'Выполняется…' : 'Запустить проверку';
  const phase = get('phase');
  phase.dataset.status = next.status;
  if (phase.textContent !== next.phase) phase.textContent = next.phase;
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
  renderModel(metrics?.models, benchmarkUpdated);
  if (justStarted || actionFinished || benchmarkUpdated) scheduleMetrics(0);
}

function connect() {
  stream = new EventSource('/events');
  stream.onopen = () => {
    if (!applicationClosed) connectionState('online', 'Подключено');
  };
  stream.onerror = () => {
    if (!applicationClosed) connectionState('waiting', 'Переподключение');
  };
  stream.onmessage = (event) => {
    const next = JSON.parse(event.data);
    if (next.closed) {
      showClosed();
      return;
    }
    if (next.ui_revision && next.ui_revision !== uiRevision) {
      disconnect();
      window.location.reload();
      return;
    }
    const restarted = applicationClosed;
    applicationClosed = false;
    if (restarted) {
      selectingModel = undefined;
      connectionState('online', 'Подключено');
    }
    renderRun(next);
    if (restarted) {
      scheduleMetrics(0);
      scheduleRam(0);
    }
  };
}

function showClosed() {
  applicationClosed = true;
  if (ramDialog.open) ramDialog.close();
  clearTimeout(metricsTimer);
  clearTimeout(ramTimer);
  connectionState('closed', 'Остановлена');
  get('phase').dataset.status = 'closed';
  get('phase').textContent = 'Lyra закрыта';
  get('sampled').textContent = 'Остановлено';
  get('context-memory-warning').hidden = true;
  renderModelList(metrics?.installed_models, metrics?.models);
  runButton.textContent = 'Запустить проверку';
  if (document.hidden) disconnect();
}

function disconnect() {
  stream?.close();
  stream = undefined;
}

runButton.addEventListener('click', async () => {
  pending = true;
  renderContextChoice();
  updateControls();
  try {
    const response = await postModel('/run', state.model);
    if (!response.ok) throw new Error(modelResponseError(response));
  } catch (error) {
    pending = false;
    refreshMemoryWarning();
    renderContextChoice();
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
  const model = displayedModel();
  if (saveContext.disabled || !model) return;
  const tokens = contextPresets[contextSize.value];
  const feedback = get('context-feedback');
  contextSaving = true;
  feedback.textContent = '';
  updateControls();
  try {
    const response = await postModel('/model/context', state?.model, { context_tokens: tokens }, model);
    if (!response.ok) throw new Error(modelResponseError(response));
    if (displayedModel() === model) {
      if (benchmarkRecord) benchmarkRecord.context_tokens = tokens;
      feedback.textContent = 'Сохранено';
    }
  } catch (error) {
    if (displayedModel() === model) feedback.textContent = error.message;
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
  const label = contextLabel(tokens) + (estimated ? ` ≈ ${memoryAmount(estimated)}` : '');
  get('context-choice').textContent = label;
  contextSize.setAttribute('aria-valuetext', label);
  get('context-note').textContent = estimated ?
    'Оценка для KV-кэша в FP16. Веса модели и рабочие буферы занимают память дополнительно.' :
    tokens && contextBytesPerToken === null ? 'Для этой модели оценка памяти недоступна.' :
    'Лимит действует при следующей загрузке.';
  const autoTokens = tokens == null ? autoContextTokens(displayedModel()) : null;
  const autoEstimate = autoTokens && contextBytesPerToken ? autoTokens * contextBytesPerToken : null;
  renderContextMemoryWarning(estimated ?? autoEstimate);
}

function autoContextTokens(model) {
  const loaded = metrics?.models?.find((entry) => !entry.context_profile && sameModelName(entry.name, model));
  const latest = benchmarkRecord?.latest;
  return configuredAutoContext ?? loaded?.context_length ??
    (latest?.context_tokens == null ? latest?.measured_context_tokens : null);
}

function modelMemoryChanging() {
  return pending || Boolean(modelSubmitting || state?.model_action || state?.benchmark_running || state?.status === 'running');
}

function refreshMemoryWarning() {
  waitingForMemorySample = true;
  memorySampleEpoch++;
  scheduleMetrics(0);
}

function memoryRoom(memory, resident) {
  // The live sample already includes this model's resident memory.
  const other = Math.max(0, memory.used - (resident ?? 0));
  return Math.max(0, memory.total - other);
}

function contextMemoryWarning(snapshot, model, resident, contextBytes) {
  const weights = snapshot?.model_sizes?.[model];
  if (!Number.isFinite(weights) || weights <= 0 || snapshot.models == null) return '';
  const estimate = weights + (contextBytes ?? 0);
  const compute = Math.max(512 * 2 ** 20, Math.ceil(estimate * 0.1));
  const systemReserve = 2 ** 30;
  const needs = estimate + compute;
  const scope = contextBytes == null ? ' · без оценки контекста' : '';
  const gpus = snapshot.gpus.filter((gpu) => gpu.vram?.total > 0);
  const ram = snapshot.ram;
  const residentRam = resident?.size != null && resident.size_vram != null ?
    Math.max(0, resident.size - resident.size_vram) : 0;
  const ramAvailable = ram?.total > 0 ? memoryRoom(ram, residentRam) : null;

  if (gpus.length === 0) {
    if (ramAvailable == null || needs + systemReserve <= ramAvailable) return '';
    return `При загрузке в RAM может не хватить ≈ ${memoryAmount(needs + systemReserve - ramAvailable)}${scope}. Учтены веса модели, запас для системы и ≈ ${memoryAmount(compute)} на вычисления.`;
  }

  // Ollama reports per-model VRAM across devices, so reclaim it only for one measured GPU.
  const gpuAvailable = Math.max(...gpus.map((gpu) =>
    memoryRoom(gpu.vram, gpus.length === 1 ? resident?.size_vram : 0)));
  const shortfall = Math.max(0, needs - gpuAvailable);
  const ramShortfall = ramAvailable == null ? 0 : Math.max(0, shortfall + systemReserve - ramAvailable);
  if (shortfall > 0) {
    const gpuLabel = gpus.length > 1 ? 'На самой свободной GPU' : 'В VRAM';
    const fallback = ramShortfall > 0 ?
      ` При переносе остатка в RAM там тоже может не хватить ≈ ${memoryAmount(ramShortfall)}.` :
      ' Ollama может перенести часть нагрузки в RAM.';
    return `${gpuLabel} может не хватить ≈ ${memoryAmount(shortfall)}${scope}. Учтён запас ≈ ${memoryAmount(compute)} на вычисления.${fallback}`;
  }
  return ramShortfall > 0 ? 'Мало свободной RAM. Под фоновые процессы и систему заложен запас 1 ГБ.' : '';
}

function renderContextMemoryWarning(contextBytes) {
  const warning = get('context-memory-warning');
  if (applicationClosed || modelMemoryChanging() || waitingForMemorySample) {
    warning.hidden = true;
    return;
  }
  const model = displayedModel();
  const message = contextMemoryWarning(metrics, model, loadedModel(metrics?.models, model), contextBytes);
  warning.hidden = !message;
  if (warning.textContent !== message) warning.textContent = message;
}

async function readContextEstimate(model) {
  const request = ++contextEstimateRequest;
  contextBytesPerToken = undefined;
  configuredAutoContext = undefined;
  renderContextChoice();
  if (!model) return;
  let estimate;
  try {
    const response = await fetch(`/model/context/estimate?model=${encodeURIComponent(model)}`);
    if (response.ok) estimate = await response.json();
  } catch {
    // The slider still works if Ollama can't provide model metadata.
  }
  if (request !== contextEstimateRequest) return;
  contextBytesPerToken = estimate?.bytes_per_token_f16 ?? null;
  configuredAutoContext = estimate?.configured_context_tokens;
  renderContextChoice();
}

description.addEventListener('input', updateControls);
get('model-description-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const model = displayedModel();
  if (saveDescription.disabled || !model || !benchmarkRecord) return;
  const text = description.value;
  descriptionSaving = true;
  modelRequestError = '';
  renderModel(metrics?.models);
  try {
    const response = await postModel('/model/description', state?.model, { description: text }, model);
    if (!response.ok) throw new Error(modelResponseError(response));
    if (displayedModel() === model && benchmarkRecord) benchmarkRecord.description = text;
  } catch (error) {
    if (displayedModel() === model) modelRequestError = `Описание не сохранено. ${error.message}`;
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
    renderContextChoice();
    renderBenchmark();
  } catch {
    if (request === benchmarkRead) renderBenchmark('Не удалось прочитать результаты.');
  }
}

function renderBenchmark(message) {
  const result = benchmarkRecord?.latest;
  const summary = get('benchmark-summary');
  const measuredAt = result && Date.parse(result.measured_at);
  summary.textContent = message ?? (result ?
    (Number.isFinite(measuredAt) ? `Результаты от ${benchmarkDate.format(measuredAt)}` : 'Результаты бенчмарка') :
    'Замеров пока нет.');
  const facts = get('benchmark-results');
  facts.hidden = !result;
  if (result) {
    const elapsed = (ms) => ms < 1000 ? `${oneDecimal.format(ms)} мс` : `${oneDecimal.format(ms / 1000)} с`;
    const rate = (tokens) => tokens == null ? '—' : oneDecimal.format(tokens);
    const launch = benchmarkRecord.average_launch?.mean_ms;
    const measuredContext = result.measured_context_tokens;
    const rows = [
      ['Контекст прогона', measuredContext != null
        ? (result.context_tokens == null ? `Авто · ${contextLabel(measuredContext)}` : contextLabel(measuredContext))
        : contextLabel(result.context_tokens)],
      ['Среднее время запуска', launch == null ? '—' : elapsed(launch), 'Среднее время загрузки модели в память по холодным запускам'],
      ['Скорость • вход / выход', `${rate(result.prompt_tokens_per_second)} / ${rate(result.generated_tokens_per_second)} ток/с`],
      ['VRAM модели', result.model_vram_bytes == null ? '—' : memoryAmount(result.model_vram_bytes), 'Видеопамять загруженной модели по данным Ollama, включая часть контекста на GPU'],
    ];
    facts.replaceChildren(...rows.map(([label, value, hint]) => {
      const row = document.createElement('div');
      const title = element('dt', label);
      if (hint) title.title = hint;
      row.append(title, element('dd', value));
      return row;
    }));
  }
  updateControls();
}

function postModel(path, selected, payload, target) {
  const options = { method: 'POST', headers: { 'X-Lyra-Model': selected ?? '' } };
  if (target && target !== selected) options.headers['X-Lyra-Target-Model'] = target;
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
  if (orchestratorMode.checked) {
    inspectedModel = model;
    modelRequestError = '';
    renderModel(metrics?.models);
    return;
  }
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
  const model = displayedModel();
  if (!model) return;
  modelSubmitting = { model, action };
  modelRequestError = '';
  renderModel(metrics?.models);
  try {
    const response = await postModel(path, state?.model, payload, model);
    if (!response.ok) throw new Error(modelResponseError(response));
  } catch (error) {
    modelSubmitting = undefined;
    refreshMemoryWarning();
    if (displayedModel() === model) modelRequestError = error.message;
    renderModel(metrics?.models);
  }
}

function memoryAmount(bytes) {
  return bytes >= 2 ** 30 ? `${oneDecimal.format(bytes / 2 ** 30)} ГБ` : `${oneDecimal.format(bytes / 2 ** 20)} МБ`;
}

function memoryLabel(value) {
  if (!value) return '—';
  const inGigabytes = value.total >= 2 ** 30;
  const unit = inGigabytes ? 2 ** 30 : 2 ** 20;
  const format = inGigabytes ? oneDecimal : whole;
  return `${format.format(value.used / unit)} / ${format.format(value.total / unit)} ${inGigabytes ? 'ГБ' : 'МБ'}`;
}

function memoryPercent(value) {
  return value?.total ? value.used / value.total * 100 : null;
}

function meter(node, value) {
  node.hidden = value == null;
  if (value != null) node.value = value;
}

function loadedModel(models, name) {
  const matches = models?.filter((model) => sameModelName(model.name, name));
  const desired = name === observedModel ? benchmarkRecord?.context_tokens : null;
  return matches?.find((model) => desired != null && model.context_length === desired) ?? matches?.[0];
}

function sameModelName(left, right) {
  return left === right || left === `${right}:latest` || `${left}:latest` === right;
}

function modelStatus(name, models) {
  const loaded = loadedModel(models, name);
  const isTaskModel = name === state?.model;
  const isOperationModel = name === state?.operation_model;
  const action = modelSubmitting?.model === name ? modelSubmitting.action :
    isOperationModel ? state?.model_action : undefined;
  const benchmarking = action === 'benchmark' || (isOperationModel && state?.benchmark_running);
  const expiry = Date.parse(loaded?.expires_at);
  const remaining = expiry - Date.now();
  // Ollama represents indefinite retention with a deadline centuries ahead.
  const pinned = Boolean(loaded && (Number.isFinite(expiry) && expiry > 0 ? remaining > farFuture : isTaskModel && state?.pinned));
  let mode = 'idle';
  let label = 'Не загружена';
  if (applicationClosed) {
    label = 'Остановлена';
  } else if (benchmarking) {
    mode = 'running';
    label = 'Бенчмарк';
  } else if (isTaskModel && state?.model_working) {
    mode = 'running';
    label = 'Работает';
  } else if (models == null) {
    mode = 'unknown';
    label = models === null ? 'Нет связи' : 'Проверяем…';
  } else if (action === 'unload') {
    mode = 'loading';
    label = 'Выгружается';
  } else if (!loaded && ((isTaskModel && state?.status === 'running') || action === 'pin')) {
    mode = 'loading';
    label = 'Загружается';
  } else if (loaded) {
    mode = 'resident';
    label = 'В памяти';
  }
  let detail = '';
  if (!applicationClosed && benchmarking) {
    detail = benchmarkStages[isOperationModel ? state?.benchmark_stage : undefined] ?? benchmarkStages.preparing;
  } else if (mode === 'resident' && !pinned && remaining > 0) {
    detail = `до выгрузки ≈ ${duration(remaining)}`;
  }
  return { loaded, mode, label, detail, pinned, benchmarking, action };
}

function renderModelList(names, loaded) {
  const installed = names ?? [];
  if (installed.length !== modelRows.size || installed.some((name) => !modelRows.has(name))) {
    modelRows = new Map(installed.map((name) => {
      const row = element('li', '');
      const button = element('button', '');
      const modelName = element('span', name);
      modelName.className = 'list-model-name';
      const badge = element('span', '');
      badge.className = 'list-model-status';
      const statusLine = element('span', '');
      statusLine.className = 'list-model-state';
      const dot = element('span', '');
      dot.className = 'model-dot';
      dot.setAttribute('aria-hidden', 'true');
      const status = element('span', '');
      const pin = get('model-pin-icon').content.firstElementChild.cloneNode(true);
      const detail = element('span', '');
      detail.className = 'list-model-detail';
      button.type = 'button';
      statusLine.append(dot, status, pin);
      badge.append(statusLine, detail);
      button.append(modelName, badge);
      button.addEventListener('click', () => selectModel(name));
      row.append(button);
      return [name, { row, button, dot, status, pin, detail }];
    }));
    modelList.replaceChildren(...Array.from(modelRows.values(), ({ row }) => row));
  }
  for (const [name, { button, dot, status, pin, detail }] of modelRows) {
    const selected = name === observedModel;
    const view = modelStatus(name, loaded);
    button.setAttribute('aria-pressed', String(selected));
    dot.dataset.state = view.mode;
    if (status.textContent !== view.label) status.textContent = view.label;
    pin.toggleAttribute('hidden', view.mode !== 'resident' || !view.pinned);
    detail.hidden = !view.detail;
    if (detail.textContent !== view.detail) detail.textContent = view.detail;
  }
  modelList.hidden = installed.length === 0;
  const message = get('model-list-message');
  message.hidden = installed.length > 0;
  message.textContent = names === undefined ? 'Получаем список моделей…' :
    names === null ? 'Список Ollama недоступен.' : 'Локальных моделей пока нет. Добавьте модель через ollama pull.';
}

function renderModel(models, benchmarkUpdated = false) {
  const names = metrics?.installed_models;
  const model = displayedModel();
  const modelChanged = model !== observedModel;
  observedModel = model;
  if (modelChanged || benchmarkUpdated) readBenchmark(model, modelChanged);
  if (modelChanged) readContextEstimate(model);
  renderModelList(names, models);
  get('models-panel').dataset.mode = orchestratorMode.checked ? 'orchestrator' : 'manual';
  get('orchestrator-placeholder').hidden = !orchestratorMode.checked;
  const hasModel = Boolean(names?.includes(model));
  get('selected-model').hidden = !hasModel;
  get('benchmark-panel').hidden = !hasModel;
  get('model-name').textContent = model ?? '';
  get('benchmark-model').textContent = model ?? '';
  renderContextChoice();
  const { loaded, action, benchmarking, pinned } = modelStatus(model, models);
  benchmarkRun.textContent = benchmarking ? 'Бенчмарк выполняется…' : 'Запустить бенчмарк';
  get('ollama').textContent = models === null ? 'Ollama недоступна' : 'Ollama';
  pinModel.checked = action === 'pin' || (action !== 'unpin' && pinned);
  const modelError = modelRequestError || (model === state?.operation_model ? state?.model_error : '');
  get('model-error').hidden = !modelError;
  get('model-error').textContent = modelError ?? '';
  get('model-details').hidden = !loaded;
  if (loaded) {
    get('context').textContent = loaded.context_length != null ? `${whole.format(loaded.context_length)} токенов` : '—';
    get('model-vram').textContent = loaded.size_vram != null ? memoryAmount(loaded.size_vram) : '—';
  }
  updateControls();
}

function gpuCard(gpu) {
  const node = element('div', '');
  node.className = 'gpu';
  const heading = element('div', '');
  heading.className = 'gpu-heading';
  const temperature = element('output', gpu.temperature != null ? `${gpu.temperature} °C` : '—');
  temperature.setAttribute('aria-label', 'Температура GPU');
  heading.append(element('strong', gpu.name), temperature);
  node.append(heading);
  const entries = [
    ['GPU', gpu.load != null ? `${gpu.load}%` : '—', gpu.load],
    ['VRAM', memoryLabel(gpu.vram), memoryPercent(gpu.vram)],
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
  renderRamTotal((ramDialog.open && detailedRam) || next.ram);
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
  if (applicationClosed || document.hidden) return;
  if (polling) {
    metricsRefreshPending ||= delay === 0;
  } else {
    metricsTimer = setTimeout(readMetrics, delay);
  }
}

async function readMetrics() {
  if (applicationClosed || document.hidden) return;
  polling = true;
  const epoch = memorySampleEpoch;
  const changing = modelMemoryChanging();
  try {
    const response = await fetch('/metrics');
    if (!response.ok) throw new Error();
    const next = await response.json();
    if (!applicationClosed) {
      if (!changing && epoch === memorySampleEpoch && !modelMemoryChanging()) waitingForMemorySample = false;
      renderMetrics(next);
    }
  } catch {
    if (!applicationClosed) get('sampled').textContent = 'Нет связи';
  } finally {
    polling = false;
    const delay = metricsRefreshPending ? 0 : (state?.status === 'running' ? 5_000 : 15_000);
    metricsRefreshPending = false;
    scheduleMetrics(delay);
  }
}

function scheduleRam(delay) {
  clearTimeout(ramTimer);
  if (!applicationClosed && !document.hidden && ramDialog.open && !readingRam) ramTimer = setTimeout(readRam, delay);
}

async function readRam() {
  if (applicationClosed || document.hidden || !ramDialog.open) return;
  readingRam = true;
  const list = get('ram-list');
  const message = get('ram-message');
  try {
    const response = await fetch('/ram');
    if (!response.ok) throw new Error();
    const usage = await response.json();
    if (applicationClosed || !ramDialog.open || document.hidden) return;
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
    message.textContent = apps === null ? 'Данные по приложениям недоступны.' : 'Нет приложений от 300 МБ.';
  } catch {
    if (applicationClosed || !ramDialog.open) return;
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

ramExpand.addEventListener('click', () => {
  ramDialog.showModal();
  scheduleRam(0);
});
ramDialog.addEventListener('close', () => {
  detailedRam = undefined;
  renderRamTotal(metrics?.ram);
  clearTimeout(ramTimer);
});

document.addEventListener('visibilitychange', () => {
  if (document.hidden) {
    // Keep the event stream open until Quit, even with telemetry polling paused.
    if (applicationClosed) disconnect();
    clearTimeout(metricsTimer);
    clearTimeout(ramTimer);
  } else {
    if (!stream) connect();
    scheduleMetrics(0);
    scheduleRam(0);
  }
});

setInterval(() => {
  if (applicationClosed || document.hidden) return;
  if (state?.status === 'running') renderTime();
  if ([...modelRows.values()].some(({ dot, detail }) => dot.dataset.state === 'resident' && !detail.hidden)) {
    renderModelList(metrics?.installed_models, metrics?.models);
  }
}, 1_000);

connect();
scheduleMetrics(0);

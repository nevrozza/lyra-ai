import { benchmarkStages, renderBenchmark } from './benchmark-results.js';
import { createContextSettings } from './context-settings.js';
import { modelResponseError, postModel, sameModelName } from './model-api.js';
import { createModelList } from './model-list.js';
import { duration, get, memoryAmount, whole } from './ui.js';

const farFuture = 100 * 365 * 24 * 60 * 60 * 1000;

export function createModels(session, { refreshControls, requestMetrics }) {
  const pinModel = get('pin-model');
  const piSupport = get('pi-support');
  const piBenchmarkRun = get('pi-benchmark-run');
  const piCheckStatus = get('pi-check-status');
  const unloadModel = get('unload-model');
  const benchmarkRun = get('benchmark-run');
  const profileRun = get('benchmark-profile-run');
  const description = get('model-description');
  const saveDescription = get('save-description');
  let modelSubmitting;
  let modelRequestError = '';
  let inspectedModel;
  let observedModel;
  let benchmarkRecord;
  let benchmarkRead = 0;
  let descriptionSaving = false;

  const roleSettings = createRoleSettings(session);
  const list = createModelList({ selectModel, statusFor: modelStatus });
  const settings = createContextSettings(session, {
    displayedModel, getRecord: () => benchmarkRecord, loadedModel, isMemoryChanging, refreshControls, requestMetrics,
  });

  function isMemoryChanging() {
    return Boolean(modelSubmitting || session.run?.model_action || session.run?.benchmark_running ||
      session.run?.delegated_model);
  }

  function isBlocked() {
    return session.closed || !session.run || !session.online ||
      modelSubmitting != null || session.run.model_action != null || session.run.benchmark_running ||
      session.run.delegated_model != null || settings.saving;
  }

  function renderControls() {
    const blocked = isBlocked();
    const installed = session.metrics?.installed_models?.includes(observedModel);
    pinModel.disabled = blocked || !installed || session.metrics?.models == null;
    unloadModel.disabled = blocked || !installed || !loadedModel(session.metrics?.models, observedModel);
    benchmarkRun.disabled = blocked || !installed || session.metrics?.models == null;
    piBenchmarkRun.disabled = benchmarkRun.disabled;
    profileRun.disabled = benchmarkRun.disabled || Boolean(session.metrics?.models?.some(entry =>
      !sameModelName(entry.name, observedModel) || Date.parse(entry.expires_at) - Date.now() > farFuture));
    const piCheck = benchmarkRecord?.pi_check;
    piSupport.checked = piCheck === true;
    const checkingPi = (modelSubmitting?.model === observedModel && modelSubmitting?.action === 'benchmark-pi') ||
      (session.run?.operation_model === observedModel && session.run?.benchmark_stage === 'checking_pi');
    piCheckStatus.textContent = checkingPi ? 'Проверяем Pi…' : piCheck === true ? 'Проверка пройдена.' :
      piCheck === false ? 'Проверка не пройдена.' : 'Пока не проверено.';
    description.readOnly = session.closed;
    description.disabled = !installed || !benchmarkRecord || descriptionSaving;
    const descriptionChanged = benchmarkRecord && description.value !== benchmarkRecord.description;
    saveDescription.hidden = !descriptionChanged;
    saveDescription.disabled = description.readOnly || description.disabled || !descriptionChanged ||
      !session.online;
    settings.updateControls(blocked, installed);
    list.setDisabled(session.closed);
    roleSettings.update();
  }

  function displayedModel() {
    const installed = session.metrics?.installed_models;
    if (installed?.includes(inspectedModel)) return inspectedModel;
    return installed?.includes(session.run?.model) ? session.run.model : installed?.[0];
  }

  pinModel.addEventListener('change', () => {
    const pinned = pinModel.checked;
    changeModel(pinned ? 'pin' : 'unpin', '/model/pin', { pinned });
  });

  unloadModel.addEventListener('click', () => changeModel('unload', '/model/unload'));
  benchmarkRun.addEventListener('click', () => changeModel('benchmark', '/model/benchmark'));
  profileRun.addEventListener('click', () => changeModel('benchmark-profile', '/model/benchmark/profile'));
  piBenchmarkRun.addEventListener('click', () => changeModel('benchmark-pi', '/model/benchmark/pi'));

  description.addEventListener('input', refreshControls);
  get('model-description-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const model = displayedModel();
    if (saveDescription.disabled || !model || !benchmarkRecord) return;
    const text = description.value;
    descriptionSaving = true;
    modelRequestError = '';
    renderModel(session.metrics?.models);
    try {
      const response = await postModel('/model/description', session.run?.model, { description: text }, model);
      if (!response.ok) throw new Error(modelResponseError(response));
      if (displayedModel() === model && benchmarkRecord) benchmarkRecord.description = text;
    } catch (error) {
      if (displayedModel() === model) modelRequestError = `Описание не сохранено. ${error.message}`;
    } finally {
      descriptionSaving = false;
      renderModel(session.metrics?.models);
    }
  });

  async function readBenchmark(model, modelChanged) {
    const request = ++benchmarkRead;
    if (modelChanged) {
      benchmarkRecord = undefined;
      description.value = '';
      settings.reset();
      renderBenchmark(benchmarkRecord, 'Загружаем результаты…');
      refreshControls();
    }
    if (!model) return;
    try {
      const response = await fetch(`/model/benchmark?model=${encodeURIComponent(model)}`);
      if (!response.ok) throw new Error();
      const record = await response.json();
      if (request !== benchmarkRead) return;
      if (description.value === (benchmarkRecord?.description ?? '')) description.value = record.description;
      settings.syncTokens(benchmarkRecord?.context_tokens, record.context_tokens);
      benchmarkRecord = record;
      settings.render();
      renderBenchmark(benchmarkRecord);
      refreshControls();
    } catch {
      if (request === benchmarkRead) {
        renderBenchmark(benchmarkRecord, 'Не удалось прочитать результаты.');
        refreshControls();
      }
    }
  }

  function selectModel(model) {
    inspectedModel = model;
    modelRequestError = '';
    renderModel(session.metrics?.models);
  }

  async function changeModel(action, path, payload) {
    const model = displayedModel();
    if (!model) return;
    modelSubmitting = { model, action };
    modelRequestError = '';
    renderModel(session.metrics?.models);
    try {
      const response = await postModel(path, session.run?.model, payload, model);
      if (!response.ok) throw new Error(modelResponseError(response));
    } catch (error) {
      modelSubmitting = undefined;
      settings.refreshMemoryWarning();
      if (displayedModel() === model) modelRequestError = error.message;
      renderModel(session.metrics?.models);
    }
  }

  function loadedModel(models, name) {
    const matches = models?.filter((model) => sameModelName(model.name, name));
    const desired = name === observedModel ? benchmarkRecord?.context_tokens : null;
    return matches?.find((model) => desired != null && model.context_length === desired) ?? matches?.[0];
  }

  function modelStatus(name, models) {
    const loaded = loadedModel(models, name);
    const isTaskModel = name === session.run?.model;
    const isOperationModel = name === session.run?.operation_model;
    const action = modelSubmitting && modelSubmitting.model === name ? modelSubmitting.action :
      isOperationModel ? session.run?.model_action : undefined;
    const benchmarking = action === 'benchmark' || action === 'benchmark-profile' || action === 'benchmark-pi' ||
      (isOperationModel && session.run?.benchmark_running);
    const delegated = name != null &&
      Object.values(session.run?.delegations ?? {}).some(task => task.model === name);
    const aiAction = delegated && !session.closed ? 'request' : session.run?.ai_model_actions?.[name];
    const expiry = Date.parse(loaded?.expires_at);
    const remaining = expiry - Date.now();
    // Ollama represents indefinite retention with a deadline centuries ahead.
    const pinned = Boolean(loaded && (Number.isFinite(expiry) && expiry > 0 ? remaining > farFuture : isTaskModel && session.run?.pinned));
    let mode = 'idle';
    let label = 'Не загружена';
    if (session.closed) {
      label = 'Остановлена';
    } else if (benchmarking) {
      mode = 'running';
      label = 'Бенчмарк';
    } else if (delegated) {
      mode = 'running';
      label = 'Обработка запроса';
    } else if (models == null) {
      mode = 'unknown';
      label = models === null ? 'Нет связи' : 'Проверяем…';
    } else if (action === 'unload') {
      mode = 'loading';
      label = 'Выгружается';
    } else if (!loaded && (action === 'pin' || action === 'load')) {
      mode = 'loading';
      label = 'Загружается';
    } else if (loaded) {
      mode = 'resident';
      label = 'В памяти';
    }
    let detail = '';
    if (!session.closed && benchmarking) {
      detail = benchmarkStages[isOperationModel ? session.run?.benchmark_stage : undefined] ?? benchmarkStages.preparing;
    } else if (mode === 'resident' && !pinned && remaining > 0) {
      detail = `до выгрузки ≈ ${duration(remaining)}`;
    }
    return { loaded, mode, label, detail, pinned, benchmarking, action, aiAction };
  }

  function renderModel(models, benchmarkUpdated = false) {
    const names = session.metrics?.installed_models;
    const model = displayedModel();
    const modelChanged = model !== observedModel;
    observedModel = model;
    if (modelChanged || benchmarkUpdated) readBenchmark(model, modelChanged);
    if (modelChanged || benchmarkUpdated) settings.readEstimate(model);
    list.render(names, models, observedModel);
    const hasModel = Boolean(names?.includes(model));
    get('selected-model').hidden = !hasModel;
    get('benchmark-panel').hidden = !hasModel;
    get('model-name').textContent = model ?? '';
    get('benchmark-model').textContent = model ?? '';
    settings.render();
    const { loaded, action, benchmarking, pinned } = modelStatus(model, models);
    const checkingPi = action === 'benchmark-pi' ||
      (model === session.run?.operation_model && session.run?.benchmark_stage === 'checking_pi');
    benchmarkRun.textContent = benchmarking && !checkingPi ? 'Бенчмарк выполняется…' : 'Быстрый бенчмарк';
    profileRun.title = 'Последовательные холодные замеры. Выгрузите другие модели и снимите закрепление выбранной.';
    get('ollama').textContent = models === null ? 'Ollama недоступна' : 'Ollama';
    pinModel.checked = action === 'pin' || (action !== 'unpin' && pinned);
    const modelError = modelRequestError || (model === session.run?.operation_model ? session.run?.model_error : '');
    get('model-error').hidden = !modelError;
    get('model-error').textContent = modelError ?? '';
    get('model-details').hidden = !loaded;
    if (loaded) {
      get('context').textContent = loaded.context_length != null ? `${whole.format(loaded.context_length)} токенов` : '—';
      get('model-vram').textContent = loaded.size_vram != null ? memoryAmount(loaded.size_vram) : '—';
    }
    refreshControls();
  }

  return {
    render: renderModel,
    updateControls: renderControls,
    isMemoryChanging,
    acceptRun() { modelSubmitting = undefined; },
    showClosed() {
      get('context-memory-warning').hidden = true;
      list.render(session.metrics?.installed_models, session.metrics?.models, observedModel);
    },
    tick() {
      list.refreshCountdown(session.metrics?.installed_models, session.metrics?.models, observedModel);
    },
    refreshMemoryWarning: settings.refreshMemoryWarning,
    beginMemorySample: settings.beginSample,
    acceptMemorySample: settings.acceptSample,
  };
}

function createRoleSettings(session) {
  const form = get('model-roles-form');
  const fields = [...form.querySelectorAll('select')];
  const button = get('save-model-roles');
  const feedback = get('model-roles-feedback');
  let saved;
  let draft;
  let busy = false;
  let online = false;
  let optionsKey;

  function render() {
    const installed = session.metrics?.installed_models ?? [];
    const key = JSON.stringify([installed, draft]);
    if (key !== optionsKey) {
      optionsKey = key;
      for (const field of fields) {
        const value = draft?.[field.name];
        const options = [new Option('Авто', ''), ...installed.map(name => new Option(name, name))];
        if (value && !installed.includes(value)) options.push(new Option(value, value));
        field.replaceChildren(...options);
        field.value = value ?? '';
      }
    }
    for (const field of fields) field.disabled = !online || busy || !draft;
    button.disabled = !online || busy || !draft || fields.every(field => draft[field.name] === saved[field.name]);
  }

  async function refresh() {
    if (busy) return;
    busy = true;
    render();
    try {
      const response = await fetch('/model/intelligence');
      if (!response.ok) throw new Error();
      saved = await response.json();
      draft = { ...saved };
      feedback.textContent = '';
    } catch {
      feedback.textContent = 'Не удалось загрузить уровни интеллекта моделей.';
    } finally {
      busy = false;
      render();
    }
  }

  for (const field of fields) {
    field.addEventListener('change', () => {
      draft[field.name] = field.value || null;
      feedback.textContent = '';
      render();
    });
  }

  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (button.disabled) return;
    busy = true;
    render();
    try {
      const response = await fetch('/model/intelligence', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(draft),
      });
      if (!response.ok) throw new Error(await response.text());
      saved = await response.json();
      draft = { ...saved };
      feedback.textContent = 'Уровни сохранены.';
    } catch (error) {
      feedback.textContent = error.message || 'Не удалось сохранить уровни интеллекта моделей.';
    } finally {
      busy = false;
      render();
    }
  });

  return {
    update() {
      const connected = session.online && !session.closed;
      const reconnected = connected && !online;
      online = connected;
      if (reconnected) refresh();
      else render();
    },
  };
}

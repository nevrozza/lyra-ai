import { benchmarkStages, renderBenchmark } from './benchmark-results.js';
import { createContextSettings } from './context-settings.js';
import { modelResponseError, postModel, sameModelName } from './model-api.js';
import { createModelList } from './model-list.js';
import { duration, get, memoryAmount, whole } from './ui.js';

const farFuture = 100 * 365 * 24 * 60 * 60 * 1000;

export function createModels(session, { refreshControls, requestMetrics }) {
  const pinModel = get('pin-model');
  const unloadModel = get('unload-model');
  const benchmarkRun = get('benchmark-run');
  const description = get('model-description');
  const saveDescription = get('save-description');
  let modelSubmitting;
  let modelRequestError = '';
  let inspectedModel;
  let observedModel;
  let benchmarkRecord;
  let benchmarkRead = 0;
  let descriptionSaving = false;

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
    description.readOnly = session.closed;
    description.disabled = !installed || !benchmarkRecord || descriptionSaving;
    const descriptionChanged = benchmarkRecord && description.value !== benchmarkRecord.description;
    saveDescription.hidden = !descriptionChanged;
    saveDescription.disabled = description.readOnly || description.disabled || !descriptionChanged ||
      !session.online;
    settings.updateControls(blocked, installed);
    list.setDisabled(session.closed);
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
    const action = modelSubmitting?.model === name ? modelSubmitting.action :
      isOperationModel ? session.run?.model_action : undefined;
    const benchmarking = action === 'benchmark' || (isOperationModel && session.run?.benchmark_running);
    const delegated = name != null && name === session.run?.delegated_model;
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
      label = 'Запрос Codex';
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
    if (modelChanged) settings.readEstimate(model);
    list.render(names, models, observedModel);
    const hasModel = Boolean(names?.includes(model));
    get('selected-model').hidden = !hasModel;
    get('benchmark-panel').hidden = !hasModel;
    get('model-name').textContent = model ?? '';
    get('benchmark-model').textContent = model ?? '';
    settings.render();
    const { loaded, action, benchmarking, pinned } = modelStatus(model, models);
    benchmarkRun.textContent = benchmarking ? 'Бенчмарк выполняется…' : 'Запустить бенчмарк';
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

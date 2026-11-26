import { contextMemoryWarning } from './memory-budget.js';
import { modelResponseError, postModel, sameModelName } from './model-api.js';
import { get, memoryAmount, whole } from './ui.js';

const contextPresets = [null, 2048, 4096, 8192, 16384, 32768, 65536, 131072];

export function contextLabel(tokens) {
  return tokens == null ? 'Авто' : `${whole.format(tokens)} токенов`;
}

export function createContextSettings(session, {
  displayedModel, getRecord, loadedModel, isMemoryChanging, refreshControls, requestMetrics,
}) {
  const contextSize = get('context-size');
  const saveContext = get('save-context');
  let contextOptions = contextPresets;
  let contextSaving = false;
  let contextEstimateRequest = 0;
  let contextBytesPerToken;
  let configuredAutoContext;
  let waitingForMemorySample = false;
  let memorySampleEpoch = 0;

  contextSize.addEventListener('input', () => {
    get('context-feedback').textContent = '';
    renderContextChoice();
    refreshControls();
  });
  get('context-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const model = displayedModel();
    if (saveContext.disabled || !model) return;
    const tokens = contextOptions[contextSize.value];
    const feedback = get('context-feedback');
    contextSaving = true;
    feedback.textContent = '';
    refreshControls();
    try {
      const response = await postModel('/model/context', session.run?.model, { context_tokens: tokens }, model);
      if (!response.ok) throw new Error(modelResponseError(response));
      if (displayedModel() === model) {
        const record = getRecord();
        if (record) record.context_tokens = tokens;
        setContextChoice(tokens);
        feedback.textContent = 'Сохранено';
      }
    } catch (error) {
      if (displayedModel() === model) feedback.textContent = error.message;
    } finally {
      contextSaving = false;
      refreshControls();
    }
  });

  function setContextChoice(tokens) {
    const selected = tokens ?? null;
    contextOptions = contextPresets.includes(selected) ? contextPresets :
      [null, ...[...contextPresets.slice(1), selected].sort((left, right) => left - right)];
    contextSize.max = String(contextOptions.length - 1);
    contextSize.value = String(contextOptions.indexOf(selected));
    renderContextChoice();
  }

  function renderContextChoice() {
    const tokens = contextOptions[contextSize.value];
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
    const loaded = session.metrics?.models?.find((entry) => !entry.context_profile && sameModelName(entry.name, model));
    const latest = getRecord()?.latest;
    return configuredAutoContext ?? loaded?.context_length ??
      (latest?.context_tokens == null ? latest?.measured_context_tokens : null);
  }

  function refreshMemoryWarning() {
    waitingForMemorySample = true;
    memorySampleEpoch++;
    requestMetrics();
  }

  function renderContextMemoryWarning(contextBytes) {
    const warning = get('context-memory-warning');
    if (session.closed || isMemoryChanging() || waitingForMemorySample) {
      warning.hidden = true;
      return;
    }
    const model = displayedModel();
    const message = contextMemoryWarning(session.metrics, model, loadedModel(session.metrics?.models, model), contextBytes);
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

  return {
    render: renderContextChoice,
    readEstimate: readContextEstimate,
    refreshMemoryWarning,
    get saving() { return contextSaving; },
    updateControls(blocked, installed) {
      const record = getRecord();
      contextSize.disabled = blocked || !installed || !record;
      saveContext.disabled = contextSize.disabled || contextOptions[contextSize.value] === record?.context_tokens;
    },
    reset() {
      get('context-feedback').textContent = '';
      setContextChoice(null);
    },
    syncTokens(previous, next) {
      if (contextOptions[contextSize.value] !== (previous ?? null)) return;
      setContextChoice(next);
    },
    beginSample() {
      return isMemoryChanging() ? undefined : memorySampleEpoch;
    },
    acceptSample(epoch) {
      if (epoch === memorySampleEpoch && !isMemoryChanging()) waitingForMemorySample = false;
    },
  };
}

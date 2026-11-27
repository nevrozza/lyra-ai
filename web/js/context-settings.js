import { contextMemoryWarning } from './memory-budget.js';
import { modelResponseError, postModel } from './model-api.js';
import { get, memoryAmount, whole } from './ui.js';

const contextPresets = [null, 2048, 4096, 8192, 16384, 32768, 65536, 131072];

export function contextLabel(tokens) {
  return tokens == null ? 'Авто' : `${whole.format(tokens)} токенов`;
}

export function createContextSettings(session, {
  displayedModel, getRecord, isMemoryChanging, refreshControls, requestMetrics,
}) {
  const contextSize = get('context-size');
  const saveContext = get('save-context');
  let contextOptions = contextPresets;
  let contextSaving = false;
  let contextEstimateRequest = 0;
  let contextMemory;
  let autoMemory;
  let configuredAutoContext;
  let autoSource;
  let autoReason;
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
    const memory = tokens == null ? autoMemory : contextMemory?.[tokens];
    const auto = tokens == null ? configuredAutoContext : null;
    const label = contextLabel(tokens) + (auto ? ` · ${whole.format(auto)} токенов` : '');
    get('context-choice').textContent = label;
    const profileAuto = get('benchmark-profile-auto');
    profileAuto.hidden = configuredAutoContext == null;
    profileAuto.textContent = profileAuto.hidden ? '' : `Auto для одной модели сейчас: ${contextLabel(configuredAutoContext)}`;
    contextSize.setAttribute('aria-valuetext', label);
    const required = memory?.total_bytes_excluding_unknown_kv;
    const budget = memory?.best_gpu_vram_available_bytes;
    const note = required == null ? '' :
      `${memory.source === 'measured' ? 'Замер' : 'Оценка'} ≈ ${memoryAmount(required)} VRAM` +
      (budget == null ? '' : ` · бюджет ${memoryAmount(budget)}`) +
      (memory.kv_included ? '.' : ' · без учёта KV-кэша.');
    get('context-note').textContent = tokens == null
      ? (auto ? [autoSource === 'pinned' ? 'Закреплённое окно.' : 'Окно уточняется при запуске.', note].filter(Boolean).join(' ')
        : autoReason ? `Авто недоступен: ${autoReason}.` : 'Ожидаем оценку Auto.')
      : note || 'Размер применяется при следующей загрузке.';
    renderContextMemoryWarning(memory);
  }

  function refreshMemoryWarning() {
    waitingForMemorySample = true;
    memorySampleEpoch++;
    requestMetrics();
  }

  function renderContextMemoryWarning(memory) {
    const warning = get('context-memory-warning');
    if (session.closed || isMemoryChanging() || waitingForMemorySample) {
      warning.hidden = true;
      return;
    }
    const message = contextMemoryWarning(memory);
    warning.hidden = !message;
    if (warning.textContent !== message) warning.textContent = message;
  }

  async function readContextEstimate(model, reset = false) {
    const request = ++contextEstimateRequest;
    if (reset) {
      contextMemory = autoMemory = configuredAutoContext = autoSource = autoReason = undefined;
      renderContextChoice();
    }
    if (!model) return;
    let estimate;
    try {
      const response = await fetch(`/model/context/estimate?model=${encodeURIComponent(model)}`);
      if (response.ok) estimate = await response.json();
    } catch {
      // The slider still works if Ollama can't provide model metadata.
    }
    if (request !== contextEstimateRequest) return;
    contextMemory = estimate?.memory_by_context;
    autoMemory = estimate?.auto_memory;
    configuredAutoContext = estimate?.auto_context_tokens;
    autoSource = estimate?.auto_context_source;
    autoReason = estimate?.auto_context_reason;
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
      if (epoch === memorySampleEpoch && !isMemoryChanging()) {
        waitingForMemorySample = false;
        readContextEstimate(displayedModel());
      }
    },
  };
}

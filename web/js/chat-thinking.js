import { modelResponseError, postModel } from './model-api.js';
import { element, get } from './ui.js';

function labelFor(think) {
  if (think === false) return 'Off';
  if (think === true) return 'On';
  return think ? think[0].toUpperCase() + think.slice(1) : '';
}

function currentChoice(settings) {
  for (const choice of [settings.think, settings.default]) {
    if (settings.options.includes(choice)) return choice;
  }
}

export function createThinkingControl() {
  const button = get('chat-thinking');
  const dialog = get('chat-thinking-dialog');
  const form = get('chat-thinking-form');
  const options = get('chat-thinking-options');
  const feedback = get('chat-thinking-feedback');
  const save = get('save-chat-thinking');
  const cancel = get('cancel-chat-thinking');
  let model;
  let dashboardModel;
  let revision;
  let requestId = 0;
  let settings;
  let editingModel;
  let saving = false;

  function render() {
    button.hidden = !settings?.options.length;
    button.disabled = saving;
    const label = settings && labelFor(currentChoice(settings));
    button.textContent = label ? `Thinking · ${label}` : 'Thinking';
    save.disabled = saving || !model || !options.querySelector(':checked');
    cancel.disabled = saving;
    options.querySelectorAll('input').forEach(input => { input.disabled = saving; });
  }

  async function readSettings(name) {
    const id = ++requestId;
    let next;
    try {
      if (name) {
        const response = await fetch(`/model/thinking?model=${encodeURIComponent(name)}`);
        if (response.ok) {
          next = await response.json();
          if (next.options.some(option => typeof option === 'string')) {
            next.options = next.options.filter(option => option !== true);
          }
        }
      }
    } catch {
      // Keep the composer usable while Ollama is unavailable.
    }
    if (id !== requestId) return;
    settings = next;
    render();
  }

  button.onclick = () => {
    editingModel = model;
    const selected = currentChoice(settings);
    options.replaceChildren(...settings.options.map(think => {
      const input = element('input', '');
      input.type = 'radio';
      input.name = 'think';
      input.value = JSON.stringify(think);
      input.checked = think === selected;
      input.autofocus = input.checked;
      const label = element('label', '');
      label.className = 'pin';
      label.append(input, labelFor(think));
      return label;
    }));
    get('chat-thinking-name').textContent = editingModel;
    get('chat-thinking-note').hidden = !settings.off_uses_lowest;
    feedback.textContent = '';
    render();
    dialog.showModal();
  };

  form.onchange = render;
  cancel.onclick = () => dialog.close();
  dialog.oncancel = event => {
    if (saving) event.preventDefault();
  };
  form.onsubmit = async event => {
    event.preventDefault();
    if (save.disabled) return;
    const think = JSON.parse(new FormData(form).get('think'));
    saving = true;
    feedback.textContent = '';
    render();
    try {
      const response = await postModel('/model/thinking', dashboardModel, { think }, editingModel);
      if (!response.ok) {
        throw new Error(response.status === 400 ? 'Модель больше не поддерживает этот уровень.' :
          modelResponseError(response));
      }
      if (editingModel === model && settings) {
        requestId++;
        settings.think = think;
      }
      dialog.close();
    } catch (error) {
      feedback.textContent = error.message;
    } finally {
      saving = false;
      render();
    }
  };

  return {
    update(next, run) {
      dashboardModel = run?.model;
      if (next === model && revision === run?.benchmark_revision) return;
      if (next !== model) {
        settings = undefined;
        if (dialog.open && !saving) dialog.close();
      }
      model = next;
      revision = run?.benchmark_revision;
      render();
      readSettings(model);
    },
  };
}

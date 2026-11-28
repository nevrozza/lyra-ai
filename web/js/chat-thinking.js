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
  const buttonLabel = get('chat-thinking-label');
  const popover = get('chat-thinking-popover');
  const options = get('chat-thinking-options');
  const feedback = get('chat-thinking-feedback');
  let model;
  let dashboardModel;
  let revision;
  let requestId = 0;
  let settings;
  let saving = false;
  let optionsKey;

  function isOpen() {
    return popover.matches(':popover-open');
  }

  function close() {
    if (isOpen()) popover.hidePopover();
  }

  function position() {
    if (!isOpen()) return;
    const rect = button.getBoundingClientRect();
    const width = popover.offsetWidth;
    const height = popover.offsetHeight;
    const top = rect.top >= height + 12 ? rect.top - height - 4 : rect.bottom + 4;
    popover.style.left = Math.max(8, Math.min(rect.right - width, window.innerWidth - width - 8)) + 'px';
    popover.style.top = Math.max(8, Math.min(top, window.innerHeight - height - 8)) + 'px';
  }

  function report(text = '') {
    feedback.textContent = text;
    feedback.hidden = !text;
    position();
  }

  function render() {
    button.hidden = !settings?.options.length;
    button.disabled = saving;
    if (button.hidden) close();
    const selected = settings && currentChoice(settings);
    const label = labelFor(selected);
    buttonLabel.textContent = label ? `Thinking · ${label}` : 'Thinking';
    button.title = buttonLabel.textContent;
    button.setAttribute('aria-label', buttonLabel.textContent);
    button.dataset.active = String(selected != null && selected !== false && selected !== 'off');
    get('chat-thinking-name').textContent = model ?? '';
    get('chat-thinking-note').hidden = !settings?.off_uses_lowest;
    const key = JSON.stringify(settings?.options ?? []);
    if (key !== optionsKey) {
      optionsKey = key;
      options.replaceChildren(...(settings?.options ?? []).map(think => {
        const choice = element('button', '');
        choice.type = 'button';
        choice.dataset.think = JSON.stringify(think);
        const check = element('span', '✓');
        check.className = 'terminal-check';
        check.setAttribute('aria-hidden', 'true');
        choice.append(element('span', labelFor(think)), check);
        choice.addEventListener('click', () => void change(think));
        return choice;
      }));
    }
    for (const choice of options.querySelectorAll('button')) {
      choice.disabled = saving;
      choice.setAttribute('aria-pressed', String(choice.dataset.think === JSON.stringify(selected)));
    }
    position();
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
    if (button.disabled || !settings?.options.length) return;
    if (isOpen()) {
      close();
      return;
    }
    report();
    render();
    popover.showPopover({ source: button });
    position();
    (options.querySelector('[aria-pressed="true"]:not(:disabled)')
      ?? options.querySelector('button:not(:disabled)'))?.focus({ preventScroll: true });
  };

  popover.addEventListener('toggle', () => {
    button.setAttribute('aria-expanded', String(isOpen()));
  });
  window.addEventListener('resize', position);
  window.addEventListener('scroll', position, true);

  async function change(think) {
    if (saving || !model || !settings?.options.includes(think)) return;
    if (think === currentChoice(settings)) {
      close();
      return;
    }
    const editingModel = model;
    saving = true;
    report();
    render();
    try {
      const response = await postModel('/model/thinking', dashboardModel, { think }, editingModel);
      if (!response.ok) {
        throw new Error(response.status === 400 ? 'Модель больше не поддерживает этот уровень.' :
          modelResponseError(response));
      }
      if (editingModel === model) {
        if (settings) {
          requestId++;
          settings.think = think;
        } else {
          void readSettings(model);
        }
        close();
      }
    } catch (error) {
      if (editingModel === model) report(error.message);
    } finally {
      saving = false;
      render();
    }
  }

  return {
    update(next, run) {
      dashboardModel = run?.model;
      if (next === model && revision === run?.benchmark_revision) return;
      if (next !== model) {
        close();
        settings = undefined;
        report();
      }
      model = next;
      revision = run?.benchmark_revision;
      render();
      void readSettings(model);
    },
  };
}

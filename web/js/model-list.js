import { element, get } from './ui.js';

export function createModelList({ selectModel, statusFor }) {
  const modelList = get('model-list');
  let modelRows = new Map();

  function render(names, loaded, selectedModel) {
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
      const selected = name === selectedModel;
      const view = statusFor(name, loaded);
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

  return {
    render,
    setDisabled(disabled) {
      for (const { button } of modelRows.values()) button.disabled = disabled;
    },
    refreshCountdown(names, loaded, selectedModel) {
      if ([...modelRows.values()].some(({ dot, detail }) => dot.dataset.state === 'resident' && !detail.hidden)) {
        render(names, loaded, selectedModel);
      }
    },
  };
}

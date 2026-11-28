import { element, get } from './ui.js';

const aiActionLabels = {
  load: 'ИИ загрузил модель',
  pin: 'ИИ закрепил модель в памяти',
  unpin: 'ИИ снял закрепление',
  unload: 'ИИ выгрузил модель',
  request: 'ИИ отправил запрос модели',
  description: 'ИИ изменил описание модели',
};

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
        const identity = element('span', '');
        identity.className = 'list-model-identity';
        const robot = get('model-ai-icon').content.firstElementChild.cloneNode(true);
        const robotTitle = robot.querySelector('title');
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
        identity.append(modelName, robot);
        button.append(identity, badge);
        button.addEventListener('click', () => selectModel(name));
        row.append(button);
        return [name, { row, button, dot, status, pin, detail, robot, robotTitle }];
      }));
      modelList.replaceChildren(...Array.from(modelRows.values(), ({ row }) => row));
    }
    for (const [name, { button, dot, status, pin, detail, robot, robotTitle }] of modelRows) {
      const selected = name === selectedModel;
      const view = statusFor(name, loaded);
      button.setAttribute('aria-pressed', String(selected));
      dot.dataset.state = view.mode;
      if (status.textContent !== view.label) status.textContent = view.label;
      pin.toggleAttribute('hidden', view.mode !== 'resident' || !view.pinned);
      const aiLabel = Object.hasOwn(aiActionLabels, view.aiAction) ? aiActionLabels[view.aiAction] : '';
      robot.toggleAttribute('hidden', !aiLabel);
      if (robotTitle.textContent !== aiLabel) {
        robotTitle.textContent = aiLabel;
        robot.setAttribute('aria-label', aiLabel);
      }
      detail.hidden = !view.detail;
      if (detail.textContent !== view.detail) detail.textContent = view.detail;
    }
    modelList.hidden = installed.length === 0;
    const message = get('model-list-message');
    message.hidden = installed.length > 0;
    message.dataset.state = names === undefined ? 'loading' : names === null ? 'error' : 'empty';
    get('model-list-message-title').textContent = names === undefined ? 'Загружаем модели' : names === null ? 'Ollama недоступна' : 'Нет локальных моделей';
    get('model-list-message-text').textContent = names === undefined ? 'Ожидаем данные Ollama.' :
      names === null ? 'Проверь, запущена ли Ollama.' : 'Добавь модель с помощью ollama pull.';
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

import { get } from './ui.js';

export function createCommands() {
  const panel = get('command-panel');
  const title = get('command-title');
  const error = get('command-error');
  const buttons = [...get('command-actions').querySelectorAll('button')];
  let current;
  let request = 0;
  let deciding = false;

  function render(command) {
    if (current?.id !== command?.id || current?.running !== command?.running) deciding = false;
    current = command;
    panel.hidden = !command;
    if (!command) return;
    title.textContent = command.stopping ? 'Остановка команды' :
      command.running ? 'Команда выполняется' : 'Разрешить запуск команды?';
    get('command-workspace').textContent = `${command.shell} · ${command.workspace}`;
    get('command-script').textContent = command.command.replace(/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/gu, character =>
      character === '\n' ? character : `\\u{${character.codePointAt(0).toString(16)}}`);
    for (const button of buttons) {
      button.hidden = (button.dataset.action === 'stop') !== command.running;
      button.disabled = deciding || command.stopping;
    }
  }

  async function refresh() {
    const version = ++request;
    try {
      const response = await fetch('/commands');
      if (!response.ok) throw new Error();
      const command = await response.json();
      if (version !== request) return;
      error.hidden = true;
      render(command);
    } catch {
      if (version !== request || !current) return;
      error.textContent = 'Не удалось обновить состояние команды.';
      error.hidden = false;
    }
  }

  for (const button of buttons) {
    button.addEventListener('click', async () => {
      if (!current || deciding) return;
      const command = current;
      deciding = true;
      error.hidden = true;
      render(command);
      try {
        const response = await fetch(`/commands/${command.id}`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(button.dataset.action),
        });
        if (response.status === 404 || response.status === 409) {
          await refresh();
        } else if (!response.ok) {
          throw new Error();
        }
      } catch {
        if (current?.id !== command.id || current.running !== command.running) return;
        deciding = false;
        render(current);
        error.textContent = 'Не удалось отправить решение. Повторите попытку.';
        error.hidden = false;
      }
    });
  }

  return { refresh, clear() { request++; render(null); } };
}

import { get } from './ui.js';

export function createCommands() {
  const panel = get('command-panel');
  const title = get('command-title');
  const error = get('command-error');
  const mode = get('command-mode');
  const settingsError = get('command-settings-error');
  const buttons = [...get('command-actions').querySelectorAll('button')];
  const notifications = createNotifications();
  let current;
  let savedMode;
  let online = false;
  let saving = false;
  let request = 0;
  let deciding = false;
  let revealOnLoad = location.hash === '#command-panel';

  function render(command) {
    if (current?.id !== command?.id || current?.running !== command?.running) deciding = false;
    current = command;
    notifications.update(command?.running ? null : command?.id);
    panel.hidden = !command;
    if (!command) return;
    if (revealOnLoad) {
      revealOnLoad = false;
      panel.scrollIntoView({ block: 'nearest' });
    }
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

  function updateMode() {
    mode.disabled = !online || !savedMode || saving;
    if (!saving && savedMode) mode.value = savedMode;
  }

  async function refresh() {
    const version = ++request;
    try {
      const response = await fetch('/commands');
      if (!response.ok) throw new Error();
      const state = await response.json();
      if (version !== request) return;
      savedMode = state.mode;
      updateMode();
      error.hidden = true;
      render(state.command);
    } catch {
      if (version !== request || !current) return;
      error.textContent = 'Не удалось обновить состояние команды.';
      error.hidden = false;
    }
  }

  mode.addEventListener('change', async () => {
    if (!online || saving || mode.value === savedMode) return;
    saving = true;
    settingsError.hidden = true;
    updateMode();
    try {
      const response = await fetch('/commands/mode', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(mode.value),
      });
      if (!response.ok) throw new Error();
      savedMode = mode.value;
      await refresh();
    } catch {
      settingsError.textContent = 'Не удалось сохранить режим команд.';
      settingsError.hidden = false;
    } finally {
      saving = false;
      updateMode();
    }
  });

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

  navigator.serviceWorker?.addEventListener('message', async event => {
    if (event.data !== 'show-command') return;
    await refresh();
    if (current) panel.scrollIntoView({ block: 'nearest' });
  });

  return {
    refresh,
    setOnline(value) { online = value; updateMode(); },
    clear() { request++; render(null); },
  };
}

function createNotifications() {
  const button = get('command-notifications');
  const status = get('command-notification-status');
  if (!navigator.serviceWorker || !window.Notification ||
      !window.ServiceWorkerRegistration?.prototype.showNotification) {
    return { update() {} };
  }

  const tag = 'lyra-command-approval';
  let registration;
  let pending;
  let notified;
  let queue = Promise.resolve();

  function renderPermission() {
    const permission = Notification.permission;
    button.hidden = permission !== 'default';
    status.hidden = permission === 'default';
    status.textContent = permission === 'granted'
      ? 'Уведомления о запросах команд включены.'
      : 'Уведомления заблокированы в настройках браузера.';
  }

  function sync() {
    queue = queue.then(async () => {
      if (!registration || Notification.permission !== 'granted') return;
      const displayed = await registration.getNotifications({ tag });
      const id = pending;
      for (const notification of displayed) {
        if (notification.data !== id) notification.close();
      }
      if (id == null || !document.hidden || notified === id) return;
      if (!displayed.some(notification => notification.data === id)) {
        await registration.showNotification('Lyra ждёт подтверждения', {
          body: 'Откройте Lyra, чтобы проверить команду Pi.',
          icon: '/lyra.svg',
          tag,
          data: id,
        });
      }
      notified = id;
    }).catch(() => {});
  }

  navigator.serviceWorker.register('/service-worker.js')
    .then(() => navigator.serviceWorker.ready)
    .then(ready => {
      registration = ready;
      renderPermission();
      sync();
    })
    .catch(() => {
      status.textContent = 'Не удалось настроить уведомления.';
      status.hidden = false;
    });

  button.addEventListener('click', async () => {
    try {
      await Notification.requestPermission();
      renderPermission();
      sync();
    } catch {
      status.textContent = 'Не удалось запросить разрешение на уведомления.';
      status.hidden = false;
    }
  });
  document.addEventListener('visibilitychange', () => {
    if (registration) renderPermission();
    sync();
  });

  return {
    update(id) {
      if (pending === id) return;
      pending = id;
      notified = undefined;
      sync();
    },
  };
}

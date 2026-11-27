import { get } from './ui.js';

export function commandLocation(command, selection) {
  if (!command) return null;
  if (!command.owner) return 'external';
  return selection?.projectId === command.owner.project_id &&
    selection?.chatId === command.owner.chat_id ? 'chat' : 'other';
}

export function createCommands({ openChat }) {
  const panel = get('command-panel');
  const title = get('command-title');
  const error = get('command-error');
  const modeButtons = [...get('command-mode').querySelectorAll('button')];
  const settingsError = get('command-settings-error');
  const buttons = [...get('command-actions').querySelectorAll('button')];
  const notifications = createNotifications();
  const chatHost = get('command-chat-host');
  const externalHost = get('command-external-host');
  const pendingLink = get('command-pending-link');
  const settingsPopover = get('command-settings-popover');
  const terminalButton = get('command-settings-global');
  const modeLabel = get('command-mode-label');
  let selection;
  let fresh = false;
  let current;
  let savedMode;
  let online = false;
  let saving = false;
  let request = 0;
  let deciding = false;
  let revealOnLoad = location.hash === '#command-panel';

  function render(command) {
    const changed = current?.id !== command?.id || current?.running !== command?.running;
    if (changed) deciding = false;
    current = command;
    notifications.update(command?.running ? null : command?.id);
    const place = commandLocation(command, selection);
    const host = place === 'external' ? externalHost : chatHost;
    const visible = place === 'chat' || place === 'external';
    const wasHidden = panel.hidden;
    chatHost.hidden = place !== 'chat';
    externalHost.hidden = place !== 'external';
    if (visible && panel.parentElement !== host) host.append(panel);
    panel.hidden = !visible;
    pendingLink.hidden = place !== 'other';
    pendingLink.textContent = command?.running ? 'Команда в другом чате' : 'Терминал ждёт подтверждения';
    if (!visible) return;
    if (place === 'chat' && (changed || wasHidden)) panel.scrollIntoView({ block: 'nearest' });
    title.textContent = command.stopping ? 'Остановка команды' :
      command.running ? 'Команда выполняется' : 'Разрешить запуск команды?';
    get('command-workspace').textContent = `${place === 'external' ? 'Вне чата · ' : ''}${command.shell} · ${command.workspace}`;
    get('command-script').textContent = command.command.replace(/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/gu, character =>
      character === '\n' ? character : `\\u{${character.codePointAt(0).toString(16)}}`);
    for (const button of buttons) {
      button.hidden = (button.dataset.action === 'stop') !== command.running;
      button.disabled = !online || !fresh || deciding || command.stopping;
    }
  }

  function updateMode() {
    const known = online && fresh && savedMode;
    for (const button of modeButtons) {
      button.disabled = !known || saving;
      button.setAttribute('aria-pressed', String(Boolean(known && button.dataset.mode === savedMode)));
    }
    terminalButton.dataset.mode = known ? savedMode : '';
    modeLabel.textContent = !known ? 'Нет данных' :
      savedMode === 'allow_all' ? 'Разрешать всегда' : 'Спрашивать всегда';
    terminalButton.title = 'Терминал — глобальные разрешения Lyra';
    terminalButton.setAttribute('aria-label', `Терминал: ${modeLabel.textContent}. Глобальные разрешения Lyra`);
  }

  async function refresh() {
    const version = ++request;
    try {
      const response = await fetch('/commands');
      if (!response.ok) throw new Error();
      const state = await response.json();
      if (version !== request) return;
      savedMode = state.mode;
      fresh = true;
      updateMode();
      error.hidden = true;
      render(state.command);
      if (revealOnLoad && state.command) {
        revealOnLoad = false;
        void reveal();
      }
    } catch {
      if (version !== request) return;
      fresh = false;
      updateMode();
      render(current);
      if (!current) return;
      error.textContent = 'Не удалось обновить состояние команды.';
      error.hidden = false;
    }
  }

  async function changeMode(next) {
    if (!online || !fresh || saving) return;
    if (next === savedMode) {
      settingsPopover.hidePopover();
      return;
    }
    saving = true;
    settingsError.hidden = true;
    updateMode();
    try {
      const response = await fetch('/commands/mode', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(next),
      });
      if (!response.ok) throw new Error();
      savedMode = next;
      await refresh();
      if (fresh && settingsPopover.matches(':popover-open')) settingsPopover.hidePopover();
    } catch {
      settingsError.textContent = 'Не удалось сохранить режим команд.';
      settingsError.hidden = false;
    } finally {
      saving = false;
      updateMode();
    }
  }
  for (const button of modeButtons) {
    button.addEventListener('click', () => void changeMode(button.dataset.mode));
  }

  for (const button of buttons) {
    button.addEventListener('click', async () => {
      if (!online || !fresh || !current || deciding || panel.hidden) return;
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
        if (!response.ok && response.status !== 404 && response.status !== 409) throw new Error();
        await refresh();
      } catch {
        if (current?.id !== command.id || current.running !== command.running) return;
        deciding = false;
        render(current);
        error.textContent = 'Не удалось отправить решение. Повторите попытку.';
        error.hidden = false;
      }
    });
  }

  async function reveal() {
    const command = current;
    if (!command) return;
    if (command.owner) await openChat(command.owner);
    if (current?.id === command.id && !panel.hidden) panel.scrollIntoView({ block: 'nearest' });
  }

  pendingLink.addEventListener('click', () => void reveal());
  function positionSettings() {
    if (!settingsPopover.matches(':popover-open')) return;
    const rect = terminalButton.getBoundingClientRect();
    const width = settingsPopover.offsetWidth;
    const height = settingsPopover.offsetHeight;
    const top = rect.top >= height + 12 ? rect.top - height - 4 : rect.bottom + 4;
    settingsPopover.style.left = Math.max(8, Math.min(rect.left, window.innerWidth - width - 8)) + 'px';
    settingsPopover.style.top = Math.max(8, Math.min(top, window.innerHeight - height - 8)) + 'px';
  }
  terminalButton.addEventListener('click', () => {
    if (settingsPopover.matches(':popover-open')) {
      settingsPopover.hidePopover();
      return;
    }
    settingsPopover.showPopover({ source: terminalButton });
    positionSettings();
    (modeButtons.find(button => button.getAttribute('aria-pressed') === 'true' && !button.disabled)
      ?? modeButtons.find(button => !button.disabled))?.focus({ preventScroll: true });
  });
  settingsPopover.addEventListener('toggle', () => {
    terminalButton.setAttribute('aria-expanded', String(settingsPopover.matches(':popover-open')));
  });
  window.addEventListener('resize', positionSettings);
  window.addEventListener('scroll', positionSettings, true);
  navigator.serviceWorker?.addEventListener('message', async event => {
    if (event.data !== 'show-command') return;
    await refresh();
    await reveal();
  });

  return {
    refresh,
    setSelection(next) {
      if (selection?.projectId === next?.projectId && selection?.chatId === next?.chatId) return;
      selection = next;
      render(current);
    },
    setOnline(value) {
      if (online && !value) fresh = false;
      online = value;
      updateMode();
      render(current);
    },
    clear() { request++; fresh = false; updateMode(); render(null); },
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
          body: 'Откройте Lyra, чтобы проверить команду терминала.',
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

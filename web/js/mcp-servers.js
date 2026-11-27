import { get } from './ui.js';

export function createMcpServers() {
  const form = get('mcp-servers-form');
  const editor = get('mcp-servers-config');
  const save = get('mcp-servers-save');
  const reload = get('mcp-servers-reload');
  const status = get('mcp-servers-status');
  const endpoint = '/settings/mcp-servers';
  let online = false;
  let busy = false;
  let loaded = false;
  let available = true;
  let saved = '';

  function render() {
    const disabled = !online || busy || !available;
    editor.disabled = disabled || !loaded;
    save.disabled = editor.disabled || editor.value === saved;
    reload.disabled = disabled;
  }

  function report(message, error = false) {
    status.textContent = message;
    status.dataset.error = String(error);
  }

  async function request(options) {
    busy = true;
    report(options ? 'Сохраняем…' : 'Загружаем…');
    render();
    try {
      const response = await fetch(endpoint, options);
      if (!response.ok) {
        if (response.status === 403) available = false;
        throw new Error(await response.text() || `HTTP ${response.status}`);
      }
      const config = await response.json();
      saved = JSON.stringify(config, null, 2);
      editor.value = saved;
      loaded = true;
      const enabled = Object.values(config.mcpServers).filter(server => server.enabled !== false).length;
      report(options ? `Сохранено. Включено серверов: ${enabled}.` : `Включено серверов: ${enabled}.`);
    } catch (error) {
      report(error.message, true);
    } finally {
      busy = false;
      render();
    }
  }

  reload.onclick = () => request();
  editor.oninput = render;
  form.onsubmit = event => {
    event.preventDefault();
    if (save.disabled) return;
    try {
      JSON.parse(editor.value);
    } catch {
      report('Проверьте синтаксис JSON.', true);
      return;
    }
    request({ method: 'POST', headers: { 'Content-Type': 'application/json' }, body: editor.value });
  };
  return {
    setOnline(value) {
      const connected = value && !online;
      online = value;
      render();
      if (connected && !loaded && !busy && available) request();
    },
  };
}

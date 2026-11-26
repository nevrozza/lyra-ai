import { element, get } from './ui.js';

export function createSkills() {
  const form = get('skill-import');
  const button = get('install-skill');
  const folder = get('skill-folder');
  const file = get('skill-file');
  const pickFolder = get('pick-skill-folder');
  const pickFile = get('pick-skill-file');
  const selection = get('skill-selection');
  const emptySelection = selection.textContent;
  const result = get('skill-result');
  const message = get('skill-catalog-message');
  const list = get('skill-catalog');
  let saving = false;
  let online = false;
  let revision = 0;
  let files = [];

  function updateControls() {
    pickFolder.disabled = pickFile.disabled = saving || !online;
    button.disabled = saving || !online || !files.length;
  }

  function select(input) {
    const chosen = [...input.files];
    input.value = '';
    if (!chosen.length) return;
    files = chosen.map(file => ({
      path: file.webkitRelativePath ? file.webkitRelativePath.split('/').slice(1).join('/') : file.name,
      file,
    }));
    result.textContent = '';
    if (!files.some(file => file.path === 'SKILL.md')) {
      result.dataset.error = 'true';
      result.textContent = 'В корне выбранного скилла нужен файл SKILL.md.';
      files = [];
    } else if (files.length > 2048 || files.reduce((size, entry) => size + entry.file.size, 0) > 16 * 1024 * 1024) {
      result.dataset.error = 'true';
      result.textContent = 'Скилл может содержать до 2048 файлов общим объёмом до 16 МБ.';
      files = [];
    }
    selection.textContent = files.length
      ? chosen[0].webkitRelativePath.split('/')[0] || chosen[0].name
      : emptySelection;
    updateControls();
  }

  async function refresh() {
    const request = ++revision;
    try {
      const response = await fetch('/skills');
      if (!response.ok) throw new Error();
      const skills = await response.json();
      if (request !== revision) return;
      message.textContent = skills.length ? '' : 'Пока нет установленных скиллов.';
      list.replaceChildren(...skills.map(skill => {
        const entry = element('li', '');
        const description = element('p', skill.description);
        description.className = 'secondary';
        entry.append(element('code', skill.name), description);
        return entry;
      }));
    } catch {
      if (request === revision) message.textContent = 'Не удалось загрузить список скиллов.';
    }
  }

  pickFolder.addEventListener('click', () => folder.click());
  pickFile.addEventListener('click', () => file.click());
  folder.addEventListener('change', () => select(folder));
  file.addEventListener('change', () => select(file));

  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (saving || !online || !files.length) return;
    saving = true;
    result.dataset.error = 'false';
    result.textContent = 'Загрузка…';
    updateControls();
    try {
      const payload = [];
      for (const { path, file } of files) {
        payload.push({ path, data: await readFile(file) });
      }
      const response = await fetch('/skills/upload', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      if (response.status === 413) throw new Error('Слишком большой объём загрузки.');
      if (!response.ok) throw new Error(await response.text());
      const skill = await response.json();
      files = [];
      selection.textContent = emptySelection;
      result.textContent = `Скилл «${skill.name}» установлен.`;
      await refresh();
    } catch (error) {
      result.dataset.error = 'true';
      result.textContent = error instanceof TypeError || !error.message
        ? 'Не удалось установить скилл.'
        : error.message;
    } finally {
      saving = false;
      updateControls();
    }
  });

  return {
    refresh,
    setOnline(value) {
      online = value;
      updateControls();
    },
  };
}

function readFile(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result.slice(reader.result.indexOf(',') + 1));
    reader.onerror = () => reject(new Error(`Не удалось прочитать файл «${file.name}».`));
    reader.readAsDataURL(file);
  });
}

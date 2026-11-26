import { element, get } from './ui.js';

export function createProjects() {
  const projectSelect = get('project-select');
  const chatSelect = get('project-chat-select');
  const activeAgents = get('project-active-agents');
  const projectForm = get('project-form');
  const folderList = get('project-folders');
  const pickFolder = get('pick-project-folder');
  const chatForm = get('project-chat-form');
  const prompt = get('project-prompt');
  const messages = get('project-messages');
  const status = get('project-message');
  const skill = get('project-skill');
  const drafts = new Map();
  const sending = new Set();
  let catalog;
  let installedSkills = [];
  let folders = [];
  let editing;
  let online = false;
  let run;
  let saving = false;
  let picking = false;
  let catalogRequest = 0;
  let historyRequest = 0;
  let preferredSelection;

  const selectedProject = () => catalog?.projects.find(project => project.id === Number(projectSelect.value));
  const selectedChat = () => selectedProject()?.sessions.find(chat => chat.id === Number(chatSelect.value));

  function report(text = '', error = false) {
    status.dataset.error = String(error);
    status.textContent = text;
  }

  function updateControls() {
    const available = online && catalog != null && !saving && !picking;
    const agents = online ? Object.values(run?.delegations ?? {}) : [];
    const chat = selectedChat();
    const active = chat && (sending.has(chat.id) ||
      agents.some(task => task.project_session_id === chat.id));
    const agentStatus = agents.length > 2
      ? `Активные агенты · ${agents.length} · ${agents.map(agent => agent.model).join(', ')}`
      : '';
    activeAgents.hidden = !agentStatus;
    if (activeAgents.textContent !== agentStatus) activeAgents.textContent = agentStatus;
    get('new-project').disabled = projectSelect.disabled = !available;
    get('edit-project').disabled = !available || !selectedProject();
    get('new-project-chat').disabled = !available;
    pickFolder.disabled = !available;
    pickFolder.textContent = picking ? 'Выбор папки…' : 'Добавить папку';
    get('cancel-project').disabled = picking;
    for (const button of folderList.querySelectorAll('button')) button.disabled = !available;
    projectForm.querySelector('[type="submit"]').disabled = !available || !folders.length;
    chatForm.querySelector('[type="submit"]').disabled = !available;
    get('send-project-prompt').disabled = !online || !run?.model || !chat || Boolean(active);
    get('send-project-prompt').textContent = active ? 'Агент работает…' : 'Отправить';
    get('project-chat-model').textContent = run?.model ? `· ${run.model}` : '· Выбери модель';
  }

  function showConversation() {
    const chat = selectedChat();
    get('project-conversation').hidden = !chat;
    get('project-chat-workspace').textContent = chat?.folder ?? '';
    prompt.value = drafts.get(chat?.id) ?? '';
    messages.replaceChildren();
    updateControls();
    refreshHistory();
  }

  function showProject(chatId) {
    const project = selectedProject();
    get('project-chats').hidden = !project;
    setOptions(chatSelect, project?.sessions.map(chat => [chat.id, chat.name]) ?? [], chatId, 'Нет чатов');
    showConversation();
  }

  async function refreshHistory() {
    const requestId = ++historyRequest;
    const project = selectedProject();
    const chat = selectedChat();
    if (!chat || !online) return;
    try {
      const history = await request(`/projects/${project.id}/sessions/${chat.id}`);
      if (requestId !== historyRequest) return;
      const atEnd = messages.scrollHeight - messages.scrollTop - messages.clientHeight < 20;
      messages.replaceChildren(...history.map(message => {
        const item = element('li', '');
        item.append(element('strong', message.role === 'user' ? 'Ты' : 'Pi'), element('p', message.text));
        return item;
      }));
      if (atEnd) messages.scrollTop = messages.scrollHeight;
    } catch (error) {
      if (requestId === historyRequest) report(error.message, true);
    }
  }

  async function refresh(preferredProject, preferredChat) {
    if (preferredProject != null) preferredSelection = { project: preferredProject, chat: preferredChat };
    const requestId = ++catalogRequest;
    try {
      const next = await request('/projects');
      if (requestId !== catalogRequest) return;
      const projectId = preferredSelection?.project ?? projectSelect.value;
      const chatId = preferredSelection?.chat ?? chatSelect.value;
      preferredSelection = undefined;
      const previousProject = selectedProject()?.id;
      const previousChat = selectedChat()?.id;
      catalog = next;
      setOptions(projectSelect, catalog.projects.map(project => [project.id, project.name]), projectId, 'Нет проектов');
      const project = selectedProject();
      get('project-chats').hidden = !project;
      setOptions(chatSelect, project?.sessions.map(chat => [chat.id, chat.name]) ?? [], chatId, 'Нет чатов');
      if (previousProject !== project?.id || previousChat !== selectedChat()?.id) showConversation();
      else refreshHistory();
      updateControls();
    } catch (error) {
      if (requestId === catalogRequest) report(error.message, true);
    }
  }

  function openProjectForm(project) {
    editing = project?.id;
    get('project-name').value = project?.name ?? '';
    folders = [...(project?.folders ?? [])];
    renderFolders();
    const selectedSkill = project?.search_skill ?? '';
    renderSkills(selectedSkill);
    projectForm.hidden = false;
    get('project-name').focus();
    request('/skills').then(skills => {
      installedSkills = skills.map(skill => skill.name);
      renderSkills(skill.value);
    }).catch(error => report(error.message, true));
  }

  function renderFolders() {
    folderList.replaceChildren(...folders.map(folder => {
      const row = element('li', '');
      const remove = element('button', 'Убрать');
      remove.type = 'button';
      remove.className = 'unload';
      remove.setAttribute('aria-label', `Убрать папку ${folder}`);
      remove.onclick = () => {
        folders = folders.filter(path => path !== folder);
        renderFolders();
        pickFolder.focus();
      };
      row.append(element('code', folder), remove);
      return row;
    }));
    updateControls();
  }

  function renderSkills(selected) {
    const names = [...new Set([...installedSkills, ...(selected ? [selected] : [])])];
    setOptions(skill, [['', 'Встроенный поиск'], ...names.map(name => [name, name])], selected);
  }

  async function save(path, data, success) {
    saving = true;
    updateControls();
    report();
    try {
      await success(await request(path, data));
    } catch (error) {
      report(error.message, true);
    } finally {
      saving = false;
      updateControls();
    }
  }

  get('new-project').onclick = () => openProjectForm();
  get('edit-project').onclick = () => openProjectForm(selectedProject());
  get('cancel-project').onclick = () => { projectForm.hidden = true; };
  pickFolder.onclick = async () => {
    if (pickFolder.disabled) return;
    picking = true;
    report();
    updateControls();
    try {
      const folder = await request('/projects/pick-folder', null);
      if (folder && !folders.includes(folder)) {
        folders.push(folder);
        renderFolders();
      }
    } catch (error) {
      report(error.message, true);
    } finally {
      picking = false;
      updateControls();
      if (!pickFolder.disabled) pickFolder.focus();
    }
  };
  projectSelect.onchange = () => {
    preferredSelection = undefined;
    projectForm.hidden = chatForm.hidden = true;
    report();
    showProject();
  };
  chatSelect.onchange = () => {
    preferredSelection = undefined;
    chatForm.hidden = true;
    report();
    showConversation();
  };

  projectForm.onsubmit = event => {
    event.preventDefault();
    if (saving || picking || !online || !folders.length) return;
    save('/projects', {
      id: editing,
      name: get('project-name').value,
      folders,
      search_skill: skill.value || null,
    }, async project => {
      projectForm.hidden = chatForm.hidden = true;
      await refresh(project.id);
    });
  };

  get('new-project-chat').onclick = () => {
    const folder = get('project-chat-folder');
    setOptions(folder, selectedProject().folders.map(path => [path, path]));
    get('project-chat-name').value = '';
    chatForm.hidden = false;
    get('project-chat-name').focus();
  };
  get('cancel-project-chat').onclick = () => { chatForm.hidden = true; };
  chatForm.onsubmit = event => {
    event.preventDefault();
    const project = selectedProject();
    if (!project || saving || !online) return;
    save('/projects/sessions', {
      project_id: project.id,
      name: get('project-chat-name').value,
      folder: get('project-chat-folder').value,
    }, async chat => {
      chatForm.hidden = true;
      await refresh(project.id, chat.id);
    });
  };

  prompt.oninput = () => drafts.set(selectedChat()?.id, prompt.value);
  get('project-prompt-form').onsubmit = async event => {
    event.preventDefault();
    if (get('send-project-prompt').disabled || !prompt.value.trim()) return;
    const project = selectedProject();
    const chat = selectedChat();
    const text = prompt.value;
    sending.add(chat.id);
    report();
    updateControls();
    try {
      const reply = await request('/projects/run', {
        project_id: project.id,
        project_session_id: chat.id,
        model: run.model,
        prompt: text,
        allow_commands: get('project-commands').checked,
      });
      if (drafts.get(chat.id) === text) drafts.delete(chat.id);
      if (selectedChat()?.id === chat.id) {
        prompt.value = drafts.get(chat.id) ?? '';
        if (reply.structuredContent?.truncated) report('Ответ достиг лимита генерации.');
      }
    } catch (error) {
      if (selectedChat()?.id === chat.id) report(error.message, true);
    } finally {
      sending.delete(chat.id);
      updateControls();
      if (selectedChat()?.id === chat.id) refreshHistory();
    }
  };

  return {
    refresh,
    update(connected, state) {
      online = connected;
      run = state;
      updateControls();
    },
  };
}

function setOptions(select, pairs, selected, empty = '') {
  select.replaceChildren(...(pairs.length ? pairs : [['', empty]]).map(([value, label]) => {
    const option = element('option', label);
    option.value = value;
    return option;
  }));
  if (pairs.some(([value]) => String(value) === String(selected))) select.value = selected;
}

async function request(path, data) {
  const response = await fetch(path, data === undefined ? undefined : {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(data),
  });
  if (!response.ok) throw new Error(await response.text() || 'Запрос к Lyra завершился ошибкой.');
  if (response.status !== 204) return response.json();
}

import { element, get } from './ui.js';

export function createProjects() {
  const tree = get('project-tree');
  const projectDialog = get('project-dialog');
  const chatDialog = get('project-chat-dialog');
  const projectForm = get('project-form');
  const chatForm = get('project-chat-form');
  const folderList = get('project-folders');
  const pickFolder = get('pick-project-folder');
  const skill = get('project-skill');
  const prompt = get('project-prompt');
  const composer = get('project-prompt-form');
  const messages = get('project-messages');
  const actions = get('project-actions');
  const sendButton = get('send-project-prompt');
  const stopButton = get('stop-project-prompt');
  const modelSelect = get('project-chat-model');
  const chatFolderSelect = get('project-chat-folder');
  const drafts = new Map();
  const chatModels = new Map();
  const chatModes = new Map();
  const chatMode = () => chatModes.get(chatId) ?? 'chat';
  const sending = new Map();
  const stopRequests = new Set();
  let catalog;
  let archiveView = false;
  let projectId;
  let chatId;
  let editing;
  let folders = [];
  let installedSkills = [];
  let online = false;
  let run;
  let installedModels = [];
  let modelOptionsKey;
  let saving = false;
  let picking = false;
  let catalogRequest = 0;
  let historyRequest = 0;
  let preferredSelection;
  let history = [];

  const selectedProject = () => catalog?.projects.find(project => project.id === projectId);
  const selectedChat = () => selectedProject()?.sessions.find(chat => chat.id === chatId);
  const available = () => online && catalog != null && !saving && !picking;
  const agents = () => online ? Object.values(run?.delegations ?? {}) : [];
  const chatRun = id => online && id != null
    ? Object.entries(run?.delegations ?? {}).find(([, task]) => task.project_session_id === id)
    : undefined;
  const visibleChats = project => project.sessions.filter(chat => project.archived || chat.archived === archiveView);

  function report(text = '', error = false) {
    const target = projectDialog.open ? 'project-form-message'
      : chatDialog.open ? 'project-chat-form-message' : 'project-message';
    get(target).dataset.error = String(error);
    get(target).textContent = text;
  }

  function updateControls() {
    const ready = available();
    const project = selectedProject();
    const chat = selectedChat();
    const delegation = chatRun(chat?.id);
    const active = chat && (sending.has(chat.id) || delegation);
    const stopping = delegation && (delegation[1].stopping || stopRequests.has(delegation[0]));
    const running = agents();
    const activeChats = new Set([...sending.keys(), ...running.map(task => task.project_session_id)]);
    for (const spinner of tree.querySelectorAll('[data-chat-progress]')) {
      spinner.hidden = !activeChats.has(Number(spinner.dataset.chatProgress));
    }
    get('project-progress').hidden = !active;
    const progressText = stopping ? 'Останавливаем…' : delegation ? 'Модель отвечает…' : 'Отправляем…';
    const progress = get('project-progress-text');
    if (progress.textContent !== progressText) progress.textContent = progressText;
    get('project-empty').hidden = Boolean(active || history.length || sending.has(chatId));
    const agentStatus = running.length > 2
      ? `Активные агенты · ${running.length} · ${running.map(agent => agent.model).join(', ')}`
      : '';
    const activeAgents = get('project-active-agents');
    activeAgents.hidden = !agentStatus;
    if (activeAgents.textContent !== agentStatus) activeAgents.textContent = agentStatus;
    get('new-project').disabled = !ready;
    get('new-project-chat').hidden = !project || project.archived || archiveView;
    get('new-project-chat').disabled = !ready;
    get('edit-project').hidden = !project || project.archived;
    get('edit-project').disabled = !ready;
    get('edit-project-chat-folders').hidden = !chat || chat.archived || project.archived;
    get('edit-project-chat-folders').disabled = !ready;
    const archiveProject = get('archive-project');
    archiveProject.textContent = project?.archived ? 'Восстановить проект' : 'Архивировать проект';
    archiveProject.disabled = !ready;
    const archiveChat = get('archive-project-chat');
    archiveChat.hidden = !chat || project.archived;
    archiveChat.textContent = chat?.archived ? 'Восстановить чат' : 'Архивировать чат';
    archiveChat.disabled = !ready;
    pickFolder.disabled = !ready;
    pickFolder.textContent = picking ? 'Выбор папки…' : 'Выбрать папку';
    get('cancel-project').disabled = saving || picking;
    get('cancel-project-chat').disabled = saving;
    for (const button of folderList.querySelectorAll('button')) button.disabled = !ready;
    projectForm.querySelector('[type="submit"]').disabled = !ready || !folders.length;
    chatForm.querySelector('[type="submit"]').disabled = !ready;
    composer.hidden = !chat || chat.archived || project.archived;
    const working = chatMode() === 'work';
    get('project-mode-chat').setAttribute('aria-pressed', String(!working));
    get('project-mode-work').setAttribute('aria-pressed', String(working));
    get('project-command-link').hidden = !working;
    prompt.placeholder = working ? 'Задача для Lyra…' : 'Спроси модель…';
    const model = refreshModels();
    modelSelect.disabled = !online || !model;
    sendButton.disabled = !online || !model || !prompt.value.trim() || composer.hidden || Boolean(active);
    sendButton.hidden = Boolean(active);
    stopButton.disabled = !delegation || Boolean(stopping);
    stopButton.textContent = stopping ? 'Останавливаем…' : 'Остановить';
  }

  function refreshModels() {
    const dashboardModel = run?.model;
    const defaultModel = installedModels.length && !installedModels.includes(dashboardModel)
      ? installedModels[0] : dashboardModel;
    const models = [...new Set([defaultModel, ...installedModels].filter(Boolean))];
    const preferred = chatModels.get(chatId);
    const selected = models.includes(preferred) ? preferred : '';
    const key = JSON.stringify([defaultModel, models, selected]);
    if (key !== modelOptionsKey) {
      modelOptionsKey = key;
      const options = [['', defaultModel ? `По умолчанию · ${defaultModel}` : 'Нет доступных моделей']];
      setOptions(modelSelect, [...options, ...models.map(model => [model, model])], selected);
    }
    return selected || defaultModel;
  }

  function renderTree(projects) {
    const focused = document.activeElement?.dataset.projectEntry;
    tree.replaceChildren(...projects.map(project => {
      const item = element('li', '');
      const heading = treeButton(project.name, project.id, undefined);
      heading.classList.add('project-tree-heading');
      item.append(heading);
      const chats = visibleChats(project);
      if (chats.length) {
        const list = element('ul', '');
        list.append(...chats.map(chat => {
          const entry = element('li', '');
          entry.append(treeButton(chat.name, project.id, chat.id));
          return entry;
        }));
        item.append(list);
      }
      return item;
    }));
    if (focused) [...tree.querySelectorAll('button')].find(button => button.dataset.projectEntry === focused)?.focus();
    get('show-active-projects').setAttribute('aria-pressed', String(!archiveView));
    get('show-archived-projects').setAttribute('aria-pressed', String(archiveView));
    get('project-tree-empty').textContent = projects.length ? ''
      : archiveView ? 'Архив пуст.' : 'Рабочих проектов пока нет.';
  }

  function treeButton(label, id, session) {
    const button = element('button', '');
    button.type = 'button';
    button.title = label;
    button.dataset.projectEntry = `${id}/${session ?? ''}`;
    button.setAttribute('aria-current', String(id === projectId && session === chatId));
    button.append(element('span', label));
    if (session != null) {
      const spinner = element('span', '');
      spinner.className = 'project-spinner';
      spinner.dataset.chatProgress = session;
      spinner.setAttribute('role', 'img');
      spinner.setAttribute('aria-label', 'Модель отвечает');
      spinner.hidden = true;
      button.append(spinner);
    }
    button.onclick = () => {
      report();
      select({ project: id, chat: session });
    };
    return button;
  }

  function select(preferred) {
    const projects = (catalog?.projects ?? []).filter(project =>
      archiveView ? project.archived || visibleChats(project).length : !project.archived);
    const previousChat = chatId;
    const project = projects.find(project => project.id === (preferred?.project ?? projectId)) ?? projects[0];
    const chats = project ? visibleChats(project) : [];
    const requested = preferred ? preferred.chat : project?.id === projectId ? chatId : chats[0]?.id;
    projectId = project?.id;
    chatId = chats.find(chat => chat.id === requested)?.id;
    const chat = selectedChat();
    if (chatId !== previousChat) {
      actions.open = false;
      history = [];
      prompt.value = drafts.get(chatId) ?? '';
    }

    renderTree(projects);
    get('project-selection').hidden = !project;
    const breadcrumb = get('project-breadcrumb');
    breadcrumb.hidden = !chat;
    breadcrumb.textContent = chat ? project.name : '';
    get('current-project-title').textContent = chat?.name ?? project?.name ?? '';
    const workspace = get('project-chat-workspace');
    workspace.hidden = !chat;
    const workingFolders = chat ? chatFolders(project, chat) : [];
    workspace.textContent = chat ? [chat.folder, ...(workingFolders.length > 1 ? [`+${workingFolders.length - 1}`] : [])].join(' · ') : '';
    workspace.title = workingFolders.join('\n');
    const notice = get('project-archive-notice');
    notice.hidden = !project?.archived && !chat?.archived;
    notice.textContent = project?.archived ? 'Проект в архиве. Восстанови проект, чтобы продолжить работу.'
      : chat?.archived ? 'Чат в архиве. Восстанови чат, чтобы продолжить переписку.' : '';
    renderHistory();
    updateControls();
    return refreshHistory();
  }

  function renderHistory(scrollToEnd = false) {
    const pending = sending.get(chatId);
    const entries = pending && !history.some(message => message.request_id === pending.request_id)
      ? [...history, pending] : history;
    const empty = get('project-empty');
    empty.hidden = entries.length > 0 || Boolean(chatRun(chatId));
    empty.textContent = chatId ? 'В этом чате пока нет сообщений.'
      : projectId ? archiveView ? 'Выбери чат, чтобы прочитать переписку.' : 'Выбери чат или начни новый.'
        : archiveView ? 'Здесь появятся архивированные проекты и чаты.' : 'Создай проект, чтобы начать работу.';
    const reading = messages.parentElement;
    const atEnd = reading.scrollHeight - reading.scrollTop - reading.clientHeight < 24;
    messages.hidden = !entries.length;
    messages.replaceChildren(...entries.map(message => {
      const item = element('li', '');
      item.dataset.role = message.role;
      item.dataset.origin = message.origin ?? '';
      const label = message.role === 'assistant' ? (message.model || 'Lyra')
        : message.origin === 'orchestrator' ? 'Оркестратор' : '';
      if (label) item.append(element('strong', label));
      item.append(element('p', message.text));
      return item;
    }));
    if (atEnd || scrollToEnd) reading.scrollTop = reading.scrollHeight;
  }

  async function refreshHistory() {
    const requestId = ++historyRequest;
    if (!chatId || !online) return;
    try {
      const next = await request(`/projects/${projectId}/sessions/${chatId}`);
      if (requestId === historyRequest) {
        history = next;
        renderHistory();
      }
      return next;
    } catch (error) {
      if (requestId === historyRequest) report(error.message, true);
    }
  }

  async function refresh(preferred) {
    if (preferred) preferredSelection = preferred;
    const requestId = ++catalogRequest;
    try {
      const next = await request('/projects');
      if (requestId !== catalogRequest) return;
      catalog = next;
      const selection = preferredSelection;
      preferredSelection = undefined;
      await select(selection);
    } catch (error) {
      if (requestId === catalogRequest) report(error.message, true);
    }
  }

  function switchView(archived) {
    if (archiveView === archived) return;
    archiveView = archived;
    preferredSelection = undefined;
    actions.open = false;
    report();
    select();
  }

  function openProjectForm(project) {
    editing = project?.id;
    get('project-dialog-title').textContent = project ? 'Настройки проекта' : 'Новый проект';
    get('project-name').value = project?.name ?? '';
    folders = [...(project?.folders ?? [])];
    renderFolders();
    const selectedSkill = project?.search_skill ?? '';
    renderSkills(selectedSkill);
    projectDialog.showModal();
    report();
    request('/skills').then(skills => {
      installedSkills = skills.map(skill => skill.name);
      renderSkills(skill.value);
    }).catch(error => report(error.message, true));
  }

  function renderFolders() {
    get('project-folder-empty').hidden = folders.length > 0;
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
    if (!available()) return;
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

  function archive(chat) {
    const project = selectedProject();
    const target = chat ?? project;
    const archived = !target.archived;
    actions.open = false;
    save('/projects/archive', {
      project_id: project.id,
      project_session_id: chat?.id,
      archived,
    }, async () => {
      if (!archived) archiveView = false;
      await refresh({ project: project.id, chat: archived ? undefined : chat?.id });
      report(chat ? archived ? 'Чат перемещён в архив.' : 'Чат восстановлен.'
        : archived ? 'Проект перемещён в архив.' : 'Проект восстановлен.');
    });
  }

  get('show-active-projects').onclick = () => switchView(false);
  get('show-archived-projects').onclick = () => switchView(true);
  get('new-project').onclick = () => openProjectForm();
  get('edit-project').onclick = () => {
    actions.open = false;
    openProjectForm(selectedProject());
  };
  get('archive-project').onclick = () => archive();
  get('archive-project-chat').onclick = () => archive(selectedChat());
  get('cancel-project').onclick = () => projectDialog.close();
  get('cancel-project-chat').onclick = () => chatDialog.close();
  projectDialog.oncancel = event => { if (picking || saving) event.preventDefault(); };
  chatDialog.oncancel = event => { if (saving) event.preventDefault(); };
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

  projectForm.onsubmit = event => {
    event.preventDefault();
    if (!folders.length) return;
    save('/projects', {
      id: editing,
      name: get('project-name').value,
      folders,
      search_skill: skill.value || null,
    }, async project => {
      projectDialog.close();
      archiveView = false;
      await refresh({ project: project.id, chat: chatId });
    });
  };

  let chatEditor;
  function openChatForm(chat) {
    const project = selectedProject();
    chatEditor = { project, chat, folders: new Set(chat ? chatFolders(project, chat) : project.folders) };
    const name = get('project-chat-name');
    name.value = chat?.name ?? '';
    name.readOnly = Boolean(chat);
    const folders = [...new Set([...(chat ? [chat.folder] : []), ...project.folders])];
    setOptions(chatFolderSelect, folders.map(path => [path, path]), chat?.folder);
    chatFolderSelect.disabled = Boolean(chat);
    get('project-chat-dialog-title').textContent = chat ? 'Папки чата' : 'Новый чат';
    get('save-project-chat').textContent = chat ? 'Сохранить' : 'Создать чат';
    renderChatFolders();
    actions.open = false;
    chatDialog.showModal();
    report();
  }

  function chatFolders(project, chat) {
    return chat.folders ?? [chat.folder, ...project.folders.filter(path => path !== chat.folder)];
  }

  function renderChatFolders() {
    const primary = chatFolderSelect.value;
    chatEditor.folders.add(primary);
    get('project-chat-folder-list').replaceChildren(...chatEditor.project.folders.map(path => {
      const label = element('label', '');
      label.className = 'pin';
      const checkbox = element('input', '');
      checkbox.type = 'checkbox';
      checkbox.checked = chatEditor.folders.has(path);
      checkbox.disabled = path === primary;
      checkbox.onchange = () => {
        if (checkbox.checked) chatEditor.folders.add(path);
        else chatEditor.folders.delete(path);
      };
      label.append(checkbox, element('span', path));
      return label;
    }));
  }

  get('new-project-chat').onclick = () => openChatForm();
  get('edit-project-chat-folders').onclick = () => openChatForm(selectedChat());
  chatFolderSelect.onchange = renderChatFolders;
  chatForm.onsubmit = event => {
    event.preventDefault();
    const { project, chat, folders } = chatEditor;
    const primary = chatFolderSelect.value;
    const selected = [primary, ...project.folders.filter(path => path !== primary && folders.has(path))];
    save(chat ? '/projects/sessions/folders' : '/projects/sessions', {
      project_id: project.id,
      ...(chat ? { project_session_id: chat.id } : { name: get('project-chat-name').value }),
      folders: selected,
    }, async chat => {
      chatDialog.close();
      await refresh({ project: project.id, chat: chat.id });
    });
  };

  modelSelect.onchange = () => {
    chatModels.set(chatId, modelSelect.value);
    updateControls();
  };
  for (const mode of ['chat', 'work']) {
    get(`project-mode-${mode}`).onclick = () => {
      chatModes.set(chatId, mode);
      updateControls();
    };
  }
  prompt.oninput = () => {
    drafts.set(chatId, prompt.value);
    updateControls();
  };
  stopButton.onclick = async () => {
    const delegation = chatRun(chatId);
    if (!delegation || stopButton.disabled) return;
    const [id, task] = delegation;
    stopRequests.add(id);
    updateControls();
    try {
      await request('/projects/run/stop', {
        delegation_id: Number(id),
        project_session_id: task.project_session_id,
      });
    } catch (error) {
      if (chatRun(chatId)?.[0] === id && error.status !== 404) report(error.message, true);
    } finally {
      stopRequests.delete(id);
      updateControls();
    }
  };

  composer.onsubmit = async event => {
    event.preventDefault();
    if (sendButton.disabled || !prompt.value.trim()) return;
    const project = selectedProject();
    const chat = selectedChat();
    const text = prompt.value;
    const model = refreshModels();
    const requestId = crypto.randomUUID();
    sending.set(chat.id, { role: 'user', origin: 'dashboard', text, request_id: requestId });
    drafts.delete(chat.id);
    prompt.value = '';
    report();
    renderHistory(true);
    updateControls();
    let restoreDraft = false;
    try {
      const reply = await request('/projects/run', {
        project_id: project.id,
        project_session_id: chat.id,
        model,
        mode: chatMode(),
        prompt: text,
        request_id: requestId,
      });
      restoreDraft = reply.structuredContent?.stop_reason === 'aborted';
      if (chatId === chat.id) {
        if (restoreDraft) report('Генерация остановлена.');
        else if (reply.structuredContent?.truncated) report('Ответ достиг лимита генерации.');
      }
    } catch (error) {
      restoreDraft = true;
      if (chatId === chat.id) report(error.message, true);
    } finally {
      const saved = chatId === chat.id ? await refreshHistory() : undefined;
      sending.delete(chat.id);
      if (restoreDraft && !drafts.has(chat.id) && !saved?.some(message => message.request_id === requestId)) {
        drafts.set(chat.id, text);
        if (chatId === chat.id) prompt.value = text;
      }
      if (chatId === chat.id) renderHistory();
      updateControls();
    }
  };

  return {
    refresh,
    update(connected, state, models) {
      online = connected;
      run = state;
      installedModels = models ?? [];
      updateControls();
    },
  };
}

function setOptions(select, pairs, selected) {
  select.replaceChildren(...pairs.map(([value, label]) => {
    const option = element('option', label);
    option.value = value;
    return option;
  }));
  if (pairs.some(([value]) => value === selected)) select.value = selected;
}

async function request(path, data) {
  const response = await fetch(path, data === undefined ? undefined : {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(data),
  });
  if (!response.ok) {
    const error = new Error(await response.text() || 'Запрос к Lyra завершился ошибкой.');
    error.status = response.status;
    throw error;
  }
  if (response.status !== 204) return response.json();
}

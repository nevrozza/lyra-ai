import { element, get } from './ui.js';
import { createThinkingControl } from './chat-thinking.js';

const taskStatuses = {
  queued: 'В очереди', running: 'В работе', completed: 'Готово',
  context_full: 'Ждёт восстановления контекста', failed: 'Ошибка', cancelled: 'Отменена', interrupted: 'Прервана', blocked: 'Заблокирована',
};

function taskStatus(job) {
  if (job.external && job.status === 'queued') return 'Внешний шаг';
  return taskStatuses[job.status] ?? '';
}

export function createProjects() {
  const tree = get('project-tree');
  const projectDialog = get('project-dialog');
  const chatDialog = get('project-chat-dialog');
  const projectForm = get('project-form');
  const chatForm = get('project-chat-form');
  const taskDialog = get('project-task-dialog');
  const taskForm = get('project-task-form');
  const taskModel = get('project-task-model');
  const taskParallel = get('project-task-parallel');
  const taskWritePaths = get('project-task-write-paths');
  function updateTaskWriteScope() {
    get('project-task-write-scope').hidden = !taskParallel.checked;
    taskWritePaths.required = taskParallel.checked;
  }
  taskParallel.onchange = updateTaskWriteScope;
  const folderList = get('project-folders');
  const pickFolder = get('pick-project-folder');
  const skill = get('project-skill');
  const prompt = get('project-prompt');
  const composer = get('project-prompt-form');
  const messages = get('project-messages');
  const liveThinking = get('project-live-thinking');
  const liveThinkingText = liveThinking.querySelector('p');
  const actions = get('project-actions');
  const sendButton = get('send-project-prompt');
  const stopButton = get('stop-project-prompt');
  const modelSelect = get('project-chat-model');
  const thinking = createThinkingControl();
  const chatFolderSelect = get('project-chat-folder');
  const drafts = new Map();
  const chatModels = new Map();
  const chatModes = new Map();
  const chatMode = () => chatModes.get(chatId) ?? (selectedChat()?.job ? 'work' : 'chat');
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
  let thinkingDelegation;

  const selectedProject = () => catalog?.projects.find(project => project.id === projectId);
  const selectedChat = () => selectedProject()?.sessions.find(chat => chat.id === chatId);
  const available = () => online && catalog != null && !saving && !picking;
  const agents = () => online ? Object.values(run?.delegations ?? {}) : [];
  const taskChat = chat => chat && (chat.job ? chat
    : selectedProject()?.sessions.find(owner => owner.job?.chat_session_id === chat.id));
  const chatRun = id => {
    if (!online || id == null) return undefined;
    const chat = selectedProject()?.sessions.find(session => session.id === id);
    const owner = taskChat(chat)?.id;
    return Object.entries(run?.delegations ?? {}).find(([, task]) =>
      task.project_session_id === id || (owner != null && task.project_session_id === owner));
  };
  const visibleChats = project => project.sessions.filter(chat => project.archived || chat.archived === archiveView);
  const pendingTask = chat => ['queued', 'running', 'context_full'].includes(chat?.job?.status);
  const chatLabel = chat => chat.job ? `#${chat.id} · ${chat.name}` : chat.name;

  function report(text = '', error = false) {
    const target = taskDialog.open ? 'project-task-form-message'
      : projectDialog.open ? 'project-form-message'
      : chatDialog.open ? 'project-chat-form-message' : 'project-message';
    get(target).dataset.error = String(error);
    get(target).textContent = text;
  }

  function updateControls() {
    const ready = available();
    const project = selectedProject();
    const chat = selectedChat();
    const delegation = chatRun(chat?.id);
    renderLiveThinking(delegation);
    const owner = taskChat(chat);
    const queued = owner?.job?.status === 'queued';
    const contextPaused = owner?.job?.status === 'context_full';
    const taskPending = pendingTask(owner);
    const active = chat && (sending.has(chat.id) || delegation || taskPending);
    const stopping = delegation && (delegation[1].stopping || stopRequests.has(delegation[0]));
    const contextNotice = delegation?.[1].context_notice ?? run?.chat_context?.[chat?.id] ?? run?.chat_context?.[owner?.id];
    const contextStatus = {
      near_limit: 'Контекст почти заполнен. Старые сообщения могут быть сжаты.',
      full: 'Контекст модели заполнен. Увеличь окно контекста, сократи запрос или начни новый чат.',
    }[contextNotice];
    const contextBanner = get('project-context-notice');
    const statusShowsOverflow = contextNotice === 'full' &&
      get('project-message').textContent.startsWith('Контекст модели заполнен.');
    contextBanner.hidden = !contextStatus || statusShowsOverflow;
    if (contextBanner.textContent !== (contextStatus ?? '')) contextBanner.textContent = contextStatus ?? '';
    const running = agents();
    const activeChats = new Set(sending.keys());
    for (const entry of running) {
      activeChats.add(entry.project_session_id);
      const child = project?.sessions.find(session => session.id === entry.project_session_id)?.job?.chat_session_id;
      if (child != null) activeChats.add(child);
    }
    for (const spinner of tree.querySelectorAll('[data-chat-progress]')) {
      spinner.hidden = !activeChats.has(Number(spinner.dataset.chatProgress));
    }
    get('project-progress').hidden = !active;
    const progressText = stopping ? 'Останавливаем…' : contextNotice === 'compacting'
      ? 'Сжимаем историю…' : delegation ? 'Модель отвечает…' : contextPaused ? 'Задача ждёт восстановления контекста…' : queued ? (owner.job.external ? 'Ждём результат от оркестратора…' : 'Задача в очереди…') : 'Отправляем…';
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
    get('new-project-task').hidden = !project || project.archived || archiveView;
    get('new-project-task').disabled = !ready || !(installedModels.length || run?.model);
    get('edit-project').hidden = !project || project.archived;
    get('edit-project').disabled = !ready;
    get('edit-project-chat-folders').hidden = !chat || chat.archived || project.archived;
    get('edit-project-chat-folders').disabled = !ready || taskPending;
    const archiveProject = get('archive-project');
    archiveProject.textContent = project?.archived ? 'Восстановить проект' : 'Архивировать проект';
    archiveProject.disabled = !ready || Boolean(project?.sessions.some(pendingTask));
    const archiveChat = get('archive-project-chat');
    archiveChat.hidden = !chat || project.archived;
    archiveChat.textContent = chat?.archived ? 'Восстановить чат' : 'Архивировать чат';
    archiveChat.disabled = !ready || taskPending;
    pickFolder.disabled = !ready;
    pickFolder.textContent = picking ? 'Выбор папки…' : 'Выбрать папку';
    get('cancel-project').disabled = saving || picking;
    get('cancel-project-chat').disabled = saving;
    get('cancel-project-task-form').disabled = saving;
    taskForm.querySelector('[type="submit"]').disabled = !ready;
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
    thinking.update(online && run && !composer.hidden ? model : undefined, run);
    sendButton.disabled = !online || !model || !prompt.value.trim() || composer.hidden || Boolean(active);
    sendButton.hidden = Boolean(active);
    stopButton.disabled = (!delegation && !queued && !contextPaused) || Boolean(stopping) || !online || saving;
    stopButton.textContent = stopping ? 'Останавливаем…' : (queued || contextPaused) && !delegation ? 'Отменить задачу' : 'Остановить';
  }

  function refreshModels() {
    const dashboardModel = run?.model;
    const defaultModel = installedModels.length && !installedModels.includes(dashboardModel)
      ? installedModels[0] : dashboardModel;
    const models = [...new Set([defaultModel, ...installedModels].filter(Boolean))];
    const preferred = chatModels.get(chatId) ?? taskChat(selectedChat())?.job?.model;
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
      const ordinary = chats.filter(chat => !chat.job);
      const tasks = chats.filter(chat => chat.job);
      for (const group of [ordinary, tasks]) {
        if (!group.length) continue;
        if (group === tasks) {
          const label = element('p', 'Задачи');
          label.className = 'secondary project-task-group';
          item.append(label);
        }
        const list = element('ul', '');
        list.append(...group.map(chat => {
          const entry = element('li', '');
          entry.append(treeButton(chatLabel(chat), project.id, chat.id, chat.job));
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

  function treeButton(label, id, session, job) {
    const button = element('button', '');
    button.type = 'button';
    button.title = label;
    button.dataset.projectEntry = `${id}/${session ?? ''}`;
    button.setAttribute('aria-current', String(id === projectId && session === chatId));
    button.append(element('span', label));
    if (job) {
      const status = element('span', taskStatus(job));
      status.className = 'project-task-state';
      button.append(status);
    } else if (session != null) {
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
    get('current-project-title').textContent = chat ? chatLabel(chat) : project?.name ?? '';
    const jobStatus = get('project-job-status');
    jobStatus.hidden = !chat?.job;
    const sourceTask = chat?.job?.workflow?.root_id;
    const chain = sourceTask && sourceTask !== chat.id ? ` · Цепочка #${sourceTask}` : '';
    const dependencies = chat?.job?.after?.map(id => `#${id}`).join(', ');
    jobStatus.textContent = chat?.job
      ? [taskStatus(chat.job), chat.job.model, dependencies && `После ${dependencies}`].filter(Boolean).join(' · ') + chain : '';
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
        : { orchestrator: 'Оркестратор', lyra: 'Lyra · Следующий шаг' }[message.origin] ?? '';
      if (label) item.append(element('strong', label));
      if (message.thinking) {
        const details = element('details', '');
        details.className = 'project-thinking';
        details.append(element('summary', 'Размышления модели'), element('p', message.thinking));
        item.append(details);
      }
      if (message.text) item.append(element('p', message.text));
      return item;
    }));
    if (atEnd || scrollToEnd) reading.scrollTop = reading.scrollHeight;
  }

  function renderLiveThinking(delegation) {
    const [id, task] = delegation ?? [];
    const text = task?.thinking ?? '';
    if (thinkingDelegation !== id) {
      thinkingDelegation = id;
      liveThinking.open = true;
    }
    if (liveThinkingText.textContent === text) return;
    const reading = messages.parentElement;
    const atEnd = reading.scrollHeight - reading.scrollTop - reading.clientHeight < 24;
    liveThinkingText.textContent = text;
    liveThinking.hidden = !text.trim();
    if (atEnd) reading.scrollTop = reading.scrollHeight;
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
    setOptions(skill, [['', 'По умолчанию'], ...names.map(name => [name, name])], selected);
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
  let taskProject;
  let taskBatch;
  get('new-project-task').onclick = () => {
    taskProject = selectedProject();
    taskBatch = crypto.randomUUID();
    taskForm.reset();
    updateTaskWriteScope();
    const models = [...new Set([run?.model, ...installedModels].filter(Boolean))];
    setOptions(taskModel, models.map(model => [model, model]), refreshModels());
    taskDialog.showModal();
    report();
  };
  get('cancel-project-task-form').onclick = () => taskDialog.close();
  taskDialog.oncancel = event => {
    if (saving) event.preventDefault();
  };
  taskForm.onsubmit = event => {
    event.preventDefault();
    const project = taskProject;
    save('/projects/tasks', {
      project_id: project.id,
      batch_id: taskBatch,
      tasks: [{
        name: get('project-task-name').value,
        prompt: get('project-task-prompt').value,
        model: taskModel.value,
        parallel: taskParallel.checked,
        ...(taskParallel.checked ? { write_paths: taskWritePaths.value.split(/\r?\n/).map(path => path.trim()).filter(Boolean) } : {}),
      }],
    }, async ([task]) => {
      taskDialog.close();
      await refresh({ project: project.id, chat: task.project_session_id });
    });
  };
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
    if (stopButton.disabled) return;
    const chat = taskChat(selectedChat());
    const delegation = chatRun(chatId);
    if (!delegation && ['queued', 'context_full'].includes(chat?.job?.status)) {
      await save('/projects/tasks/cancel', {
        project_id: selectedProject().id,
        project_session_id: chat.id,
      }, () => refresh());
      return;
    }
    if (!delegation) return;
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

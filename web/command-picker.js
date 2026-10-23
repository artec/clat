let commandCatalogRequest = 0;
let commandCatalog = null;
let commandCandidates = [];
let commandIndex = 0;
let commandPanel = null;
let commandComposing = false;

function commandQuery() {
  return /^\/[\p{L}\p{N}_-]*$/u.test(dom.prompt.value) ? dom.prompt.value.slice(1).toLocaleLowerCase() : null;
}

function closeCommandPanel() {
  if (commandPanel) hide(commandPanel);
  dom.prompt.removeAttribute('aria-activedescendant');
  dom.prompt.setAttribute('aria-expanded', 'false');
}

/// HIST-PWA-1/W3：命令/skills picker 打开时由 picker 独占方向键——composer
/// 的历史召回必须显式让位，不依赖监听注册顺序。
function commandPanelOpen() {
  return Boolean(commandPanel) && !commandPanel.classList.contains('hidden');
}

function catalogCandidates(catalog, query) {
  const matches = (values) => values.some((text) => String(text || '').toLocaleLowerCase().includes(query));
  const commands = (catalog.commands || []).filter((entry) => matches(
    [entry.name, entry.description, ...(entry.aliases || [])]
  )).map((entry) => ({ ...entry, value: '/' + entry.name + ' ', section: 'commands' }));
  const skills = (catalog.skills?.entries || []).filter((entry) => matches(
    [entry.name, entry.description]
  )).map((entry) => ({ ...entry, value: '/skill ' + entry.name + ' ', section: 'skills',
    usage: entry.source + (entry.requires_execution ? ' · requires-execution' : '') }));
  const rank = (entry) => entry.name.toLocaleLowerCase() === query ? 0
    : entry.name.toLocaleLowerCase().startsWith(query) ? 1 : 2;
  return [...commands.sort((a, b) => rank(a) - rank(b)), ...skills.sort((a, b) => rank(a) - rank(b))];
}

function renderCommandCandidates() {
  const query = commandQuery();
  if (query === null || commandComposing || state.switching || state.compactionActive) {
    closeCommandPanel(); return;
  }
  commandPanel.replaceChildren();
  show(commandPanel);
  dom.prompt.setAttribute('aria-expanded', 'true');
  if (!commandCatalog) {
    commandPanel.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.loading_commands_and_skills", {}, "Loading commands and skills…")))); return;
  }
  commandCandidates = catalogCandidates(commandCatalog, query).slice(0, 100);
  commandIndex = Math.max(0, Math.min(commandIndex, commandCandidates.length - 1));
  commandPanel.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.command_hint",{},"↑↓ browse · Tab/Enter select · Esc close · selection never runs a command"))));
  let section = '';
  commandCandidates.slice(0, 100).forEach((entry, index) => {
    if (section !== entry.section) {
      section = entry.section; const heading=section; commandPanel.append(el('p', 'section-kicker', () => CLATI18n.t(heading==='commands'?'tui.commands':'tui.skills')));
    }
    const option = el('button', 'command-option');
    option.type = 'button'; option.id = 'command-option-' + index;
    option.setAttribute('role', 'option');
    option.setAttribute('aria-selected', String(index === commandIndex));
    const unavailable = entry.unavailable_reason || (['quit', 'exit'].includes(entry.name) && entry.section === 'commands');
    option.setAttribute('aria-disabled', String(Boolean(unavailable)));
    option.append(el('strong', '', () => (entry.section === 'skills' ? entry.name : '/' + entry.name)),
      el('small', '', () => entry.description_presentation ? CLATI18n.t(entry.description_presentation.key,entry.description_presentation.args,entry.description) : entry.description), el('small', 'picker-hint', () => (unavailable
        ? entry.unavailable_reason || CLATI18n.msg("web.close_this_browser_tab_to_detach", {}, "Close this browser tab to detach") : entry.usage || CLATI18n.msg("web.edit_arguments_before_sending", {}, "Edit arguments before sending"))));
    option.addEventListener('pointerdown', (event) => event.preventDefault());
    option.addEventListener('click', () => selectCommandCandidate(index));
    commandPanel.appendChild(option);
  });
  if (!commandCandidates.length) commandPanel.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.no_matching_commands_or_skills", {}, "No matching commands or skills"))));
  dom.prompt.setAttribute('aria-activedescendant', 'command-option-' + commandIndex);
  commandPanel.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
}

async function refreshCommandCatalog() {
  const request = ++commandCatalogRequest;
  const owner = composerScope;
  commandCatalog = null;
  try {
    const catalog = await rpc('interaction.catalog', {});
    if (request !== commandCatalogRequest || owner !== composerScope) return;
    commandCatalog = catalog;
    if (commandQuery() !== null && !commandPanel.classList.contains('hidden')) renderCommandCandidates();
  } catch (error) {
    if (request === commandCatalogRequest && owner === composerScope && commandQuery() !== null) {
      commandPanel.replaceChildren(el('p', 'error', () => (CLATI18n.msg("web.line_command_discovery_unavailable_value",{value1:(error.message)},"Command discovery unavailable: {value1}"))));
    }
  }
}

function selectCommandCandidate(index) {
  const entry = commandCandidates[index];
  if (!entry || entry.unavailable_reason || (entry.section === 'commands' && ['quit', 'exit'].includes(entry.name))) return;
  closeCommandPanel();
  if (entry.section === 'commands') {
    const dedicated = {
      model: () => { void openModelPicker(); },
      perm: () => openSettings('permissions'),
      permission: () => openSettings('permissions'),
      resume: () => dom['sessions-open'].click(),
      rename: () => dom['session-title'].click(),
      compact: () => { setInspector('open'); dom['compact-session'].focus(); },
    }[entry.name];
    if (dedicated) {
      dom.prompt.value = ''; state.composerGeneration += 1;
      saveComposerText(); invalidateSuggestion(); resizePrompt(); dedicated(); return;
    }
  }
  dom.prompt.value = entry.value;
  state.composerGeneration += 1;
  saveComposerText(); invalidateSuggestion(); resizePrompt(); dom.prompt.focus();
}

function installCommandPicker() {
  commandPanel = el('div', 'command-panel hidden');
  commandPanel.id = 'command-panel'; commandPanel.setAttribute('role', 'listbox');
  CLATI18n.attribute(commandPanel,'aria-label',()=>CLATI18n.msg("web.commands_and_skills", {}, "Commands and skills"));
  dom['composer-shell'].appendChild(commandPanel);
  dom.prompt.setAttribute('aria-controls', 'command-panel');
  dom.prompt.setAttribute('aria-autocomplete', 'list');
  dom.prompt.addEventListener('compositionstart', () => { commandComposing = true; closeCommandPanel(); });
  dom.prompt.addEventListener('compositionend', () => { commandComposing = false; renderCommandCandidates(); });
  dom.prompt.addEventListener('paste', closeCommandPanel);
  dom.prompt.addEventListener('input', (event) => {
    if (event.inputType === 'insertFromPaste' || event.isComposing) { closeCommandPanel(); return; }
    commandIndex = 0; renderCommandCandidates();
    if (commandQuery() !== null && !commandCatalog) { void refreshCommandCatalog(); }
  });
  dom.prompt.addEventListener('keydown', (event) => {
    if (commandComposing || event.isComposing || event.keyCode === 229 || event.shiftKey || event.ctrlKey || event.altKey || commandPanel.classList.contains('hidden')) return;
    if (['ArrowUp', 'ArrowDown', 'Tab', 'Enter', 'Escape'].includes(event.key)) {
      event.preventDefault(); event.stopImmediatePropagation();
      if (event.key === 'Escape') closeCommandPanel();
      else if (event.key === 'Tab' || event.key === 'Enter') selectCommandCandidate(commandIndex);
      else {
        commandIndex = (commandIndex + (event.key === 'ArrowDown' ? 1 : -1) + commandCandidates.length)
          % Math.max(1, commandCandidates.length);
        renderCommandCandidates();
      }
    }
  }, true);
}

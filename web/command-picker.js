let commandCatalogRequest = 0;
let commandCatalog = null;
let commandCandidates = [];
let commandIndex = 0;
let commandPanel = null;
let commandComposing = false;

function commandQuery() {
  const input = dom.prompt.value;
  return (/^\/[\p{L}\p{N}_-]*$/u.test(input) || /^\/skills?[ \t]+[\p{L}\p{N}_-]*$/iu.test(input))
    ? input.slice(1).toLocaleLowerCase() : null;
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
  const skillCommand = /^skills?$/u.test(query);
  const skillArgument = /^skills?[ \t]+([\p{L}\p{N}_-]*)$/u.exec(query);
  const skillQuery = skillCommand ? '' : skillArgument?.[1] ?? query;
  const matches = (values, needle) => values.some((text) => String(text || '').toLocaleLowerCase().includes(needle));
  const commands = (catalog.commands || []).filter((entry) => !skillArgument && (skillCommand
    ? [entry.name, ...(entry.aliases || [])].includes(query)
    : matches([entry.name, entry.description, ...(entry.aliases || [])], query)
  )).map((entry) => ({ ...entry, value: '/' + entry.name + ' ', section: 'commands' }));
  const skills = (catalog.skills?.entries || []).filter((entry) => matches(
    [entry.name, entry.description], skillQuery
  )).map((entry) => ({ ...entry, value: '/skill ' + entry.name + ' ', section: 'skills' }));
  const rank = (entry, needle) => entry.name.toLocaleLowerCase() === needle ? 0
    : entry.name.toLocaleLowerCase().startsWith(needle) ? 1 : 2;
  return [...commands.sort((a, b) => rank(a, query) - rank(b, query)),
    ...skills.sort((a, b) => rank(a, skillQuery) - rank(b, skillQuery))];
}

function commandPickerOption(entry, index) {
  const option = el('button', 'command-option');
  const isSkill = entry.section === 'skills';
  const unavailable = entry.unavailable_reason || (!isSkill && ['quit', 'exit'].includes(entry.name));
  const title = isSkill ? entry.name : '/' + entry.name;
  option.append(el('strong', 'command-option-title', title));
  if (entry.description || entry.description_presentation) {
    option.append(el('small', 'command-option-description', () => CLATI18n.present(entry.description_presentation, entry.description)));
  }
  if (unavailable) {
    option.append(el('small', 'command-option-description', () => entry.unavailable_reason
      || CLATI18n.msg('web.close_this_browser_tab_to_detach')));
  }
  if (isSkill) {
    const source = { bundled: 'extension.bundled', user: 'skill_settings.user', project: 'skill_settings.project' }[entry.source];
    const meta = el('span', 'command-option-meta');
    if (source) meta.append(el('span', '', () => CLATI18n.msg(source)));
    if (entry.requires_execution) meta.append(el('span', '', () => CLATI18n.msg('skill_settings.requires_execution')));
    if (meta.childElementCount) option.append(meta);
  } else if (!unavailable && entry.usage && entry.usage !== title && entry.name !== 'skill') {
    option.append(el('code', 'command-option-usage', entry.usage.startsWith(title + ' ')
      ? entry.usage.slice(title.length + 1).trim() : entry.usage));
  }
  option.type = 'button'; option.tabIndex = -1; option.id = 'command-option-' + index;
  option.setAttribute('role', 'option');
  option.setAttribute('aria-selected', String(index === commandIndex));
  option.setAttribute('aria-disabled', String(Boolean(unavailable)));
  option.addEventListener('pointerdown', (event) => event.preventDefault());
  option.addEventListener('click', () => selectCommandCandidate(index));
  return option;
}

function commandPickerFooter() {
  const footer = el('div', 'command-picker-footer');
  footer.setAttribute('role', 'group');
  CLATI18n.attribute(footer, 'aria-label', () => CLATI18n.msg('web.command_hint'));
  for (const [keys, label] of [[['↑', '↓'], 'web.command_browse'], [['Tab', 'Enter'], 'web.command_fill'], [['Esc'], 'common.close']]) {
    const shortcut = el('span', 'command-picker-shortcut');
    shortcut.append(...keys.map(key => el('kbd', '', key)), el('span', '', () => CLATI18n.msg(label)));
    footer.append(shortcut);
  }
  return footer;
}

function renderCommandCandidates() {
  const query = commandQuery();
  if (query === null || commandComposing || state.switching || state.compactionActive) {
    closeCommandPanel(); return;
  }
  const scroll = commandPanel.querySelector('.command-picker-list')?.scrollTop || 0;
  commandPanel.replaceChildren();
  show(commandPanel);
  dom.prompt.setAttribute('aria-expanded', 'true');
  dom.prompt.removeAttribute('aria-activedescendant');
  dom.prompt.removeAttribute('aria-controls');
  commandCandidates = commandCatalog ? catalogCandidates(commandCatalog, query).slice(0, 100) : [];
  if (!commandCandidates.length) {
    const key = commandCatalog ? 'web.no_matching_commands_or_skills' : 'web.loading_commands_and_skills';
    commandPanel.append(el('p', 'command-picker-status', () => CLATI18n.msg(key)));
    return;
  }
  commandIndex = Math.max(0, Math.min(commandIndex, commandCandidates.length - 1));
  const list = el('div', 'command-picker-list');
  list.id = 'command-picker-list';
  list.setAttribute('role', 'listbox');
  CLATI18n.attribute(list, 'aria-label', () => CLATI18n.msg('web.commands_and_skills'));
  const showHeadings = commandCandidates.length > 1;
  let section = null, group = null;
  commandCandidates.forEach((entry, index) => {
    if (section !== entry.section) {
      section = entry.section;
      const key = section === 'commands' ? 'tui.commands' : 'tui.skills';
      group = el('div', 'command-picker-group');
      group.setAttribute('role', 'group');
      CLATI18n.attribute(group, 'aria-label', () => CLATI18n.msg(key));
      if (showHeadings) {
        const heading = el('p', 'command-picker-heading', () => CLATI18n.msg(key));
        heading.setAttribute('aria-hidden', 'true');
        group.append(heading);
      }
      list.append(group);
    }
    group.append(commandPickerOption(entry, index));
  });
  commandPanel.append(list, commandPickerFooter());
  if (commandIndex) list.scrollTop = scroll;
  dom.prompt.setAttribute('aria-controls', list.id);
  dom.prompt.setAttribute('aria-activedescendant', 'command-option-' + commandIndex);
  list.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
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
      commandPanel.replaceChildren(el('p', 'command-picker-status error', () => (CLATI18n.msg("web.line_command_discovery_unavailable_value",{value1:(error.message)},"Command discovery unavailable: {value1}"))));
    }
  }
}

function selectCommandCandidate(index) {
  const entry = commandCandidates[index];
  if (!entry || entry.unavailable_reason || (entry.section === 'commands' && ['quit', 'exit'].includes(entry.name))) return;
  closeCommandPanel();
  if (entry.section === 'commands') {
    const dedicated = {
      model: () => { void openModelPicker({ keyboard: true }); },
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

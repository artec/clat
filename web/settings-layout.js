/* Settings presentation only. Existing controls retain their owners and save paths. */
const settingsUi = key => CLATI18n.msg('settings_ui.' + key);
let settingsLayout = null;

function settingsControlRow(title, control, description) {
  const row = el('label', 'settings-row'), copy = el('span', 'settings-row-copy');
  copy.append(el('strong', '', title));
  if (description) copy.append(el('small', '', description));
  row.append(copy, control);
  return row;
}

function settingsGroup(host, title) {
  const section = el('section', 'settings-group-block'), card = el('div', 'settings-card');
  section.append(el('h4', '', title), card); host.append(section);
  return card;
}

function decorateSettingsPanel(section) {
  let header = section.querySelector(':scope > .settings-section-header');
  if (!header) {
    header = section.querySelector(':scope > div.settings-copy') || el('header', 'settings-copy');
    header.classList.add('settings-section-header');
    if (!header.querySelector('h2,h3')) {
      const title = section.querySelector(':scope > h2,:scope > h3');
      header.prepend(title || el('h2', '', () => settingsCategoryLabel(section.dataset.settingsPanel)));
    }
    section.prepend(header);
  }
  const scope = section.querySelector(':scope > .page-scope');
  if (scope) header.append(scope);
  else if (!header.querySelector('.page-scope')) header.append(el('p','page-scope',()=>settingsScope(section.dataset.settingsPanel)));
}

function decorateDesktopSettings(section, panel) {
  decorateSettingsPanel(section);
  if (panel.querySelector(':scope > .settings-card')) return;
  const card = el('div', 'settings-card settings-form');
  for (const child of [...panel.children]) {
    if (child.matches('h2,.desktop-language,[data-action=close]')) continue;
    card.append(child);
  }
  panel.append(card);
  panel.querySelector('input[type=checkbox]')?.classList.add('settings-switch');
}

function installSettingsLayout() {
  const page = dom['settings-dialog'], nav = page.querySelector('.settings-nav');
  const content = page.querySelector('.settings-content'), header = page.querySelector('.settings-panel > header');
  const search = navigationSearch(null, () => settingsUi('search_categories'));
  header.append(search.element);
  const empty = el('p', 'settings-search-empty', () => settingsUi('no_categories')); empty.hidden = true; nav.append(empty);
  const groups = [
    ['workbench', [['appearance','settings'],['terminal','terminal'],['projects','folder']]],
    ['capabilities', [['models','wasm'],['utility','sparkles'],['permissions','permission-write']]],
    ['integrations', [['web','search'],['wechat','chat'],['git','branch']]],
    ['application', [['desktop','info']]],
  ];
  function filter() {
    const query = search.input.value.trim().toLocaleLowerCase(); let count = 0;
    for (const group of nav.querySelectorAll('.settings-nav-group')) {
      for (const row of group.querySelectorAll('.settings-nav-item')) {
        const button=row.querySelector('button');
        row.hidden = !button || !button.textContent.toLocaleLowerCase().includes(query);
        if (!row.hidden) count++;
      }
      group.hidden = !group.querySelector('.settings-nav-item:not([hidden])');
    }
    empty.hidden = count > 0;
  }
  function arrange() {
    nav.querySelectorAll(':scope > .settings-group').forEach(node => node.remove());
    for (const [key, items] of groups) {
      let group = nav.querySelector(`[data-settings-group="${key}"]`);
      if (!group) { group = el('div','settings-nav-group'); group.dataset.settingsGroup=key; group.append(el('small','settings-group',()=>pageText(key))); nav.insertBefore(group,empty); }
      for (const [category, icon] of items) {
        const button = nav.querySelector(`[data-settings-category="${category}"]`);
        if (!button || button.parentElement.classList.contains('settings-nav-item')) continue;
        const row = el('div', 'settings-nav-item'); row.append(svgIcon(icon), button); group.append(row);
      }
    }
    filter();
  }
  arrange();
  new MutationObserver(() => {
    if (nav.querySelector(':scope > [data-settings-category]')) arrange();
  }).observe(nav, {childList:true});
  search.input.oninput = filter;
  search.input.onkeydown = event => {
    if (event.key === 'Enter' && !event.isComposing) { event.preventDefault(); nav.querySelector('.settings-nav-item:not([hidden]) button')?.click(); }
  };
  CLATI18n.onChange(() => requestAnimationFrame(filter));
  for (const section of content.querySelectorAll('[data-settings-panel]')) decorateSettingsPanel(section);
  const back = navigationIcon(() => pageText('back_list'),'previous',()=>{ delete page.dataset.detail; refreshViewContext(); });
  back.classList.add('settings-mobile-back'); content.prepend(back);
  const positions = new Map(); let epoch = 0;
  settingsLayout = {
    capture(category) { positions.set(category, content.scrollTop); },
    select(category) { const request=++epoch; page.dataset.detail='true'; requestAnimationFrame(()=>{if(request===epoch)content.scrollTop=positions.get(category)||0;refreshViewContext();}); },
  };
}

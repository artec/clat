function installFileBrowser() {
  const trigger = el('button', 'ghost', () => (CLATI18n.msg("web.files", {}, "Files"))); trigger.type = 'button'; trigger.id = 'files-open';
  document.getElementById('review-open').after(trigger);
  const browser = createDevelopmentEditor(); developmentEditor = browser.controller;
  document.body.append(browser.element);
  trigger.addEventListener('click', browser.open);
}

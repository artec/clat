'use strict';
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const status = document.getElementById('status');
CLATI18n.bind(status,'textContent',()=>CLATI18n.t('desktop.checking_host'));
let opening = false, refreshing = false, actionError = '';
async function refresh() {
  if (opening || refreshing) return;
  refreshing = true;
  try {
    const info = await invoke('desktop_info');
    document.getElementById('version').textContent = `CLAT ${info.desktopVersion}`;
    CLATI18n.bind(status,'textContent',()=>CLATI18n.present(actionError) || info.backendError || CLATI18n.present(info.connectionPresentation,info.connectionError) || CLATI18n.t('desktop.welcome'));
  } catch (error) { CLATI18n.bind(status,'textContent',()=>CLATI18n.present(actionError) || CLATI18n.present(error,String(error))); }
  finally { refreshing = false; }
}
document.getElementById('open').onclick = async event => {
  opening = true; actionError = '';
  event.currentTarget.disabled = true;
  CLATI18n.bind(status,'textContent',()=>CLATI18n.t('desktop.choose_folder'));
  try { await invoke('desktop_open_project'); }
  catch (error) { actionError = error?.key ? error : String(error); }
  finally {
    opening = false; document.getElementById('open').disabled = false;
    await refresh();
  }
};
document.getElementById('settings').onclick = () => window.CLATDesktop?.showSettings();
refresh();
setInterval(refresh, 2000);

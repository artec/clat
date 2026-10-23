'use strict';
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const status = document.getElementById('status');
let opening = false, refreshing = false, actionError = '';
async function refresh() {
  if (opening || refreshing) return;
  refreshing = true;
  try {
    const info = await invoke('desktop_info');
    document.getElementById('version').textContent = `CLAT ${info.desktopVersion}`;
    status.textContent = actionError || info.backendError || info.connectionError || '选择项目后，可在授权范围内使用本地工具。';
  } catch (error) { status.textContent = actionError || String(error); }
  finally { refreshing = false; }
}
document.getElementById('open').onclick = async event => {
  opening = true; actionError = '';
  event.currentTarget.disabled = true;
  status.textContent = '请选择项目目录…';
  try { await invoke('desktop_open_project'); }
  catch (error) { actionError = String(error); }
  finally {
    opening = false; document.getElementById('open').disabled = false;
    await refresh();
  }
};
document.getElementById('settings').onclick = () => window.CLATDesktop?.showSettings();
refresh();
setInterval(refresh, 2000);

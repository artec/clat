'use strict';
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const status = document.getElementById('status');
document.getElementById('open').onclick = async event => {
  event.currentTarget.disabled = true;
  status.textContent = '请选择项目目录…';
  try { await invoke('desktop_open_project'); status.textContent = ''; }
  catch (error) { status.textContent = String(error); }
  finally { document.getElementById('open').disabled = false; }
};
document.getElementById('settings').onclick = () => window.CLATDesktop?.showSettings();
invoke('desktop_info').then(info => {
  document.getElementById('version').textContent = `CLAT Desktop ${info.desktopVersion}`;
  status.textContent = info.backendError || info.connectionError || '选择项目后，可在授权范围内使用本地工具。';
}).catch(error => { status.textContent = String(error); });

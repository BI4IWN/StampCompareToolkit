// 预加载桥：向渲染进程暴露受控的桌面能力（contextIsolation 安全模型）
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('stampDesktop', {
  available: true,
  platform: process.platform,

  // 截图
  onCaptureDone: (cb) => ipcRenderer.on('capture-done', (_e, data) => cb(data)),
  onShortcutError: (cb) => ipcRenderer.on('shortcut-error', (accel) => cb(accel)),
  captureCancel: () => ipcRenderer.send('capture-cancel'),
  captureStart: () => ipcRenderer.send('capture-start'),
  captureDone: (payload) => ipcRenderer.send('capture-done', payload),

  // 印章库
  libSave: (payload) => ipcRenderer.invoke('lib:save', payload),
  libList: () => ipcRenderer.invoke('lib:list'),
  libSearch: (q) => ipcRenderer.invoke('lib:search', q),
  libLoad: (id) => ipcRenderer.invoke('lib:load', id),
  libDelete: (id) => ipcRenderer.invoke('lib:delete', id),
  libChoosePath: () => ipcRenderer.invoke('lib:choosePath'),
  libReveal: () => ipcRenderer.invoke('lib:reveal'),

  // 设置
  settingsGet: () => ipcRenderer.invoke('settings:get'),
  settingsSet: (patch) => ipcRenderer.invoke('settings:set', patch),
  settingsReset: () => ipcRenderer.invoke('settings:reset'),
});

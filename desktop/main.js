// 印章比对工具 · Electron 主进程
// 职责：主窗口 + 本地静态服务（同源加载 stamp-compare.html）+ Python 后端子进程
//       （可选，OCR/AI）+ 全局截图快捷键 + 选区截图 + 印章库文件管理 + 设置
const { app, BrowserWindow, globalShortcut, clipboard, desktopCapturer,
        ipcMain, dialog, nativeImage, screen, shell, Menu } = require('electron');

// macOS 菜单栏/Dock 显示应用名（开发模式下默认为 Electron，打包版兜底）
if (process.platform === 'darwin') app.setName('印章比对工具');
const path = require('path');
const fs = require('fs');
const http = require('http');
const { spawn } = require('child_process');

const isDev = process.argv.includes('--dev');
// 资源目录：打包版在 resources/stamp-toolkit，开发版在上层目录 —— 自动探测
const _pkgRes = process.resourcesPath && path.join(process.resourcesPath, 'stamp-toolkit');
const RES = (!isDev && _pkgRes && fs.existsSync(_pkgRes)) ? _pkgRes : path.join(__dirname, '..', 'stamp-toolkit');
const SRC_DIR = path.join(RES, 'src');

// ---------- 设置 ----------
const userDataDir = () => {
  const dir = app.getPath('userData');
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
  return dir;
};
const settingsFile = () => path.join(userDataDir(), 'settings.json');
const DEFAULTS = {
  libPath: path.join(app.getPath('documents'), '印章库'),
  captureShortcut: 'CommandOrControl+Shift+S',
  pasteTargetAfterCapture: 'ask',   // ask | ref | test | clipboard-only
  autoStartBackend: true,
};
let settings = { ...DEFAULTS };
const loadSettings = () => {
  try {
    if (fs.existsSync(settingsFile())) {
      settings = { ...DEFAULTS, ...JSON.parse(fs.readFileSync(settingsFile(), 'utf8')) };
    }
  } catch (e) { /* 损坏则用默认 */ }
};
const saveSettings = () => fs.writeFileSync(settingsFile(), JSON.stringify(settings, null, 2));

// ---------- 本地静态服务（同源，避免 file:// 的跨源限制）+ API 反代 ----------
const HTTP_PORT = 18799;   // 前端页面
const PY_PORT = 8799;      // stamp_app.py（若可用）
let server = null;

function startStaticServer() {
  const mime = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css',
                 '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.svg': 'image/svg+xml' };
  server = http.createServer((req, res) => {
    const url = new URL(req.url, `http://127.0.0.1:${HTTP_PORT}`);
    // API 反代到 Python 后端（/ocr /ai-ocr /ai/ /health）
    if (url.pathname === '/health' || url.pathname === '/ocr' || url.pathname === '/ai-ocr' || url.pathname.startsWith('/ai/')) {
      const proxy = http.request({ host: '127.0.0.1', port: PY_PORT, method: req.method,
        path: req.url, headers: { ...req.headers, host: `127.0.0.1:${PY_PORT}` } }, pr => {
        res.writeHead(pr.statusCode, pr.headers);
        pr.pipe(res);
      });
      proxy.on('error', () => { res.writeHead(502, { 'Content-Type': 'application/json' }); res.end('{"ok":false,"error":"backend unavailable"}'); });
      req.pipe(proxy);
      return;
    }
    // 静态文件
    let p = decodeURIComponent(url.pathname);
    if (p === '/') p = '/stamp-compare.html';
    const file = path.normalize(path.join(SRC_DIR, p));
    if (!file.startsWith(SRC_DIR)) { res.writeHead(403); res.end(); return; }
    fs.readFile(file, (err, data) => {
      if (err) { res.writeHead(404); res.end('not found'); return; }
      res.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-cache' });
      res.end(data);
    });
  });
  server.on('error', (e) => {
    if (e.code === 'EADDRINUSE') {
      // 上个实例的静态服务仍在（残留）：直接复用，不再自建
      console.log('[static] port in use, reusing existing service');
      server = null;
    } else {
      console.error('[static] server error:', e);
    }
  });
  server.listen(HTTP_PORT, '127.0.0.1');
}

// ---------- Python 后端子进程（OCR / AI 代理）----------
// 优先级：安装包内置便携运行时（随包分发 RapidOCR + onnxruntime，开箱即用）
//       → 系统安装的 python3 → 放弃（前端显示"需启动服务"）
let pyProc = null;
function bundledPython() {
  const cands = process.platform === 'win32'
    ? [path.join(__dirname, 'runtime', 'python-win-x64', 'python.exe'),
       process.resourcesPath && path.join(process.resourcesPath, 'runtime', 'python-win-x64', 'python.exe')]
    : [path.join(__dirname, 'runtime', `python-mac-${process.arch}`, 'bin', 'python3'),
       process.resourcesPath && path.join(process.resourcesPath, 'runtime', `python-mac-${process.arch}`, 'bin', 'python3')];
  for (const p of cands) if (p && fs.existsSync(p)) return p;
  return null;
}
function startBackend() {
  if (!settings.autoStartBackend) return;
  const entry = path.join(RES, 'src', 'stamp_app.py');
  if (!fs.existsSync(entry)) return;
  const py = bundledPython() || (process.platform === 'win32' ? 'python' : 'python3');
  try {
    pyProc = spawn(py, [entry, '--port', String(PY_PORT), '--no-browser'], { stdio: 'ignore', detached: false });
    pyProc.on('error', () => { pyProc = null; });   // 无任何 Python 环境时静默降级
    pyProc.on('exit', () => { pyProc = null; });
    watchBackendReady();
  } catch (e) { pyProc = null; }
}

// 后端就绪轮询：OCR 模型后台加载完成后通知渲染进程自动重连
// （前端自身的短轮询会在模型加载完成前超时，这里兜底）
function watchBackendReady() {
  let tries = 0;
  const timer = setInterval(() => {
    tries++;
    const http = require('http');
    const req = http.get(`http://127.0.0.1:${PY_PORT}/health`, res => {
      let body = '';
      res.on('data', c => body += c);
      res.on('end', () => {
        try {
          const h = JSON.parse(body);
          if (h.status === 'ok' && h.ocr === true) {
            clearInterval(timer);
            if (win && !win.isDestroyed()) win.webContents.send('backend-ready');
          }
          // 'loading' 继续等；false（不可用）继续等一会儿（引擎可能在切换）
        } catch (e) { /* 非 JSON，继续 */ }
      });
    });
    req.on('error', () => {});
    if (tries > 120) clearInterval(timer);   // 最长 2 分钟
  }, 1000);
}

// ---------- 主窗口 ----------
let win = null;
function createWindow() {
  win = new BrowserWindow({
    width: 1440,
    height: 960,
    minWidth: 1100,
    minHeight: 700,
    title: '印章比对工具',
    show: false,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  win.once('ready-to-show', () => win.show());
  win.loadURL(`http://127.0.0.1:${HTTP_PORT}/`);
  if (isDev) win.webContents.openDevTools({ mode: 'detach' });
  win.on('closed', () => { win = null; });
}

// ---------- 截图：全屏捕获 → 选区窗口（每屏一窗）----------
let captureWins = [];
let sourcesCache = [];

async function startCapture() {
  if (captureWins.length) return;
  const primary = screen.getPrimaryDisplay();
  const sources = await desktopCapturer.getSources({ types: ['screen'], thumbnailSize: primary.size });
  if (!sources.length) return;
  sourcesCache = sources;
  const cursor = screen.getCursorScreenPoint();
  // 光标所在屏最后创建（获得焦点），其余屏在前铺底
  const ordered = sources.slice().sort((a, b) => {
    const onA = displayHasPoint(a, cursor) ? 1 : 0;
    const onB = displayHasPoint(b, cursor) ? 1 : 0;
    return onA - onB;
  });
  for (const src of ordered) {
    const disp = screen.getAllDisplays().find(d => d.id === Number(src.display_id)) || primary;
    const { x, y, width, height } = disp.bounds;
    const cwin = new BrowserWindow({
      x, y, width, height,
      frame: false, transparent: true, alwaysOnTop: true, skipTaskbar: true,
      resizable: false, movable: false, enableLargerThanScreen: true, show: false,
      webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true },
    });
    cwin.setFullScreen(true);
    cwin.loadFile(path.join(__dirname, 'capture.html'), {
      query: { shot: src.thumbnail.toDataURL(), idx: String(sources.indexOf(src)) },
    });
    cwin.once('ready-to-show', () => cwin.show());
    cwin.on('closed', () => { captureWins = captureWins.filter(w => w !== cwin); });
    captureWins.push(cwin);
  }
}
function displayHasPoint(src, pt) {
  const disp = screen.getAllDisplays().find(d => d.id === Number(src.display_id)) || screen.getPrimaryDisplay();
  const b = disp.bounds;
  return pt.x >= b.x && pt.x <= b.x + b.width && pt.y >= b.y && pt.y <= b.y + b.height;
}
function closeAllCaptures() {
  captureWins.forEach(w => { try { w.destroy(); } catch (e) {} });
  captureWins = [];
}
ipcMain.on('capture-cancel', () => closeAllCaptures());
ipcMain.on('capture-start', () => startCapture());   // 页面按钮触发
ipcMain.on('capture-done', (e, { idx, rect }) => {
  const senderWin = BrowserWindow.fromWebContents(e.sender);
  const dispIdx = captureWins.indexOf(senderWin);
  const src = sourcesCache[dispIdx >= 0 ? dispIdx : idx];
  closeAllCaptures();
  if (!src) return;
  const img = src.thumbnail;
  const size = img.getSize();
  const disp = screen.getAllDisplays().find(d => d.id === Number(src.display_id)) || screen.getPrimaryDisplay();
  const scale = size.width / disp.bounds.width;   // 捕获缩放（retina 等）
  const crop = nativeImage.createFromBitmap(img.toBitmap(), size).crop({
    x: Math.max(0, Math.round(rect.x * scale)),
    y: Math.max(0, Math.round(rect.y * scale)),
    width: Math.max(1, Math.round(rect.width * scale)),
    height: Math.max(1, Math.round(rect.height * scale)),
  });
  clipboard.writeImage(crop);
  if (win) win.webContents.send('capture-done', { dataUrl: crop.toDataURL(), target: settings.pasteTargetAfterCapture });
});

function registerCaptureShortcut() {
  globalShortcut.unregisterAll();
  try {
    globalShortcut.register(settings.captureShortcut, () => startCapture());
  } catch (e) {
    if (win) win.webContents.send('shortcut-error', settings.captureShortcut);
  }
}

// ---------- 印章库（文件系统：PNG + index.json 索引）----------
const libIndexFile = () => path.join(settings.libPath, 'index.json');
function ensureLib() {
  if (!fs.existsSync(settings.libPath)) fs.mkdirSync(settings.libPath, { recursive: true });
}
function readIndex() {
  ensureLib();
  try { return JSON.parse(fs.readFileSync(libIndexFile(), 'utf8')); } catch (e) { return { seals: [] }; }
}
function writeIndex(idx) { ensureLib(); fs.writeFileSync(libIndexFile(), JSON.stringify(idx, null, 2)); }
function safeName(name) {
  return (name || '未命名印章').replace(/[\\/:*?"<>|\n\r]/g, '_').trim().slice(0, 80);
}

ipcMain.handle('lib:save', (e, { name, note, png, original, ocr }) => {
  ensureLib();
  const idx = readIndex();
  const id = `${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
  const base = safeName(name);
  const stampFile = `${base}_${id}.png`;
  fs.writeFileSync(path.join(settings.libPath, stampFile), Buffer.from(png.split(',')[1], 'base64'));
  let originalFile = null;
  if (original) {
    originalFile = `${base}_${id}_原图.png`;
    fs.writeFileSync(path.join(settings.libPath, originalFile), Buffer.from(original.split(',')[1], 'base64'));
  }
  idx.seals.push({ id, name: base, note: note || '', file: stampFile, originalFile,
                   ocr: ocr || null, savedAt: new Date().toISOString() });
  writeIndex(idx);
  return { ok: true, id };
});
ipcMain.handle('lib:list', () => readIndex().seals);
ipcMain.handle('lib:search', (e, q) => {
  const kw = (q || '').trim().toLowerCase();
  const seals = readIndex().seals;
  if (!kw) return seals;
  return seals.filter(s => s.name.toLowerCase().includes(kw)
    || (s.note || '').toLowerCase().includes(kw)
    || ((s.ocr && s.ocr.company) || '').toLowerCase().includes(kw));
});
ipcMain.handle('lib:load', (e, id) => {
  const s = readIndex().seals.find(x => x.id === id);
  if (!s) return { ok: false, error: 'not found' };
  const p = path.join(settings.libPath, s.file);
  if (!fs.existsSync(p)) return { ok: false, error: 'file missing' };
  return { ok: true, png: 'data:image/png;base64,' + fs.readFileSync(p).toString('base64'),
           originalPng: s.originalFile && fs.existsSync(path.join(settings.libPath, s.originalFile))
             ? 'data:image/png;base64,' + fs.readFileSync(path.join(settings.libPath, s.originalFile)).toString('base64') : null,
           meta: s };
});
ipcMain.handle('lib:delete', (e, id) => {
  const idx = readIndex();
  const s = idx.seals.find(x => x.id === id);
  if (!s) return { ok: false };
  try { fs.unlinkSync(path.join(settings.libPath, s.file)); } catch (e) {}
  if (s.originalFile) { try { fs.unlinkSync(path.join(settings.libPath, s.originalFile)); } catch (e) {} }
  idx.seals = idx.seals.filter(x => x.id !== id);
  writeIndex(idx);
  return { ok: true };
});
ipcMain.handle('lib:choosePath', async () => {
  const r = await dialog.showOpenDialog(win, { properties: ['openDirectory', 'createDirectory'], title: '选择印章库保存路径' });
  if (r.canceled || !r.filePaths.length) return { ok: false };
  settings.libPath = r.filePaths[0];
  saveSettings();
  return { ok: true, path: settings.libPath };
});
ipcMain.handle('lib:reveal', () => { ensureLib(); shell.openPath(settings.libPath); });

// ---------- 设置 ----------
ipcMain.handle('settings:get', () => ({ ...settings, platform: process.platform }));
ipcMain.handle('settings:set', (e, patch) => {
  settings = { ...settings, ...patch };
  saveSettings();
  if (patch.captureShortcut) registerCaptureShortcut();
  return { ...settings };
});
ipcMain.handle('settings:reset', () => { settings = { ...DEFAULTS }; saveSettings(); registerCaptureShortcut(); return { ...settings }; });

// ---------- 生命周期 ----------
app.whenReady().then(() => {
  loadSettings();
  startStaticServer();
  startBackend();
  createWindow();
  registerCaptureShortcut();

  const appName = '印章比对工具';
  const template = [
    // 手动构建应用菜单（role:appMenu 的菜单栏标题取 bundle 名，开发模式显示 Electron）
    ...(process.platform === 'darwin' ? [{
      label: appName,
      submenu: [
        { role: 'about', label: '关于 ' + appName },
        { type: 'separator' },
        { role: 'services', submenu: [] },
        { type: 'separator' },
        { role: 'hide', label: '隐藏 ' + appName },
        { role: 'hideOthers', label: '隐藏其他' },
        { role: 'unhide', label: '全部显示' },
        { type: 'separator' },
        { role: 'quit', label: '退出 ' + appName },
      ],
    }] : []),
    { role: 'editMenu' },
    { label: '视图', submenu: [
        { role: 'reload', label: '重新加载' },
        { role: 'toggleDevTools', label: '开发者工具' },
        { type: 'separator' },
        { role: 'resetZoom', label: '实际大小' },
        { role: 'zoomIn', label: '放大' },
        { role: 'zoomOut', label: '缩小' },
        { role: 'togglefullscreen', label: '全屏' },
      ] },
    { label: '操作', submenu: [
        { label: '截图', accelerator: settings.captureShortcut, click: () => startCapture() },
        { label: '印章库目录', click: () => shell.openPath(settings.libPath) },
      ] },
  ];
  Menu.setApplicationMenu(Menu.buildFromTemplate(template));

  app.on('activate', () => { if (BrowserWindow.getAllWindows().length === 0) createWindow(); });
});

app.on('will-quit', () => { globalShortcut.unregisterAll(); });
app.on('window-all-closed', () => {
  if (pyProc) { try { pyProc.kill(); } catch (e) {} }
  if (server) server.close();
  if (process.platform !== 'darwin') app.quit();
});

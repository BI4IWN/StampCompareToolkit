# Windows 版安装与使用

> 版本：v3.6.22 文档 ｜ 适用系统：Windows 10 / 11（64 位）

印章比对工具（Stamp Compare Toolkit）在 Windows 与 macOS 上的**官方发行线统一为 Tauri + Rust**。Electron 版仅作中间版存档备选；本轮不提供 Windows Electron 包。

---

## 一、安装包

下表只列本轮已核验的 Windows 包。

| 文件 | 说明 | 适用场景 |
|---|---|---|
| `StampCompareToolkit-Tauri-3.6.22-win-x64-setup.exe` | Tauri NSIS 安装向导，可选安装目录，创建开始菜单/桌面快捷方式 | 正式线，长期使用 |

- 大小：16,895,288 B（约 16.1 MB）。
- SHA-256：`e3b2ae25476c9c41d5a9180e25682387504e607d96fbdf45d6cca7116347038d`
- 产品版本：FileVersion / ProductVersion = 3.6.22；bundle id `com.bi4iwn.stampcompare`；产品名「印章比对工具」。
- 需系统自带 WebView2（Windows 10 1803+ / Windows 11 均内置）。
- **未做代码签名**。Windows SmartScreen 可能提示「已保护你的电脑」→ 点「更多信息 → 仍要运行」。
- 在已安装/可运行的环境中，本地服务 `http://127.0.0.1:18970/health` 应返回类似 `{"status":"ok","version":"3.6.22","ocr":true,"engine":"rust"}`。

安装包见 [Release v3.6.22](https://github.com/BI4IWN/StampCompareToolkit/releases/tag/v3.6.22)。旧版 v3.6.17 的 Windows Tauri 包仍保留在对应 Release 中，新安装请改用 3.6.22。

## 二、系统要求

| 项目 | 最低 | 建议 |
|---|---|---|
| 系统 | Windows 10 1803+（64 位，含 WebView2） | Windows 11 |
| CPU | x64 双核 | 四核及以上 |
| 内存 | 4 GB | 8 GB+ |
| 磁盘 | 200 MB 可用 | 500 MB+ |
| 显示 | 1280×800 | 1440×900 及以上 |

## 三、首次启动

1. 双击安装包完成安装。若出现 SmartScreen 提示，选择「更多信息 → 仍要运行」。
2. 程序会在本机启动 Rust 服务（默认 `http://127.0.0.1:18970/`）并加载界面。右上角显示版本号，或访问 `/health` 见到 `engine=rust` 即表示就绪。
3. Tauri 版识别模型随包分发，一般无需像旧 Electron 版那样等待 10–30 秒加载便携 Python。
4. 拖入（或 `Ctrl+V` 粘贴）两枚印章图片 → 自动提取、自动对齐 → 右侧显示判定/识别/候选。

## 四、数据与目录

| 内容 | 位置 |
|---|---|
| 设置（快捷键、印章库路径、主题） | `%APPDATA%\印章比对工具\settings.json` |
| 印章库索引 | 取决于设置中的「印章库路径」，默认 `%USERPROFILE%\印章库` |
| 本地 AI 引擎（Windows 不支持本地视觉大模型，见下） | `%APPDATA%\印章比对工具\engines\` |
| 后端日志 | 程序窗口内「使用说明 → 关于」可见版本与路径 |

卸载程序**不会**删除印章库与设置；需要彻底清理时手动删除上述目录。

## 五、快捷键

| 操作 | 快捷键 |
|---|---|
| 全局截图（选区后直接贴入） | `Ctrl+Shift+S`（可在「设置」中修改） |
| 打开图片 | `Ctrl+O` |
| 导出对比图（PNG） | `Ctrl+E` |
| 切换操作对象（参考章 / 待验章） | `Ctrl+1` / `Ctrl+2` |
| 清空当前对比 | `Ctrl+Shift+Backspace` |
| 粘贴剪贴板图片 | `Ctrl+V` |
| 画布缩放 / 旋转 | 滚轮；`Alt`+滚轮旋转；方向键微调 |

## 六、平台差异

| 能力 | macOS | Windows |
|---|---|---|
| 正式线（Tauri + Rust OCR） | ✅ 已核验（Apple Silicon） | ✅ 已核验（x64） |
| 本地视觉大模型（PaddleOCR-VL，MLX 加速） | ✅ 可选安装（约 1.5 GB，仅 Electron 备选 / Apple Silicon） | ❌ 不可用（依赖 Apple Metal/MLX） |
| 云端视觉识别（DeepSeek / 智谱 / 火山方舟豆包 / 自定义） | ✅ | ✅ |
| 印章库名称匹配（Top-3 候选） | ✅ | ✅ |
| 圆章校正（椭圆 → 正圆） | ✅ | ✅ |
| 黑/灰章与淡色章取色、剔除、放大镜 | ✅ | ✅ |
| 全局截图快捷键 | ✅ | ✅ |

> Windows 正式线使用内置 Rust OCR（`stamp-ocr`），不依赖便携 Python。本地视觉大模型在 Windows 上不可用；如需更高识别率，请在「AI」中配置云端视觉模型。

## 七、常见问题

**1. SmartScreen 提示「已保护你的电脑」**
本包未做代码签名。点「更多信息 → 仍要运行」即可。这不是病毒扫描结论，只是未签名程序的常规拦截。

**2. 启动后界面空白，或 `/health` 访问失败**
确认本机 `127.0.0.1:18970` 未被占用，并已安装 WebView2。可用浏览器打开 `http://127.0.0.1:18970/health` 检查。若仍在使用旧 Electron 备选包，其端口为 `18799` / `8799`，不要与正式线混淆。

**3. 提取结果为空白**
章不是红色/黑色（例如蓝色章）→ 在「⋯ 更多 → 墨色」中选「蓝」，或用「点选取色」在章上点一下；淡色章会自动启用淡墨提取，也可手动加色。

**4. 章被拍歪了、边不重合**
默认开启「圆章校正」（⋯ 更多 → 圆章校正）。若识别成异形章，可关掉该开关或先用「框选区域」只框章体。

**5. 杀毒软件拦截或拖慢**
可将安装目录加入白名单。未签名安装包更容易被启发式扫描。

## 八、构建说明（开发者）

Windows 正式包必须在 **Windows 主机或 CI** 上构建，不要从 macOS 交叉编译。推荐命令：

```bash
cd tauri
# 前端需先同步到 tauri/web（macOS/Linux 可用 npm run build 中的 sync-web.sh）
npx tauri build --target x86_64-pc-windows-msvc --bundles nsis
```

仓库 CI 配方见 `.github/workflows/tauri-windows.yml`。产物默认可能是中文文件名 `印章比对工具_<版本>_x64-setup.exe`，发行时重命名为 `StampCompareToolkit-Tauri-<版本>-win-x64-setup.exe`。

Electron 兼容备选仍可按旧方式构建（本轮不发布 Windows Electron 包）：

```bash
cd desktop
npm install
npx electron-builder --win nsis --x64
```

前置条件（Electron 备选）：`desktop/runtime/python-win-x64/` 存在且包含 `python.exe`、`rapidocr_onnxruntime`、`onnxruntime`、`opencv`、`numpy`、`Pillow`（已随仓库本地保留，不入库）；`stamp-toolkit/` 通过 `extraResources` 打进去。

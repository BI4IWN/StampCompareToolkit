# Tauri + Rust 桌面外壳（正式线）

自 3.6.21 起，**Windows 与 macOS 的官方发行统一走本目录**（Tauri 2 + Rust），不再以 Electron 为主。Electron（`desktop/`）仅作中间版存档备选。

本目录最初是 P2 spike，用于验证「Electron 换 Tauri、Python 换 Rust」在包体和识别上的收益；现已作为正式桌面线维护。下文保留当时的实测数据，供对照。

## 结构

```
tauri/
├── package.json            # @tauri-apps/cli
├── web/                    # sync-web 生成（index.html 不入库）
└── src-tauri/
    ├── Cargo.toml          # tauri 2 + axum + stamp-core + stamp-ocr
    ├── tauri.conf.json     # version 3.6.21，identifier com.bi4iwn.stampcompare
    ├── models/             # 随包 ONNX 与字典
    └── src/main.rs         # 本地 axum：/、/health、/ocr 等，默认 127.0.0.1:18970
```

## 关键设计

- **前端零改动**：Rust 里起一个本地 HTTP 服务（同 Electron 的做法），页面照旧 `fetch('/ocr')`。
- 页面经 `npm run sync-web` 同步后由本地服务托管；`tauri/web/index.html` 被 gitignore，不要提交。
- `/health` 在正式包中返回 `engine=rust`、当前版本与 `ocr` 状态。
- `/ocr` 走 `stamp-core` 几何 + `stamp-ocr` 识别，返回结构与原 Python `/ocr` 对齐。

## 实测（M1，macOS，spike 阶段对照）

| 项 | Electron + Python（3.6.15） | Tauri + Rust（含识别） |
|---|---|---|
| App 解包 | **580 MB** | **40 MB**（含 11MB 模型） |
| 安装包（dmg） | 218 MB | **17.6 MB**（当时 spike；3.6.21 已核验 macOS arm64 dmg 为 24,121,767 B） |
| 启动 | ~1.5s | < 0.6s |
| `/ocr` 单枚 | 3–5 s（Python+RapidOCR 热态） | **0.4–0.9 s** |

包体差主要来自去掉 Electron Framework 与 Python 运行时。3.6.21 已核验 Windows x64 NSIS 安装包为 16,895,288 B。

## 识别管线

- crate `stamp-ocr`：`ink_strength` → 直线抑制 + RANSAC 圆心 → `stamp_core` 去畸变 → `radial_bands` + `unwrap_ring_band` → `cut_strips` → `rec_batch1.onnx` + `cls_static.onnx`（tract）+ `dict.txt` → `postprocess`（机构名/编码/类型）。
- 模型随包：`src-tauri/models/` → 资源目录，启动时按「env / resource_dir / 可执行同级 / 上层 Resources / cwd」顺序探测。

## 构建

macOS（默认 dmg）：

```bash
cd tauri
npm i
export PATH="$HOME/.cargo/bin:$PATH"
npm run build
```

Windows 必须在 Windows 主机或 CI 上打包，**不要从 macOS 交叉编译**：

```bash
cd tauri
npx tauri build --target x86_64-pc-windows-msvc --bundles nsis
```

CI 配方见仓库 `.github/workflows/tauri-windows.yml`。发行文件名：`StampCompareToolkit-Tauri-<版本>-win-x64-setup.exe`。当前默认 `bundle.targets` 仅为 `dmg`，Windows 构建需显式传入 `--bundles nsis`。

## 仍可改进的点

- 识别质量对齐：部分双通道策略仍可能弱于历史最优对照。
- `/engine/*`（可选本地视觉引擎）在 Tauri 下的等价实现，或明确不纳入正式线。
- 未做 Developer ID / Windows 代码签名；用户首次打开需按系统提示允许。

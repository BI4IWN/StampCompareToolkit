# Tauri + Rust 外壳 spike（P2）

目标：验证「Electron 换 Tauri、Python 换 Rust」在**包体**上的收益，并确认 Rust 内核能在 App 进程内跑通。

## 结构

```
09-tauri-spike/
├── package.json            # @tauri-apps/cli
├── web/index.html          # = stamp-toolkit/src/stamp-compare.html（前端一行没改）
└── src-tauri/
    ├── Cargo.toml          # tauri 2 + axum + stamp-core
    ├── tauri.conf.json     # 窗口 url = http://127.0.0.1:18970/
    └── src/main.rs         # 起本地 axum：/、/health、/ocr（几何走 stamp-core）
```

## 关键设计

- **前端零改动**：Rust 里起一个本地 HTTP 服务（同 Electron 的做法），页面照旧 `fetch('/ocr')`。
- 页面用 `include_str!` 编进二进制，不需要额外的资源目录。
- `/ocr` 目前只跑**几何**（`stamp_core::fit_ellipse` + `dewarp_iterative`），返回结构与 Python `/ocr` 对齐（`stamps[0].detail` 里放 fit/dewarp 明细），前端不会崩。

## 实测（M1，macOS）

| 项 | Electron + Python（3.6.15） | Tauri + Rust（含识别） |
|---|---|---|
| App 解包 | **580 MB** | **40 MB**（含 11MB 模型） |
| 安装包（dmg） | 218 MB | **17.6 MB** |
| 启动 | ~1.5s | < 0.6s |
| `/ocr` 单枚 | 3–5 s（Python+RapidOCR 热态） | **0.4–0.9 s** |

包体差 66×，主要来自 Electron Framework（224MB）与 Python 运行时（353MB）都被去掉。

## 识别已接通（P3-1）

- 新 crate `stamp-ocr`（lib）：`ink_strength` → 直线抑制 + RANSAC 圆心 → `stamp_core` 去畸变 → `radial_bands` + `unwrap_ring_band` → `cut_strips`（切点吸附列投影最小） → `rec_batch1.onnx` + `cls_static.onnx`（tract）+ `dict.txt` → `postprocess`（机构名/编码/类型）。
- 模型随包：`src-tauri/models/` → `Resources/models`，启动时按「env / resource_dir / 可执行同级 / 上层 Resources / cwd」顺序探测。
- 实测（bundled app，dmg 挂载运行）：
  - 红章「东元科技」→ 文本 `e9006901东元科技有限公司`、**company 正确**、398ms
  - 灰/黑章「工商银行费县」→ `股份有限公司费县支行中国工商银行月`、895ms（冷启动首枚）
  - 极淡章、复杂背景照 → 仍较弱（与 Rust 管线 0.796 一致的短板）

## 还没做（P3-2）

0. 识别质量对齐：`auto` 双通道（rg 不给力时退 dark）尚未接进 Tauri 版，接上应回到 0.796。
2. `/engine/*`（可选本地视觉引擎）在 Tauri 下的等价实现，或直接砍掉（Tauri 版本来就不带 Python）。
3. 截图快捷键、印章库（主进程 IPC → Tauri command）、设置持久化。
4. Windows 构建：Tauri 跨平台但**不能从 macOS 交叉出 Windows 包**，要在 Windows/CI 上打。

## 复现

```bash
cd 09-tauri-spike
npm i
export PATH="$HOME/.cargo/bin:$PATH"
npx tauri build --bundles dmg     # 产物：src-tauri/target/release/bundle/dmg/
```

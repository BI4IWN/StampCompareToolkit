# 印章对比工具 — 技术文档（v3.6.8）

## 目录

- [一、系统架构](#一系统架构)
- [二、前端架构](#二前端架构)
- [三、印章提取算法](#三印章提取算法)
- [四、Canvas 变换模型](#四canvas-变换模型)
- [五、自动对齐算法](#五自动对齐算法v200)
- [六、对比渲染引擎](#六对比渲染引擎)
- [七、交互系统](#七交互系统)
- [八、OCR 后端](#八ocr-后端)
- [九、AI 文字提取后端](#九ai-文字提取后端v280)
- [十、性能优化](#十性能优化)
- [十一、启动器实现](#十一启动器实现)
- [十二、圆章校正](#十二圆章校正v363)
- [十三、原图查看器与取色放大镜](#十三原图查看器与取色放大镜v350v367)
- [十四、判定与检查器](#十四判定与检查器v362)

---

## 桌面版架构（Electron，v3.0.0）

```
desktop/
  main.js      主进程：窗口管理、本地静态服务(127.0.0.1:18799，同源加载前端页)、
               API 反代 → Python 后端(8799)、Python 子进程托管（无环境静默降级）、
               全局快捷键（globalShortcut）、选区截图（desktopCapturer + 每屏覆盖窗）、
               印章库文件管理、设置持久化（userData/settings.json）
  preload.js   contextIsolation 受控桥（window.stampDesktop）：截图事件、印章库 IPC、设置 IPC
  capture.html 选区截图覆盖窗：拖框 + 尺寸提示 + 确认/取消；确认后主进程按显示器缩放
               换算裁剪 → clipboard.writeImage → webContents.send('capture-done')
```

- 印章库存储：`{libPath}/印章名_{id}.png`（提取结果）+ `印章名_{id}_原图.png` + `index.json`（名称/备注/OCR/时间索引）；名称默认取 `state[target].ocrResult.company`
- 前端集成：`stamp-compare.html` 末尾独立模块，无 `window.stampDesktop` 桥时 early return（网页版零感知）；截图完成浮层 → `processStamp(img, 'ref'|'test')` 复用完整提取管线
- OCR 运行时（v3.0.0）：安装包内置便携 Python（python-build-standalone，npmmirror 镜像）+ RapidOCR/onnxruntime + opencv-headless（`scripts/setup-runtime.sh` 一键构建，按平台放入 desktop/runtime/），随 extraResources 分发。主进程启动优先级：内置 runtime → 系统 python3；引擎优先级 RapidOCR（轻量、模型内置、离线可用）→ PaddleOCR（run_ocr_on_image 多引擎统一接口，_ocr_engine 标记当前引擎）
- 打包：electron-builder，stamp-toolkit 与 runtime 作为 extraResources 随包分发；mac(dmg arm64/x64 + zip) · win(nsis/portable)

## 一、系统架构

### 整体架构

系统采用前后端分离架构，前端为纯 HTML 单文件应用，后端为 Python HTTP 服务。

```
┌──────────────────────────────────────────────────────────┐
│                    浏览器 (stamp-compare.html)             │
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────┐ │
│  │ 图片加载  │  │ 印章提取  │  │ 对比渲染  │  │ 交互控制 │ │
│  │ 粘贴/拖拽 │  │ 红色检测  │  │ 叠加/滑块 │  │ 变换/手柄│ │
│  │ 文件上传  │  │ 归一化    │  │ 差异高亮  │  │ 拖拽/缩放│ │
│  └──────────┘  └──────────┘  └──────────┘  └─────────┘ │
│                                              │           │
│                                         fetch /ocr       │
└──────────────────────────────────────────────┼───────────┘
                                               │
                                    HTTP (127.0.0.1:8765)
                                               │
┌──────────────────────────────────────────────┼───────────┐
│                  Python 后端 (stamp_app.py)   │           │
│                                              │           │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐   │           │
│  │ HTTP 服务 │  │ 红色提取  │  │ 极坐标展开│←──┘           │
│  │ 页面+API  │  │ HSV 检测  │  │ cv2.remap │             │
│  └──────────┘  └──────────┘  └──────────┘               │
│                                                          │
│  ┌──────────────────────────────────────────────────┐    │
│  │              PaddleOCR 识别引擎                    │    │
│  │  多参数组合 × 二值化 → 最优结果 → 文字解析          │    │
│  └──────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────┘
```

上图为 v2.x 的精简示意。**v3.6.8 的实际分层**：

| 层 | 组件 | 说明 |
|---|---|---|
| 外壳 | Electron `desktop/main.js`（406 行） | 固定端口：静态服务 `127.0.0.1:18799`、反代 Python `8799`；全局截图快捷键；印章库与设置持久化 |
| 前端 | `stamp-compare.html`（约 11100 行） | 三栏 UI、提取/校正/对齐/比对/检查器、印章库与设置；不依赖后端也能完成比对与导出 |
| 服务 | `stamp_app.py`（约 3300 行） | 页面托管 + `/ocr`、`/ai/*`、`/engine/*`；线程池 + OCR 全局锁 |
| 识别 | RapidOCR（主）→ PaddleOCR（回退） | 环形展开管线；印章库 Top-3 校验 |
| 视觉模型（可选） | `vlm_engine.py` + mlx-vlm | 独立子进程，stdout 仅 JSON；未安装时识别自动回退 RapidOCR |
| 云端（可选） | AI 代理 `/ai-ocr` | DeepSeek / 智谱 / 火山方舟豆包 / 自定义，Key 只在浏览器 localStorage |

### 文件职责

**stamp-compare.html**（约 11100 行）：前端单文件应用，包含全部 UI 样式、HTML 结构与 JavaScript 逻辑。v3.6.2 起为三栏布局（工具区 + 画布 + 右侧检查器），实现图片加载、印章提取（四墨色/手工取色/颜色增强）、圆章校正、自动对齐、三种对比模式、差异高亮、原图查看器、识别编排、印章库与设置。通过 fetch 调用后端 OCR / AI / 引擎管理 API。

**stamp_app.py**（约 3300 行）：Python 后端，`ThreadingHTTPServer` 内嵌服务。承担页面托管、本地 OCR（RapidOCR 优先，PaddleOCR 回退）、环形文字展开管线、印章库匹配、AI 代理（`/ai-ocr`、`/ai/test`、`/ai/stamp`）与本地识别引擎管理（`/engine/*`）。

**vlm_engine.py**（约 130 行）：本地视觉模型的独立推理进程（`--model-dir/--image/--crop`），stdout 只输出 JSON，供 `stamp_app.py` 以子进程方式调用，避免 mlx 与主进程依赖耦合。

**launcher.sh / launcher.bat / 启动印章对比工具.command**：一键启动器，负责环境检测、依赖安装、服务启动和浏览器打开。macOS 用户双击 .command 文件即可运行。

### 通信协议

前端通过 HTTP POST 请求向后端发送 OCR 请求：

```
POST /ocr
Content-Type: application/json

{
  "image": "<base64 编码的原始图片>",
  "color": "red"           // 墨色: red|blue|purple 或逗号分隔多选（如 "red,blue"）；
                            // 兼容 'auto'（红/蓝/紫取占优者）。默认 red（v2.1.0）
  "hues": [120, 45]        // 手工取色的自选色相列表（0-360 度），与 color 取并集（v2.3.0）
}

Response (成功):
{
  "success": true,
  "stamps": [
    {
      "center": [x, y],
      "radius": r,
      "texts": [{"text": "文字", "score": 0.95}, ...],
      "full_text": "完整文字"
    }
  ],
  "color": "red,blue",      // 实际使用的墨色集合
  "image_size": [width, height]
}

Response (失败，v2.0.0 结构化错误):
{
  "success": false,
  "error": "OCR 模型未加载 — 请安装依赖: pip install paddlepaddle paddleocr opencv-python 后重启服务"
}
```

`/ocr` 响应在 v3.2.0 之后新增以下字段（前端检查器直接消费）：

| 字段 | 说明 |
|---|---|
| `engine` | 实际使用的识别引擎：`rapid` / `paddle` / `mlx-paddleocr-vl` |
| `elapsed_ms` | 单次识别耗时 |
| `company` / `type` / `code` | 拆分后的公司名、印章类型、编码 |
| `confidence` | 匹配置信度（库匹配为相似度得分） |
| `match_source` | `library` / `ocr` / `ai` |
| `needs_review` | 置信度落在复核区间时为 true |
| `candidates` | 印章库 Top-3 候选（名称 + 得分） |
| `raw_text` / `raw_company` / `match_margin` / `library_size` | 原始识别与匹配余量、库规模 |

健康检查通过 GET /health 端点实现：

```
{"status": "ok", "version": "3.6.8", "ocr": true}
```

本地识别引擎管理（v3.5.0 起，全部为 `POST`/`GET` + JSON）：`/engine/probe`（硬件探测与建议）、`/engine/status`、`/engine/install`（返回 job id）、`/engine/job/<id>`（轮询）、`/engine/cancel`、`/engine/remove`、`/engine/activate`、`/engine/import`（手动导入模型目录）、`/engine/runtime`（安装 Python 运行时）。状态落盘在 `~/Library/Application Support/印章比对工具/engines.json`（临时文件 + 原子替换，读写加锁，损坏时回落默认值）。

`ocr` 为 false 表示服务在线但 OCR 模型未加载（依赖未安装），前端据此区分"无服务"与"有服务无模型"两种提示。

---

## 二、前端架构

### 状态管理

前端采用集中式状态对象 `state`，管理所有运行时数据：

```javascript
const state = {
  ref:  { original, extracted, circular, rotate, scaleX, scaleY, offsetX, offsetY },
  test: { original, extracted, circular, rotate, scaleX, scaleY, offsetX, offsetY },
  target: 'test',          // 当前操作目标
  scaleAxis: 'uniform',    // 缩放轴模式
  mode: 'overlay',         // 对比模式
  opacity: 50,             // 待验章透明度
  diffMode: false,         // 差异高亮开关
  sliderPos: 0.5,          // 滑块位置（0-1）
  // ... 交互状态
};
```

每个印章（ref/test）维护独立的状态：原始图片、提取后的画布、是否为圆形章、旋转角度、XY 缩放比例、XY 偏移量。

### 渲染管线

渲染采用 requestAnimationFrame 节流机制，多次连续状态变更合并为一帧一次渲染：

```
状态变更 → scheduleRender() → requestAnimationFrame → render()
```

render() 函数根据当前模式分发到具体渲染函数：

```
render()
├── 清除画布 + 白色背景
├── 绘制网格（交互中跳过）
├── 绘制十字参考线
├── 根据 mode 分发:
│   ├── overlay  → renderOverlay()
│   ├── slider   → renderSlider()
│   └── sidebyside → renderSideBySide()
├── 差异高亮覆盖层（叠加模式 + diffMode + 非交互中）
├── 模式标签
└── 缩放手柄和选中边框
```

---

## 三、印章提取算法

### 墨色多选（v2.1.0，v2.4.0 起每枚印章独立）

v2.4.0 起墨色按印章独立配置：`state.ink.ref / state.ink.test` 分别保存 `{presets, customs}`（customs 为自选色相列表），每枚印章的墨色面板位于各自提取区上方，改动只重提对应印章。检测/提取统一经 `activeInkSpec(target)` 取 `{presets, hues}` 并集规格（默认 `['red']`）：

```javascript
const INK_BANDS = {
  red:    { hueLo: [0, 40],    hueHi: [320, 360], center: 0     },  // 红（跨 0° 分两段）
  blue:   { hueLo: [180, 265], hueHi: null,       center: 222.5 },
  purple: { hueLo: [250, 330], hueHi: null,       center: 290   },
};
```

- 检测阶段：像素命中任一选中色带即算墨迹（并集）；`hueDistToBand` 计算到色带的最短距离用于置信度
- 提取阶段：色带各向外扩 5° 保留褪色边缘；每像素取距离最近的命中色带计算置信度，并按色带增强对应通道（红→R、蓝→B、紫→R+B）
- UI 约束：至少保留一种墨色（最后一种不可取消）；切换后已加载印章自动重提取并重置变换
- 后端 `/ocr` 的 `color` 参数接受逗号分隔多值（如 `red,blue,purple`），`_single_ink_mask` 按色取并集
- 手工取色（v2.3.0 起，v2.4.0 按印章独立，v2.5.0 增加排除，v2.9.4 增量模型，v2.11.5 精确颜色模型）：`state.ink[target].customs` 保存自选色 `{hue, sat, val, hex, on, exclude, set}`，`set` 为点选位置邻域采集的颜色簇 `[[r,g,b],...]`（collectColorSet 聚类去重，≤ 6 个代表色）。匹配规则（检测 isStampPixel、排除 isExcludedBy、增量提取 recomputeExtracted 三处同步）：
  - **精确匹配**（v2.11.5）：像素 RGB 与颜色簇中任一色的欧氏距离 ≤ 26（PICK_COLOR_TOL）即命中，无任何 HSV 窗口推断——点选什么颜色就叠加/删除什么颜色
  - 性能：增量叠加为纯整数距离比较（无逐像素 HSV 转换），先按全部点选色的 RGB 包围盒（±26 margin）快速排除，单次全量重算 ~18ms（1200² 画布）
  - 无 set 的旧数据回退 v2.11.3 HSV 窗口规则；后端 OCR 的 hues 命中仍按色相环 ±18°（含 0°/360° 回绕）
- **颜色排除（v2.5.0）**：`activeInkSpec(target)` 返回 `{presets, hues, excludes}`；排除色优先——像素命中排除色相即判为非墨迹（前端 `isStampPixel`/`extractStamp` 与后端 `extract_ink_mask` 均先做排除相减）。后端 `/ocr` 新增 `excludes` 参数，经 `parse_ink_colors` 解析后贯通印章中心/多印章检测与通道提取
- 预设色可全部清空、仅按自选色提取：`activeInkSpec(target)` 与后端 `parse_ink_colors` 仅在预设与 hues 均为空时才兜底红章；两枚印章的预设/自选色完全独立

### 前端提取流程（extractStamp / detectStampCandidates 函数）

前端在图片加载后立即执行印章提取，整个过程在浏览器中完成：

**第一步：墨色像素检测**

遍历图片每个像素，转换到 HSV 色彩空间按选中墨色集合判断：

```
红色条件：色相 0°-45° 或 315°-360°
蓝色条件：色相 175°-270°
紫色条件：色相 245°-335°
共同条件：饱和度 (S) ≥ 0.03；与白色的距离: delta ≥ 0.02 且 max(R,G,B) ≥ 0.03
```

**第二步：形态学膨胀**

对二值化的印章掩码执行膨胀操作（半径随墨迹跨度自适应，最小 3px），连接断裂的笔画和间隙。

**第三步：连通域分析与多候选（v2.0.0）**

对膨胀后的掩膜做 BFS 4-连通标记，得到每个独立区域的面积与边界框。过滤条件：面积 ≥ max(500, 最大区域面积 × 12%)。按面积降序取前 6 个进入碎片合并池，合并后保留最多 3 个候选。

**第三步半：断裂碎片合并（v2.11.3）**

极淡/深浅不均的盖印会使圆环断裂成多个连通域（如仅一段弧最浓），直接取最大域会把定位框缩到一个角。两轮合并：

1. **贴近合并**：候选包围盒相交或贴近（中心距 ≤ 两框半径和 × 1.05），迭代传递合并
2. **环形合并**：贴近合并后仍 ≥ 3 组时，取组中心最远点对中点为圆心、半距为半径，检验所有组中心距圆心 ∈ [0.5R, 1.15R] 且圆周角度最大空缺 < 140° —— 断裂圆章的碎片散布圆周各处（角度覆盖广），独立多枚印章的碎片聚成簇（角度空缺大），以此区分后合并为单一候选

**第四步：质心、圆度与半径（v2.1.0 强化）**

每个候选独立计算质心与墨迹距离分布。圆度采用**双通道判定**，任一命中即为圆形章：

1. 边界框宽高比 < 18%（原有通道）
2. 距离分布呈环形：p50 > 0.5 × p97 —— 半数墨迹位于 p97 半径的外半段。该通道使**圆环有缺口/断笔的残缺圆章**不会被边界框宽高比误判为异形章

半径估计（v2.11.6）：圆形章取 max(p97 + 2, maxR + 3, bbox 长边/2)——p97 抗毛刺与拖尾，maxR（候选框内墨迹到质心最远距离）在质心因浓墨不均偏移时兜住远端外圈，bbox 半边保证完整包含；异形章取 bbox 长边/2（拖尾毛刺使 maxR 虚大）。提取框 cropHalf = 半径 + 6px 余量（外圈不贴边），maybeRefineRegion 手工色重定位同步。

maybeRefineRegion（v2.11.9 圆覆盖模型；v2.11.10 覆盖输入改为墨迹圆）：每次取色后无条件重建提取框，确保覆盖**全部**手工色墨迹与已有提取内容 —— 原逻辑在检测中心与当前框偏移 ≤25% 时直接跳过（框外手工色墨迹被 crop 映射 clamp 裁掉）、且锚定/并集模型会把偏小的初始框偏差持续传递。覆盖集合 =「全部手工色候选圆（质心+半径）」+「**已提取内容墨迹圆**」（baseContentCircle：从基础画布非透明像素 bbox 反算回原图，被排除色删除的像素不计入；无内容时用当前框圆兜底）

定位框钳制（v2.11.11）：候选半径使框径超过图宽/高时（宽矮扫描件大章），框的宽/高分别钳制到图像尺寸、位置平移入图（bbox 允许非正方形，fitSize = max(w,h)）；候选返回的 cx/cy 保持圆心真值（与钳制框中心解耦）。detectStampCandidates Pass 5 与 maybeRefineRegion 尾部两处同步 —— 框超图会使 drawImage 源区域超界，产生错位/拉伸伪影，按半径从大到小做最小覆盖圆（增量近似：不覆盖时圆心向其移动 (d+r-R)/2、半径同增）。框圆曾是覆盖输入之一，但框是估计值且会被结果再撑大，造成每取一色框大一圈、章在画布中相对缩小偏移（v2.11.10 移除）。完全分离（另一枚印章）时迁移到最大候选自身框；框无变化时不重置基础提取缓存。精确色匹配容差 PICK_COLOR_TOL = 20（v2.11.7 由 26 收紧，点选色与底色 RGB 距离 <26 时会误吞底色）。

印章居中与章外同色内容排除（v2.11.8，detectStampCandidates 内 compStats）：
- 圆形候选的框心用 **bbox 几何中心**而非墨迹质心 —— 质心按面积加权，框内混入章外同色文字/线块时被拉偏导致印章不居中
- **环心拟合**（非圆大组件，comCount > 3000）：章与同色杂物在膨胀层粘连成单组件时质心/bbox 中心都失效。以候选框 bbox 中心为基准，±8×(边长/30) 网格搜索中心，指标 = 峰带 [0.85R, 1.15R] 墨迹占比 × 16 桶角度覆盖率（R = 距离直方图峰值，即环半径）。印章环墨迹距环心等距密集（直方图尖峰）且角度覆盖 360°；杂物距离分散、集中扇区。score ≥ 0.12 且 cov ≥ 0.85 判定环心，半径 = R × 1.15（环中线到外缘）
- 碎片贴近合并增加**完整圆环保护**：任一组自身 circular 时只合并中心距 ≤ 1.3 × 较大半径的组件（环带内），章外贴近同色文字块不再被拽进候选

**第五步：裁剪与归一化**

以候选区域质心为中心裁剪原图区域，缩放到 **1200×1200 像素**（v2.1.0 由 800 提升，配合源图上限 2048 与画布高质量插值，保持参考章墨迹清晰）。使用高质量插值保证缩放质量。

**第六步：透明度精细映射**

对裁剪区域内的每个像素，按命中色带生成精细的透明度值并微调色彩通道（见"墨色多选"）。

**第七步：噪声去除**

通过 BFS（广度优先搜索）连通域分析，找出所有透明度 ≥ 20 的连通区域，移除面积过小的噪点连通域。

### 黑/灰墨（v3.3.0）

扫描件、黑白复印件、传真件里的章没有色相，用「黑/灰」芯片：

```javascript
const INK_BANDS = { ..., gray: { name: '黑/灰章', gray: true } };
const GRAY_DEFAULT_THRESHOLD = 0.62;   // UI 滑杆 30%–88%
const GRAY_SAT_MAX = 0.45;

function isGrayInkPixel(r, g, b, thr) {
  const v = Math.max(r, g, b) / 255;
  const mn = Math.min(r, g, b) / 255;
  const sat = v > 0 ? (v - mn) / v : 0;
  return v >= 0.03 && v <= thr && sat <= GRAY_SAT_MAX;
}
```

- 命中判定排在「排除色」之后（排除优先），灰度分支跳过彩色通道增强，`alpha = 0.18 + 0.82·t`（t 为由 `thr` 归一化的亮度），保留笔画浓淡层次。
- 阈值越高收进的笔画越多；正文与章同为深灰时，调低阈值保留章体结构、牺牲内部浅字。

### 淡色章自动检测（v3.6.7）

`detectFaintInk(canvasEl)` 用积分图求 21×21 局部底色，候选像素需同时满足：

```
bgMean − lum ≥ 6      // 比局部底色明显更深
S ≥ 0.035             // 不是纯灰纸纹
V ∈ [0.45, 0.97]      // 排除极暗（正文）与极亮（纸底）
```

候选按 (h,s,v) 量化分桶，取样本量最大的簇作为「淡墨」代表色。触发条件（`processStamp` 内）：

- 基础提取平均 alpha < 150，或墨量 < 画布面积 0.4% 且候选覆盖率 ≥ 0.4%；
- 命中后自动 `addCustomInk(color, { light: true, auto: true })`，状态栏提示「检测到淡色印章，已自动提取淡墨 #hex」，颜色列表里可随时停用。

红章实拍实测 `customs = 0`，不会误触发。

### 浅色取色的两道门（v3.5.2 / v3.6.8）

点选接近白色/浅粉的颜色时，若只按 RGB 距离匹配，会把整片纸底、正文灰边一起收进来（表现为「晕染」）。因此浅色（`val ≥ 0.72` 或 `light`）额外加两道门：

1. **局部对比**：`lum ≤ bgMean(21×21) − 6`，即比周围底色暗 6 个灰度以上才算墨；
2. **色彩窗口**：`|s − c.sat| ≤ 0.10` 且 `|v − c.val| ≤ 0.12`；当 `s < 0.05` 而所选颜色有彩度（`c.sat ≥ 0.06`）时直接跳过——中性灰属于正文/纸边，不是印章的彩色淡墨。

深色/高彩度取色仍走精确 RGB 匹配（`PICK_COLOR_TOL = 20`），不做窗口推断。

### 剔除容差（v3.5.1）

「⊘ 排除」按 HSV 窗口匹配，而不是精确 RGB，才能删掉渐变的笔画与抗锯齿边：

```
|Δv| ≤ pickTol                     // 默认 0.14，UI 滑杆 4%–40%
彩色还需 |Δs| ≤ 0.32 且 |Δh| ≤ 10°
灰章（s ≤ 0.18）只看亮度，并要求像素 s ≤ 0.45
```

容差越大，连带删除的相近色越多；剔除大面积杂色时调大，只清细碎噪点时调小。

### 去孤立噪点（v3.5.2）

存在手工/自动颜色时，提取完成后对 alpha > 40 的二值掩膜做一遍 3×3 邻居计数，邻居 < 2 的像素置 0。单趟处理，不清除正常细笔画（1200² 画布上约 3–8px 笔画均有 ≥3 个邻居）。

### 颜色增强（v3.6.0 / v3.6.8）

淡色章提出来「发灰、看不清」，增强负责把墨迹压实：

```javascript
// 1) 统计（隔点采样，1200² 约 36 万点）
enhanceStats(canvas) → { meanA, meanS }

// 2) 自动强度：alpha 越低、彩度越低 → 强度越高
f = 1 + max(0, (190 − meanA)) / 78 + max(0, (0.34 − meanS)) × 2.2
f = clamp(f, 1, 2.6)

// 3) 应用（cut = 先切掉的极淡边阈值）
a' = 1 − (1 − a)^(1 + 1.6(f−1))
s' = max(s × (1 + 0.85(f−1)), 0.22 + 0.30(f−1))
v' = v × (1 − 0.13(f−1))
RGB → 向 estimateInkColor 收拢 min(0.7, 0.5(f−1))
```

- `estimateInkColor`：取 alpha ≥ max(40, 0.55×maxA) 的像素做 alpha 加权平均，得到「墨色本色」，淡章的笔画被纸白冲淡，收拢后同一 alpha 也显得更实；`sat < 0.06` 时返回 null，不染色（灰章保持中性）。
- **浅色自定义色时**：`f ≤ 1.25`、`cut = 0.28`。这两条是为「晕染」加的——淡章的基础提取本身就靠低 alpha 像素堆出来，放大 2 倍会把抗锯齿边连成一片；压低倍数并先切掉最淡的一圈，环与字才清晰。
- 红章等常规提取 `cut = 0`，保持原强度曲线。
- 落地位置：`recomputeExtracted()` 末尾、写入 `st.extracted` 之前，强度记录在 `st.enhanceFactor`；`st.baseExtracted` 始终保留未增强结果，供切换参数时重算。

### 圆形遮罩（v3.3.0）

判定为圆形章（`region.circular && region.radius`）时，提取后按 `rOut = radius × scaleRatio × 1.06` 清除圆外 alpha，避免把圆外的正文、表格线带进比对；1.06 是给粗边环留的余量。

### 后端提取流程（stamp_app.py）

后端的提取流程针对 OCR 优化，与前端略有不同：

**墨色通道提取**（extract_ink_mask / extract_ink_channel / _single_ink_mask，v2.1.0 多色并集）：`/ocr` 请求的 `color` 参数经 `parse_ink_colors` 解析为颜色列表（或兼容的 'auto'），红/蓝/紫三色掩膜（红色为宽泛 4 组 HSV 条件，蓝色 H ∈ [90,140]、紫色 H ∈ [125,168]，OpenCV 0-180 标度）取并集；'auto' 时三色分别统计取占优者。

**印章中心检测**（detect_stamp_centers）：使用多种核尺寸（11/15/21/25）进行膨胀和连通域分析，选择面积最大、最接近圆形、最接近图像中心的区域作为印章。v2.3.0 起接收 hues 参数，手工取色同样参与检测。

**多印章检测**（detect_multiple_stamps）：当图像中存在多个印章时，分别检测每个印章的位置和大小，最多返回 3 个。v2.3.0 起接收 hues 参数。

---

## 四、Canvas 变换模型

### 变换链

印章在 Canvas 上的绘制遵循固定的变换链：

```
translate(cx, cy) → rotate(θ) → translate(ox, oy) → scale(sx, sy)
```

其中 (cx, cy) 为画布中心，θ 为旋转角度，(ox, oy) 为偏移量，(sx, sy) 为缩放比例。

这意味着：

1. 旋转围绕画布中心进行（而非印章自身中心）
2. 偏移在旋转之后应用，属于屏幕空间偏移
3. 缩放最后应用

### 屏幕坐标计算

印章的屏幕中心位置由以下公式给出：

```
screenX = cx + ox × cos(θ) - oy × sin(θ)
screenY = cy + ox × sin(θ) + oy × cos(θ)
```

即画布中心加上旋转后的偏移向量。这个公式在手柄位置计算、命中检测、虚线边框绘制中都需要使用。

### 逆变换（屏幕坐标 → 本地坐标）

当用户在画布上点击或拖拽时，需要将屏幕坐标转换回印章的本地坐标系：

```
dx = mouseX - screenCenterX
dy = mouseY - screenCenterY
localX = dx × cos(-θ) - dy × sin(-θ)
localY = dx × sin(-θ) + dy × cos(-θ)
```

逆变换用于命中检测（判断点击是否在手柄上）和拖拽位移计算。

### 角点变形（v2.9.2）

拖动四角手柄时执行**角点变形**：基础仿射变换（旋转/缩放/偏移）保持不动，因此**其余三个角天然固定**，仅记录被拖角相对其基础位置的偏移（局部坐标系，单位为画布像素）：

```
state.ref.corners = { nw: [dx,dy], ne: [dx,dy], sw: [dx,dy], se: [dx,dy] }
```

- 偏移在"旋转后局部坐标系"中定义（与 offset 同系），渲染/命中时按印章旋转角旋转到屏幕方向
- 渲染：`drawImageQuad` 将矩形印章以 48 条垂直条带仿射近似绘制到目标四边形（`stampQuadPoints` 计算基础四角与变形四角）；叠加模式的浅色调经 `getTintedCanvas` 离屏染色缓存（source-atop 只染不透明像素）
- 命中：四角手柄按屏幕距离（≤12px）优先判定，支持变形后的角位置；边柄沿用局部投影
- 差异计算：`renderStampToSize` 同样按四边形透视渲染（偏移按 s/baseSize 换算），缓存 key 含角点偏移
- 选中轮廓/手柄虚线框在变形后绘制为实际四边形路径
- 清除时机：重置（↺/R）、自动对齐（假设仿射）、切换候选、重新提取、移除印章
- `Shift+拖角` 走原等比缩放路径（对角锚定），与角点变形互斥；拖拽模式在 mousedown 时确定

### 变换一致性

以下所有函数必须使用相同的变换模型，否则会出现视觉错位：

- `drawStamp()`：印章绘制
- `getHandleGeom()`：手柄位置计算
- `getHandleAtPoint()`：手柄命中检测
- `renderOverlay()` 中的选中边框
- `renderDiffOverlay()` 中的差异计算
- 虚线边框绘制
- 拖拽位移的逆旋转

---

## 五、自动对齐算法（v2.0.0）

`autoAlignStamps()` 将待验章自动对齐到参考章的当前姿态。整个过程在 256×256 的低分辨率空间中完成，通常耗时 < 300ms。

### 掩膜分析（analyzeStampMask）

参考章带当前变换、待验章重置为基准姿态，分别渲染到 256×256 临时画布后提取墨迹掩膜（alpha > 30），并计算：

- **质心** (cx, cy)：墨迹像素坐标均值
- **RMS 半径**：像素到质心距离的均方根（比极值半径更抗噪）
- **角度直方图**：360 桶，按 atan2 角度统计墨迹像素数并归一化

### 四阶段求解

**阶段 1 — 缩放初值**：k = refRMS / testRMS，截断到 [0.32, 1.98]（对应缩放滑块 30%-200% 范围）。

**阶段 2 — 旋转初值（圆相关）**：对两枚印章的角度直方图做圆相关：

```
score(d) = Σᵢ refHist[i] · testHist[(i+d) mod 360]
```

取最大 score 的 d，则需施加的旋转角 θ = -d（测试章内容位于"旋转 d 度后"的位置，施加 -d 抵消）。环形文字块与五角星等角度特征使该相关峰明确。

**阶段 3 — 偏移初值（质心对齐）**：变换链为 `R(θ)·(o + k·p)`（p 为基准姿态下墨迹点相对画布中心的坐标），令测试章墨迹质心映射到参考章质心：

```
o = R(-θ)·refRel - k·testRel
```

**阶段 4 — IoU 精调**：以测试章墨迹点云（约 1-2 万点）的直接坐标变换代替反复渲染画布，评分函数为掩膜 IoU：

```
IoU(θ, k, o) = match / (refCount + testCount - match)
```

三轮网格搜索依次精调：旋转 ±4°（步长 0.5°）→ 缩放 ±5%（步长 0.5%）→ 平移 ±2px（步长 1px）。每轮以上一轮最优解为中心，θ/k 变化时偏移按质心公式同步重算。

### 圆心 + 半径对齐（v3.6.3，现行主路径）

v3.6.3 起对齐基准从「墨迹质心」换成「拟合外圈圆心与半径」——质心会被星徽、文字分布拉偏，导致中心对上但边缘错位：

1. 两枚章各自 `fitStampEllipse()` 拟合外圈（细节见 §十二），`goodFit()` 要求 `inlierRatio ≥ 0.5`，否则回退质心 + RMS 半径；
2. 缩放初值优先 `k = a_ref / a_test`（长短轴均值），回退 `refRMS / testRMS`；
3. 旋转仍用角度直方图圆相关（环形文字与五角星提供明确峰）；
4. 偏移：把测试章圆心映射到参考章圆心；
5. 精调阶段的点集只取环带 `0.55R–1.18R`，环带外侧（`1.18R` 以外 38% 条带）权重 ×3——章外正文、签名伸出圆外时不再把对齐带偏。

状态栏输出 `自动对齐完成：旋转 −8.5° · 缩放 99.1% · 偏移 (0,0)px · 相似度 81.7% · 圆章校正 0.82→1.00`，其中校正前后轴比便于判断几何是否被拉正。

### 应用到状态

最终解换算回交互参数：offset 从 256 空间按 `stampSize/256` 比例缩放（偏移量以对比画布像素为单位存储），旋转/缩放无量纲直接写入 `state.test`，随后同步滑块并触发渲染与相似度计算。

---

## 六、对比渲染引擎

### 叠加模式 (renderOverlay)

两枚印章绘制在同一位置。参考章叠加 `rgba(30,80,200,0.15)` 蓝色调，待验章叠加 `rgba(220,40,40,0.15)` 红色调。待验章的 globalAlpha 由透明度滑块控制。

选中印章的轮廓以虚线矩形标示，参考章为深灰色，待验章为暗红色。

### 滑块模式 (renderSlider)

使用 Canvas 的 clip() 方法将画布分为左右两个裁剪区域。左侧绘制参考章，右侧绘制待验章。分割线位置由 state.sliderPos（0-1 比例）控制，通过 DOM 元素 slider-handle 显示可拖拽的分割线手柄。

### 并排模式 (renderSideBySide)

两枚印章分别绘制在画布左 1/4 和右 3/4 位置，中间以虚线分隔。尺寸自动适应画布宽度。

### 差异高亮 (computeStampDiff + renderDiffOverlay)

仅在叠加模式下激活。v2.0.0 起计算与绘制分离，并带结果缓存：

**计算（computeStampDiff）**

1. 将两枚印章分别渲染到临时 Canvas（分辨率上限 420×420，偏移量按比例换算，保证与全尺寸渲染几何一致）
2. 获取两个临时 Canvas 的像素数据 (ImageData)
3. 逐像素对比：
   - 两枚印章都有内容（alpha > 30）：计算 RGB 平均差值，差值 ≤ 20 标记为吻合（绿色半透明），差值 > 20 标记为差异（红橙色半透明，透明度与差异程度成正比）
   - 仅一枚有内容：标记为纯红色（结构差异）
4. 计算相似度百分比，生成差异图 Canvas

**缓存**

以（分辨率、双方印章版本号 `_sig`、全部变换参数）拼接为缓存键。参数未变化时直接复用上次的差异图与相似度，透明度调节、窗口缩放等无关操作零重算。

**绘制（renderDiffOverlay）**

将缓存的差异图按当前印章尺寸缩放绘制到对比画布（globalAlpha = 0.55），右上角显示相似度。差异高亮与相似度指标复用同一计算函数。

---

## 七、交互系统

### 拖拽移动

mousedown 时记录起始位置并缓存 `getBoundingClientRect()`（避免拖拽过程中反复触发强制布局）。mousemove 时计算位移增量，使用亚像素累积（dragAccX/dragAccY）保证平滑移动。位移通过逆变换转换到印章本地坐标系后应用到偏移量。

### 缩放手柄

8 个手柄（四角 + 四边），跟随印章旋转角度。拖拽手柄时：

- 角手柄：以对角手柄为锚点，根据鼠标位移计算缩放比
- 边手柄：仅调整对应轴的缩放（e/w → scaleX，n/s → scaleY）

缩放时锚点通过偏移校正保持屏幕位置不变。

### 滚轮缩放

在对比区域内纵向滚动鼠标滚轮进行缩放（v2.11.4 起步进 0.2%/步，累积制：滚轮增量累积达到阈值 15 触发一步，触控板可逐 0.2% 微调）。缩放值累积后四舍五入到 0.1%避免浮点尾差；滑块步进 0.1、键盘 ↑↓ 每次 0.1%。悬停缩放滑块上滚轮同样累积触发 0.2%/步。

### 旋转调节

三种方式：旋转滑块拖拽、滑块悬停滚轮、键盘左右方向键。旋转角度范围 -180° 到 180°，步进 0.1°。

### 光标自适应

手柄光标方向根据印章旋转角度动态计算。基础角度（n=90°, e=0° 等）加上印章旋转角度后归一化，映射到对应的 resize 光标类型。

---

## 八、OCR 后端

### 服务架构（v2.0.0）

后端使用 `ThreadingHTTPServer` 多线程 HTTP 服务：OCR 识别（可达数十秒）期间，页面加载与健康检查请求不再被阻塞。PaddleOCR 实例非线程安全，推理阶段通过全局锁 `_ocr_lock` 串行化。

**依赖容错**：启动时尝试加载 PaddleOCR，ImportError 或加载失败不终止服务——`ocr` 置为 None，/health 返回 `ocr: false`，/ocr 返回带安装指引的结构化错误。对比功能不依赖后端，因此页面照常可用。

**前端页面按请求读取**：每次 GET / 从磁盘重读 stamp-compare.html，前端文件更新后浏览器刷新即生效，无需重启服务。

**PaddleOCR 版本兼容**（run_ocr_on_image）：3.x 走 `ocr.predict(path)`（解析 `rec_texts` / `rec_scores`），2.x 走 `ocr.ocr(path, cls=True)`（解析 `line[1] = (text, score)`）。通过 `hasattr(ocr, 'predict')` 自动分派。

### PaddleOCR 集成

OCR 实例在启动时全局加载一次：

```python
from paddleocr import PaddleOCR
ocr = PaddleOCR(lang='ch')
```

### 印章 OCR 管线 (process_stamp_ocr)

针对印章的弧形文字特点，采用极坐标展开策略：

**第一步：墨色通道提取**

按请求的 color 参数（auto/red/blue）与 hues 自选色相提取对应墨色印章像素；红色使用宽泛的 HSV 阈值（4 组掩码），蓝色使用 OpenCV H ∈ [90,140] 的两组掩码，自选色相使用色相环 ±18° 掩膜（含回绕）。v2.3.0 起 stamp_ocr_pipeline 的印章中心/多印章检测同样按 color+hues 提取掩膜，手工取色的绿色等非预设颜色印章也能被正确定位与 OCR。

**第二步：极坐标展开**

使用 `cv2.remap` 向量化实现极坐标到直角坐标的变换，将弧形文字展开为水平排列。默认参数：

```
内径系数: 0.50    外径系数: 0.90
输出宽度: 1600    输出高度: 200
角度范围: π+0.15 ~ 2π-0.15（上弧）
```

**第三步：多参数组合优化**

使用 4 组不同参数进行展开和识别，取最优结果：

| 参数组 | 内径 | 外径 | 宽度 | 高度 | 倍数 | 形态学 |
|--------|------|------|------|------|------|--------|
| 1 | 0.50 | 0.90 | 1600 | 200 | 3x | 否 |
| 2 | 0.45 | 0.88 | 1800 | 220 | 3x | 否 |
| 3 | 0.40 | 0.95 | 1800 | 220 | 3x | 否 |
| 4 | 0.50 | 0.90 | 1600 | 200 | 3x | 是 |

**第四步：二值化**

每组参数配合 2 种二值化方法（Otsu 和自适应阈值），共产生最多 8 次 OCR 调用。

**第五步：提前终止**

当某次结果的置信度之和超过 4.0 时，提前终止后续参数组合的尝试，减少处理时间。典型处理时间约 15-27 秒/图。

### 引擎优先级与环形文字管线（v3.2.0 起）

`load_ocr_model()` 的加载顺序：**RapidOCR（onnxruntime）→ PaddleOCR**。RapidOCR 模型随安装包分发、无需系统 Python 依赖；`--no-ocr` 可跳过加载（仅对比功能）。缺模型时 `/health` 返回 `ocr:false`，`/ocr` 返回结构化错误而不是 500。

`stamp_ocr_pipeline(image, ...)` 针对环形文字的完整流程：

1. `estimate_stamp_circle`：Hough 圆 + 最小二乘精修；失败时用连通域质心与最远距离兜底（v3.2.0 起全 360° 展开，取代旧的 165° 弧段）；
2. `unwrap_stamp_ring`：以圆心为中心把 `0.55R–0.92R` 环带按极坐标展开成矩形条带（弧度→像素列），支持双极性（亮字/暗字）与 0°/180° 两个方向；
3. `_split_text_bands`：按水平投影切出行；主循环对条带做 **4 个变体**（自适应/Otsu 二值 × 正/负极性，其中两路再转 180°），每个变体整条带识别一次，`_best_ring_text` 评分；
   - 早停：至少采样完两个极性后才允许在 `score ≥ 0.90` 时停止；
   - 残差兜底：最优分 < 0.50 时再走 `_ocr_projected_lines`（按行切开分别识别）；
   - 低彩度兜底：`_red_pixel_fraction < 0.02` 且最优分 < 0.60 时，追加 `_whole_image_color_fallback`（整图灰度兜底）；
4. `_merge_ordered_fragments` + `normalize_stamp_text`：去重、拼接、归一化（全半角、括号、空格）；
5. `_split_company_and_type`：按后缀（有限公司/委员会/分公司…）拆公司名与类型；
6. `rank_library_candidates`：与印章库做相似度排序，返回 Top-3。

**库匹配安全阀（v3.3.1）**：`top1 ≥ 0.72` 且 `margin ≥ 0.04` 才用库名覆盖识别结果；`0.55–0.72` 之间只给建议、保留原始文本并置 `needs_review=true`；低于 0.55 不覆盖。

**实测（20 张样本，v3.3.1）**：库内可用结论 10/20、Top-3 召回 19/20、P50 4.35s；禁用印章库后裸识别 0/20。说明库校验只是兜底，本地 RapidOCR 对环形印章的直接识别能力有限——这也是引入视觉模型（§九、§十二引擎管理）的原因。

### 结果解析

OCR 返回的文字经过以下处理：

1. 合并所有印章的识别文本
2. 去除中文字符间的多余空格（PaddleOCR 常见问题）
3. 通过正则表达式解析公司名称（上弧）、编码（下弧数字/字母串）和类型（"专用章""公章"等关键词）

---

## 九、AI 文字提取后端（v2.8.0）

### 设计

AI 提取通过本地后端**代理**调用 OpenAI 兼容接口（`/ai-ocr`、`/ai/test`），避免浏览器 CORS 限制，且 API Key 只存在于前端 localStorage（经代理转发，不落盘、不写日志）。

服务商预设（`AI_PROVIDERS`，与前端 `AI_PRESETS` 同源）：

| 预设 | Base URL | 默认模型 | 模式 |
|------|----------|----------|------|
| deepseek | `https://api.deepseek.com/v1` | `deepseek-chat` | parse（文本解析） |
| zhipu | `https://open.bigmodel.cn/api/paas/v4` | `glm-4v-flash` | vision（视觉识别） |
| ark-doubao | `https://ark.cn-beijing.volces.com/api/v3` | `doubao-seed-2-1-pro-260915` | vision（OpenAI 兼容 chat/completions） |
| custom | 用户填写 | 用户填写 | 用户选择 |

### 两种提取模式

**视觉模式（vision）**：前端将印章原图 base64 随请求发送，后端组装 OpenAI 多模态消息（`content: [{type:'text'}, {type:'image_url'}]`）直接调用 `chat/completions`。无需 PaddleOCR。提示词要求模型返回 JSON：`{company, code, type, raw}`。

**文本解析模式（parse）**：后端先运行本地 PaddleOCR 管线（`stamp_ocr_pipeline`，含墨色选择），把识别出的文本片段拼入提示词，由纯文本模型（如 deepseek-chat）清洗、纠错、结构化。PaddleOCR 不可用时明确报错并提示改用视觉模式。

**火山方舟豆包视觉（ark-doubao，v3.1.0）**：复用现有视觉模式，走 `POST {base_url}/chat/completions`（OpenAI 兼容），模型 `doubao-seed-2-1-pro-260915`。

- 豆包默认开启深度思考：实测一次图片识别在 `max_tokens=1500` 下思考耗尽预算、响应 4 分钟以上且输出退化；因此 `call_ai_chat` 对 `provider == 'ark-doubao'` 显式追加 `thinking: {"type": "disabled"}`，实测约 3 秒返回
- 准确率：清晰/方正印章较好；严重旋转、低对比、叠字的圆形公章可能误读，建议更换更高能力的视觉模型
- 定位裁剪由 `/ai/stamp` 视觉定位完成，要求配置为 vision 模式

### 实现要点

- `call_ai_chat`：urllib 直连，HTTPError（含 401/429 及响应体细节）与 URLError 均转为带原因的中文错误，超时 90s（测试连接 30s）
- `parse_ai_json`：从回复中稳健抽取 JSON（容忍 ```json 代码块标记与前后缀文字）；解析失败时整段回复降级为 `raw` 返回
- `validate_ai_config`：校验地址格式、模型名、密钥非空
- `/ai/test`：`max_tokens=8` 的极小请求验证连通性，返回模型应答片段
- 墨色选择（color/hues/excludes）在 parse 模式中透传给本地 OCR 管线，视觉模式由模型自行看图

### 前端集成（v2.9.0 拆分）

- 配置存储于 `localStorage['stampAIConfig_v1']`（provider/base_url/model/api_key/mode）
- **识别方式显式切换**：`state.recMode`（`'local'` 默认 | `'ai'`）由**页面左上角 header 内**的「识别」按钮组控制；点击 AI 且 `aiCfgReady()`（配置齐全）失败时提示先配置，不切换
- **每枚印章独立 AI 按钮**：`updateAIButtonVisibility()` 在 `recMode === 'ai'` 时显示 `#ai-ocr-ref` / `#ai-ocr-test` 按钮，点击调用 `runStampOCR(target)` 单章识别
- **加载后自动识别**：`autoRecognitionReady()`——本地模式要求 `_ocrAvailable`（PaddleOCR 就绪），AI 模式（视觉）要求 AI 配置就绪；`processStamp` 依此在印章加载后自动触发对应识别通道
- 「🤖 AI」按钮高亮表示配置就绪；模型名输入时按关键词（`4v`/`4o`/`vision`/`gemini` 等）自动推荐视觉/文本模式
- `runStampOCR` 在 `state.recMode === 'ai' && aiCfgReady()` 时请求 `/ai-ocr`，否则走 `/ocr`；AI 模式配置不完整时明确提示「AI 识别未配置」，不静默回退；`aiCfgReady` 要求地址、模型与 API Key 齐全
- PaddleOCR 缺失（`/health` 返回 `ocr:false`）时，仅当识别方式为 AI 且配置视觉模式时识别按钮保持可用，状态条提示「PaddleOCR 未加载 — AI 视觉提取可用」
- **识别方式持久化（v3.6.1）**：`state.recMode` 读写 `localStorage['stamp.recMode']`，切换即存、启动即读；「清空」不再重置识别方式，只同步按钮高亮与两章的 inline 识别面板
- **视觉提示词（v3.2.1）**：四步指令——① 判断朝向，必要时转 180°（返回 `rot`）；② 裁出印章区域；③ 沿圆周 12 点顺时针读字，读不全按 0/90/180/270 重试；④ 读下缘编码与类型
- **`detail` 参数**：视觉请求的 `image_url` 带 `detail`（`high` / `xhigh`，`_image_detail(cfg)` 决定，默认 high），避免服务端默认压分辨率导致小字丢失
- **已下线**：`ark-agent`（火山方舟智能体，异步会话、单枚数分钟）在 v3.2.0 移除；旧配置（provider 不在 `AI_PRESETS` 中）自动迁移到 `ark-doubao` 并保留 api_key。`/ai-ocr` 的 `job_id` 轮询与 `/ai-ocr/status` 一并删除，`/ai/ocr` 别名（曾允许）已撤销——未知 provider 返回 400 而不是 500

---

## 十、性能优化

### 渲染优化

**requestAnimationFrame 节流**：多次连续状态变更（如拖拽过程中的连续 mousemove）合并为一帧一次渲染，避免冗余绘制。

**getBoundingClientRect 缓存**：拖拽开始时缓存画布的边界矩形，拖拽过程中复用，避免每帧触发强制布局（forced layout）。

**亚像素累积**：拖拽使用浮点数累积位移（dragAccX/dragAccY），仅在渲染时取整，避免 Math.round 导致的抖动和精度丢失。

**交互期间跳过装饰性绘制**：拖拽或缩放过程中跳过网格背景绘制和差异高亮计算，大幅减少 Canvas 绘制开销。

**DOM 滑块同步延迟**：拖拽手柄缩放时，滑块 DOM 元素的值更新延迟到 rAF 回调中统一刷新（通过 _pendingSliderSync 标志），避免频繁 DOM 操作。

**差异计算缓存（v2.0.0）**：差异图与相似度以变换参数 + 印章版本号为缓存键，参数不变时跳过逐像素计算；分辨率上限 420×420，计算量比全尺寸（可达 ~490×490）降低约 25% 以上且视觉无感。

**自动对齐低分辨率求解（v2.0.0）**：对齐计算在 256×256 空间完成，精调阶段用点云坐标变换代替画布重渲染，单次对齐 < 300ms。

### 内存优化

**印章提取输出尺寸**：1200×1200 像素（v2.1.0，原 800）——在保留笔画级细节（供放大镜 4 倍检视与人工比对）和控制内存之间取得平衡。

**输入图片最大尺寸**：2048 像素（v2.1.0，原 1600），避免超大图片导致内存和性能问题的同时保留更多源图细节。

**差异高亮临时 Canvas**：仅在需要时创建，计算完成后由垃圾回收器释放；计算分辨率上限 420×420（v2.1.0 起与提取分辨率解耦）。

**新增阶段的复杂度（v3.3–v3.6）**：颜色增强与 3×3 去噪都是对 1200² 画布的单趟 O(N) 遍历（增强按需跳过 cut 以下的像素）；圆章校正每轮做 720 条射线取点 + 4 轮拟合剔除 + 一次仿射重采样，最多 3 轮，且仅在开关打开、拟合通过可信度闸门时执行；原图查看器分栏绘制按 pane 尺寸缩放，不复制原图位图。

---

## 十一、启动器实现

### macOS / Linux 启动器 (launcher.sh)

启动器按以下顺序执行：

1. 定位脚本所在目录（解析符号链接）
2. 查找 Python 3：遍历 `python3.13` ~ `python3.8`、`python3`、`/usr/bin/python3`、`python`，**优先选择 3.8-3.13**（PaddlePaddle 兼容性最好；同一台机器同时装有 3.14 与 3.9 时自动跳过 3.14、选用 3.9），找不到则提供自动安装选项（brew / apt-get）
3. **运行环境选择（PEP 668 兼容）**：先扫描全部候选，**优先使用已装好依赖（cv2+paddle+paddleocr）的 Python**（免安装直用）；否则创建项目内 `.venv`（标准 `-m venv` → `uv venv` → `--without-pip` + get-pip 逐级兜底，覆盖 Homebrew / Ubuntu / uv 管理的 Python）；全部失败时回退系统 Python，pip 安装自动加 `--break-system-packages` 重试，镜像源失败自动回退官方 PyPI
4. 检查 pip 是否可用
5. 检查 PaddleOCR、PaddlePaddle、OpenCV 是否已安装，缺失则使用百度镜像源安装（PaddlePaddle 安装失败且提示版本过新时，给出安装 Python 3.9-3.12 的指引）
6. 检查核心文件（stamp_app.py、stamp-compare.html）是否存在
7. 检查端口 8765 是否被占用，提供终止旧进程选项
8. 后台启动 Python 服务
9. 轮询健康检查端点（最多等待 60 秒）
10. 打开浏览器
11. 设置 SIGINT/SIGTERM/EXIT 信号处理，确保退出时清理服务进程（SIGTERM 等待 5 秒后 SIGKILL 兜底——PaddlePaddle 运行时会吞掉 SIGTERM）

**命令行参数**（v2.3.0）：`--port <端口>` 覆盖默认端口；`--no-install` 跳过依赖检查与安装（依赖已就绪时启动更快）；`--no-browser` 不自动打开浏览器。健康检查在 curl 缺失时自动回退为 Python urllib 实现。

**macOS 双击启动器**（v2.3.0）：新增 `启动印章对比工具.command`——.command 扩展名由系统固定用「终端」打开，彻底解决 .sh 双击被文本编辑器打开的问题；其逻辑为 `cd` 到自身目录后 `exec ./launcher.sh`。后端 `stamp_app.py` 另支持 `--no-ocr` 跳过 PaddleOCR 模型加载（仅使用对比/导出功能时显著加快启动）。

### Windows 启动器 (launcher.bat)

Windows 版本使用批处理脚本实现类似功能，额外包含：

- 自动下载并静默安装 Python 3.12（使用 certutil 或 PowerShell）
- 安装后刷新 PATH 环境变量（从注册表读取）
- 使用 goto 循环替代 for 循环（避免批处理嵌套括号的展开问题）
- 使用 Python urllib 进行健康检查（旧版 Windows 可能没有 curl）

**重要注意事项**：launcher.bat 必须使用 CRLF 行尾格式。如果文件被转换为 LF 格式，Windows cmd 将无法正确解析，表现为闪退。

---

BI4IWN · 李劲松

---

## 十二、圆章校正（v3.6.3）

### 目标与启用

拍照/扫描常把圆形印章压成椭圆（透视或纸张变形）。校正把外圈椭圆"拉回"正圆并统一尺寸，使自动对齐、差异高亮与相似度都在同一尺度上比较。

- 入口：`dewarpExtracted(target, canvasEl)`，在 `recomputeExtracted()` 算出 `cur` 后、写 `st.extracted` 之前执行；
- 保留原始：`st.baseExtracted` 未校正，`st.extracted` 为校正结果，`st.warpInfo = {ratio, a, b, rounds, chain}`；
- 开关：`localStorage['stamp.dewarp'] !== 'off'`（「⋯ 更多 → 圆章校正」，默认开）；
- 范围：异形章（椭圆以外的形状）与"整块正文被误当章"的情形一律不处理。

### 边界取点

`boundaryPointsOfMask(canvasEl, maxS=320)`：对 alpha>40 的掩膜降采样到 ≤320 边长，从质心向外发 720 条射线，取每条射线最外侧的命中点；再过滤 `r ≥ 0.86 × median(r)`，去掉内圈文字与零星噪点。

### 椭圆拟合

`ellipseFromPoints(pts)` 用**二阶矩**而不是二次曲线代数拟合：由点集协方差求 `a = √(2λ1)`、`b = √(2λ2)`，长轴方向由两轴方差比较确定 `θ`（早期版本用代数拟合会退化——`ratio` 恒为 1 且半轴虚大，已废弃）。

`fitStampEllipse()` 做 4 轮迭代：每轮按归一化残差 ≤0.16 剔除离群点后重拟合，输出 `inlierRatio`、`pointCount`、`coverage`（72 个 5° 分箱的占用比例）。

> 射线取最外点会系统性把轴比拉向 1（真值 0.76 会被估成 0.872）。`rayRatioHat(ρ)` 对解析椭圆按 N=240 角度采样求出"估计值"，`trueRatioFromRayRatio()` 二分反查真值，修正后 0.759。

### 变换与尺寸归一

点映射：`p' = C + R(−θ)·diag(1, a/b)·R(θ)·(p − C)`。

画布 API 的变换按"点被应用"的顺序累乘，所以调用顺序必须是：

```js
ctx.rotate(θ); ctx.scale(1, a / b); ctx.rotate(-θ);
```

写反（先 `rotate(-θ)`）会把椭圆越转越扁——`1×0.76 @22°` 校正后轴比 0.633 就是这条 bug。

尺寸归一：`rTarget = min(W, H) × 0.45`、`u = rTarget / ell.a`，外圈半径固定为画布 45%，两枚章因此同尺度（对齐时缩放比取 1）。

### 迭代

单轮仿射不足以消化粗边环与轻微透视（0.899 → 0.943），因此每轮重拟合再拉，直到 `ratio ≥ 0.988` 或满 3 轮（`first.ratio > 0.995` 时只做 1 轮）。实测 0.899 → 0.943 → 0.995。

### 可信度闸门

| 闸门 | 阈值 | 作用 |
|---|---|---|
| 内点率 | ≥0.6 | 拟合必须贴合一整圈边界 |
| 采样点 | ≥80 | 太少的点不可信 |
| 角度覆盖 | ≥0.6 | 只有局部弧（正文、表格线）时不动 |
| 轴比 | ≥0.62 | 极端细长的"椭圆"不是印章 |
| 半长轴 | 画布 12%–65% | 排除整张纸/小图标 |
| 短轴/长轴 | ≥0.62 | 同上 |
| 墨迹填充率 | ≤0.55 | 整块内容被当成章时不动 |
| 收益校验 | 新 ratio ≥ max(0.9, 旧+0.02) | 拉不圆就回退原图 |

淡色章手动取色后的掩膜常是"正文 + 一点点章"（实测 ratio 0.15、coverage 0.11），这类输入会被前三条直接拦下，保持原样不斜切。

### 与对齐、判定的关系

自动对齐、差异高亮、相似度全部基于校正后的 `st.extracted`；状态栏显示形如「圆章校正 0.82→1.00」，便于确认是否生效。

---

## 十三、原图查看器与取色放大镜（v3.5.0–v3.6.7）

### 结构

`#origin-viewer`（`.ov-panel`）分四区：

1. 头部：`查看 / 框选区域 / 点选取色` 三标签 + 目标切换（参考章 / 待验章）+ 缩放控制（`−` `100%` `+` `适应`）+ 关闭；
2. 主体 `#ov-stage`：flex 布局，`#ov-pane-a`（原图 canvas）与 `#ov-pane-b`（提取结果 canvas，仅框选/取色模式显示）；
3. 底部：颜色列表（chip：色块 + 提取/剔除 + 启用/停用 + 删除）、剔除容差滑杆、`提取 / 排除 / 完成`；
4. 浮层：取色放大镜 `#ov-loupe`。

### 状态机与坐标映射

`OV = {target, mode: 'browse' | 'crop' | 'pick', zoom, panX, panY, crop, drag}`。

坐标换算：`ovPaneRect()` 取 pane 的屏幕矩形，`ovImageToScreen()` / `ovScreenToImage()` 按 `zoom` 与 pane 尺寸互转，绘制按设备像素比缩放，保证 1:1 取色。

### 框选

- 空白处 `mousedown` 新建框，框内拖动整体移动，8 个手柄缩放（命中容差 9px）；
- `←→↑↓` 微调 1px（`⇧` 10px）、`Enter` 应用、`Esc` 退出；
- `ovApplyCrop()` 写 `st.bbox`、`_regionLocked = true`、`_baseKey = null` 后重算，**查看器不关闭**，可反复调整；
- 「重置框」清除 `_regionLocked`，恢复自动定位。

### 取色与剔除

- `ovPickAt()`：7×7 邻域内按暗度与彩度挑"最像墨"的像素，调 `addCustomInk()`；
- `ovPickFromResult()`：在右栏按 `s = min((W−24)/w, (H−24)/h)` 反算到 `st.extracted`，3×3 邻域取最深色，以 `exclude = true` 加入；
- 顶部切换「提取 / 排除」决定新增颜色的语义；`Esc` 退出取色模式；
- 颜色列表点 chip 等于切换启用状态，`✕` 删除，任一改动都会重算并刷新右栏读数（`墨 X px · ±Δ`）。

### 放大镜（v3.6.7）

`OV_LOUPE = {size: 132, zoom: 5.5}`：132×132 canvas、`image-rendering: pixelated`、白色描边与十字准线；跟随光标偏移 18px，靠近视口边缘自动翻转；实时显示该点 hex 与「点击提取/剔除该颜色」；`mouseleave`、切换模式、完成取色后隐藏。左栏读原图像素，右栏读提取结果像素。

### 剔除容差

`#ov-tol`（4%–40%，默认 14%，步进 1）→ `state.ink[target].pickTol`，直接进排除的 HSV 窗口（见 §三「剔除容差」）。容差越大，一次点击去掉的同色像素越多。

---

## 十四、判定与检查器（v3.6.2）

### 判定

相似度取自 `computeStampDiff().matchPct`（墨迹重合度，见 §六），阈值：

| 区间 | 结论 |
|---|---|
| ≥ 90% | 一致 |
| 75%–90% | 可疑 |
| < 75% | 不一致 |

`v0` 只用墨迹重合度；文字一致性、几何结构、编码三项在检查器里显示为「—」，等 T3-JS 接入后替换占位。

### 检查器三卡

1. **判定**：等级徽标、百分比、进度条，底部一行说明「v0 判定仅基于墨迹重合度」；
2. **识别**：两章各自的 company / 类型 / 编码、来源徽标（库内匹配 · 本地 OCR · AI）、置信度；两章名称不同时给出「两章名称不一致」提示条；
3. **候选**：Top-3 库候选与得分，点「采用」用候选名称覆盖当前显示名（只改显示，不动图像）。

### 数据流

`state[target].ocrResult = {company, code, type, confidence, matchSource, needsReview, candidates, rawText, engine, elapsedMs}` 由 `/ocr` 或 `/ai-ocr` 填充；`matchSource` 取 `library` / `ocr` / `ai`，用于检查器徽标与「未识别 / 未匹配」文案。本地识别在 `needs_review` 为 true 时把结论降级为提示，而不是直接给名称。

### 布局

≥1200px 常驻右栏 300px；≤1199px 收成抽屉，`⌘I` 或页头「判定」按钮展开；展开状态与画布尺寸联动，保证一屏不出现纵向滚动。

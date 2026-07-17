
一款面向法务合规与文档核验场景的本地印章对比工具，支持将单据印章与合同印章进行可视化对比和文字识别。

开发者：李劲松 (BI4IWN)

## 功能特性

- **智能印章提取**：自动检测红色印章区域，提取边缘并归一化为标准圆形，支持异形章识别
- **三种对比模式**：叠加模式（双色叠印）、滑块模式（左右分割对比）、并排模式（等大并列展示）
- **差异高亮**：像素级对比两枚印章，绿色表示吻合、红色表示偏差，实时计算相似度百分比
- **交互微调**：旋转、缩放（等比/水平/垂直独立调节）、偏移、拖拽，精确对齐两枚印章
- **放大镜**：4 倍局部放大，方便检查细节差异
- **OCR 文字识别**：集成 PaddleOCR 后端，自动识别印章公司名称、编码和类型
- **一键导出**：将当前对比画面导出为 PNG 图片
- **纯本地运行**：所有数据处理均在本地完成，不上传任何信息

## 项目结构

```
stamp-toolkit/
├── README.md                  # 本文件
├── LICENSE
├── docs/
│   ├── operation-manual.md    # 操作手册（用户使用指南）
│   └── technical-doc.md       # 技术文档（架构与实现细节）
└── src/
    ├── stamp-compare.html     # 前端页面（~2700 行单文件应用）
    ├── stamp_app.py           # Python 后端（HTTP 服务 + PaddleOCR API）
    ├── launcher.sh            # macOS / Linux 一键启动器
    └── launcher.bat           # Windows 一键启动器
```

## 快速开始

### 方式一：一键启动（推荐）

**macOS / Linux：**

```bash
chmod +x launcher.sh
./launcher.sh
```

**Windows：**

双击 `launcher.bat`

启动器会自动完成以下工作：

1. 检测 Python 3 环境，缺失则提示安装
2. 检测并安装 PaddlePaddle、PaddleOCR、OpenCV 依赖
3. 启动本地 HTTP 服务（端口 8765）
4. 自动打开浏览器访问工具页面

### 方式二：手动启动

如果只需要印章对比功能（不需要 OCR），可以直接用浏览器打开 `stamp-compare.html`：

```bash
# macOS
open stamp-compare.html

# Linux
xdg-open stamp-compare.html

# Windows
start stamp-compare.html
```

### 方式三：命令行启动完整服务

```bash
# 安装依赖
pip install paddlepaddle paddleocr opencv-python

# 启动服务
python3 stamp_app.py              # 默认端口 8765
python3 stamp_app.py --port 9000  # 自定义端口
```

## 环境要求

- **操作系统**：macOS、Windows、Linux
- **浏览器**：Chrome、Safari、Firefox、Edge 等现代浏览器
- **Python**：3.8+（仅 OCR 功能需要）
- **Python 依赖**（仅 OCR 功能需要）：
  - PaddlePaddle（~700MB）
  - PaddleOCR
  - opencv-python

> 注意：即使不安装 Python 和 OCR 依赖，印章对比、差异高亮、导出等核心功能仍可正常使用。

## 使用流程

1. **加载图片**：通过粘贴（Ctrl+V）、拖拽或点击上传，分别加载参考章和待验章图片
2. **自动提取**：工具自动检测红色印章区域并提取归一化
3. **选择对比模式**：叠加 / 滑块 / 并排，快捷键 1/2/3 切换
4. **微调对齐**：使用工具栏滑块或鼠标交互调整旋转、缩放、偏移
5. **差异分析**：开启差异高亮查看像素级对比结果和相似度
6. **OCR 识别**（可选）：点击"全部识别"提取印章文字信息
7. **导出结果**：点击"导出"保存对比画面

详细操作说明请参阅 [操作手册](docs/operation-manual.md)。

## 文档

- [操作手册](docs/operation-manual.md) — 完整的功能使用说明和操作指南
- [技术文档](docs/technical-doc.md) — 系统架构、算法原理、变换模型等技术细节

## 许可

本项目仅供学习和内部使用。

---

BI4IWN · 李劲松

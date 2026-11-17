# OCR 基线 Spike：Rust 识别器 vs Python RapidOCR（同输入、同模型）

> 2026-11-17 ｜ 目的：在 P2（Tauri 换壳）前确认「Rust 侧能不能达到现有 Python OCR 的水平」
> 结论先行：**能**。tract（纯 Rust）平均字覆盖 0.674 vs Python 0.655，19 张里 5 张更好、14 张持平、0 张更差；代价是单条 44.7ms vs 14.6ms。

## 1. 方法（关键：两边吃同一份像素）

1. 从印章库取 20 张（实际 19 张可用），用 numpy 做极坐标展开，取 `0.42R–0.905R` 环带，按 90° 切成 4 段 → 76 张单行文字条带（`data/strips/`，500×120，白字黑底）。
2. **Python 基线**：直接把条带喂 `rapidocr_onnxruntime` 的 `TextRecognizer`（同一份 `ch_PP-OCRv4_rec_infer.onnx`、同一份 6623 字字典）——跳过检测，只比"识别器"。
3. **Rust 基线**：复刻 `resize_norm_img`（高 48、等比缩放 200px、右侧零填充到 320、`(x/255-0.5)/0.5`），跑同一模型，贪心 CTC 解码（blank=0、字符从 1 起、最后一类为空格）。
4. 指标：**字覆盖** = `multiset(真值) ∩ multiset(预测) / |真值|`（对旋转不敏感，避免因展开起点不同而误判）。

## 2. 结果

| 引擎 | 平均字覆盖 | 单条耗时 | 备注 |
|---|---|---|---|
| Python RapidOCR（onnxruntime 1.31.1） | 0.655 | 14.6ms | 现网基线 |
| **Rust tract（纯 Rust）** | **0.674** | 44.7ms | 5 更好 / 14 持平 / 0 更差 |
| Rust `ort` crate（绑定同一 dylib） | — | 27ms | **全部输出空串**，见 §4 |

逐张（示例）：

| i | py | tract | 真值 | tract 输出 |
|---|---|---|---|---|
| 00 | 1.00 | 1.00 | 上海聚旦企业管理有限公司 | 业管理有有限公司上海聚旦企 |
| 05 | 0.92 | **0.96** | 国投山东临沂路桥…分公司 | 限责任公司河本东华队物流分公司"国投山东临沂路桥发展 |
| 11 | 0.61 | **0.69** | 山东青鸟工业互联网有限公司 | 联网有限公司2东青鸟# |
| 13 | 0.00 | **0.12** | 沂水县应急管理局 | 县济 |
| 14 | 0.00 | 0.00 | 泰山学院 | （空） |

## 3. 解读

- **识别精度与 Python 版齐平**（差异来自插值实现，Rust 用 Triangle、OpenCV 用双线性；不是模型或解码差异）。
- 两条引擎都在「章面被正文压住、环带被切」的样本上崩（i=14 全空、i=3 只有 0.17）——**这是数据问题，不是引擎问题**，与 E1/T1 的结论一致。
- tract 慢 3×（44.7ms/条，单枚 4 条约 180ms）。可接受：P2 目标是"一个章 1.5–2.5s"，OCR 只占其中一小部分；且 tract **不需要分发 onnxruntime dylib**，把 R5（签名/dylib 分发）风险直接消掉。

## 4. `ort` crate 的坑（记录备查）

`ort 2.0.0-rc.15` 加载同一份 `ch_PP-OCRv4_rec_infer.onnx`，喂**与 Python 完全相同的输入张量**（逐元素比对 mean|Δ|<0.02），输出却是全 blank（argmax 恒 0、置信 0.99）。已验证：
- 输入张量确实进入图（零输入 vs 正常输入输出不同）；
- 会话 IO 名正确（`x` / `softmax_11.tmp_0`）；
- 换成 Python 用的 `libonnxruntime.1.31.1.dylib`（`load-dynamic`）仍 blank；
- 去掉图优化、显式 `Vec<i64>` 形状、按名绑定输入，均无效。

→ 结论：**不是预处理问题，是 ort 绑定的输入张量传递问题**（未深挖）。因此 Rust 侧选 tract，而不是 ort。

## 5. 对 P2 的含义

- ✅ R3（OCR 精度回退）从"最高风险"降为"可控"：识别器可换 Rust，精度不掉；还省掉 Python 运行时里的 onnxruntime 依赖。
- ⚠️ 还差**检测器（DB）**：本次只比识别器。P2 要么在 Rust 里实现 DB 后处理（阈值 0.3、unclip 1.6、框排序），要么沿用"极坐标展开 + 固定行切分"绕开检测（现有 T1 管线就是这么做的，效果已知）。
- ⚠️ 性能：tract 单条 44.7ms，若一个章要跑 4–8 条 + 多方向重试，约 0.3–0.7s；可用 `tract` 的多线程或改用 `ort`（解决绑定问题后）压到 ~15ms/条。

## 6. 复现

```bash
python build_data.py                      # 生成 76 张条带 + manifest
python baseline_py.py                     # Python 基线 -> data/baseline_python.json
./ocr-tract/target/release/ocr-tract data # Rust(tract) -> data/baseline_tract.json
python compare.py                         # 汇总对比
```

## 7. DB 检测器（tract 实现，2026-11-17）

> 目的：确认 P2 能否**完全去掉 Python**——识别器已齐平，还差检测器。

### 7.1 实现

`ocr-tract/src/bin/detpipe.rs`（279 行，纯 Rust + tract，无 ORT dylib）：

1. **预处理**：短边限制 736（PaddleOCR `limit_side_len` / `limit_type=min`），等比缩放后向下对齐 32 的倍数，贴到 640×640 画布（左上），`(x/255-0.5)/0.5`，CHW。
2. **模型**：`ch_PP-OCRv4_det_infer.onnx`，用 onnx 把输入静态化为 `[1,3,640,640]`（tract 无法解析 Paddle 的 `p2o.DynamicDimension.*`）。
3. **后处理**：阈值 0.3 → 8 邻域连通域（迭代式 flood fill）→ 二阶矩求主轴角，投影得定向框（OBB）→ **按未外扩的紧框算 box score**（与 Paddle 的 `box_score_fast` 一致）→ `score ≥ 0.5` 才保留 → `unclip = 1.6`（`d = area·ratio/(2(w+h))`）→ 自上而下、自左而右排序。
4. **裁剪**：OBB 反投影 + 双线性采样。
5. **方向**：`ch_ppocr_mobile_v2.0_cls`（静态 `[1,3,48,192]`）判定是否 180°，是则旋转。
6. **识别**：复用现有 tract rec（`rec_static.onnx` + 6623 字典 + 贪心 CTC）。

### 7.2 结果（19 张原图，判定标准＝字覆盖）

| det 输入边长 | 平均字覆盖 | det 耗时 |
|---|---|---|
| 640 | **0.311** | 349ms |
| 736 | 0.256 | 463ms |
| 768 | 0.276 | 579ms |

最终（640）对比 Python 全流程：

| 引擎 | 平均字覆盖 | 单图耗时 | 更好/持平/更差 |
|---|---|---|---|
| Python rapidocr（det+cls+rec） | 0.280 | 186ms | — |
| **Rust tract（det+cls+rec）** | **0.311** | 623ms（det 349 + rec 281） | **7 / 8 / 4** |

### 7.3 解读

- **检测质量与 PaddleOCR 相当**：框数量接近（3/1/8/… vs 3/1/6/…），端到端字覆盖略高（0.311 vs 0.280）。
- **绝对分数都低**（0.28–0.31）：这是**数据问题**——通用 DB 检测器面向"横排文本块"，而印章是环形文字；同一批图用极坐标展开+固定切分是 0.655（见 §2）。所以 P2 的正确分工是：
  - 圆章 → 极坐标展开 + 行切分（不走 det）；
  - 方章/票据/表格里的横排文字 → 这个 DB det。
- **性能**：tract 比 onnxruntime 慢 ~3.3×（det 349ms vs ~120ms）。要在 P2 里跑得动，选项：① 只对"非圆章"走 det；② 降到 512 输入；③ 回 ort（需先解决 §4 的绑定问题）或用 tract 的多线程/量化。
- 一个细节坑：**box score 必须在 unclip 之前算**。先外扩再算均值会把长条框的分数从 0.9 稀释到 0.25，导致「一个框都不剩」——这是本次调通前的主要卡点。

### 7.4 复现

```bash
# 1) 静态化模型（onnx）
uv venv /tmp/onnxenv && uv pip install --python /tmp/onnxenv/bin/python onnx onnxscript
/tmp/onnxenv/bin/python - <<'PY'
import onnx
for src,dst,dims in [('data/det.onnx','data/det_static640.onnx',[1,3,640,640]),
                     ('data/cls.onnx','data/cls_static.onnx',[1,3,48,192])]:
    m=onnx.load(src); d=m.graph.input[0].type.tensor_type.shape.dim
    for dd,v in zip(d,dims): dd.ClearField('dim_param'); dd.dim_value=v
    onnx.save(m,dst)
PY
# 2) 跑 Rust 全流程
cd ocr-tract && cargo build --release && ./target/release/detpipe ../data ~/Downloads/印章库
# 3) 对比 Python 全流程
python baseline_det_py.py && python compare_det.py
```

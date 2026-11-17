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

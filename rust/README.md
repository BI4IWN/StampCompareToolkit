# Rust 内核（P1 几何 + OCR 基线 spike）

> 分支 `rust-rewrite` ｜ 状态：**本地试验，未合入 main、未推送**
> 目标：为 P2（Tauri 换壳）验证「几何内核 + OCR 识别器」能否用 Rust 复现现有 JS/Python 的行为与精度。

## 目录

| 路径 | 内容 | 验证 |
|---|---|---|
| `stamp-core/` | 圆章几何内核：`fit`（边界射线 + 二阶矩椭圆拟合 + 射线偏置反查）、`dewarp`（迭代去畸变 + 闸门 + best-round 守卫）、`align`（圆心/半径对齐）、`synth`（已知真值合成章） | 合成 16/16 轴比误差 ≤0.001；真实 8 张 vs JS 最大差 0.013；1200×1200 单枚 5–7ms（no-op）/33–49ms（校正） |
| `ocr/` | PP-OCRv4 **识别器**（tract，纯 Rust，CTC 贪心解码），含造数据/基线/对比脚本 | 同输入同模型下，平均字覆盖 **0.674 vs Python 0.655**（5 更好 / 14 持平 / 0 更差），单条 44.7ms vs 14.6ms |

明细见 `stamp-core/README.md`（P1 全部判据与三个数学坑）与 `ocr/README.md`（方法、`ort` 绑定的坑、对 P2 的含义）。

## 快速验证

```bash
# 几何内核
cd stamp-core && cargo build --release
./target/release/stamp-core synth /tmp/synth          # 生成 16 张已知真值合成章
./target/release/stamp-core fit /tmp/synth/*.png      # 轴比/角度/覆盖率/闸门
./target/release/stamp-core dewarp /tmp/synth/synth_r0.76_t22.png --out /tmp/a.png

# OCR 识别器（需要自己准备 data/：strips/*.png + manifest.json + rec_static.onnx + dict.txt）
cd ../ocr && cargo build --release && ./target/release/ocr-tract <data-dir>
```

## 已知缺口（进 P2 前要补）

1. **检测器（DB）未实现**：OCR spike 只比了识别器，条带由极坐标展开 + 固定切分给出。
2. `ort` crate 绑定存在输入传递问题（喂同样张量输出全 blank），已改用 tract；若将来要用 ort 需先定位。
3. `theta` 符号与常见约定相反（内部自洽）。
4. 剪贴板/拖放取图（R2）未验证。

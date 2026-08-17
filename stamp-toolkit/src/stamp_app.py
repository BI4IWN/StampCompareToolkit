#!/usr/bin/env python3
"""
印章对比工具 — 统一启动文件 (BI4IWN · 李劲松) v2.1.0
双击或命令行运行即可自动启动服务并打开浏览器。

用法:
  python3 stamp_app.py              # 默认端口 8765
  python3 stamp_app.py --port 9000  # 自定义端口

关闭方式:
  终端按 Ctrl+C 或关闭终端窗口

依赖:
  pip install paddleocr paddlepaddle opencv-python

v2.5.0 更新:
  - 颜色排除：/ocr 新增 excludes 参数，排除色相优先从掩膜中剔除（贯通印章检测与通道提取）

v2.3.0 更新:
  - 手工取色 hues 全面贯通：印章中心检测/多印章检测均按自选色相提取（此前仅 OCR 通道内生效）
  - 允许清空全部预设色、仅按自选色相提取（前端 💧 取色 + × 剔除后重新提取）
  - 新增 --no-ocr 参数：跳过 PaddleOCR 模型加载，仅使用对比功能时启动更快
  - 端口被占用时给出明确错误提示；GET /favicon.ico 返回 204 避免 404 噪音

v2.1.0 更新:
  - 墨色多选：/ocr 请求 color 支持 'red'、'blue'、'purple' 或逗号分隔组合（并集提取）
  - 其余同 v2.0.0（多线程、PaddleOCR 2.x/3.x 兼容、OCR 依赖容错、结构化错误）
"""

import cv2
import numpy as np
import os
import sys
import json
import base64
import argparse
import signal
import tempfile
import webbrowser
import threading
import traceback
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

APP_VERSION = '2.7.0'

# ============================================================
# 内嵌 HTML 前端 (stamp-compare.html)
# ============================================================
# 如果同目录下存在 stamp-compare.html 文件则优先读取，否则使用内嵌版本
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
HTML_FILE = os.path.join(SCRIPT_DIR, 'stamp-compare.html')

def check_html_exists():
    """启动时校验前端文件存在"""
    if not os.path.exists(HTML_FILE):
        print("警告: 未找到 stamp-compare.html，请确保该文件与本程序在同一目录。")
        sys.exit(1)

def load_html_content():
    """每次请求时从磁盘读取前端页面（本地工具，便于前端更新后刷新即生效）"""
    with open(HTML_FILE, 'r', encoding='utf-8') as f:
        return f.read()

# ============================================================
# PaddleOCR 印章识别后端
# ============================================================

# Global OCR instance (loaded once at startup); None 表示 OCR 不可用
ocr = None
# PaddleOCR 推理锁：模型实例非线程安全，串行化并发请求
_ocr_lock = threading.Lock()


VALID_INK_COLORS = ('red', 'blue', 'purple')


def parse_ink_colors(color_param, hues_param=None, excludes_param=None, exclude_colors_param=None):
    """解析 /ocr 请求的墨色参数。

    color: 单色、逗号分隔多色或 'auto'（红/蓝/紫取占优者）
    hues: 手工取色的提取色相列表（0-360 度），与预设色取并集
    excludes: 手工取色的排除色相列表（0-360 度），提取时去除（兼容旧调用）
    exclude_colors: 排除色完整信息 [{hue, sat, val}]，用于区分无彩色（黑）与彩色（红）
    """
    if color_param == 'auto':
        colors = 'auto'
    else:
        colors = []
        for c in str(color_param).split(','):
            c = c.strip().lower()
            if c in VALID_INK_COLORS and c not in colors:
                colors.append(c)

    hues = []
    if hues_param:
        try:
            for h in list(hues_param)[:20]:
                hf = float(h) % 360
                if 0 <= hf < 360 and hf not in hues:
                    hues.append(hf)
        except (TypeError, ValueError):
            hues = []

    excludes = []
    if excludes_param:
        try:
            for h in list(excludes_param)[:20]:
                hf = float(h) % 360
                if 0 <= hf < 360 and hf not in excludes:
                    excludes.append(hf)
        except (TypeError, ValueError):
            excludes = []

    exclude_colors = []
    if exclude_colors_param:
        try:
            for ex in list(exclude_colors_param)[:20]:
                hue = float(ex.get('hue', 0)) % 360
                sat = max(0.0, min(1.0, float(ex.get('sat', 1))))
                val = max(0.0, min(1.0, float(ex.get('val', 1))))
                exclude_colors.append((hue, sat, val))
        except (TypeError, ValueError, AttributeError):
            exclude_colors = []

    # 兜底：预设色与自选色均为空时才回退红章；
    # 提供了 hues 自选色时允许清空全部预设（仅按自选色提取）
    if colors != 'auto' and not colors and not hues:
        colors = ['red']
    return colors, hues, excludes, exclude_colors


def _custom_hue_mask(h, s, v, hue_deg, tol_deg=18):
    """手工取色掩膜：色相环上 ±tol 度（OpenCV H ∈ [0,180]，含回绕）。"""
    hc = hue_deg / 2.0
    tol = tol_deg / 2.0
    dh = np.abs(h.astype(np.float32) - hc)
    dh = np.minimum(dh, 180 - dh)  # 色相环回绕
    return (dh <= tol) & (s > 25) & (v > 30)


def _custom_exclude_mask(h, s, v, ex, tol_deg=8):
    """排除色掩膜：ex 为 (hue_deg, sat, val)，sat/val ∈ [0,1]。
    无彩色（黑/白/灰，sat<0.15）按饱和度+亮度贴近度匹配；彩色按色相+亮度匹配。"""
    hue_deg, es, ev = ex
    if es < 0.15:
        return (np.abs(s.astype(np.float32) - es * 255) < 0.25 * 255) & (np.abs(v.astype(np.float32) - ev * 255) < 0.22 * 255)
    hc = hue_deg / 2.0
    tol = tol_deg / 2.0
    dh = np.abs(h.astype(np.float32) - hc)
    dh = np.minimum(dh, 180 - dh)
    return (dh <= tol) & (s > 15) & (np.abs(v.astype(np.float32) - ev * 255) < 0.45 * 255)


def _single_ink_mask(h, s, v, color):
    """单色墨迹掩膜（OpenCV HSV，H ∈ [0,180]）。"""
    if color == 'blue':
        # 蓝色 ≈ 100-130（360° 制 200-260），放宽到 90-140
        m1 = (h >= 90) & (h <= 140) & (s > 30) & (v > 30)
        m2 = (h >= 95) & (h <= 135) & (s > 15) & (v > 80)
        return (m1 | m2)
    if color == 'purple':
        # 紫色 ≈ 250-330° → 125-165，含紫红
        m1 = (h >= 125) & (h <= 168) & (s > 25) & (v > 30)
        m2 = (h >= 130) & (h <= 165) & (s > 15) & (v > 70)
        return (m1 | m2)
    # 红色（默认）
    m1 = (h < 15) & (s > 30) & (v > 30)
    m2 = (h > 165) & (s > 30) & (v > 30)
    m3 = (h >= 150) & (h <= 175) & (s > 20) & (v > 30)
    m4 = ((h < 10) | (h > 170)) & (s > 15) & (s < 80) & (v > 100)
    return (m1 | m2 | m3 | m4)


def extract_ink_mask(img, colors='red', hues=None, excludes=None, exclude_colors=None):
    """按墨色提取印章像素掩膜（多色取并集，排除色去除）。colors: 'auto' 或颜色列表；hues: 提取色相；excludes: 排除色相；exclude_colors: 排除色完整信息 [(hue,sat,val)]。"""
    hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
    h, s, v = cv2.split(hsv)

    if colors == 'auto':
        # 红蓝紫取占优者（兼容旧 'auto' 语义）
        counts = {c: _single_ink_mask(h, s, v, c).sum() for c in VALID_INK_COLORS}
        best = max(counts, key=counts.get)
        combined = _single_ink_mask(h, s, v, best)
    else:
        color_list = colors if isinstance(colors, (list, tuple)) else [colors]
        combined = None
        for c in color_list:
            if c not in VALID_INK_COLORS:
                continue
            m = _single_ink_mask(h, s, v, c)
            combined = m if combined is None else (combined | m)
        # 不在此处兜底红章：hues 自选色存在时允许仅按自选色提取，统一在下方判断

    for hue_deg in (hues or []):
        m = _custom_hue_mask(h, s, v, float(hue_deg))
        combined = m if combined is None else (combined | m)

    # 排除色优先：优先按带饱和度/亮度的 exclude_colors 精确匹配；否则退回色相列表
    if combined is not None:
        if exclude_colors:
            for ex in exclude_colors:
                m = _custom_exclude_mask(h, s, v, ex)
                combined = combined & ~m
        else:
            for hue_deg in (excludes or []):
                m = _custom_hue_mask(h, s, v, float(hue_deg), tol_deg=8)
                combined = combined & ~m

    if combined is None:
        combined = _single_ink_mask(h, s, v, 'red')

    # 布尔掩膜需转为 uint8（0/255），供 cv2.dilate 等形态学操作使用
    return combined.astype(np.uint8)


def extract_ink_channel(img, colors='red', hues=None, excludes=None, exclude_colors=None):
    """提取指定墨色的印章像素（多色并集，排除色去除），其余置白。返回 (结果图, 掩膜, 实际墨色串)。"""
    if colors == 'auto':
        hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
        h, s, v = cv2.split(hsv)
        counts = {c: _single_ink_mask(h, s, v, c).sum() for c in VALID_INK_COLORS}
        eff = [max(counts, key=counts.get)]
        red_mask = _single_ink_mask(h, s, v, eff[0])
    else:
        color_list = [c for c in (colors if isinstance(colors, (list, tuple)) else [colors])
                      if c in VALID_INK_COLORS]
        # 预设色为空但提供了自选色时，允许仅按自选色提取
        eff = color_list if color_list else ([] if (hues or excludes) else ['red'])
        red_mask = extract_ink_mask(img, color_list, hues)
    hue_list = [float(x) for x in (hues or [])]
    if hue_list:
        hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
        h, s, v = cv2.split(hsv)
        for hue_deg in hue_list:
            m = _custom_hue_mask(h, s, v, hue_deg)
            red_mask = red_mask | m
    # 排除色：去除命中所选排除色的像素（优先精确匹配，否则退回色相列表）
    if exclude_colors:
        hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
        h, s, v = cv2.split(hsv)
        for ex in exclude_colors:
            m = _custom_exclude_mask(h, s, v, ex)
            red_mask = red_mask & ~m
    else:
        excl_list = [float(x) for x in (excludes or [])]
        if excl_list:
            hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
            h, s, v = cv2.split(hsv)
            for hue_deg in excl_list:
                m = _custom_hue_mask(h, s, v, hue_deg, tol_deg=8)
                red_mask = red_mask & ~m
    result = np.ones_like(img) * 255
    result[red_mask > 0] = img[red_mask > 0]
    eff_str = ','.join(eff) if eff else ''
    if hue_list:
        eff_str = (eff_str + '|' if eff_str else '') + 'hues:' + ','.join(f'{x:.0f}' for x in hue_list)
    return result, red_mask, eff_str


def detect_stamp_centers(img, color='red', hues=None, excludes=None, exclude_colors=None):
    """检测印章中心：多印章时返回最多 3 个 (cx, cy, radius, area)，否则返回单个最佳估计。"""
    red_mask = extract_ink_mask(img, color, hues, excludes, exclude_colors)
    best_result = None
    best_score = 0
    for kernel_size in [11, 15, 21, 25]:
        kernel = np.ones((kernel_size, kernel_size), np.uint8)
        dilated = cv2.dilate(red_mask, kernel, iterations=2)
        num_labels, labels, stats, centroids = cv2.connectedComponentsWithStats(dilated)
        if num_labels <= 1:
            continue
        largest_label = 1
        largest_area = stats[1, cv2.CC_STAT_AREA]
        for i in range(2, num_labels):
            area = stats[i, cv2.CC_STAT_AREA]
            if area > largest_area:
                largest_area = area
                largest_label = i
        x = stats[largest_label, cv2.CC_STAT_LEFT]
        y = stats[largest_label, cv2.CC_STAT_TOP]
        w = stats[largest_label, cv2.CC_STAT_WIDTH]
        h_comp = stats[largest_label, cv2.CC_STAT_HEIGHT]
        cx = x + w // 2
        cy = y + h_comp // 2
        radius = max(w, h_comp) // 2
        aspect = w / h_comp if h_comp > 0 else 0
        circularity = min(aspect, 1/aspect) if aspect > 0 else 0
        img_center_dist = np.sqrt((cx - img.shape[1]//2)**2 + (cy - img.shape[0]//2)**2)
        img_diag = np.sqrt(img.shape[0]**2 + img.shape[1]**2)
        center_score = 1 - (img_center_dist / img_diag)
        score = largest_area * circularity * (0.5 + 0.5 * center_score)
        if score > best_score:
            best_score = score
            best_result = (cx, cy, radius)
    if best_result is None:
        h_img, w_img = img.shape[:2]
        return [(w_img//2, h_img//2, min(w_img, h_img)//2, 0)]
    return [(*best_result, best_score)]


def detect_multiple_stamps(img, color='red', hues=None, excludes=None, exclude_colors=None):
    """Detect multiple stamps in an image."""
    red_mask = extract_ink_mask(img, color, hues, excludes, exclude_colors)
    kernel = np.ones((15, 15), np.uint8)
    dilated = cv2.dilate(red_mask, kernel, iterations=2)
    num_labels, labels, stats, centroids = cv2.connectedComponentsWithStats(dilated)
    stamps = []
    min_area = 5000
    for i in range(1, num_labels):
        area = stats[i, cv2.CC_STAT_AREA]
        if area < min_area:
            continue
        x = stats[i, cv2.CC_STAT_LEFT]
        y = stats[i, cv2.CC_STAT_TOP]
        w = stats[i, cv2.CC_STAT_WIDTH]
        h_comp = stats[i, cv2.CC_STAT_HEIGHT]
        aspect = w / h_comp if h_comp > 0 else 0
        if aspect > 2.0 or aspect < 0.5:
            continue
        cx = x + w // 2
        cy = y + h_comp // 2
        radius = max(w, h_comp) // 2
        stamps.append((cx, cy, radius, area))
    stamps.sort(key=lambda s: s[3], reverse=True)
    return stamps[:3]


def unwrap_upper_arc_fast(img, cx, cy, radius, inner_r_factor=0.50, outer_r_factor=0.90,
                          out_w=1600, out_h=200):
    """Fast vectorized polar unwrap using cv2.remap."""
    start_angle = 2 * np.pi - 0.15
    end_angle = np.pi + 0.15
    inner_r = radius * inner_r_factor
    outer_r = radius * outer_r_factor
    out_x = np.arange(out_w, dtype=np.float32)
    out_y = np.arange(out_h, dtype=np.float32)
    grid_x, grid_y = np.meshgrid(out_x, out_y)
    r = inner_r + (outer_r - inner_r) * grid_y / out_h
    angle = start_angle + (end_angle - start_angle) * grid_x / out_w
    src_x = (cx + r * np.cos(angle)).astype(np.float32)
    src_y = (cy + r * np.sin(angle)).astype(np.float32)
    result = cv2.remap(img, src_x, src_y, cv2.INTER_LINEAR,
                       borderMode=cv2.BORDER_CONSTANT, borderValue=(255, 255, 255))
    return result


def binarize_v3(img):
    """Two best binarization approaches for speed."""
    gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    results = []
    _, otsu = cv2.threshold(gray, 0, 255, cv2.THRESH_BINARY | cv2.THRESH_OTSU)
    if np.mean(otsu) < 128:
        otsu = 255 - otsu
    results.append(otsu)
    adaptive = cv2.adaptiveThreshold(gray, 255, cv2.ADAPTIVE_THRESH_GAUSSIAN_C,
                                      cv2.THRESH_BINARY, 51, 10)
    if np.mean(adaptive) < 128:
        adaptive = 255 - adaptive
    results.append(adaptive)
    return results


def morphological_cleanup(binary, iterations=1):
    kernel = np.ones((3, 3), np.uint8)
    closed = cv2.morphologyEx(binary, cv2.MORPH_CLOSE, kernel, iterations=iterations)
    opened = cv2.morphologyEx(closed, cv2.MORPH_OPEN, kernel, iterations=1)
    return opened


def run_ocr_on_image(img_array):
    """Run PaddleOCR on a numpy image array. Returns list of (text, score).

    兼容 PaddleOCR 3.x (predict) 与 2.x (ocr) 两种 API。失败时抛出异常。
    """
    with tempfile.NamedTemporaryFile(suffix='.png', delete=False) as f:
        cv2.imwrite(f.name, img_array)
        temp_path = f.name
    results = []
    try:
        with _ocr_lock:
            if hasattr(ocr, 'predict'):
                # PaddleOCR 3.x
                res = ocr.predict(temp_path)
                for r in res:
                    j = r.json if hasattr(r, 'json') else r
                    res_data = j.get('res', {}) if isinstance(j, dict) else {}
                    texts = res_data.get('rec_texts', [])
                    scores = res_data.get('rec_scores', [])
                    for text, score in zip(texts, scores):
                        results.append((text, float(score)))
            else:
                # PaddleOCR 2.x
                res = ocr.ocr(temp_path, cls=True)
                for page in (res or []):
                    for line in (page or []):
                        if len(line) >= 2 and line[1]:
                            text, score = line[1][0], line[1][1]
                            results.append((text, float(score)))
    finally:
        os.unlink(temp_path)
    return results


# Optimized parameter sets (4 combos for quality, 2 binarization for speed)
PARAM_SETS = [
    (0.50, 0.90, 1600, 200, 3, False, 1),  # baseline
    (0.45, 0.88, 1800, 220, 3, False, 1),  # wider radius
    (0.40, 0.95, 1800, 220, 3, False, 1),  # max radius range
    (0.50, 0.90, 1600, 200, 3, True, 1),   # with morphological cleanup
]


def process_stamp_ocr(img, cx, cy, radius, color, hues=None, excludes=None, exclude_colors=None):
    """Process a single stamp and return OCR results."""
    h, w = img.shape[:2]
    margin = 30
    y1 = max(0, cy - radius - margin)
    y2 = min(h, cy + radius + margin)
    x1 = max(0, cx - radius - margin)
    x2 = min(w, cx + radius + margin)
    ink_img, _, eff_colors = extract_ink_channel(img, color, hues, excludes, exclude_colors)
    cropped = ink_img[y1:y2, x1:x2]
    cx_crop = cx - x1
    cy_crop = cy - y1

    best_texts = []
    best_score_sum = 0

    for params in PARAM_SETS:
        inner_r, outer_r, out_w, out_h, scale, use_morph, morph_iter = params
        try:
            cropped_up = cv2.resize(cropped, (cropped.shape[1]*scale, cropped.shape[0]*scale),
                                     interpolation=cv2.INTER_CUBIC)
            unwrap_up = unwrap_upper_arc_fast(cropped_up, cx_crop*scale, cy_crop*scale, radius*scale,
                                               inner_r_factor=inner_r, outer_r_factor=outer_r,
                                               out_w=out_w, out_h=out_h)
            binaries = binarize_v3(unwrap_up)
            for bin_img in binaries:
                if use_morph:
                    bin_img = morphological_cleanup(bin_img, morph_iter)
                ocr_results = run_ocr_on_image(bin_img)
                filtered = [(t, s) for t, s in ocr_results if s > 0.3]
                score_sum = sum(s for _, s in filtered)
                if score_sum > best_score_sum:
                    best_score_sum = score_sum
                    best_texts = filtered
            # Early termination: if high confidence found, skip remaining params
            if best_score_sum > 4.0:
                break
        except Exception:
            continue

    return best_texts, eff_colors


def stamp_ocr_pipeline(img, color='red', hues=None, excludes=None, exclude_colors=None):
    """Full stamp OCR pipeline. color: 'auto' 或颜色列表；hues: 提取色相；excludes: 排除色相；exclude_colors: 排除色完整信息。"""
    all_results = []
    eff_colors = color

    # Check for multiple stamps
    multi_stamps = detect_multiple_stamps(img, eff_colors, hues, excludes, exclude_colors)

    if len(multi_stamps) >= 2:
        for cx, cy, radius, area in multi_stamps:
            texts, eff_colors = process_stamp_ocr(img, cx, cy, radius, color, hues, excludes, exclude_colors)
            if texts:
                all_results.append({
                    'center': [int(cx), int(cy)],
                    'radius': int(radius),
                    'texts': [{'text': t, 'score': round(s, 3)} for t, s in texts],
                    'full_text': ' '.join(t for t, s in texts)
                })
    else:
        cx, cy, radius, _ = detect_stamp_centers(img, eff_colors, hues, excludes, exclude_colors)[0]
        texts, eff_colors = process_stamp_ocr(img, cx, cy, radius, color, hues, excludes, exclude_colors)
        if texts:
            all_results.append({
                'center': [int(cx), int(cy)],
                'radius': int(radius),
                'texts': [{'text': t, 'score': round(s, 3)} for t, s in texts],
                'full_text': ' '.join(t for t, s in texts)
            })

    return all_results, eff_colors


# ============================================================
# HTTP 服务 — 同时提供 HTML 页面和 OCR API
# ============================================================

MAX_BODY_BYTES = 30 * 1024 * 1024  # 30MB 上限


class StampAppHandler(BaseHTTPRequestHandler):
    """统一处理：HTML 页面 + OCR API（多线程，OCR 期间不阻塞其他请求）"""
    protocol_version = 'HTTP/1.1'

    def _send_json(self, payload, status=200):
        body = json.dumps(payload, ensure_ascii=False).encode('utf-8')
        self.send_response(status)
        self.send_header('Content-Type', 'application/json; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Access-Control-Allow-Origin', '*')
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        """Handle CORS preflight."""
        self.send_response(200)
        self.send_header('Access-Control-Allow-Origin', '*')
        self.send_header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
        self.send_header('Access-Control-Allow-Headers', 'Content-Type')
        self.send_header('Content-Length', '0')
        self.end_headers()

    def do_GET(self):
        if self.path == '/' or self.path == '/index.html':
            # 提供 HTML 前端页面（每次从磁盘读取，前端更新后刷新即生效）
            body = load_html_content().encode('utf-8')
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(body)))
            self.send_header('Access-Control-Allow-Origin', '*')
            self.end_headers()
            self.wfile.write(body)
        elif self.path == '/health':
            self._send_json({
                'status': 'ok',
                'version': APP_VERSION,
                'ocr': ocr is not None,
            })
        elif self.path == '/favicon.ico':
            # 无图标文件，返回 204 避免控制台 404 噪音
            self.send_response(204)
            self.send_header('Content-Length', '0')
            self.end_headers()
        else:
            self.send_error(404, 'Not found')

    def do_POST(self):
        if self.path != '/ocr':
            self.send_error(404, 'Not found')
            return

        if ocr is None:
            self._send_json({
                'success': False,
                'error': 'OCR 模型未加载 — 请安装依赖: pip install paddlepaddle paddleocr opencv-python 后重启服务',
            })
            return

        try:
            content_length = int(self.headers.get('Content-Length', 0))
            if content_length <= 0 or content_length > MAX_BODY_BYTES:
                self._send_json({'success': False, 'error': '请求体大小无效'}, 400)
                return
            body = self.rfile.read(content_length)

            data = json.loads(body)
            image_b64 = data.get('image', '')
            # color 预设色（多选）+ hues 提取色相 + excludes 排除色相 + exclude_colors 排除色完整信息
            color, hues, excludes, exclude_colors = parse_ink_colors(
                data.get('color', 'red'), data.get('hues'), data.get('excludes'), data.get('exclude_colors'))

            # Decode base64 image
            img_bytes = base64.b64decode(image_b64)
            img_array = np.frombuffer(img_bytes, dtype=np.uint8)
            img = cv2.imdecode(img_array, cv2.IMREAD_COLOR)

            if img is None:
                self._send_json({'success': False, 'error': '无法解析图像数据'}, 400)
                return

            # Run OCR pipeline
            results, eff_colors = stamp_ocr_pipeline(img, color, hues, excludes, exclude_colors)

            self._send_json({
                'success': True,
                'stamps': results,
                'color': eff_colors,
                'hues': hues or [],
                'excludes': excludes or [],
                'image_size': [int(img.shape[1]), int(img.shape[0])]
            })

        except json.JSONDecodeError:
            self._send_json({'success': False, 'error': '无效的 JSON 请求'}, 400)
        except (base64.binascii.Error, ValueError):
            self._send_json({'success': False, 'error': '无效的 base64 图像数据'}, 400)
        except Exception as e:
            traceback.print_exc()
            self._send_json({'success': False, 'error': f'识别失败: {e}'}, 500)

    def log_message(self, format, *args):
        """Suppress default logging for cleaner output."""
        pass


def open_browser_delayed(port, delay=1.0):
    """延迟打开浏览器，等待服务完全启动"""
    def _open():
        import time
        time.sleep(delay)
        url = f'http://127.0.0.1:{port}'
        try:
            webbrowser.open(url)
            print(f"浏览器已打开: {url}")
        except Exception:
            print(f"请手动打开浏览器访问: {url}")
    t = threading.Thread(target=_open, daemon=True)
    t.start()


def load_ocr_model():
    """加载 PaddleOCR 模型；依赖缺失时返回 None 而不是崩溃（对比功能不依赖后端）。"""
    global ocr
    print("加载 PaddleOCR 模型（首次运行需下载，请耐心等待）...")
    try:
        from paddleocr import PaddleOCR
        ocr = PaddleOCR(lang='ch')
        print("模型加载完成!")
        return True
    except ImportError as e:
        ocr = None
        print(f"\n[警告] PaddleOCR 未安装（{e}），OCR 功能不可用。")
        print("        页面服务将正常启动，印章对比/导出等功能不受影响。")
        print("        如需 OCR，请安装依赖后重启:")
        print("        pip install paddlepaddle paddleocr opencv-python\n")
        return False
    except Exception as e:
        ocr = None
        print(f"\n[警告] PaddleOCR 模型加载失败（{e}），OCR 功能不可用，其余功能正常。\n")
        return False


def main():
    parser = argparse.ArgumentParser(description='印章对比工具 — 统一启动文件 (BI4IWN)')
    parser.add_argument('--port', type=int, default=8765, help='服务端口 (默认: 8765)')
    parser.add_argument('--no-browser', action='store_true', help='不自动打开浏览器')
    parser.add_argument('--no-ocr', action='store_true',
                        help='跳过 PaddleOCR 模型加载（启动更快，仅使用对比/导出功能）')
    args = parser.parse_args()

    # 加载 HTML 前端
    print("加载前端页面...")
    check_html_exists()
    print("前端页面已就绪")

    # 加载 PaddleOCR 模型（可选；--no-ocr 时跳过以加快启动）
    if args.no_ocr:
        print("已跳过 OCR 模型加载（--no-ocr），OCR 功能不可用，其余功能正常。")
    else:
        load_ocr_model()

    # 启动 HTTP 服务（多线程：OCR 长耗时请求不阻塞页面/健康检查）
    try:
        server = ThreadingHTTPServer(('127.0.0.1', args.port), StampAppHandler)
    except OSError as e:
        print(f"\n[错误] 无法绑定端口 {args.port}: {e}")
        print("  端口可能已被占用。可结束占用该端口的进程，或改用其他端口：")
        print("  python3 stamp_app.py --port 9000")
        sys.exit(1)
    server.daemon_threads = True

    # 安装 SIGTERM 处理：PaddlePaddle 运行时会吞掉 SIGTERM 信号，
    # 导致 launcher 停止服务时进程不退出、端口无法释放
    def _handle_sigterm(signum, frame):
        print("\n收到停止信号，正在关闭服务...")
        sys.exit(0)
    signal.signal(signal.SIGTERM, _handle_sigterm)

    print(f"\n{'='*50}")
    print(f"  印章对比工具已启动 (BI4IWN) v{APP_VERSION}")
    print(f"  访问地址: http://127.0.0.1:{args.port}")
    print(f"  OCR 服务: {'已就绪' if ocr is not None else '不可用（对比功能不受影响）'}")
    print(f"  按 Ctrl+C 停止服务")
    print(f"{'='*50}\n")

    # 自动打开浏览器
    if not args.no_browser:
        open_browser_delayed(args.port)

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n正在关闭服务...")
        server.shutdown()
        print("服务已停止")


if __name__ == '__main__':
    main()

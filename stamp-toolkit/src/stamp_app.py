#!/usr/bin/env python3
"""
印章对比工具 — 统一启动文件 (BI4IWN · 李劲松) v2.10.0
双击或命令行运行即可自动启动服务并打开浏览器。

用法:
  python3 stamp_app.py              # 默认端口 8765
  python3 stamp_app.py --port 9000  # 自定义端口

关闭方式:
  终端按 Ctrl+C 或关闭终端窗口

依赖:
  pip install paddleocr paddlepaddle opencv-python

v2.8.0 更新:
  - AI 文字提取：/ai-ocr 与 /ai/test 代理调用 OpenAI 兼容接口
    （默认预设 DeepSeek / 智谱，支持自定义地址、模型、密钥）
  - 视觉模式直接发图识别；文本解析模式 = 本地 PaddleOCR + AI 清洗结构化

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
import re
import argparse
import signal
import tempfile
import webbrowser
import threading
import time
import platform
import shutil
import subprocess
import uuid
import datetime
import urllib.request
import urllib.error
import urllib.parse
import traceback
import unicodedata
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

APP_VERSION = '3.6.8'

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
_ocr_engine = None   # 'rapid' | 'paddle' | None
ocr_loading = True    # 模型后台加载中（/health 返回 'loading'）
# PaddleOCR 推理锁：模型实例非线程安全，串行化并发请求
_ocr_lock = threading.Lock()
# 本地 VLM 每次调用都会加载约 3GB 峰值内存，单进程内必须串行。
_VLM_LOCK = threading.Lock()
_VLM_LOCK_TIMEOUT_SEC = 180
VLM_ENGINE_FILE = os.path.join(SCRIPT_DIR, 'vlm_engine.py')


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


def _fit_circle_least_squares(points):
    """Alias-free algebraic circle fit; returns (cx, cy, R) or None."""
    pts = np.asarray(points, dtype=np.float64).reshape(-1, 2)
    if pts.shape[0] < 8:
        return None
    x, y = pts[:, 0], pts[:, 1]
    a = np.column_stack((2 * x, 2 * y, np.ones_like(x)))
    b = x * x + y * y
    try:
        (cx, cy, c), _, rank, _ = np.linalg.lstsq(a, b, rcond=None)
    except np.linalg.LinAlgError:
        return None
    radius_sq = c + cx * cx + cy * cy
    if rank < 3 or radius_sq <= 0 or not np.isfinite(radius_sq):
        return None
    return float(cx), float(cy), float(np.sqrt(radius_sq))


def _component_circle_fallback(mask, img_shape):
    """Largest plausible ink component, refined with a least-squares circle fit."""
    h_img, w_img = img_shape[:2]
    kernel = np.ones((7, 7), np.uint8)
    closed = cv2.morphologyEx(mask, cv2.MORPH_CLOSE, kernel, iterations=2)
    num_labels, _, stats, _ = cv2.connectedComponentsWithStats(closed)
    if num_labels <= 1:
        radius = min(w_img, h_img) * 0.42
        return float(w_img / 2), float(h_img / 2), float(radius), 0.1, 'fallback'

    best = None
    best_score = -1.0
    for i in range(1, num_labels):
        x, y, w, h, area = stats[i]
        if area < 200 or w < 20 or h < 20:
            continue
        aspect = min(w, h) / max(w, h)
        extent = max(w, h) / max(w_img, h_img)
        score = area * (0.35 + 0.65 * aspect) * (0.5 + min(extent, 1.0))
        if score > best_score:
            best_score = score
            best = (i, x, y, w, h, area)
    if best is None:
        radius = min(w_img, h_img) * 0.42
        return float(w_img / 2), float(h_img / 2), float(radius), 0.1, 'fallback'

    _, x, y, w, h, area = best
    roi = np.zeros_like(mask)
    roi[y:y + h, x:x + w] = mask[y:y + h, x:x + w]
    contours, _ = cv2.findContours(roi, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_NONE)
    boundary = []
    for contour in contours:
        for p in contour[::max(1, len(contour) // 300)]:
            boundary.append(p[0])
    fitted = _fit_circle_least_squares(boundary)
    if fitted is None:
        (cx, cy), radius = cv2.minEnclosingCircle(roi)
    else:
        cx, cy, radius = fitted
    radius = max(radius, max(w, h) * 0.5)
    # A fitted ring centre is more reliable than the component bbox centre.
    center_shift = np.hypot(cx - (x + w / 2), cy - (y + h / 2))
    confidence = float(np.clip(1.0 - center_shift / max(radius, 1), 0.15, 0.72))
    return float(cx), float(cy), float(radius), confidence, 'least_squares'


def estimate_stamp_circle(img, mask=None):
    """Estimate the dominant stamp circle as (cx, cy, R, confidence, method)."""
    if mask is None:
        mask = extract_ink_mask(img, 'red')
    h_img, w_img = img.shape[:2]
    short = min(h_img, w_img)
    blur = cv2.GaussianBlur(mask, (5, 5), 0)
    min_r = max(18, int(short * 0.16))
    max_r = max(min_r + 4, int(short * 0.56))
    candidates = None
    for p2 in (30, 24, 18):
        try:
            candidates = cv2.HoughCircles(
                blur, cv2.HOUGH_GRADIENT, dp=1.2, minDist=max(30, short * 0.28),
                param1=100, param2=p2, minRadius=min_r, maxRadius=max_r,
            )
        except cv2.error:
            candidates = None
        if candidates is not None and len(candidates[0]):
            break

    if candidates is not None and len(candidates[0]):
        best = None
        for raw in candidates[0][:5]:
            cx, cy, radius = map(float, raw[:3])
            if radius < min_r * 0.75:
                continue
            yy, xx = np.ogrid[:h_img, :w_img]
            ring = (np.abs(np.hypot(xx - cx, yy - cy) - radius) <= max(2.5, radius * 0.035))
            support = float((mask[ring] > 0).mean()) if np.any(ring) else 0.0
            center_dist = np.hypot(cx - w_img / 2, cy - h_img / 2) / max(short, 1)
            score = support + 0.18 * (1.0 - min(center_dist, 1.0))
            if best is None or score > best[0]:
                best = (score, cx, cy, radius, support)
        if best is not None:
            score, cx, cy, radius, support = best
            confidence = float(np.clip(0.45 + support * 0.5 - 0.08 * score, 0.25, 0.98))
            return cx, cy, radius, confidence, 'hough'

    return _component_circle_fallback(mask, img.shape)


def detect_stamp_centers(img, color='red', hues=None, excludes=None, exclude_colors=None):
    """检测主印章，返回 [(cx, cy, radius, confidence)]（兼容旧调用）。"""
    mask = extract_ink_mask(img, color, hues, excludes, exclude_colors)
    cx, cy, radius, confidence, _ = estimate_stamp_circle(img, mask)
    return [(int(round(cx)), int(round(cy)), int(round(radius)), confidence)]


def detect_multiple_stamps(img, color='red', hues=None, excludes=None, exclude_colors=None):
    """Detect large stamp-like components; the main geometry still uses Hough."""
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
        c_mask = (labels == i).astype(np.uint8) * 255
        cx, cy, radius, confidence, _ = estimate_stamp_circle(img, c_mask)
        stamps.append((int(round(cx)), int(round(cy)), int(round(radius)), area, confidence))
    stamps.sort(key=lambda s: s[3], reverse=True)
    return stamps[:3]


def unwrap_stamp_ring(img, cx, cy, radius, inner_r_factor=0.55, outer_r_factor=0.92,
                      out_h=96, max_width=1600):
    """Unwrap the complete 360-degree stamp ring in clockwise reading order."""
    mid_radius = radius * (inner_r_factor + outer_r_factor) * 0.5
    desired_width = max(384, int(round(2 * np.pi * mid_radius)))
    out_w = min(max_width, desired_width)
    if desired_width > max_width:
        out_h = max(48, int(round(out_h * max_width / desired_width)))
    angle = np.linspace(np.pi, 3 * np.pi, out_w, endpoint=False, dtype=np.float32)
    radial = np.linspace(outer_r_factor * radius, inner_r_factor * radius,
                         out_h, dtype=np.float32)
    src_x = (cx + np.cos(angle)[None, :] * radial[:, None]).astype(np.float32)
    src_y = (cy + np.sin(angle)[None, :] * radial[:, None]).astype(np.float32)
    return cv2.remap(
        img, src_x, src_y, cv2.INTER_CUBIC,
        borderMode=cv2.BORDER_CONSTANT, borderValue=(255, 255, 255),
    )


def unwrap_upper_arc_fast(img, cx, cy, radius, inner_r_factor=0.50, outer_r_factor=0.90,
                          out_w=1600, out_h=200):
    """Compatibility wrapper for the old diagnostic hooks; now always full 360°."""
    del out_w
    return unwrap_stamp_ring(
        img, cx, cy, radius, inner_r_factor=inner_r_factor,
        outer_r_factor=outer_r_factor, out_h=max(48, int(out_h)),
    )


def binarize_v3(img):
    """Two best binarization approaches for speed."""
    gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    results = []
    _, otsu = cv2.threshold(255 - gray, 0, 255, cv2.THRESH_BINARY | cv2.THRESH_OTSU)
    results.append(otsu)
    adaptive = cv2.adaptiveThreshold(
        255 - gray, 255, cv2.ADAPTIVE_THRESH_GAUSSIAN_C,
        cv2.THRESH_BINARY, 41, 8,
    )
    results.append(adaptive)
    return results


def morphological_cleanup(binary, iterations=1):
    kernel = np.ones((3, 3), np.uint8)
    closed = cv2.morphologyEx(binary, cv2.MORPH_CLOSE, kernel, iterations=iterations)
    opened = cv2.morphologyEx(closed, cv2.MORPH_OPEN, kernel, iterations=1)
    return opened


def _run_ocr_detailed(img_array):
    """Run OCR and retain box geometry for clockwise fragment alignment."""
    results = []
    if _ocr_engine == 'rapid':
        with _ocr_lock:
            res, _elapse = ocr(img_array)
        for item in (res or []):
            if len(item) < 3 or not item[1]:
                continue
            box = np.asarray(item[0], dtype=np.float32)
            x1, y1 = float(box[:, 0].min()), float(box[:, 1].min())
            x2, y2 = float(box[:, 0].max()), float(box[:, 1].max())
            results.append({
                'text': str(item[1]), 'score': float(item[2]),
                'x': x1, 'y': y1, 'x2': x2, 'y2': y2,
            })
        return results

    with tempfile.NamedTemporaryFile(suffix='.png', delete=False) as f:
        cv2.imwrite(f.name, img_array)
        temp_path = f.name
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
                    boxes = res_data.get('rec_polys') or res_data.get('dt_polys') or []
                    for idx, (text, score) in enumerate(zip(texts, scores)):
                        box = np.asarray(boxes[idx], dtype=np.float32) if idx < len(boxes) else None
                        results.append({
                            'text': str(text), 'score': float(score),
                            'x': float(box[:, 0].min()) if box is not None else float(idx * 100),
                            'y': float(box[:, 1].min()) if box is not None else 0.0,
                            'x2': float(box[:, 0].max()) if box is not None else float(idx * 100 + 80),
                            'y2': float(box[:, 1].max()) if box is not None else 20.0,
                        })
            else:
                # PaddleOCR 2.x
                res = ocr.ocr(temp_path, cls=True)
                for page in (res or []):
                    for line in (page or []):
                        if len(line) >= 2 and line[1]:
                            text, score = line[1][0], line[1][1]
                            box = np.asarray(line[0], dtype=np.float32)
                            results.append({
                                'text': str(text), 'score': float(score),
                                'x': float(box[:, 0].min()), 'y': float(box[:, 1].min()),
                                'x2': float(box[:, 0].max()), 'y2': float(box[:, 1].max()),
                            })
    finally:
        os.unlink(temp_path)
    return results


def run_ocr_on_image(img_array):
    """Run OCR on a numpy image array. Returns list of (text, score).

    多引擎（v3.0.0）：优先 RapidOCR（onnxruntime，桌面安装包内置，轻量），
    回退 PaddleOCR（兼容 3.x predict 与 2.x ocr 两种 API）。失败时抛出异常。
    """
    return [(item['text'], item['score']) for item in _run_ocr_detailed(img_array)]


_TEXT_STRIP_RE = re.compile(
    r'[\s·•,，。;；:：、!！?？"\'“”‘’《》<>\[\]【】{}()（）\-—_/\\|]'
)
_TYPE_RE = re.compile(
    r'(?:合同|财务|发票|业务|检验检测|人事|行政|报关|收款|发货)?'
    r'(?:专用章|合同章|财务章|发票章|业务章|公章)'
)
_LIBRARY_CACHE = {'key': None, 'names': []}


def normalize_stamp_text(value):
    """NFKC + remove whitespace and punctuation for evaluation/library matching."""
    text = unicodedata.normalize('NFKC', str(value or ''))
    return _TEXT_STRIP_RE.sub('', text)


def _longest_common_subsequence(a, b):
    if not a or not b:
        return 0
    prev = [0] * (len(b) + 1)
    for ca in a:
        cur = [0]
        for j, cb in enumerate(b, 1):
            cur.append(prev[j - 1] + 1 if ca == cb else max(prev[j], cur[-1]))
        prev = cur
    return prev[-1]


def _longest_common_run(a, b):
    if not a or not b:
        return 0
    prev = [0] * (len(b) + 1)
    best = 0
    for ca in a:
        cur = [0] * (len(b) + 1)
        for j, cb in enumerate(b, 1):
            if ca == cb:
                cur[j] = prev[j - 1] + 1
                best = max(best, cur[j])
        prev = cur
    return best


def _text_similarity(observed, candidate):
    if not observed or not candidate:
        return 0.0
    if observed == candidate:
        return 1.0
    lcs = _longest_common_subsequence(observed, candidate)
    run = _longest_common_run(observed, candidate)
    coverage = lcs / len(candidate)
    f1 = 2 * lcs / (len(observed) + len(candidate))
    longest = run / len(candidate)
    common = 0
    remaining = list(candidate)
    for ch in observed:
        if ch in remaining:
            remaining.remove(ch)
            common += 1
    multiset = common / len(candidate)
    return float(np.clip(
        0.40 * coverage + 0.25 * longest + 0.20 * f1 + 0.15 * multiset,
        0.0, 1.0,
    ))


def _stamp_library_sources():
    paths = []
    env_path = os.environ.get('STAMP_SETTINGS_FILE')
    if env_path:
        paths.append(os.path.expanduser(env_path))
    app_support = os.path.expanduser('~/Library/Application Support')
    for dirname in ('印章比对工具', 'stamp-compare-tool', 'StampCompareToolkit', 'Electron'):
        paths.append(os.path.join(app_support, dirname, 'settings.json'))
    try:
        for entry in os.scandir(app_support):
            if entry.is_dir() and ('印章' in entry.name or 'stamp' in entry.name.lower()):
                paths.append(os.path.join(entry.path, 'settings.json'))
    except OSError:
        pass
    return list(dict.fromkeys(paths))


def _load_stamp_library():
    lib_path = os.path.expanduser('~/Downloads/印章库')
    for settings_path in _stamp_library_sources():
        try:
            with open(settings_path, 'r', encoding='utf-8') as f:
                configured = json.load(f).get('libPath')
            if configured:
                lib_path = os.path.expanduser(str(configured))
                break
        except (OSError, ValueError, TypeError):
            continue
    index_path = os.path.join(lib_path, 'index.json')
    try:
        stat = os.stat(index_path)
        cache_key = (index_path, stat.st_mtime_ns)
    except OSError:
        return []
    if _LIBRARY_CACHE['key'] == cache_key:
        return _LIBRARY_CACHE['names']
    try:
        with open(index_path, 'r', encoding='utf-8') as f:
            raw = json.load(f)
        records = raw.get('seals', []) if isinstance(raw, dict) else raw
        if not isinstance(records, list):
            records = []
    except (OSError, ValueError, TypeError):
        return []
    names = []
    seen = set()
    for record in records:
        if not isinstance(record, dict):
            continue
        name = normalize_stamp_text(record.get('name'))
        if name and name not in seen:
            seen.add(name)
            names.append(name)
    _LIBRARY_CACHE.update(key=cache_key, names=names)
    return names


# ============================================================
# 本地视觉引擎探测与安装管理
# ============================================================

ENGINE_MANAGEMENT_ENABLED = True
LOCAL_ENGINE_ID = 'paddleocr-vl-4bit'
LOCAL_ENGINE_VERSION = '4bit'
LOCAL_ENGINE_MODEL_BYTES = 703562711
LOCAL_ENGINE_SIZE_MB = LOCAL_ENGINE_MODEL_BYTES // 1000000
LOCAL_ENGINE_SEGMENTS = 12
LOCAL_ENGINE_BASE_URL = (
    'https://huggingface.co/mlx-community/PaddleOCR-VL-4bit/resolve/main/'
)
LOCAL_ENGINE_FILES = (
    'added_tokens.json',
    'chat_template.jinja',
    'config.json',
    'configuration_paddleocr_vl.py',
    'generation_config.json',
    'image_processing_paddleocr_vl.py',
    'model.safetensors',
    'model.safetensors.index.json',
    'modeling_paddleocr_vl.py',
    'preprocessor_config.json',
    'processing_paddleocr_vl.py',
    'processor_config.json',
    'special_tokens_map.json',
    'tokenizer.json',
    'tokenizer_config.json',
)
ENGINE_STATES = {
    'absent', 'downloading', 'verifying', 'installing', 'ready', 'failed',
}
ENGINE_TRANSIENT_STATES = {'downloading', 'verifying', 'installing'}
ENGINE_RUNNING_STATES = {'queued', 'downloading', 'verifying', 'installing'}

_ENGINE_LOCK = threading.RLock()
_ENGINE_JOBS_LOCK = threading.Lock()
_ENGINE_JOBS = {}
_ENGINE_HARDWARE_CACHE = {'measuredAt': 0.0, 'payload': None}
_ENGINE_HARDWARE_LOCK = threading.Lock()


def _app_data_dir():
    """应用数据目录；测试或便携部署可通过环境变量覆盖。"""
    configured = os.environ.get('STAMP_APP_DATA_DIR')
    path = os.path.expanduser(
        configured or '~/Library/Application Support/印章比对工具'
    )
    os.makedirs(path, exist_ok=True)
    return path


def _engines_state_path():
    return os.path.join(_app_data_dir(), 'engines.json')


def _engine_dir(engine_id=LOCAL_ENGINE_ID):
    return os.path.join(_app_data_dir(), 'engines', engine_id)


def _engine_model_dir(engine_id=LOCAL_ENGINE_ID):
    return os.path.join(_engine_dir(engine_id), 'model')


def _engine_manifest_path(engine_id=LOCAL_ENGINE_ID):
    return os.path.join(_engine_dir(engine_id), 'manifest.json')


def _default_engines_state():
    return {'active': 'rapidocr', 'dismissedVersion': None, 'engines': {}}


def _read_engines_state_unlocked():
    """读取持久化状态；损坏或字段异常时无异常回落到默认值。"""
    try:
        with open(_engines_state_path(), 'r', encoding='utf-8') as f:
            raw = json.load(f)
    except (OSError, ValueError, TypeError):
        return _default_engines_state()
    if not isinstance(raw, dict):
        return _default_engines_state()

    state = _default_engines_state()
    active = raw.get('active')
    if active in ('rapidocr', LOCAL_ENGINE_ID, 'cloud'):
        state['active'] = active
    state['dismissedVersion'] = raw.get('dismissedVersion')
    engines = raw.get('engines')
    if isinstance(engines, dict):
        for engine_id, record in engines.items():
            if engine_id == LOCAL_ENGINE_ID and isinstance(record, dict):
                clean = dict(record)
                if clean.get('state') not in ENGINE_STATES:
                    clean['state'] = 'absent'
                state['engines'][engine_id] = clean
    if state['active'] == LOCAL_ENGINE_ID:
        local = state['engines'].get(LOCAL_ENGINE_ID) or {}
        if local.get('state') != 'ready':
            state['active'] = 'rapidocr'
    return state


def _load_engines_state():
    with _ENGINE_LOCK:
        return _read_engines_state_unlocked()


def _atomic_write_json(path, payload):
    parent = os.path.dirname(path)
    os.makedirs(parent, exist_ok=True)
    temp_path = None
    try:
        fd, temp_path = tempfile.mkstemp(prefix='.tmp-', suffix='.json', dir=parent)
        with os.fdopen(fd, 'w', encoding='utf-8') as f:
            json.dump(payload, f, ensure_ascii=False, indent=2)
            f.write('\n')
        os.replace(temp_path, path)
    finally:
        if temp_path and os.path.exists(temp_path):
            try:
                os.unlink(temp_path)
            except OSError:
                pass


def _save_engines_state(state):
    with _ENGINE_LOCK:
        _atomic_write_json(_engines_state_path(), state)


def _initialize_engine_state():
    """启动时把上次进程中断遗留的临时安装状态标为可续传的 failed。"""
    with _ENGINE_LOCK:
        state = _read_engines_state_unlocked()
        record = state['engines'].get(LOCAL_ENGINE_ID)
        changed = False
        if record and record.get('state') in ENGINE_TRANSIENT_STATES:
            record['state'] = 'failed'
            record['error'] = '上次下载被中断，可继续'
            record['sizeMb'] = 0
            changed = True
        if changed:
            _atomic_write_json(_engines_state_path(), state)


def _now_iso():
    return datetime.datetime.now().astimezone().isoformat(timespec='seconds')


def _run_sysctl(name):
    try:
        result = subprocess.run(
            ['sysctl', '-n', name], capture_output=True, text=True,
            timeout=2, check=False,
        )
        value = result.stdout.strip()
        return value if result.returncode == 0 and value else None
    except (OSError, subprocess.SubprocessError):
        return None


def _system_ram_gb():
    raw = _run_sysctl('hw.memsize')
    try:
        if raw:
            return float(raw) / (1024 ** 3)
        page_size = os.sysconf('SC_PAGE_SIZE')
        page_count = os.sysconf('SC_PHYS_PAGES')
        return float(page_size * page_count) / (1024 ** 3)
    except (AttributeError, OSError, ValueError, TypeError):
        return 0.0


def _system_cpu_name(arch):
    value = _run_sysctl('machdep.cpu.brand_string')
    if value:
        return value
    try:
        result = subprocess.run(
            ['system_profiler', 'SPHardwareDataType', '-json'],
            capture_output=True, text=True, timeout=4, check=False,
        )
        overview = json.loads(result.stdout).get('SPHardwareDataType') or []
        chip = overview[0].get('chip_type') if overview else None
        if chip:
            return str(chip)
    except (OSError, ValueError, IndexError, TypeError, subprocess.SubprocessError):
        pass
    return platform.processor() or arch


def _measure_host_hardware():
    arch = platform.machine()
    cpu = _system_cpu_name(arch)
    ram_gb = _system_ram_gb()
    macos = platform.mac_ver()[0] or ''
    try:
        disk_free_gb = shutil.disk_usage(_app_data_dir()).free / (1000 ** 3)
    except OSError:
        disk_free_gb = 0.0

    reasons = []
    hard_unsupported = False
    if arch == 'arm64':
        reasons.append('Apple Silicon')
    else:
        hard_unsupported = True
        reasons.append('本地视觉模型需要 Apple Silicon')

    if ram_gb < 8:
        hard_unsupported = True
        support = 'unsupported'
        reasons.append(f'内存 {ram_gb:.1f}GB < 8GB')
    elif ram_gb < 16:
        support = 'supported'
        reasons.append(f'内存 {ram_gb:.1f}GB，可用，速度较慢')
    else:
        support = 'recommended'
        reasons.append(f'内存 {ram_gb:g}GB ≥ 16GB')

    try:
        macos_major = int(macos.split('.', 1)[0])
    except (ValueError, IndexError):
        macos_major = 0
    if macos_major >= 13:
        reasons.append(f'macOS {macos} ≥ 13')
    else:
        hard_unsupported = True
        reasons.append(f'macOS {macos or "未知"} < 13')

    if disk_free_gb < 3.0:
        if support == 'recommended':
            support = 'supported'
        reasons.append('磁盘不足 3GB，安装前需清理')
    else:
        reasons.append(f'磁盘剩余 {disk_free_gb:.1f}GB ≥ 3GB')

    if hard_unsupported:
        support = 'unsupported'
    return {
        'arch': arch,
        'cpu': cpu,
        'ramGb': round(ram_gb, 1),
        'diskFreeGb': round(disk_free_gb, 1),
        'macos': macos,
        'support': support,
        'reasons': reasons,
    }


def _hardware_probe(force=False):
    now = time.monotonic()
    with _ENGINE_HARDWARE_LOCK:
        cached = _ENGINE_HARDWARE_CACHE
        if (
            not force and cached['payload'] is not None
            and now - cached['measuredAt'] <= 5.0
        ):
            return dict(cached['payload'])
        payload = _measure_host_hardware()
        _ENGINE_HARDWARE_CACHE.update(measuredAt=now, payload=dict(payload))
        return payload


def _engine_public_state(engine_id):
    if engine_id == 'rapidocr':
        return 'ready'
    with _ENGINE_JOBS_LOCK:
        for job in reversed(list(_ENGINE_JOBS.values())):
            if (
                job.get('kind', 'install') == 'install'
                and job.get('id') == engine_id
                and job.get('state') in ENGINE_TRANSIENT_STATES
            ):
                return job['state']
    record = _load_engines_state()['engines'].get(engine_id) or {}
    state = record.get('state', 'absent')
    return state if state in ENGINE_STATES else 'absent'


def _local_engine_record():
    return _load_engines_state()['engines'].get(LOCAL_ENGINE_ID) or {}


def _local_engine_runtime_ready():
    return bool(_local_engine_record().get('runtimeReady'))


def _vlm_python_path():
    override = os.environ.get('STAMP_VLM_VENV')
    if override:
        return os.path.abspath(os.path.expanduser(override))
    if platform.system() == 'Windows':
        return os.path.join(_engine_dir(), 'venv', 'Scripts', 'python.exe')
    return os.path.join(_engine_dir(), 'venv', 'bin', 'python')


def _engine_probe_payload(force_hardware=False):
    payload = _hardware_probe(force=force_hardware)
    state = _load_engines_state()
    local_state = _engine_public_state(LOCAL_ENGINE_ID)
    runtime_ready = _local_engine_runtime_ready()
    payload.update({
        'active': state['active'],
        'engines': [
            {
                'id': 'rapidocr',
                'state': 'ready',
                'sizeMb': 0,
                'runtimeReady': bool(ocr is not None or ocr_loading),
            },
            {
                'id': LOCAL_ENGINE_ID,
                'state': local_state,
                'sizeMb': LOCAL_ENGINE_SIZE_MB
                if local_state == 'ready' else 0,
                'runtimeReady': runtime_ready,
            },
        ],
    })
    return payload


def _update_job(job_id, **fields):
    with _ENGINE_JOBS_LOCK:
        job = _ENGINE_JOBS.get(job_id)
        if job is not None:
            job.update(fields)
            return job
    return None


def _job_public(job):
    if job.get('kind') == 'runtime':
        return {
            'kind': 'runtime',
            'state': job.get('state', 'queued'),
            'progress': round(float(job.get('progress') or 0.0), 4),
            'stage': job.get('stage'),
            'lineCount': int(job.get('lineCount') or 0),
            'error': job.get('error'),
        }
    speed = float(job.get('speedMbps') or 0.0)
    eta = job.get('etaSec')
    return {
        'state': job.get('state', 'queued'),
        'progress': round(float(job.get('progress') or 0.0), 4),
        'downloadedMb': round(float(job.get('downloaded') or 0) / (1024 ** 2), 1),
        'totalMb': round(LOCAL_ENGINE_MODEL_BYTES / (1024 ** 2), 1),
        'speedMbps': round(speed, 2),
        'etaSec': int(eta) if eta is not None else None,
        'error': job.get('error'),
        'resumedFromMb': round(float(job.get('resumedFrom') or 0) / (1024 ** 2), 1),
    }


def _engine_manifest_payload(job, state):
    files = dict(job.get('files') or {})
    download_dir = os.path.join(_engine_dir(), 'tmp')
    downloaded = 0
    try:
        for name in os.listdir(download_dir):
            path = os.path.join(download_dir, name)
            if os.path.isfile(path):
                downloaded += os.path.getsize(path)
    except OSError:
        pass
    downloaded = min(downloaded, LOCAL_ENGINE_MODEL_BYTES)
    installed_at = job.get('installedAt') or _now_iso()
    return {
        'id': LOCAL_ENGINE_ID,
        'version': LOCAL_ENGINE_VERSION,
        'state': state,
        'source': LOCAL_ENGINE_BASE_URL,
        'totalBytes': LOCAL_ENGINE_MODEL_BYTES,
        'downloaded': downloaded,
        'sha256': None,
        'etag': job.get('etag'),
        'files': files,
        'missingFiles': list(job.get('missingFiles') or []),
        'runtimeReady': False,
        'startedAt': job.get('startedAt') or installed_at,
        'installedAt': installed_at if state == 'ready' else None,
    }


def _write_engine_manifest(job, state):
    path = os.path.join(_engine_dir(), 'manifest.json')
    _atomic_write_json(path, _engine_manifest_payload(job, state))
    job['lastManifestWrite'] = time.monotonic()


def _set_engine_record(engine_id, **fields):
    with _ENGINE_LOCK:
        state = _read_engines_state_unlocked()
        record = state['engines'].get(engine_id) or {}
        record.update(fields)
        state['engines'][engine_id] = record
        if state['active'] == engine_id and record.get('state') != 'ready':
            state['active'] = 'rapidocr'
        _atomic_write_json(_engines_state_path(), state)


def _job_part_layout():
    base, remainder = divmod(LOCAL_ENGINE_MODEL_BYTES, LOCAL_ENGINE_SEGMENTS)
    layout = []
    cursor = 0
    for index in range(LOCAL_ENGINE_SEGMENTS):
        length = base + (1 if index < remainder else 0)
        layout.append((index, cursor, cursor + length - 1, length))
        cursor += length
    return layout


def _part_sizes(job):
    sizes = []
    for index, _start, _end, _length in job['segments']:
        path = os.path.join(job['tmpDir'], f'{index}.part')
        try:
            sizes.append(os.path.getsize(path))
        except OSError:
            sizes.append(0)
    return sizes


def _publish_download_progress(job):
    with job['progressLock']:
        downloaded = min(sum(_part_sizes(job)), LOCAL_ENGINE_MODEL_BYTES)
        now = time.monotonic()
        samples = job['speedSamples']
        if job['lastSpeedSample'] == 0.0:
            job['lastSpeedSample'] = now
        elif now - job['lastSpeedSample'] >= 0.5:
            samples.append((now, downloaded))
            job['lastSpeedSample'] = now
            cutoff = now - 10.0
            while len(samples) > 2 and samples[0][0] < cutoff:
                samples.pop(0)
            if len(samples) >= 2:
                first, last = samples[0], samples[-1]
                elapsed = last[0] - first[0]
                if elapsed > 0:
                    job['speedMbps'] = max(0.0, (last[1] - first[1]) / elapsed / 1000000)
            remaining = max(0, LOCAL_ENGINE_MODEL_BYTES - downloaded)
            bytes_per_sec = job['speedMbps'] * 1000000
            job['etaSec'] = int(remaining / bytes_per_sec) if bytes_per_sec > 0 else None

        job['downloaded'] = downloaded
        job['progress'] = downloaded / LOCAL_ENGINE_MODEL_BYTES
        with _ENGINE_JOBS_LOCK:
            with _ENGINE_LOCK:
                current = _ENGINE_JOBS.get(job['jobId'])
                if current is not None:
                    current.update({
                        'downloaded': job['downloaded'],
                        'progress': job['progress'],
                        'speedMbps': job.get('speedMbps', 0.0),
                        'etaSec': (
                            job.get('etaSec')
                            if current.get('state') in ENGINE_RUNNING_STATES
                            else None
                        ),
                    })

        if now - job['lastManifestWrite'] >= 2.0:
            try:
                _write_engine_manifest(job, 'downloading')
            except OSError:
                pass


def _download_segment(job, index, start, end, length, errors):
    path = os.path.join(job['tmpDir'], f'{index}.part')
    try:
        existing = os.path.getsize(path) if os.path.exists(path) else 0
        if existing > length:
            with open(path, 'r+b') as f:
                f.truncate(length)
            existing = length
        if existing == length:
            _publish_download_progress(job)
            return

        range_start = start + existing
        request = urllib.request.Request(
            LOCAL_ENGINE_BASE_URL + 'model.safetensors',
            headers={
                'Range': f'bytes={range_start}-{end}',
                'User-Agent': f'StampCompareToolkit/{APP_VERSION}',
            },
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            status = getattr(response, 'status', response.getcode())
            if existing > 0 and status != 206:
                raise RuntimeError('服务器未按 Range 返回续传数据')
            if status not in (200, 206):
                raise RuntimeError(f'下载响应异常 HTTP {status}')
            etag = (
                response.headers.get('x-linked-etag')
                or response.headers.get('ETag')
            )
            if etag:
                with _ENGINE_JOBS_LOCK:
                    if not job.get('etag'):
                        job['etag'] = etag.strip()

            remaining = length - existing
            with open(path, 'ab', buffering=0) as f:
                while remaining > 0:
                    if job['cancelEvent'].is_set():
                        raise _EngineCancelled()
                    chunk = response.read(min(256 * 1024, remaining))
                    if not chunk:
                        break
                    f.write(chunk)
                    remaining -= len(chunk)
                    _publish_download_progress(job)

        final_size = os.path.getsize(path)
        if final_size != length:
            raise RuntimeError(
                f'分片 {index} 大小不完整: {final_size}/{length}'
            )
        _publish_download_progress(job)
    except _EngineCancelled:
        return
    except Exception as exc:
        errors.append(exc)


class _EngineCancelled(Exception):
    pass


def _download_model_parts(job):
    started_parts = 0
    for _index, _start, _end, length in job['segments']:
        path = os.path.join(job['tmpDir'], f'{_index}.part')
        if os.path.exists(path):
            started_parts += min(os.path.getsize(path), length)
    job['resumedFrom'] = started_parts
    job['downloaded'] = started_parts
    job['progress'] = started_parts / LOCAL_ENGINE_MODEL_BYTES
    _publish_download_progress(job)

    for attempt in range(1, 4):
        pending = []
        for index, start, end, length in job['segments']:
            path = os.path.join(job['tmpDir'], f'{index}.part')
            have = os.path.getsize(path) if os.path.exists(path) else 0
            if have != length:
                pending.append((index, start, end, length))
        if not pending:
            break
        errors = []
        threads = []
        for index, start, end, length in pending:
            thread = threading.Thread(
                target=_download_segment,
                args=(job, index, start, end, length, errors),
                daemon=True,
            )
            thread.start()
            threads.append(thread)
        for thread in threads:
            thread.join()
        if job['cancelEvent'].is_set():
            raise _EngineCancelled()
        if errors:
            job['retryCount'] = attempt
            if attempt == 3:
                raise RuntimeError(str(errors[0]))
            time.sleep(min(2 * attempt, 5))
    if sum(_part_sizes(job)) != LOCAL_ENGINE_MODEL_BYTES:
        missing = [index for index, _s, _e, length in job['segments']
                   if os.path.getsize(os.path.join(job['tmpDir'], f'{index}.part')) != length]
        raise RuntimeError(f'下载分片不完整: {missing[:5]}')


def _download_optional_file(job, filename):
    request = urllib.request.Request(
        LOCAL_ENGINE_BASE_URL + filename,
        headers={'User-Agent': f'StampCompareToolkit/{APP_VERSION}'},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            etag = (
                response.headers.get('x-linked-etag')
                or response.headers.get('ETag')
            )
            if etag and not job.get('etag'):
                job['etag'] = etag.strip()
            destination = os.path.join(job['modelDir'], filename)
            temporary = destination + '.download'
            size = 0
            with open(temporary, 'wb') as f:
                while True:
                    if job['cancelEvent'].is_set():
                        raise _EngineCancelled()
                    chunk = response.read(256 * 1024)
                    if not chunk:
                        break
                    f.write(chunk)
                    size += len(chunk)
            os.replace(temporary, destination)
            job['files'][filename] = {
                'bytes': size,
                'etag': etag.strip() if etag else None,
            }
            return True
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            job['missingFiles'].append(filename)
            return False
        raise RuntimeError(f'{filename} 下载失败: HTTP {exc.code}') from exc


def _download_engine_files(job):
    stop_monitor = threading.Event()

    def progress_monitor():
        while not stop_monitor.wait(0.5):
            if job['cancelEvent'].is_set():
                return
            _publish_download_progress(job)

    monitor = threading.Thread(target=progress_monitor, daemon=True)
    monitor.start()
    try:
        _download_model_parts(job)
        for filename in LOCAL_ENGINE_FILES:
            if filename != 'model.safetensors':
                _download_optional_file(job, filename)
        _publish_download_progress(job)
    finally:
        stop_monitor.set()
        monitor.join(timeout=1.0)


def _assemble_model_file(job):
    final_path = os.path.join(job['modelDir'], 'model.safetensors')
    assembling_path = final_path + '.assembling'
    try:
        with open(assembling_path, 'wb') as output:
            for index, _start, _end, length in job['segments']:
                part_path = os.path.join(job['tmpDir'], f'{index}.part')
                if os.path.getsize(part_path) != length:
                    raise RuntimeError(f'分片 {index} 校验失败')
                with open(part_path, 'rb') as part:
                    while True:
                        if job['cancelEvent'].is_set():
                            raise _EngineCancelled()
                        chunk = part.read(4 * 1024 * 1024)
                        if not chunk:
                            break
                        output.write(chunk)
        actual_size = os.path.getsize(assembling_path)
        if actual_size != LOCAL_ENGINE_MODEL_BYTES:
            raise RuntimeError(
                f'模型总大小校验失败: {actual_size}/{LOCAL_ENGINE_MODEL_BYTES}'
            )
        os.replace(assembling_path, final_path)
        job['files']['model.safetensors'] = {
            'bytes': LOCAL_ENGINE_MODEL_BYTES,
            'etag': job.get('etag'),
        }
    finally:
        if os.path.exists(assembling_path):
            try:
                os.unlink(assembling_path)
            except OSError:
                pass


def _engine_install_worker(job):
    job_id = job['jobId']
    try:
        _update_job(job_id, state='downloading', error=None)
        _set_engine_record(
            LOCAL_ENGINE_ID, state='downloading', sizeMb=0, error=None,
            manifest=f'engines/{LOCAL_ENGINE_ID}/manifest.json',
        )
        _write_engine_manifest(job, 'downloading')
        _download_engine_files(job)
        if job['cancelEvent'].is_set():
            raise _EngineCancelled()

        _update_job(job_id, state='verifying')
        _set_engine_record(LOCAL_ENGINE_ID, state='verifying', sizeMb=0)
        _write_engine_manifest(job, 'verifying')
        _assemble_model_file(job)

        _update_job(job_id, state='installing')
        _set_engine_record(LOCAL_ENGINE_ID, state='installing', sizeMb=0)
        _write_engine_manifest(job, 'installing')
        installed_at = _now_iso()
        job['installedAt'] = installed_at
        _set_engine_record(
            LOCAL_ENGINE_ID,
            state='ready',
            version=LOCAL_ENGINE_VERSION,
            sizeMb=LOCAL_ENGINE_SIZE_MB,
            installedAt=installed_at,
            manifest=f'engines/{LOCAL_ENGINE_ID}/manifest.json',
            error=None,
        )
        _write_engine_manifest(job, 'ready')
        try:
            shutil.rmtree(job['tmpDir'])
        except OSError:
            pass
        _update_job(
            job_id, state='ready', progress=1.0,
            downloaded=LOCAL_ENGINE_MODEL_BYTES, etaSec=0,
        )
    except _EngineCancelled:
        try:
            _write_engine_manifest(job, 'absent')
        except OSError:
            pass
        _set_engine_record(LOCAL_ENGINE_ID, state='absent', sizeMb=0, error=None)
        _update_job(job_id, state='cancelled', etaSec=None)
    except Exception as exc:
        if job['cancelEvent'].is_set():
            try:
                _write_engine_manifest(job, 'absent')
            except OSError:
                pass
            _set_engine_record(LOCAL_ENGINE_ID, state='absent', sizeMb=0, error=None)
            _update_job(job_id, state='cancelled', etaSec=None)
            return
        try:
            _write_engine_manifest(job, 'failed')
        except OSError:
            pass
        _set_engine_record(
            LOCAL_ENGINE_ID, state='failed', sizeMb=0, error=str(exc),
        )
        _update_job(job_id, state='failed', error=str(exc), etaSec=None)


def _runtime_error_message(stage, output):
    text = str(output or '')
    lowered = text.lower()
    if any(token in lowered for token in (
        'no space left', 'disk full', 'enospc', 'not enough space',
    )):
        category = '磁盘空间不足'
    elif any(token in lowered for token in (
        'connection', 'network', 'timed out', 'timeout', 'ssl',
        'temporary failure', 'proxy', 'could not find a version',
        'failed to establish', 'readtimeout', '503',
    )):
        category = '网络错误'
    elif any(token in lowered for token in (
        'failed building wheel', 'error: subprocess-exited-with-error',
        'compiler', 'clang:', 'linker command failed',
    )):
        category = '编译失败'
    else:
        category = '安装错误'
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    detail = lines[-1][:300] if lines else '未提供详细错误'
    return f'{stage}失败（{category}）: {detail}'


def _run_runtime_command(job, command, stage, deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError('运行时安装超过 1800 秒')

    _update_job(job['jobId'], stage=stage)
    env = os.environ.copy()
    env['PYTHONUNBUFFERED'] = '1'
    env['PIP_DISABLE_PIP_VERSION_CHECK'] = '1'
    try:
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding='utf-8',
            errors='replace',
            bufsize=1,
            env=env,
        )
    except OSError as exc:
        raise RuntimeError(_runtime_error_message(stage, str(exc))) from exc

    output_lines = []

    def read_output():
        for raw_line in process.stdout:
            line = raw_line.rstrip()
            if not line:
                continue
            output_lines.append(line)
            if len(output_lines) > 400:
                del output_lines[:100]
            with _ENGINE_JOBS_LOCK:
                current = _ENGINE_JOBS.get(job['jobId'])
                if current is not None:
                    current['lineCount'] = int(current.get('lineCount') or 0) + 1
                    # pip does not expose a reliable total line count. Advance
                    # monotonically and reserve the final 6% for verification.
                    current['progress'] = min(
                        0.94,
                        0.05 + min(current['lineCount'], 500) / 500 * 0.89,
                    )

    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    timed_out = False
    cancelled = False
    while process.poll() is None:
        if job['cancelEvent'].is_set():
            cancelled = True
            process.terminate()
            break
        if time.monotonic() >= deadline:
            timed_out = True
            process.terminate()
            break
        time.sleep(0.1)

    if process.poll() is None:
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)
    reader.join(timeout=2)
    if process.stdout:
        process.stdout.close()
    output = '\n'.join(output_lines)
    if cancelled:
        raise _EngineCancelled()
    if timed_out:
        raise TimeoutError('运行时安装超过 1800 秒')
    if process.returncode != 0:
        raise RuntimeError(_runtime_error_message(stage, output))
    return output


def _write_runtime_manifest(runtime_ready, runtime=None, error=None):
    path = _engine_manifest_path()
    try:
        with open(path, 'r', encoding='utf-8') as f:
            manifest = json.load(f)
        if not isinstance(manifest, dict):
            manifest = {}
    except (OSError, ValueError, TypeError):
        manifest = {
            'id': LOCAL_ENGINE_ID,
            'version': LOCAL_ENGINE_VERSION,
            'state': 'ready',
        }
    manifest['runtimeReady'] = bool(runtime_ready)
    manifest['runtime'] = runtime
    if error:
        manifest['runtimeError'] = str(error)
    else:
        manifest.pop('runtimeError', None)
    _atomic_write_json(path, manifest)



def _find_uv():
    """优先使用 uv（安装速度通常快一个数量级），找不到时回落到 pip。"""
    candidates = [os.environ.get('STAMP_UV'), shutil.which('uv')]
    candidates += [
        os.path.expanduser('~/.local/bin/uv'),
        os.path.expanduser('~/.cargo/bin/uv'),
        '/opt/homebrew/bin/uv',
        '/usr/local/bin/uv',
    ]
    for path in candidates:
        if path and os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    return None

def _runtime_install_worker(job):
    job_id = job['jobId']
    deadline = time.monotonic() + 1800
    installed_at = _now_iso()
    try:
        _update_job(job_id, state='installing', stage='creating-venv', progress=0.02)
        engine_dir = _engine_dir()
        venv_dir = os.path.join(engine_dir, 'venv')
        os.makedirs(engine_dir, exist_ok=True)
        uv_path = _find_uv()
        if uv_path:
            _run_runtime_command(
                job,
                [uv_path, 'venv', '--python', sys.executable, '--allow-existing', venv_dir],
                '创建虚拟环境',
                deadline,
            )
        else:
            _run_runtime_command(
                job,
                [sys.executable, '-m', 'venv', venv_dir],
                '创建虚拟环境',
                deadline,
            )
        venv_python = (
            os.path.join(venv_dir, 'Scripts', 'python.exe')
            if platform.system() == 'Windows'
            else os.path.join(venv_dir, 'bin', 'python')
        )
        if not os.path.isfile(venv_python):
            raise RuntimeError('虚拟环境创建失败（未生成 python 可执行文件）')

        if not uv_path:
            _run_runtime_command(
                job,
                [venv_python, '-m', 'pip', 'install', '--upgrade', 'pip'],
                '升级 pip',
                deadline,
            )
        mirrors = [None]
        env_index = os.environ.get('STAMP_PIP_INDEX')
        if env_index:
            mirrors.insert(0, env_index)
        mirrors += [
            'https://pypi.tuna.tsinghua.edu.cn/simple',
            'https://mirrors.aliyun.com/pypi/simple',
        ]
        last_error = None
        installed_ok = False
        for mirror in mirrors:
            if uv_path:
                command = [
                    uv_path, 'pip', 'install', '--python', venv_python,
                    'mlx-vlm>=0.3.11', 'pillow',
                ]
                if mirror:
                    command += ['--index-url', mirror]
            else:
                command = [
                    venv_python, '-m', 'pip', 'install',
                    '--timeout', '120', '--retries', '8',
                    'mlx-vlm>=0.3.11', 'pillow',
                ]
                if mirror:
                    command[5:5] = ['-i', mirror]
            try:
                _update_job(job_id, stage='installing-packages', progress=0.3)
                _run_runtime_command(job, command, '安装 mlx-vlm 与 pillow', deadline)
                installed_ok = True
                break
            except _EngineCancelled:
                raise
            except Exception as exc:
                last_error = exc
        if not installed_ok:
            raise RuntimeError(str(last_error))

        _update_job(job_id, stage='verifying-runtime', progress=0.96)
        version_result = subprocess.run(
            [
                venv_python, '-c',
                "import importlib.metadata as m; print(m.version('mlx-vlm'))",
            ],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        if version_result.returncode != 0:
            raise RuntimeError(
                _runtime_error_message(
                    '验证运行时',
                    version_result.stdout + version_result.stderr,
                )
            )
        mlx_vlm_version = version_result.stdout.strip()
        if not mlx_vlm_version:
            raise RuntimeError('验证运行时失败（无法读取 mlx-vlm 版本）')

        runtime = {
            'python': venv_python,
            'mlxVlmVersion': mlx_vlm_version,
            'installedAt': installed_at,
        }
        _write_runtime_manifest(True, runtime=runtime)
        _set_engine_record(
            LOCAL_ENGINE_ID,
            runtimeReady=True,
            runtime=runtime,
            runtimeError=None,
        )
        _update_job(
            job_id,
            state='ready',
            progress=1.0,
            stage='ready',
            error=None,
        )
    except _EngineCancelled:
        _update_job(job_id, state='cancelled', stage='cancelled')
    except Exception as exc:
        error = str(exc)
        try:
            _write_runtime_manifest(False, runtime=None, error=error)
        except OSError:
            pass
        _set_engine_record(
            LOCAL_ENGINE_ID,
            runtimeReady=False,
            runtimeError=error,
        )
        _update_job(job_id, state='failed', error=error, stage='failed')


def _start_engine_runtime():
    model_path = os.path.join(_engine_model_dir(), 'model.safetensors')
    if not os.path.isfile(model_path):
        raise RuntimeError('模型文件尚未安装，请先安装本地视觉模型')

    with _ENGINE_JOBS_LOCK:
        for running_job in _ENGINE_JOBS.values():
            if (
                running_job.get('kind') == 'runtime'
                and running_job.get('state') in ENGINE_RUNNING_STATES
            ):
                return running_job['jobId']

        job_id = str(uuid.uuid4())
        job = {
            'jobId': job_id,
            'id': LOCAL_ENGINE_ID,
            'kind': 'runtime',
            'state': 'queued',
            'progress': 0.0,
            'stage': 'queued',
            'lineCount': 0,
            'error': None,
            'cancelEvent': threading.Event(),
        }
        thread = threading.Thread(
            target=_runtime_install_worker,
            args=(job,),
            daemon=True,
        )
        job['thread'] = thread
        _ENGINE_JOBS[job_id] = job
    thread.start()
    return job_id


def _start_engine_install(engine_id):
    if engine_id != LOCAL_ENGINE_ID:
        raise ValueError(f'不支持的引擎: {engine_id}')
    probe = _engine_probe_payload(force_hardware=True)
    if probe['support'] == 'unsupported':
        return None

    with _ENGINE_JOBS_LOCK:
        for job in _ENGINE_JOBS.values():
            if (
                job.get('kind', 'install') == 'install'
                and job.get('id') == engine_id
                and job.get('state') in ENGINE_RUNNING_STATES
            ):
                return job['jobId']
    if not _wait_engine_threads(engine_id, timeout=8.0):
        raise RuntimeError('上一次安装正在停止，请稍后重试')

    state = _load_engines_state()
    record = state['engines'].get(engine_id) or {}
    model_path = os.path.join(_engine_dir(), 'model', 'model.safetensors')
    if (
        record.get('state') == 'ready'
        and os.path.isfile(model_path)
        and os.path.getsize(model_path) == LOCAL_ENGINE_MODEL_BYTES
    ):
        job_id = str(uuid.uuid4())
        with _ENGINE_JOBS_LOCK:
            _ENGINE_JOBS[job_id] = {
                'state': 'ready', 'progress': 1.0,
                'downloaded': LOCAL_ENGINE_MODEL_BYTES,
                'speedMbps': 0.0, 'etaSec': 0, 'error': None,
                'id': engine_id, 'jobId': job_id, 'kind': 'install',
            }
        return job_id

    job_id = str(uuid.uuid4())
    engine_dir = _engine_dir()
    tmp_dir = os.path.join(engine_dir, 'tmp')
    model_dir = os.path.join(engine_dir, 'model')
    os.makedirs(tmp_dir, exist_ok=True)
    os.makedirs(model_dir, exist_ok=True)
    job = {
        'jobId': job_id,
        'id': engine_id,
        'kind': 'install',
        'state': 'queued',
        'progress': 0.0,
        'downloaded': 0,
        'speedMbps': 0.0,
        'etaSec': None,
        'error': None,
        'resumedFrom': 0,
        'segments': _job_part_layout(),
        'tmpDir': tmp_dir,
        'modelDir': model_dir,
        'cancelEvent': threading.Event(),
        'progressLock': threading.Lock(),
        'speedSamples': [],
        'lastSpeedSample': 0.0,
        'lastManifestWrite': 0.0,
        'etag': None,
        'files': {},
        'missingFiles': [],
        'startedAt': _now_iso(),
        'installedAt': None,
    }
    with _ENGINE_JOBS_LOCK:
        _ENGINE_JOBS[job_id] = job
    thread = threading.Thread(
        target=_engine_install_worker, args=(job,), daemon=True,
    )
    job['thread'] = thread
    thread.start()
    return job_id


def _cancel_engine_job(job_id):
    with _ENGINE_JOBS_LOCK:
        job = _ENGINE_JOBS.get(job_id)
        if job is None:
            return False
        event = job.get('cancelEvent')
        thread = job.get('thread')
        if event:
            event.set()
        if job.get('state') in ENGINE_RUNNING_STATES:
            job['state'] = 'cancelled'
            job['etaSec'] = None
    if thread and thread.is_alive():
        thread.join(timeout=6.0)
    return True


def _wait_engine_threads(engine_id, timeout=8.0):
    """等待已取消任务的所有下载线程退出，避免新旧任务同时写分片。"""
    deadline = time.monotonic() + timeout
    with _ENGINE_JOBS_LOCK:
        threads = [
            job.get('thread') for job in _ENGINE_JOBS.values()
            if job.get('id') == engine_id and job.get('thread') is not None
        ]
    for thread in threads:
        remaining = max(0.0, deadline - time.monotonic())
        if thread.is_alive() and remaining > 0:
            thread.join(timeout=remaining)
    return not any(thread.is_alive() for thread in threads)


def _engine_job_payload(job_id):
    with _ENGINE_JOBS_LOCK:
        job = _ENGINE_JOBS.get(job_id)
        if job is None:
            return None
        return _job_public(dict(job))


def _remove_engine(engine_id):
    if engine_id != LOCAL_ENGINE_ID:
        raise ValueError(f'不支持的引擎: {engine_id}')
    with _ENGINE_JOBS_LOCK:
        running = [
            job_id for job_id, job in _ENGINE_JOBS.items()
            if (
                job.get('id') == engine_id
                and job.get('state') in ENGINE_RUNNING_STATES
            )
        ]
    for job_id in running:
        _cancel_engine_job(job_id)
    if not _wait_engine_threads(engine_id):
        raise RuntimeError('引擎安装线程尚未停止，请稍后重试')
    engine_dir = _engine_dir(engine_id)
    if os.path.isdir(engine_dir):
        shutil.rmtree(engine_dir)
    with _ENGINE_LOCK:
        state = _read_engines_state_unlocked()
        state['engines'].pop(engine_id, None)
        if state['active'] == engine_id:
            state['active'] = 'rapidocr'
        _atomic_write_json(_engines_state_path(), state)


def _copy_or_link(source, destination):
    try:
        os.symlink(os.path.abspath(source), destination)
    except OSError:
        shutil.copy2(source, destination)


def _import_engine(engine_id, source_dir):
    if engine_id != LOCAL_ENGINE_ID:
        raise ValueError(f'不支持的引擎: {engine_id}')
    if not isinstance(source_dir, str) or not os.path.isabs(source_dir):
        raise ValueError('dir 必须是绝对路径')
    source_dir = os.path.abspath(source_dir)
    model_source = os.path.join(source_dir, 'model.safetensors')
    config_source = os.path.join(source_dir, 'config.json')
    if not os.path.isfile(model_source) or not os.path.isfile(config_source):
        raise ValueError('目录内必须存在 model.safetensors 与 config.json')
    model_size = os.path.getsize(model_source)
    if model_size != LOCAL_ENGINE_MODEL_BYTES:
        raise ValueError(
            f'model.safetensors 大小应为 {LOCAL_ENGINE_MODEL_BYTES} 字节，'
            f'实际为 {model_size} 字节'
        )

    with _ENGINE_JOBS_LOCK:
        running = [
            job_id for job_id, job in _ENGINE_JOBS.items()
            if (
                job.get('id') == engine_id
                and job.get('state') in ENGINE_RUNNING_STATES
            )
        ]
    for job_id in running:
        _cancel_engine_job(job_id)
    if not _wait_engine_threads(engine_id):
        raise RuntimeError('引擎安装线程尚未停止，请稍后重试')

    engine_dir = _engine_dir(engine_id)
    if os.path.isdir(engine_dir):
        shutil.rmtree(engine_dir)
    model_dir = os.path.join(engine_dir, 'model')
    os.makedirs(model_dir, exist_ok=True)
    files = {}
    for filename in LOCAL_ENGINE_FILES:
        source = os.path.join(source_dir, filename)
        if not os.path.isfile(source):
            continue
        destination = os.path.join(model_dir, filename)
        _copy_or_link(source, destination)
        files[filename] = {
            'bytes': os.path.getsize(source),
            'etag': None,
            'source': source,
        }
    installed_at = _now_iso()
    manifest = {
        'id': LOCAL_ENGINE_ID,
        'version': LOCAL_ENGINE_VERSION,
        'state': 'ready',
        'source': 'import',
        'importedFrom': source_dir,
        'totalBytes': LOCAL_ENGINE_MODEL_BYTES,
        'downloaded': LOCAL_ENGINE_MODEL_BYTES,
        'sha256': None,
        'etag': None,
        'files': files,
        'missingFiles': [
            name for name in LOCAL_ENGINE_FILES
            if name not in files
        ],
        'runtimeReady': False,
        'installedAt': installed_at,
    }
    _atomic_write_json(os.path.join(engine_dir, 'manifest.json'), manifest)
    _set_engine_record(
        engine_id,
        state='ready',
        version=LOCAL_ENGINE_VERSION,
        sizeMb=LOCAL_ENGINE_SIZE_MB,
        installedAt=installed_at,
        manifest=f'engines/{engine_id}/manifest.json',
        error=None,
    )


def _activate_engine(engine_id):
    if engine_id not in ('rapidocr', LOCAL_ENGINE_ID, 'cloud'):
        raise ValueError(f'不支持的引擎: {engine_id}')
    if engine_id != 'cloud' and _engine_public_state(engine_id) != 'ready':
        raise RuntimeError('目标引擎尚未就绪')
    with _ENGINE_LOCK:
        state = _read_engines_state_unlocked()
        if engine_id == LOCAL_ENGINE_ID:
            local = state['engines'].get(engine_id) or {}
            if local.get('state') != 'ready':
                raise RuntimeError('目标引擎尚未就绪')
        state['active'] = engine_id
        _atomic_write_json(_engines_state_path(), state)
    return engine_id


def rank_library_candidates(text, limit=3):
    """Return normalized library names ordered by edit/run similarity."""
    observed = normalize_stamp_text(text)
    if not observed:
        return []
    ranked = [
        (_text_similarity(observed, name), name)
        for name in _load_stamp_library()
    ]
    ranked.sort(key=lambda item: item[0], reverse=True)
    return [
        {'name': name, 'score': round(float(score), 4), 'source': 'library'}
        for score, name in ranked[:max(1, int(limit))]
    ]


def _red_pixel_fraction(img):
    hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
    h, s, v = cv2.split(hsv)
    red = (((h < 10) | (h > 170)) & (s > 60) & (v > 50))
    return float(red.mean())


def _extract_ring_strip(img, cx, cy, radius, low_color=False):
    strip = unwrap_stamp_ring(img, cx, cy, radius)
    b, g, r = cv2.split(strip.astype(np.float32))
    enhanced = r - (g + b) * 0.5
    if low_color:
        gray = cv2.cvtColor(strip, cv2.COLOR_BGR2GRAY)
        ink = (gray < 175) | ((enhanced > 10) & (r > 50))
    else:
        ink = (enhanced > 20) & (r > 70)
    ink_u8 = cv2.morphologyEx(
        ink.astype(np.uint8), cv2.MORPH_CLOSE, np.ones((2, 2), np.uint8),
    )
    result = np.full_like(strip, 255)
    selected = ink_u8 > 0
    result[selected] = strip[selected]
    return result


def _split_text_bands(strip):
    """Horizontal projection -> up to three radial text bands."""
    h, _ = strip.shape[:2]
    b, g, r = cv2.split(strip.astype(np.float32))
    ink = ((r - (g + b) * 0.5) > 12) & (r > 50)
    density = ink.mean(axis=1)
    smooth = np.convolve(density, np.ones(5) / 5.0, mode='same')
    threshold = max(0.06, float(np.median(density) + 0.06), float(smooth.max() * 0.12))
    active = (smooth > threshold) & (density < 0.82)
    runs = []
    row = 0
    while row < h:
        if not active[row]:
            row += 1
            continue
        end = row + 1
        while end < h and active[end]:
            end += 1
        if runs and row - runs[-1][1] <= 4:
            runs[-1][1] = end
        else:
            runs.append([row, end])
        row = end
    bands = [(max(0, a - 5), min(h, bnd + 5)) for a, bnd in runs if bnd - a >= 5]
    if len(bands) < 2:
        bands = [(0, min(h, 46)), (max(0, int(h * 0.32)), min(h, int(h * 0.73))),
                 (max(0, int(h * 0.58)), h)]
    return bands[:3]


def _binarize_stamp_line(img, method, polarity='positive'):
    gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    source = 255 - gray if polarity == 'positive' else gray
    if method == 'otsu':
        _, binary = cv2.threshold(source, 0, 255, cv2.THRESH_BINARY + cv2.THRESH_OTSU)
    else:
        binary = cv2.adaptiveThreshold(
            source, 255, cv2.ADAPTIVE_THRESH_GAUSSIAN_C,
            cv2.THRESH_BINARY, 41, 8,
        )
    return cv2.copyMakeBorder(binary, 20, 20, 20, 20, cv2.BORDER_CONSTANT, value=255)


def _ocr_projected_lines(strip, bands, existing_items):
    """OCR each projection band independently with at most four combinations."""
    combined = list(existing_items)
    calls = 0
    for y1, y2 in bands:
        band = strip[y1:y2]
        if band.size == 0:
            continue
        scale = min(2.5, max(1.0, 96.0 / max(1, band.shape[0])))
        line_img = cv2.resize(
            band, None, fx=scale, fy=scale,
            interpolation=cv2.INTER_CUBIC if scale > 1 else cv2.INTER_AREA,
        )
        if line_img.shape[1] > 1600:
            resize_scale = 1600.0 / line_img.shape[1]
            line_img = cv2.resize(
                line_img, None, fx=resize_scale, fy=resize_scale,
                interpolation=cv2.INTER_AREA,
            )
        line_best = 0.0
        for bin_method in ('otsu', 'adaptive'):
            for rotated in (False, True):
                work = cv2.rotate(line_img, cv2.ROTATE_180) if rotated else line_img
                try:
                    items = _run_ocr_detailed(_binarize_stamp_line(work, bin_method))
                except Exception:
                    items = []
                calls += 1
                for item in items:
                    item['source'] = f'line:{bin_method}:{"rot180" if rotated else "forward"}'
                combined.extend(items)
                _, candidates, _ = _best_ring_text(combined)
                line_best = candidates[0]['score'] if candidates else 0.0
                if line_best >= 0.68:
                    break
            if line_best >= 0.68:
                break
    return combined, calls


def _dedupe_ocr_items(items):
    best_by_text = {}
    for item in items:
        text = normalize_stamp_text(item['text'])
        if not text:
            continue
        existing = best_by_text.get(text)
        if existing is None or float(item.get('score', 0.0)) > float(existing.get('score', 0.0)):
            best_by_text[text] = item
    return list(best_by_text.values())


def _dedupe_ocr_texts(items, limit=6):
    """Normalize-dedupe OCR fragments, keeping the highest score for each text."""
    best_by_text = {}
    for item in items:
        text = normalize_stamp_text(item.get('text'))
        if len(text) < 2 or text.isdigit():
            continue
        score = float(item.get('score', 0.0))
        previous = best_by_text.get(text)
        if previous is None or score > previous['score']:
            best_by_text[text] = {'text': text, 'score': score}
    ranked = sorted(
        best_by_text.values(),
        key=lambda item: item['score'],
        reverse=True,
    )
    return ranked[:max(0, int(limit))]


def _merge_ordered_fragments(items):
    output = ''
    for item in items:
        text = normalize_stamp_text(item['text'])
        if not text or text in output:
            continue
        overlap = 0
        for size in range(min(len(output), len(text)), 1, -1):
            if output[-size:] == text[:size]:
                overlap = size
                break
        output += text[overlap:]
    return output


def _ring_text_variants(items):
    items = [item for item in _dedupe_ocr_items(items) if item['score'] >= 0.30]
    if not items:
        return []
    ordered = sorted(items, key=lambda item: (item['x'], item['y']))
    variants = []
    for sequence in (ordered, list(reversed(ordered))):
        for start in range(len(sequence)):
            cyclic = sequence[start:] + sequence[:start]
            text = _merge_ordered_fragments(cyclic)
            if text:
                variants.append(text)
    line_groups = {}
    for item in items:
        line_key = int(item['y'] // 24)
        line_groups.setdefault(line_key, []).append(item)
    for group in line_groups.values():
        line_text = _merge_ordered_fragments(sorted(group, key=lambda item: item['x']))
        if line_text:
            variants.append(line_text)
    return list(dict.fromkeys(variants))


def _best_ring_text(items):
    variants = _ring_text_variants(items)
    if not variants:
        return '', [], []
    candidates_by_variant = [(variant, rank_library_candidates(variant, 3)) for variant in variants]
    best_text, best_candidates = max(
        candidates_by_variant,
        key=lambda pair: (pair[1][0]['score'] if pair[1] else 0.0, len(pair[0])),
    )
    return best_text, best_candidates, variants


def _split_company_and_type(text):
    clean = normalize_stamp_text(text)
    seal_type = ''
    matches = list(_TYPE_RE.finditer(clean))
    if matches:
        seal_type = matches[-1].group(0)
        clean = clean[:matches[-1].start()] + clean[matches[-1].end():]
    return clean, seal_type


def _whole_image_color_fallback(img):
    scale = min(1.0, 1600.0 / max(img.shape[:2]))
    work = img if scale >= 1.0 else cv2.resize(
        img, None, fx=scale, fy=scale, interpolation=cv2.INTER_AREA,
    )
    gray = cv2.cvtColor(work, cv2.COLOR_BGR2GRAY)
    ink = np.full_like(work, 255)
    selected = gray < 205
    ink[selected] = work[selected]
    return _run_ocr_detailed(ink)


class _VlmBusyError(RuntimeError):
    pass


class _VlmCallError(RuntimeError):
    pass


def _clip_vlm_error(value):
    compact = re.sub(r'\s+', ' ', str(value or '')).strip()
    return compact[:120] or '本地视觉引擎调用失败'


def _vlm_preflight():
    """Return (call_config, None) or (None, fallback_reason)."""
    state = _load_engines_state()
    if state.get('active') != LOCAL_ENGINE_ID:
        return None, None

    record = state['engines'].get(LOCAL_ENGINE_ID) or {}
    if not bool(record.get('runtimeReady')):
        return None, '本地视觉引擎运行时未就绪'

    model_dir = _engine_model_dir()
    model_file = os.path.join(model_dir, 'model.safetensors')
    config_file = os.path.join(model_dir, 'config.json')
    if not os.path.isfile(model_file) or not os.path.isfile(config_file):
        return None, '本地视觉模型文件不完整'

    python_path = _vlm_python_path()
    if not os.path.isfile(python_path) or not os.access(python_path, os.X_OK):
        return None, f'VLM Python 不可执行: {python_path}'
    if not os.path.isfile(VLM_ENGINE_FILE):
        return None, f'推理脚本不存在: {VLM_ENGINE_FILE}'
    return {
        'model_dir': model_dir,
        'python': python_path,
    }, None


def _meaningful_vlm_text(raw):
    normalized = normalize_stamp_text(raw)
    return bool(normalized and any(ch.isalnum() for ch in normalized))


def _parse_vlm_stdout(stdout):
    text = str(stdout or '').strip()
    if not text:
        raise _VlmCallError('VLM 未返回 JSON')
    try:
        payload = json.loads(text)
    except json.JSONDecodeError:
        # Keep the stdout contract strict for vlm_engine.py, while still being
        # resilient to an unexpected trailing library diagnostic line.
        payload = None
        for line in reversed(text.splitlines()):
            candidate = line.strip()
            if not candidate.startswith('{'):
                continue
            try:
                payload = json.loads(candidate)
                break
            except json.JSONDecodeError:
                continue
        if payload is None:
            raise _VlmCallError(f'VLM 返回非 JSON: {_clip_vlm_error(text)}')
    if not isinstance(payload, dict):
        raise _VlmCallError('VLM JSON 结构无效')
    return payload


def _run_vlm_engine(img, cx, cy, radius, call_config):
    half_side = max(1, int(round(float(radius) * 1.20)))
    x1 = max(0, int(round(float(cx))) - half_side)
    y1 = max(0, int(round(float(cy))) - half_side)
    x2 = min(img.shape[1], int(round(float(cx))) + half_side)
    y2 = min(img.shape[0], int(round(float(cy))) + half_side)
    crop = img[y1:y2, x1:x2]
    if crop.size == 0:
        raise _VlmCallError('章区裁剪为空')

    fd, crop_path = tempfile.mkstemp(prefix='stamp-vlm-crop-', suffix='.png')
    os.close(fd)
    acquired = False
    try:
        if not cv2.imwrite(crop_path, crop):
            raise _VlmCallError('无法写入章区临时图片')
        acquired = _VLM_LOCK.acquire(timeout=_VLM_LOCK_TIMEOUT_SEC)
        if not acquired:
            raise _VlmBusyError('本地视觉引擎忙')

        started = time.perf_counter()
        command = [
            call_config['python'],
            VLM_ENGINE_FILE,
            '--model-dir', call_config['model_dir'],
            '--image', crop_path,
            '--prompt', 'OCR:',
            '--max-tokens', '64',
        ]
        try:
            completed = subprocess.run(
                command,
                capture_output=True,
                text=True,
                encoding='utf-8',
                errors='replace',
                timeout=120,
                check=False,
            )
        except subprocess.TimeoutExpired as exc:
            raise _VlmCallError('VLM 调用超时（120 秒）') from exc
        except OSError as exc:
            raise _VlmCallError(f'VLM 启动失败: {exc}') from exc

        payload = _parse_vlm_stdout(completed.stdout)
        if completed.returncode != 0 or not payload.get('ok'):
            reason = payload.get('error') or completed.stderr or f'exit {completed.returncode}'
            raise _VlmCallError(f'VLM 调用失败: {reason}')
        raw_text = str(payload.get('text') or '')
        if not _meaningful_vlm_text(raw_text):
            raise _VlmCallError('VLM 输出为空或仅包含符号')
        elapsed_ms = payload.get('elapsedMs')
        try:
            elapsed_ms = int(elapsed_ms)
        except (TypeError, ValueError):
            elapsed_ms = int(round((time.perf_counter() - started) * 1000))
        return raw_text, max(0, elapsed_ms)
    finally:
        if acquired:
            _VLM_LOCK.release()
        try:
            os.unlink(crop_path)
        except OSError:
            pass


def _build_vlm_processed(raw_text, eff_colors, confidence, method, elapsed_ms):
    normalized = normalize_stamp_text(raw_text)
    line_variants = []
    for line in re.split(r'[\r\n]+', raw_text or ''):
        value = normalize_stamp_text(line)
        if value and any(ch.isalnum() for ch in value):
            line_variants.append(value)
    variants = list(dict.fromkeys([normalized, *line_variants]))

    best_by_name = {}
    for variant in variants:
        for candidate in rank_library_candidates(variant, 3):
            previous = best_by_name.get(candidate['name'])
            if previous is None or candidate['score'] > previous['score']:
                best_by_name[candidate['name']] = candidate
    candidates = sorted(
        best_by_name.values(),
        key=lambda item: item.get('score', 0.0),
        reverse=True,
    )[:3]

    raw_company, seal_type = _split_company_and_type(normalized)
    top_score = float(candidates[0]['score']) if candidates else 0.0
    second_score = float(candidates[1]['score']) if len(candidates) > 1 else 0.0
    match_margin = round(top_score - second_score, 4)
    company = raw_company
    match_source = 'raw'
    needs_review = True
    if candidates and top_score >= 0.72 and match_margin >= 0.04:
        company = candidates[0]['name']
        match_source = 'library'
        needs_review = False
    elif candidates and top_score >= 0.55:
        candidates[0]['suggested'] = True

    texts = [
        {'text': value, 'score': 1.0}
        for value in dict.fromkeys(line_variants or [normalized])
        if value
    ]
    return {
        'texts': texts,
        'eff_colors': eff_colors,
        'candidates': candidates,
        'company': company,
        'raw_text': normalized,
        'raw_company': raw_company,
        'match_source': match_source,
        'needs_review': needs_review,
        'match_margin': match_margin,
        'type': seal_type,
        'confidence': float(confidence if confidence is not None else 0.0),
        'method': method or 'hough',
        'bands': 0,
        'line_ocr_calls': 0,
        'low_color': False,
        'engine': 'mlx-paddleocr-vl',
        'fallback': False,
        'elapsed_ms': int(elapsed_ms),
        'vlm_raw': str(raw_text or '').strip(),
    }


def process_stamp_ocr(img, cx, cy, radius, color, hues=None, excludes=None,
                      exclude_colors=None, confidence=None, method=None):
    """Process one stamp with local VLM-first OCR and RapidOCR fallback.

    The VLM receives a 1.20xR square crop around the detected stamp circle.
    Any call failure returns to the existing bounded full-ring RapidOCR path.
    """
    _, _, eff_colors = extract_ink_channel(img, color, hues, excludes, exclude_colors)
    vlm_config, vlm_error = _vlm_preflight()
    if vlm_config is not None:
        try:
            raw_text, vlm_elapsed_ms = _run_vlm_engine(
                img, cx, cy, radius, vlm_config,
            )
            return _build_vlm_processed(
                raw_text, eff_colors, confidence, method, vlm_elapsed_ms,
            )
        except _VlmBusyError:
            raise
        except Exception as exc:
            vlm_error = _clip_vlm_error(exc)

    low_color = _red_pixel_fraction(img) < 0.02
    strip = _extract_ring_strip(img, cx, cy, radius, low_color=low_color)
    bands = _split_text_bands(strip)
    all_items = []
    item_groups = []
    line_ocr_calls = 0
    best_score = 0.0

    completed_variants = 0
    variants = [
        (False, 'adaptive', 'positive'),
        (False, 'adaptive', 'negative'),
        (True, 'otsu', 'negative'),
        (True, 'otsu', 'positive'),
    ]
    for rotated, bin_method, polarity in variants:
        line_img = cv2.rotate(strip, cv2.ROTATE_180) if rotated else strip
        binary = _binarize_stamp_line(line_img, bin_method, polarity)
        try:
            items = _run_ocr_detailed(binary)
        except Exception:
            items = []
        source = f'{bin_method}:{polarity}:{"rot180" if rotated else "forward"}'
        for item in items:
            item['source'] = source
        all_items.extend(items)
        item_groups.append(items)
        completed_variants += 1
        _, candidates, _ = _best_ring_text(all_items)
        best_score = candidates[0]['score'] if candidates else 0.0
        # Both polarities must be sampled before a high-confidence result may stop OCR.
        if completed_variants >= 2 and best_score >= 0.90:
            break

    if best_score < 0.50:
        all_items, line_ocr_calls = _ocr_projected_lines(strip, bands, all_items)
        item_groups.append(all_items)
        _, line_candidates, _ = _best_ring_text(all_items)
        best_score = line_candidates[0]['score'] if line_candidates else best_score

    if low_color and best_score < 0.60:
        try:
            fallback_items = _whole_image_color_fallback(img)
            for item in fallback_items:
                item['source'] = 'gray-black-fallback'
            all_items.extend(fallback_items)
            item_groups.append(fallback_items)
        except Exception:
            pass

    best_raw, candidates, _ = _best_ring_text(all_items)
    for group in item_groups:
        group_raw, group_candidates, _ = _best_ring_text(group)
        if group_candidates and (
            not candidates or group_candidates[0]['score'] > candidates[0]['score']
        ):
            best_raw, candidates = group_raw, group_candidates
    raw_company, seal_type = _split_company_and_type(best_raw)
    top_score = float(candidates[0]['score']) if candidates else 0.0
    second_score = float(candidates[1]['score']) if len(candidates) > 1 else 0.0
    match_margin = round(top_score - second_score, 4)
    match_source = 'none'
    needs_review = True
    company = raw_company
    if candidates:
        if top_score >= 0.72 and match_margin >= 0.04:
            company = candidates[0]['name']
            match_source = 'library'
            needs_review = False
        elif top_score >= 0.55:
            candidates[0]['suggested'] = True
            match_source = 'raw'

    texts = _dedupe_ocr_texts(all_items, limit=6)
    result = {
        'texts': texts,
        'eff_colors': eff_colors,
        'candidates': candidates,
        'company': company,
        'raw_text': best_raw,
        'raw_company': raw_company,
        'match_source': match_source,
        'needs_review': needs_review,
        'match_margin': match_margin,
        'type': seal_type,
        'confidence': float(confidence if confidence is not None else 0.0),
        'method': method or 'hough',
        'bands': len(bands),
        'line_ocr_calls': line_ocr_calls,
        'low_color': low_color,
        'engine': 'rapidocr',
        'fallback': bool(vlm_error),
    }
    if vlm_error:
        result['vlm_error'] = _clip_vlm_error(vlm_error)
    return result


class _StampOcrResultList(list):
    """List-compatible result container carrying top-level OCR metadata."""


def stamp_ocr_pipeline(img, color='red', hues=None, excludes=None, exclude_colors=None):
    """Full 360-degree stamp OCR pipeline with library-constrained candidates."""
    all_results = _StampOcrResultList()
    _, _, eff_colors = extract_ink_channel(
        img, color, hues, excludes, exclude_colors,
    )
    mask = extract_ink_mask(img, color, hues, excludes, exclude_colors)
    cx, cy, radius, confidence, method = estimate_stamp_circle(img, mask)
    started = time.perf_counter()
    processed = process_stamp_ocr(
        img, cx, cy, radius, color, hues, excludes, exclude_colors,
        confidence=confidence, method=method,
    )
    all_results.metadata = {
        'engine': processed.get('engine'),
        'fallback': bool(processed.get('fallback')),
        'vlm_error': processed.get('vlm_error'),
    }
    elapsed_ms = int(round((time.perf_counter() - started) * 1000))
    texts = processed['texts']
    if texts or processed['candidates']:
        stamp_result = {
            'engine': processed.get('engine') or (
                'rapidocr' if _ocr_engine == 'rapid' else (_ocr_engine or 'paddleocr')
            ),
            'elapsed_ms': elapsed_ms,
            'library_size': len(_load_stamp_library()),
            'center': [int(round(cx)), int(round(cy))],
            'radius': int(round(radius)),
            'confidence': round(processed['confidence'], 3),
            'method': processed['method'],
            'texts': [
                {'text': item['text'], 'score': round(item['score'], 3)}
                for item in texts
            ],
            'full_text': processed['raw_text'],
            'company': processed['company'],
            'raw_text': processed['raw_text'],
            'raw_company': processed['raw_company'],
            'type': processed['type'],
            'candidates': processed['candidates'],
            'match_source': processed['match_source'],
            'needs_review': processed['needs_review'],
            'match_margin': processed['match_margin'],
            'text_bands': processed['bands'],
            'line_ocr_calls': processed['line_ocr_calls'],
            'low_color_fallback': processed['low_color'],
            'fallback': bool(processed.get('fallback')),
        }
        for key in ('vlm_error', 'vlm_raw'):
            if processed.get(key):
                stamp_result[key] = processed[key]
        all_results.append(stamp_result)

    return all_results, eff_colors


# ============================================================
# AI 文字提取 — OpenAI 兼容接口代理（DeepSeek / 智谱 / 自定义）
# ============================================================

# 服务商预设（前端设置面板同源）；mode: vision=多模态视觉识别, parse=本地OCR文本+AI清洗
AI_PROVIDERS = {
    'deepseek': {'name': 'DeepSeek', 'base_url': 'https://api.deepseek.com/v1', 'model': 'deepseek-chat', 'mode': 'parse'},
    'zhipu':    {'name': '智谱 AI',  'base_url': 'https://open.bigmodel.cn/api/paas/v4', 'model': 'glm-4v-flash', 'mode': 'vision'},
    'custom':   {'name': '自定义',   'base_url': '', 'model': '', 'mode': 'vision'},
    # 火山方舟豆包视觉模型（OpenAI 兼容 /chat/completions，走现有 vision 分支）
    'ark-doubao': {
        'name': '火山方舟（豆包视觉）',
        'base_url': 'https://ark.cn-beijing.volces.com/api/v3',
        'model': 'doubao-seed-2-1-pro-260915',
        'mode': 'vision',
    },
}

AI_SYSTEM_PROMPT = (
    '你是印章文字提取助手。根据给出的印章图片或 OCR 原始文本，提取印章信息。'
    '严格只输出一个 JSON 对象，不要输出任何解释、注释或代码块标记。'
)

AI_VISION_PROMPT = (
    '这是一枚印章（公章/财务章/合同章等）的图片，可能拍摄/扫描方向不正。按以下顺序处理，不要跳步：\n'
    '1. 先判断朝向：确认印章文字是正立还是上下颠倒/侧向；如果整枚印章是倒置的（例如需要转 180° 才能正常阅读），'
    '先在脑中把它转到正立再读，不要按原方向硬读。\n'
    '2. 再裁出印章：只保留印章外圈以内的区域，排除纸张背景、表格线、手写批注、其它印章。\n'
    '3. 沿圆周读字：印章名称为环形排列，从正上方（12 点方向）起始，顺时针逐字读出；'
    '不要按普通横排从左到右读，不要遗漏弧段；读不全时依次尝试 0°/90°/180°/270° 四个朝向，取最通顺的一次。\n'
    '4. 再读下缘编码与中部类型文字（如「合同专用章」）。\n'
    '返回 JSON（只输出 JSON，不要解释）：\n'
    '{"company": "单位名称（环形文字，去除空格与分隔符，无则空字符串", '
    '"code": "印章下缘的编码（数字或字母串，无则空字符串", '
    '"type": "印章类型（如 合同专用章/财务专用章/发票专用章，无则空字符串", '
    '"raw": "图片中识别到的全部文字", '
    '"rot": 0 或 90 或 180 或 270（让印章正立所需顺时针旋转角度）}'
)

AI_PARSE_PROMPT = (
    '以下是一枚印章图片经本地 OCR 识别的原始文本（弧形文字可能乱序、含错字或多行）：\n%s\n'
    '请清洗并结构化，返回 JSON：\n'
    '{"company": "单位名称（无则空字符串", "code": "印章编码（无则空字符串", '
    '"type": "印章类型（无则空字符串", "raw": "整理后的全部文字"}'
)

AI_STAMP_PROMPT = (
    '这是一张图片，可能包含一枚或多枚印章（圆形公章、椭圆章、方章、财务章等）。'
    '先判断整张图的朝向（是否旋转/倒置，例如需要转 180°），在判断圆心与半径时按校正后的方向估算；'
    '再找出图中最显著、最完整的一枚印章，返回其外接圆位置。'
    '严格只输出一个 JSON 对象，不要输出任何解释或代码块标记：\n'
    '{"found": true 或 false, '
    '"cx": 圆心横坐标占图片宽度的比例(0-1), '
    '"cy": 圆心纵坐标占图片高度的比例(0-1), '
    '"r": 半径占图片短边一半的比例(0-1，例如印章刚好接触上下边缘则为1)}'
    '若图中没有印章则 found=false，其余字段填 0。'
)


def validate_ai_config(cfg):
    """校验 AI 配置，返回 (规范化配置, 错误信息)。"""
    if not isinstance(cfg, dict):
        return None, '配置格式无效'
    provider = str(cfg.get('provider') or 'custom').strip()
    if provider not in AI_PROVIDERS:
        return None, f'不支持的 AI 服务商: {provider}'
    preset = AI_PROVIDERS[provider]
    base = str(cfg.get('base_url') or preset.get('base_url') or '').strip().rstrip('/')
    api_key = str(cfg.get('api_key') or '').strip()
    if not base:
        return None, '未配置 API 地址（Base URL）'
    if not base.startswith(('http://', 'https://')):
        return None, 'API 地址必须以 http:// 或 https:// 开头'
    if not api_key:
        return None, '未配置 API Key'

    model = str(cfg.get('model') or preset.get('model') or '').strip()
    mode = cfg.get('mode') if cfg.get('mode') in ('vision', 'parse') else preset.get('mode', 'vision')
    if not model:
        return None, '未配置模型名称'
    detail = str(cfg.get('detail') or '').strip().lower()
    if detail not in ('high', 'xhigh'):
        detail = 'high'
    return {'provider': provider, 'base_url': base, 'model': model, 'api_key': api_key,
            'mode': mode, 'detail': detail}, None


def _image_detail(cfg):
    """视觉输入精度：默认 high，配置允许时用 xhigh，避免默认低分辨率压缩。"""
    value = str((cfg or {}).get('detail') or '').strip().lower()
    return 'xhigh' if value == 'xhigh' else 'high'


def call_ai_chat(cfg, messages, max_tokens=1500, timeout=90):
    """调用 OpenAI 兼容的 chat/completions 接口，返回回复文本。"""
    url = cfg['base_url'] + '/chat/completions'
    payload = {
        'model': cfg['model'],
        'messages': messages,
        'temperature': 0.1,
        'max_tokens': max_tokens,
    }
    # 火山方舟豆包视觉：默认深度思考会耗尽 max_tokens 且响应很慢（实测 4 分钟以上），
    # 印章文字提取不需要长推理，显式关闭 thinking
    if cfg.get('provider') == 'ark-doubao':
        payload['thinking'] = {'type': 'disabled'}
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode('utf-8'),
        headers={
            'Content-Type': 'application/json',
            'Authorization': 'Bearer ' + cfg['api_key'],
        },
        method='POST',
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            resp = json.loads(r.read().decode('utf-8'))
    except urllib.error.HTTPError as e:
        detail = ''
        try:
            detail = e.read().decode('utf-8', 'ignore')[:300]
        except Exception:
            pass
        raise RuntimeError(f'AI 服务返回 HTTP {e.code}: {detail or e.reason}') from e
    except urllib.error.URLError as e:
        raise RuntimeError(f'无法连接 AI 服务: {e.reason}') from e

    try:
        content = resp['choices'][0]['message']['content']
    except (KeyError, IndexError, TypeError) as e:
        raise RuntimeError(f'AI 响应格式异常: {json.dumps(resp, ensure_ascii=False)[:300]}') from e
    if isinstance(content, list):  # 部分多模态接口返回分段 content
        content = ''.join(seg.get('text', '') for seg in content if isinstance(seg, dict))
    return content or ''


def parse_ai_json(text):
    """从 AI 回复中稳健提取 JSON 对象（容忍代码块标记/前后缀文本）。"""
    if not text:
        return None
    cleaned = text.strip()
    if cleaned.startswith('```'):
        cleaned = re.sub(r'^```[a-zA-Z]*\s*', '', cleaned)
        cleaned = re.sub(r'\s*```$', '', cleaned)
    start = cleaned.find('{')
    end = cleaned.rfind('}')
    if start >= 0 and end > start:
        try:
            obj = json.loads(cleaned[start:end + 1])
            return obj if isinstance(obj, dict) else None
        except json.JSONDecodeError:
            return None
    return None


def ai_extract(cfg, image_b64=None, paddle_texts=None):
    """执行 AI 文字提取。vision 模式发送图片，parse 模式发送本地 OCR 文本。"""
    if cfg['mode'] == 'vision':
        if not image_b64:
            raise RuntimeError('视觉模式需要图像数据')
        messages = [
            {'role': 'system', 'content': AI_SYSTEM_PROMPT},
            {'role': 'user', 'content': [
                {'type': 'text', 'text': AI_VISION_PROMPT},
                {'type': 'image_url', 'image_url': {'url': 'data:image/png;base64,' + image_b64, 'detail': _image_detail(cfg)}},
            ]},
        ]
    else:
        if paddle_texts is None:
            raise RuntimeError('文本解析模式需要本地 PaddleOCR 先识别（请安装依赖或改用视觉模式）')
        joined = '\n'.join(paddle_texts) if paddle_texts else '（未识别到任何文字）'
        messages = [
            {'role': 'system', 'content': AI_SYSTEM_PROMPT},
            {'role': 'user', 'content': AI_PARSE_PROMPT % joined},
        ]

    reply = call_ai_chat(cfg, messages)
    parsed = parse_ai_json(reply)
    if parsed is None:
        # 容错：整段回复作为 raw 返回
        return {'company': '', 'code': '', 'type': '', 'raw': reply.strip()[:500]}
    return {
        'company': str(parsed.get('company') or ''),
        'code': str(parsed.get('code') or ''),
        'type': str(parsed.get('type') or ''),
        'raw': str(parsed.get('raw') or ''),
    }


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
                # 三态：true 已就绪 | 'loading' 模型后台加载中 | false 不可用
                'ocr': True if ocr is not None else ('loading' if ocr_loading else False),
            })
        elif self.path.startswith('/engine/') and ENGINE_MANAGEMENT_ENABLED:
            self._handle_engine_get()
        elif self.path == '/favicon.ico':
            # 无图标文件，返回 204 避免控制台 404 噪音
            self.send_response(204)
            self.send_header('Content-Length', '0')
            self.end_headers()
        else:
            self.send_error(404, 'Not found')

    def do_POST(self):
        if self.path.startswith('/engine/') and ENGINE_MANAGEMENT_ENABLED:
            self._handle_engine_post()
            return
        if self.path == '/ai/test':
            self._handle_ai_test()
            return
        if self.path == '/ai-ocr':
            self._handle_ai_ocr()
            return
        if self.path == '/ai/stamp':
            self._handle_ai_stamp()
            return
        if self.path != '/ocr':
            self.send_error(404, 'Not found')
            return

        vlm_config, vlm_reason = _vlm_preflight()
        if ocr is None and vlm_config is None:
            self._send_json({
                'success': False,
                'error': (
                    f'本地视觉引擎不可用: {vlm_reason}'
                    if vlm_reason
                    else 'OCR 模型未加载 — 请安装依赖: pip install paddlepaddle paddleocr opencv-python 后重启服务'
                ),
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
            started = time.perf_counter()
            results, eff_colors = stamp_ocr_pipeline(img, color, hues, excludes, exclude_colors)
            elapsed_ms = int(round((time.perf_counter() - started) * 1000))
            candidates = []
            for stamp_result in results:
                candidates.extend(stamp_result.get('candidates', []))
            if not candidates:
                full_text = ' '.join(s.get('full_text', '') for s in results)
                candidates = rank_library_candidates(full_text, 3)
            candidates = sorted(
                candidates, key=lambda item: item.get('score', 0), reverse=True,
            )[:3]

            result_metadata = getattr(results, 'metadata', {})
            response_engine = (
                (results[0].get('engine') if results else None)
                or result_metadata.get('engine')
                or ('rapidocr' if _ocr_engine == 'rapid' else (_ocr_engine or 'paddleocr'))
            )
            response_fallback = (
                bool(result_metadata.get('fallback'))
                or any(bool(stamp.get('fallback')) for stamp in results)
            )
            response = {
                'success': True,
                'stamps': results,
                'results': results,
                'full_text': '\n'.join(
                    stamp.get('full_text', '') for stamp in results if stamp.get('full_text')
                ),
                'engine': response_engine,
                'fallback': response_fallback,
                'elapsed_ms': elapsed_ms,
                'candidates': candidates,
                'color': eff_colors,
                'hues': hues or [],
                'excludes': excludes or [],
                'image_size': [int(img.shape[1]), int(img.shape[0])]
            }
            vlm_errors = [
                stamp.get('vlm_error') for stamp in results
                if stamp.get('vlm_error')
            ]
            if not vlm_errors and result_metadata.get('vlm_error'):
                vlm_errors.append(result_metadata['vlm_error'])
            if response_fallback and vlm_errors:
                response['vlm_error'] = str(vlm_errors[0])[:120]
            self._send_json(response)

        except json.JSONDecodeError:
            self._send_json({'success': False, 'error': '无效的 JSON 请求'}, 400)
        except (base64.binascii.Error, ValueError):
            self._send_json({'success': False, 'error': '无效的 base64 图像数据'}, 400)
        except _VlmBusyError:
            self._send_json(
                {'success': False, 'error': '本地视觉引擎忙'}, 503,
            )
        except Exception as e:
            traceback.print_exc()
            self._send_json({'success': False, 'error': f'识别失败: {e}'}, 500)

    def _read_json_body(self):
        """读取并解析 JSON 请求体；失败时直接回错并返回 None。"""
        content_length = int(self.headers.get('Content-Length', 0))
        if content_length <= 0 or content_length > MAX_BODY_BYTES:
            self._send_json({'success': False, 'error': '请求体大小无效'}, 400)
            return None
        try:
            return json.loads(self.rfile.read(content_length))
        except json.JSONDecodeError:
            self._send_json({'success': False, 'error': '无效的 JSON 请求'}, 400)
            return None

    def _handle_engine_get(self):
        try:
            if self.path == '/engine/probe':
                self._send_json(_engine_probe_payload(force_hardware=True))
                return
            if self.path == '/engine/status':
                self._send_json(_engine_probe_payload(force_hardware=False))
                return
            if self.path.startswith('/engine/job/'):
                job_id = urllib.parse.unquote(self.path[len('/engine/job/'):])
                payload = _engine_job_payload(job_id)
                if payload is None:
                    self._send_json(
                        {'success': False, 'error': '安装任务不存在'}, 404,
                    )
                    return
                self._send_json(payload)
                return
            self._send_json({'success': False, 'error': '接口不存在'}, 404)
        except Exception as e:
            traceback.print_exc()
            self._send_json({'success': False, 'error': str(e)}, 500)

    def _handle_engine_post(self):
        if self.path == '/engine/runtime':
            try:
                content_length = int(self.headers.get('Content-Length', 0) or 0)
                if content_length < 0 or content_length > MAX_BODY_BYTES:
                    self._send_json(
                        {'success': False, 'error': '请求体大小无效'}, 400,
                    )
                    return
                if content_length:
                    self.rfile.read(content_length)
                job_id = _start_engine_runtime()
                self._send_json({'jobId': job_id})
            except (OSError, RuntimeError, ValueError) as exc:
                self._send_json(
                    {'success': False, 'error': str(exc)}, 400,
                )
            except Exception as exc:
                traceback.print_exc()
                self._send_json(
                    {'success': False, 'error': str(exc)}, 500,
                )
            return

        data = self._read_json_body()
        if data is None:
            return
        try:
            if self.path == '/engine/install':
                engine_id = data.get('id')
                if engine_id != LOCAL_ENGINE_ID:
                    self._send_json(
                        {'success': False, 'error': '不支持的引擎'}, 400,
                    )
                    return
                probe = _engine_probe_payload(force_hardware=True)
                if probe['support'] == 'unsupported':
                    self._send_json({
                        'success': False,
                        'error': '这台电脑不支持本地视觉模型',
                    }, 400)
                    return
                job_id = _start_engine_install(engine_id)
                self._send_json({'jobId': job_id})
                return

            if self.path == '/engine/cancel':
                job_id = data.get('jobId')
                if not isinstance(job_id, str) or not job_id:
                    self._send_json(
                        {'success': False, 'error': '缺少 jobId'}, 400,
                    )
                    return
                if not _cancel_engine_job(job_id):
                    self._send_json(
                        {'success': False, 'error': '安装任务不存在'}, 404,
                    )
                    return
                self._send_json({'ok': True})
                return

            if self.path == '/engine/remove':
                engine_id = data.get('id')
                if engine_id != LOCAL_ENGINE_ID:
                    self._send_json(
                        {'success': False, 'error': '不支持的引擎'}, 400,
                    )
                    return
                _remove_engine(engine_id)
                self._send_json({'ok': True})
                return

            if self.path == '/engine/activate':
                engine_id = data.get('id')
                try:
                    active = _activate_engine(engine_id)
                except (ValueError, RuntimeError) as exc:
                    self._send_json(
                        {'success': False, 'error': str(exc)}, 400,
                    )
                    return
                self._send_json({'ok': True, 'active': active})
                return

            if self.path == '/engine/import':
                engine_id = data.get('id')
                source_dir = data.get('dir')
                if engine_id != LOCAL_ENGINE_ID:
                    self._send_json(
                        {'success': False, 'error': '不支持的引擎'}, 400,
                    )
                    return
                try:
                    _import_engine(engine_id, source_dir)
                except (ValueError, OSError) as exc:
                    self._send_json(
                        {'success': False, 'error': str(exc)}, 400,
                    )
                    return
                self._send_json({'ok': True, 'id': engine_id})
                return

            self._send_json({'success': False, 'error': '接口不存在'}, 404)
        except Exception as e:
            traceback.print_exc()
            self._send_json({'success': False, 'error': str(e)}, 500)

    def _handle_ai_test(self):
        """AI 连接测试：用极小的文本请求验证地址/密钥/模型可用性。"""
        data = self._read_json_body()
        if data is None:
            return
        cfg, err = validate_ai_config(data.get('config') or {})
        if err:
            self._send_json({'success': False, 'error': err}, 400)
            return
        try:
            reply = call_ai_chat(
                cfg,
                [{'role': 'user', 'content': '回复"OK"两个字母即可，不要输出其他内容。'}],
                max_tokens=8, timeout=30)
            self._send_json({'success': True, 'model': cfg['model'], 'reply': (reply or '').strip()[:20]})
        except Exception as e:
            self._send_json({'success': False, 'error': str(e)}, 200)

    def _handle_ai_ocr(self):
        """AI 文字提取：vision 模式直接发图；parse 模式先本地 PaddleOCR 再交 AI 清洗。"""
        data = self._read_json_body()
        if data is None:
            return
        cfg, err = validate_ai_config(data.get('config') or {})
        if err:
            self._send_json({'success': False, 'error': err}, 400)
            return

        paddle_texts = None
        if cfg['mode'] == 'parse':
            if ocr is None:
                self._send_json({
                    'success': False,
                    'error': '文本解析模式依赖本地 PaddleOCR — 请安装依赖后重启服务，或在 AI 设置中改用视觉模式',
                })
                return
            try:
                img_bytes = base64.b64decode(data.get('image', ''))
                img = cv2.imdecode(np.frombuffer(img_bytes, dtype=np.uint8), cv2.IMREAD_COLOR)
                if img is None:
                    self._send_json({'success': False, 'error': '无法解析图像数据'}, 400)
                    return
                color, hues, excludes, exclude_colors = parse_ink_colors(
                    data.get('color', 'red'), data.get('hues'),
                    data.get('excludes'), data.get('exclude_colors'))
                results, _ = stamp_ocr_pipeline(img, color, hues, excludes, exclude_colors)
                paddle_texts = []
                for s in results:
                    paddle_texts.extend(t['text'] for t in s.get('texts', []))
            except Exception as e:
                traceback.print_exc()
                self._send_json({'success': False, 'error': f'本地 OCR 失败: {e}'}, 500)
                return

        try:
            ai = ai_extract(cfg, image_b64=data.get('image', ''), paddle_texts=paddle_texts)
            self._send_json({
                'success': True,
                'ai': ai,
                'mode': cfg['mode'],
                'model': cfg['model'],
                'paddle_texts': paddle_texts or [],
            })
        except Exception as e:
            self._send_json({'success': False, 'error': str(e)}, 200)

    def _handle_ai_stamp(self):
        """AI 印章定位：视觉模型返回印章外接圆位置，前端据此几何裁剪提取。"""
        data = self._read_json_body()
        if data is None:
            return
        cfg, err = validate_ai_config(data.get('config') or {})
        if err:
            self._send_json({'success': False, 'error': err}, 400)
            return
        if cfg['mode'] != 'vision':
            self._send_json({
                'success': False,
                'error': 'AI 提取印章需要视觉模型 — 请在「🤖 AI」设置中切换为视觉识别模式',
            })
            return
        image_b64 = data.get('image', '')
        if not image_b64:
            self._send_json({'success': False, 'error': '缺少图像数据'}, 400)
            return
        try:
            reply = call_ai_chat(cfg, [
                {'role': 'system', 'content': AI_SYSTEM_PROMPT},
                {'role': 'user', 'content': [
                    {'type': 'text', 'text': AI_STAMP_PROMPT},
                    {'type': 'image_url', 'image_url': {'url': 'data:image/png;base64,' + image_b64, 'detail': _image_detail(cfg)}},
                ]},
            ], timeout=90)
            parsed = parse_ai_json(reply)
            if parsed is None:
                self._send_json({'success': False, 'error': f'AI 返回内容无法解析: {reply[:120]}'})
                return
            if not bool(parsed.get('found', True)):
                self._send_json({'success': False, 'error': 'AI 未在图片中找到印章'})
                return
            cx = float(parsed.get('cx', 0.5))
            cy = float(parsed.get('cy', 0.5))
            r = float(parsed.get('r', 0.3))
            if not (0 <= cx <= 1 and 0 <= cy <= 1 and 0 < r <= 1):
                self._send_json({'success': False, 'error': f'AI 返回的坐标非法: {parsed}'})
                return
            self._send_json({
                'success': True,
                'stamp': {'cx': cx, 'cy': cy, 'r': r},
                'model': cfg['model'],
            })
        except Exception as e:
            self._send_json({'success': False, 'error': str(e)}, 200)

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
    """加载 OCR 引擎；依赖缺失时返回 None 而不是崩溃（对比功能不依赖后端）。

    优先 RapidOCR（onnxruntime，轻量，桌面安装包内置运行时自带），
    回退 PaddleOCR。无论成败都清除 ocr_loading（/health 三态）。
    """
    global ocr, _ocr_engine, ocr_loading
    try:
        from rapidocr_onnxruntime import RapidOCR
        print("加载 RapidOCR 引擎（onnxruntime）...")
        ocr = RapidOCR()
        _ocr_engine = 'rapid'
        ocr_loading = False
        print("模型加载完成!")
        return True
    except ImportError:
        pass
    except Exception as e:
        print(f"[提示] RapidOCR 初始化失败（{e}），尝试 PaddleOCR...")

    print("加载 PaddleOCR 模型（首次运行需下载，请耐心等待）...")
    try:
        from paddleocr import PaddleOCR
        ocr = PaddleOCR(lang='ch')
        _ocr_engine = 'paddle'
        ocr_loading = False
        print("模型加载完成!")
        return True
    except ImportError as e:
        ocr = None
        ocr_loading = False
        print(f"\n[警告] OCR 引擎未安装（{e}），OCR 功能不可用。")
        print("        页面服务将正常启动，印章对比/导出等功能不受影响。")
        print("        如需 OCR，请安装依赖后重启:")
        print("        pip install rapidocr-onnxruntime opencv-python-headless  （推荐，轻量）")
        print("        或 pip install paddlepaddle paddleocr opencv-python\n")
        return False
    except Exception as e:
        ocr = None
        ocr_loading = False
        print(f"\n[警告] PaddleOCR 模型加载失败（{e}），OCR 功能不可用，其余功能正常。\n")
        return False


def main():
    global ENGINE_MANAGEMENT_ENABLED
    parser = argparse.ArgumentParser(description='印章对比工具 — 统一启动文件 (BI4IWN)')
    parser.add_argument('--port', type=int, default=8765, help='服务端口 (默认: 8765)')
    parser.add_argument('--no-browser', action='store_true', help='不自动打开浏览器')
    parser.add_argument('--no-ocr', action='store_true',
                        help='跳过 PaddleOCR 模型加载（启动更快，仅使用对比/导出功能）')
    parser.add_argument('--no-engine', action='store_true',
                        help='禁用本地视觉引擎安装与管理接口（排障用）')
    args = parser.parse_args()
    ENGINE_MANAGEMENT_ENABLED = not args.no_engine
    if ENGINE_MANAGEMENT_ENABLED:
        _initialize_engine_state()

    # 加载 HTML 前端
    print("加载前端页面...")
    check_html_exists()
    print("前端页面已就绪")

    # 启动 HTTP 服务（多线程：OCR 长耗时请求不阻塞页面/健康检查）
    # v3.0.2：服务先启动（窗口/前端秒连），OCR 模型在后台线程加载，
    # /health 在加载完成前返回 ocr:'loading'，前端持续轮询直至就绪
    try:
        server = ThreadingHTTPServer(('127.0.0.1', args.port), StampAppHandler)
    except OSError as e:
        print(f"\n[错误] 无法绑定端口 {args.port}: {e}")
        print("  端口可能已被占用。可结束占用该端口的进程，或改用其他端口：")
        print("  python3 stamp_app.py --port 9000")
        sys.exit(1)
    server.daemon_threads = True

    def _load_ocr_async():
        if args.no_ocr:
            print("已跳过 OCR 模型加载（--no-ocr），OCR 功能不可用，其余功能正常。")
        else:
            load_ocr_model()

    threading.Thread(target=_load_ocr_async, daemon=True).start()

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

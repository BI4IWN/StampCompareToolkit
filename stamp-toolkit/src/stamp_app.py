#!/usr/bin/env python3
"""
印章对比工具 — 统一启动文件 (BI4IWN · 李劲松)
双击或命令行运行即可自动启动服务并打开浏览器。

用法:
  python3 stamp_app.py              # 默认端口 8765
  python3 stamp_app.py --port 9000  # 自定义端口

关闭方式:
  终端按 Ctrl+C 或关闭终端窗口

依赖:
  pip install paddleocr paddlepaddle opencv-python
"""

import cv2
import numpy as np
import os
import sys
import json
import base64
import argparse
import tempfile
import webbrowser
import threading
from http.server import HTTPServer, BaseHTTPRequestHandler

# ============================================================
# 内嵌 HTML 前端 (stamp-compare.html)
# ============================================================
# 如果同目录下存在 stamp-compare.html 文件则优先读取，否则使用内嵌版本
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
HTML_FILE = os.path.join(SCRIPT_DIR, 'stamp-compare.html')

def load_html_content():
    """加载 HTML 前端内容"""
    if os.path.exists(HTML_FILE):
        with open(HTML_FILE, 'r', encoding='utf-8') as f:
            return f.read()
    else:
        print("警告: 未找到 stamp-compare.html，请确保该文件与本程序在同一目录。")
        sys.exit(1)

# ============================================================
# PaddleOCR 印章识别后端
# ============================================================

# Global OCR instance (loaded once at startup)
ocr = None

def extract_red_channel_v3(img):
    """Extract red/pink/purple stamp pixels with broader color range."""
    hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
    h, s, v = cv2.split(hsv)
    mask1 = (h < 15) & (s > 30) & (v > 30)
    mask2 = (h > 165) & (s > 30) & (v > 30)
    mask3 = (h >= 150) & (h <= 175) & (s > 20) & (v > 30)
    mask4 = ((h < 10) | (h > 170)) & (s > 15) & (s < 80) & (v > 100)
    red_mask = (mask1 | mask2 | mask3 | mask4)
    result = np.ones_like(img) * 255
    result[red_mask] = img[red_mask]
    return result, red_mask.astype(np.uint8)

def detect_stamp_center_v3(img):
    """Detect stamp center with broader color detection and multi-stamp handling."""
    hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
    h, s, v = cv2.split(hsv)
    mask1 = (h < 15) & (s > 30) & (v > 30)
    mask2 = (h > 165) & (s > 30) & (v > 30)
    mask3 = (h >= 150) & (h <= 175) & (s > 20) & (v > 30)
    mask4 = ((h < 10) | (h > 170)) & (s > 15) & (s < 80) & (v > 100)
    red_mask = (mask1 | mask2 | mask3 | mask4).astype(np.uint8)
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
        return w_img//2, h_img//2, min(w_img, h_img)//2
    return best_result

def detect_multiple_stamps(img):
    """Detect multiple stamps in an image."""
    hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
    h, s, v = cv2.split(hsv)
    mask1 = (h < 15) & (s > 30) & (v > 30)
    mask2 = (h > 165) & (s > 30) & (v > 30)
    mask3 = (h >= 150) & (h <= 175) & (s > 20) & (v > 30)
    red_mask = (mask1 | mask2 | mask3).astype(np.uint8)
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
    """Run PaddleOCR on a numpy image array. Returns list of (text, score)."""
    with tempfile.NamedTemporaryFile(suffix='.png', delete=False) as f:
        cv2.imwrite(f.name, img_array)
        temp_path = f.name
    results = []
    try:
        res = ocr.predict(temp_path)
        for r in res:
            j = r.json
            res_data = j.get('res', {})
            texts = res_data.get('rec_texts', [])
            scores = res_data.get('rec_scores', [])
            for text, score in zip(texts, scores):
                results.append((text, float(score)))
    except Exception:
        pass
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

def process_stamp_ocr(img, cx, cy, radius):
    """Process a single stamp and return OCR results."""
    h, w = img.shape[:2]
    margin = 30
    y1 = max(0, cy - radius - margin)
    y2 = min(h, cy + radius + margin)
    x1 = max(0, cx - radius - margin)
    x2 = min(w, cx + radius + margin)
    red_img, _ = extract_red_channel_v3(img)
    cropped = red_img[y1:y2, x1:x2]
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

    return best_texts

def stamp_ocr_pipeline(img):
    """Full stamp OCR pipeline. Returns list of {text, score, center, radius}."""
    all_results = []

    # Check for multiple stamps
    multi_stamps = detect_multiple_stamps(img)

    if len(multi_stamps) >= 2:
        for cx, cy, radius, area in multi_stamps:
            texts = process_stamp_ocr(img, cx, cy, radius)
            if texts:
                all_results.append({
                    'center': [int(cx), int(cy)],
                    'radius': int(radius),
                    'texts': [{'text': t, 'score': s} for t, s in texts],
                    'full_text': ' '.join(t for t, s in texts)
                })
    else:
        cx, cy, radius = detect_stamp_center_v3(img)
        texts = process_stamp_ocr(img, cx, cy, radius)
        if texts:
            all_results.append({
                'center': [int(cx), int(cy)],
                'radius': int(radius),
                'texts': [{'text': t, 'score': s} for t, s in texts],
                'full_text': ' '.join(t for t, s in texts)
            })

    return all_results


# ============================================================
# HTTP 服务 — 同时提供 HTML 页面和 OCR API
# ============================================================

class StampAppHandler(BaseHTTPRequestHandler):
    """统一处理：HTML 页面 + OCR API"""

    html_content = None  # 启动时加载

    def do_OPTIONS(self):
        """Handle CORS preflight."""
        self.send_response(200)
        self.send_header('Access-Control-Allow-Origin', '*')
        self.send_header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
        self.send_header('Access-Control-Allow-Headers', 'Content-Type')
        self.end_headers()

    def do_GET(self):
        if self.path == '/' or self.path == '/index.html':
            # 提供 HTML 前端页面
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Access-Control-Allow-Origin', '*')
            self.end_headers()
            self.wfile.write(self.html_content.encode('utf-8'))
        elif self.path == '/health':
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Access-Control-Allow-Origin', '*')
            self.end_headers()
            self.wfile.write(json.dumps({'status': 'ok'}).encode('utf-8'))
        else:
            self.send_error(404, 'Not found')

    def do_POST(self):
        if self.path == '/ocr':
            content_length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(content_length)

            try:
                data = json.loads(body)
                image_b64 = data.get('image', '')

                # Decode base64 image
                img_bytes = base64.b64decode(image_b64)
                img_array = np.frombuffer(img_bytes, dtype=np.uint8)
                img = cv2.imdecode(img_array, cv2.IMREAD_COLOR)

                if img is None:
                    self.send_error(400, 'Invalid image data')
                    return

                # Run OCR pipeline
                results = stamp_ocr_pipeline(img)

                response = {
                    'success': True,
                    'stamps': results,
                    'image_size': [int(img.shape[1]), int(img.shape[0])]
                }

                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Access-Control-Allow-Origin', '*')
                self.end_headers()
                self.wfile.write(json.dumps(response, ensure_ascii=False).encode('utf-8'))

            except json.JSONDecodeError:
                self.send_error(400, 'Invalid JSON')
            except base64.binascii.Error:
                self.send_error(400, 'Invalid base64')
            except Exception as e:
                self.send_error(500, str(e))
        else:
            self.send_error(404, 'Not found')

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


def main():
    global ocr

    parser = argparse.ArgumentParser(description='印章对比工具 — 统一启动文件 (BI4IWN)')
    parser.add_argument('--port', type=int, default=8765, help='服务端口 (默认: 8765)')
    parser.add_argument('--no-browser', action='store_true', help='不自动打开浏览器')
    args = parser.parse_args()

    # 加载 HTML 前端
    print("加载前端页面...")
    StampAppHandler.html_content = load_html_content()
    print("前端页面已就绪")

    # 加载 PaddleOCR 模型
    print("加载 PaddleOCR 模型（首次运行需下载，请耐心等待）...")
    from paddleocr import PaddleOCR
    ocr = PaddleOCR(lang='ch')
    print("模型加载完成!")

    # 启动 HTTP 服务
    server = HTTPServer(('127.0.0.1', args.port), StampAppHandler)
    print(f"\n{'='*50}")
    print(f"  印章对比工具已启动 (BI4IWN)")
    print(f"  访问地址: http://127.0.0.1:{args.port}")
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

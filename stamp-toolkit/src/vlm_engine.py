#!/usr/bin/env python3
"""One-shot PaddleOCR-VL inference helper for the local stamp OCR engine."""

import argparse
import contextlib
import json
import os
import resource
import sys
import tempfile
import time


def _parse_crop(value):
    if value is None:
        return None
    try:
        parts = [int(item.strip()) for item in value.split(',')]
    except (AttributeError, ValueError) as exc:
        raise ValueError('--crop 格式应为 x,y,w,h') from exc
    if len(parts) != 4:
        raise ValueError('--crop 格式应为 x,y,w,h')
    x, y, width, height = parts
    if x < 0 or y < 0 or width <= 0 or height <= 0:
        raise ValueError('--crop 的坐标必须大于等于 0，宽高必须大于 0')
    return x, y, width, height


def _peak_mb():
    peak = int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)
    if sys.platform != 'darwin':
        peak *= 1024
    return max(0, int(round(peak / (1024 * 1024))))


def _result_text(result):
    value = getattr(result, 'text', result)
    if isinstance(value, (list, tuple)):
        value = value[0] if value else ''
    return str(value or '').strip()


def run_inference(args):
    from PIL import Image
    from mlx_vlm import generate, load
    from mlx_vlm.prompt_utils import apply_chat_template
    from mlx_vlm.utils import load_config

    crop_box = _parse_crop(args.crop)
    image_path = args.image
    temporary_path = None
    try:
        with Image.open(args.image) as source:
            image = source.convert('RGB')
            if crop_box is not None:
                x, y, width, height = crop_box
                left = min(x, image.width)
                top = min(y, image.height)
                right = min(x + width, image.width)
                bottom = min(y + height, image.height)
                if right <= left or bottom <= top:
                    raise ValueError('--crop 区域落在图片之外')
                image = image.crop((left, top, right, bottom))
                fd, temporary_path = tempfile.mkstemp(prefix='stamp-vlm-', suffix='.png')
                os.close(fd)
                image.save(temporary_path, format='PNG')
                image_path = temporary_path

        config = load_config(args.model_dir)
        model, processor = load(args.model_dir)
        formatted_prompt = apply_chat_template(
            processor, config, args.prompt, num_images=1,
        )
        output = generate(
            model,
            processor,
            formatted_prompt,
            image=image_path,
            max_tokens=args.max_tokens,
            temperature=0.0,
            verbose=False,
        )
        return _result_text(output)
    finally:
        if temporary_path and os.path.exists(temporary_path):
            try:
                os.unlink(temporary_path)
            except OSError:
                pass


def build_parser():
    parser = argparse.ArgumentParser(description='PaddleOCR-VL one-shot inference')
    parser.add_argument('--model-dir', required=True, help='本地 MLX 模型目录')
    parser.add_argument('--image', required=True, help='输入图片路径')
    parser.add_argument('--prompt', default='OCR:', help='识别提示词（默认 OCR:）')
    parser.add_argument('--max-tokens', type=int, default=64, help='最大输出 token 数')
    parser.add_argument('--crop', help='可选裁剪区域 x,y,w,h')
    return parser


def main():
    real_stdout = sys.stdout
    args = build_parser().parse_args()
    started = time.perf_counter()
    try:
        # mlx-vlm and its dependencies occasionally write progress output to stdout.
        # Keep stdout exclusively for the one machine-readable result object.
        with contextlib.redirect_stdout(sys.stderr):
            text = run_inference(args)
        payload = {
            'ok': True,
            'text': text,
            'elapsedMs': int(round((time.perf_counter() - started) * 1000)),
            'peakMb': _peak_mb(),
        }
        exit_code = 0
    except Exception as exc:
        payload = {
            'ok': False,
            'error': f'{type(exc).__name__}: {exc}',
        }
        exit_code = 2
    print(json.dumps(payload, ensure_ascii=False, separators=(',', ':')), file=real_stdout, flush=True)
    return exit_code


if __name__ == '__main__':
    sys.exit(main())

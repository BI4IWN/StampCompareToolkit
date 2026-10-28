#!/usr/bin/env bash
# 准备桌面版内置的便携 Python 运行时（OCR 开箱即用，无需用户安装 Python）
# 用法:
#   scripts/setup-runtime.sh mac-arm64   # macOS Apple Silicon
#   scripts/setup-runtime.sh mac-x64     # macOS Intel
#   scripts/setup-runtime.sh win-x64     # Windows x64（在任意平台均可下载 win 运行时与依赖）
# 产物: desktop/runtime/python-<platform>/   （electron-builder 打包时随 extraResources 分发）
set -euo pipefail

TARGET="${1:-mac-arm64}"
PBS_VER="20261021"          # python-build-standalone 版本（npmmirror 镜像）
PY_TAG="cpython-3.12.15+${PBS_VER}"
MIRROR="https://registry.npmmirror.com/-/binary/python-build-standalone/${PBS_VER}"
PYPIMIRROR="https://pypi.tuna.tsinghua.edu.cn/simple"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/runtime"

case "$TARGET" in
  mac-arm64) ART="${PY_TAG}-aarch64-apple-darwin-install_only.tar.gz";    DIR="python-mac-arm64" ;;
  mac-x64)   ART="${PY_TAG}-x86_64-apple-darwin-install_only.tar.gz";     DIR="python-mac-x64" ;;
  win-x64)   ART="${PY_TAG}-x86_64-pc-windows-msvc-install_only.tar.gz";  DIR="python-win-x64" ;;
  *) echo "未知目标: $TARGET（支持 mac-arm64 / mac-x64 / win-x64）"; exit 1 ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
echo "==> 下载便携 Python: $ART"
curl -sL --retry 3 -o "$TMP/py.tar.gz" "$MIRROR/$ART"
mkdir -p "$TMP/extract"
tar xzf "$TMP/py.tar.gz" -C "$TMP/extract"

# install_only 包解压后即完整 python 目录（mac: python/，win: python/）
rm -rf "$OUT/$DIR"
mv "$TMP/extract/python" "$OUT/$DIR"

PYEXE="$OUT/$DIR/bin/python3"
[ -x "$PYEXE" ] || PYEXE="$OUT/$DIR/python.exe"
if [ ! -e "$PYEXE" ]; then echo "解压后未找到 python 可执行文件"; exit 1; fi

echo "==> 安装 OCR 依赖（RapidOCR onnxruntime，内置模型，无网络也能识别）"
if [[ "$TARGET" == win-* ]]; then
  # Windows 依赖需用 win 平台 wheel：在非 Windows 构建机上用 pip download + 指定平台
  "$PYEXE" -m pip install --no-warn-script-location -i "$PYPIMIRROR" \
      rapidocr-onnxruntime opencv-python-headless numpy pillow && true || {
    echo "（非 Windows 主机安装 win 依赖失败时，请在 Windows 机器上执行本脚本）"; exit 1; }
else
  "$PYEXE" -m pip install --no-warn-script-location -i "$PYPIMIRROR" \
      rapidocr-onnxruntime opencv-python-headless numpy pillow
fi

echo "==> 验证"
"$PYEXE" -c "from rapidocr_onnxruntime import RapidOCR; import cv2; print('runtime OK:', '$DIR')"
echo "完成: $OUT/$DIR"

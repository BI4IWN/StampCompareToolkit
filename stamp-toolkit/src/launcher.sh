#!/bin/bash
# ============================================================
# 印章对比工具 — 一键启动器 (BI4IWN · 李劲松)
#
# 双击此文件即可运行（macOS / Linux）
# 自动检测环境 → 安装缺失依赖 → 启动服务 → 打开浏览器
# Ctrl+C 或关闭终端窗口即可停止服务
# ============================================================

set -e

# 定位脚本所在目录（解析符号链接）
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

PORT=8765
SERVER_PID=""

# ---- 清理函数 ----
cleanup() {
    echo ""
    echo -e "${BLUE}正在停止服务...${NC}"
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null
        wait "$SERVER_PID" 2>/dev/null
        echo -e "${GREEN}服务已停止${NC}"
    fi
    exit 0
}
trap cleanup SIGINT SIGTERM EXIT

# ---- 横幅 ----
echo ""
echo -e "${CYAN}${BOLD}╔══════════════════════════════════════╗${NC}"
echo -e "${CYAN}${BOLD}║     印章对比工具 — 一键启动器       ║${NC}"
echo -e "${CYAN}${BOLD}╚══════════════════════════════════════╝${NC}"
echo ""

# ============================================================
# 1. 检查 Python 3
# ============================================================
PYTHON=""
for cmd in python3 python; do
    if command -v "$cmd" &>/dev/null; then
        ver=$("$cmd" -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')" 2>/dev/null || echo "0.0")
        major=$(echo "$ver" | cut -d. -f1)
        if [ "$major" -ge 3 ]; then
            PYTHON="$cmd"
            break
        fi
    fi
done

if [ -z "$PYTHON" ]; then
    echo -e "${RED}✗ 未找到 Python 3${NC}"
    echo ""
    echo "  请先安装 Python 3.8+："
    echo "    macOS  : brew install python3"
    echo "    Ubuntu : sudo apt install python3 python3-pip"
    echo "    官网   : https://www.python.org/downloads/"
    echo ""
    read -p "  是否尝试自动安装？(y/N) " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        if command -v brew &>/dev/null; then
            echo -e "${BLUE}→ brew install python3 ...${NC}"
            brew install python3
        elif command -v apt-get &>/dev/null; then
            echo -e "${BLUE}→ sudo apt install python3 python3-pip ...${NC}"
            sudo apt-get update && sudo apt-get install -y python3 python3-pip
        else
            echo -e "${RED}  无法自动安装，请手动安装 Python 3${NC}"
            exit 1
        fi
        PYTHON="python3"
    else
        exit 1
    fi
fi
echo -e "${GREEN}✓ Python${NC}: $($PYTHON --version 2>&1)"

# ============================================================
# 2. 检查 pip
# ============================================================
if ! $PYTHON -m pip --version &>/dev/null 2>&1; then
    echo -e "${YELLOW}! pip 未安装，正在安装...${NC}"
    $PYTHON -m ensurepip --upgrade 2>/dev/null || {
        curl -sS https://bootstrap.pypa.io/get-pip.py | $PYTHON
    }
fi

# ============================================================
# 3. 检查 PaddleOCR 及依赖
# ============================================================
NEED_INSTALL=0

if ! $PYTHON -c "import paddleocr" 2>/dev/null; then
    echo -e "${YELLOW}! PaddleOCR 未安装${NC}"
    NEED_INSTALL=1
fi

if ! $PYTHON -c "import paddle" 2>/dev/null; then
    echo -e "${YELLOW}! PaddlePaddle 未安装${NC}"
    NEED_INSTALL=1
fi

if ! $PYTHON -c "import cv2" 2>/dev/null; then
    echo -e "${YELLOW}! OpenCV 未安装${NC}"
    NEED_INSTALL=1
fi

if [ "$NEED_INSTALL" -eq 1 ]; then
    echo ""
    echo -e "${BOLD}需要安装以下 Python 依赖：${NC}"
    echo "  • PaddlePaddle  (~700MB，首次安装需几分钟)"
    echo "  • PaddleOCR     (印章文字识别引擎)"
    echo "  • opencv-python (图像处理)"
    echo ""
    read -p "是否立即安装？(Y/n) " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Nn]$ ]]; then
        echo -e "${YELLOW}已取消安装。下次运行时会再次提示。${NC}"
        exit 0
    fi

    echo ""
    echo -e "${BLUE}[1/3] 安装 PaddlePaddle (CPU 版)...${NC}"
    $PYTHON -m pip install paddlepaddle -i https://mirror.baidu.com/pypi/simple --trusted-host mirror.baidu.com
    echo ""
    echo -e "${BLUE}[2/3] 安装 PaddleOCR...${NC}"
    $PYTHON -m pip install paddleocr -i https://mirror.baidu.com/pypi/simple --trusted-host mirror.baidu.com
    echo ""
    echo -e "${BLUE}[3/3] 安装 OpenCV...${NC}"
    $PYTHON -m pip install opencv-python -i https://mirror.baidu.com/pypi/simple --trusted-host mirror.baidu.com
    echo ""
    echo -e "${GREEN}${BOLD}✓ 所有依赖安装完成${NC}"
    echo ""
fi

# ============================================================
# 4. 检查核心文件
# ============================================================
if [ ! -f "stamp_app.py" ]; then
    echo -e "${RED}✗ 找不到 stamp_app.py${NC}"
    echo "  请确保 stamp_app.py 与本脚本在同一目录下"
    exit 1
fi

if [ ! -f "stamp-compare.html" ]; then
    echo -e "${RED}✗ 找不到 stamp-compare.html${NC}"
    echo "  请确保 stamp-compare.html 与本脚本在同一目录下"
    exit 1
fi

# ============================================================
# 5. 检查端口是否被占用
# ============================================================
if lsof -ti:$PORT &>/dev/null 2>&1; then
    echo -e "${YELLOW}! 端口 $PORT 已被占用${NC}"
    read -p "是否终止旧进程并继续？(Y/n) " -n 1 -r
    echo ""
    if [[ ! $REPLY =~ ^[Nn]$ ]]; then
        lsof -ti:$PORT | xargs kill 2>/dev/null
        sleep 1
    else
        exit 0
    fi
fi

# ============================================================
# 6. 启动服务
# ============================================================
echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${BLUE}正在启动 OCR 服务（首次加载模型约需 15 秒）...${NC}"

$PYTHON stamp_app.py --port $PORT --no-browser &
SERVER_PID=$!

# 等待服务就绪（最多 60 秒）
READY=0
for i in $(seq 1 60); do
    if curl -s "http://127.0.0.1:$PORT/health" &>/dev/null; then
        READY=1
        break
    fi
    # 检查进程是否还活着
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        echo -e "${RED}✗ 服务进程异常退出${NC}"
        exit 1
    fi
    printf "."
    sleep 1
done
echo ""

if [ "$READY" -eq 0 ]; then
    echo -e "${RED}✗ 服务启动超时（60 秒）${NC}"
    kill "$SERVER_PID" 2>/dev/null
    exit 1
fi

echo -e "${GREEN}${BOLD}✓ 服务已就绪${NC}"

# ============================================================
# 7. 打开浏览器
# ============================================================
URL="http://127.0.0.1:$PORT/"
echo -e "${BLUE}正在打开浏览器...${NC}"
open "$URL" 2>/dev/null || xdg-open "$URL" 2>/dev/null || echo -e "${YELLOW}请手动打开: $URL${NC}"

echo ""
echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${GREEN}${BOLD}  印章对比工具已启动${NC}"
echo -e "  浏览器访问: ${BOLD}$URL${NC}"
echo -e "  停止服务  : 按 ${BOLD}Ctrl+C${NC} 或关闭此窗口"
echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo ""

# 保持脚本运行（等待服务进程结束或用户中断）
wait "$SERVER_PID" 2>/dev/null

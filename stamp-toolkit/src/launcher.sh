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
SKIP_INSTALL=0
NO_BROWSER=0
SERVER_PID=""

# 命令行参数：--port <端口> 覆盖默认端口；--no-install 跳过依赖检查与安装
while [ $# -gt 0 ]; do
  case "$1" in
    --port)
      PORT="${2:-8765}"
      shift 2
      ;;
    --no-install)
      SKIP_INSTALL=1
      shift
      ;;
    --no-browser)
      NO_BROWSER=1
      shift
      ;;
    *)
      shift
      ;;
  esac
done

# ---- 清理函数 ----
cleanup() {
    echo ""
    echo -e "${BLUE}正在停止服务...${NC}"
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        # 先 SIGTERM 请求优雅退出；等待最多 5 秒仍未退出则 SIGKILL 强制终止
        # （PaddlePaddle 运行时会吞掉 SIGTERM，因此必须保留兜底）
        kill "$SERVER_PID" 2>/dev/null
        for i in 1 2 3 4 5; do
            sleep 1
            kill -0 "$SERVER_PID" 2>/dev/null || break
        done
        if kill -0 "$SERVER_PID" 2>/dev/null; then
            echo -e "${YELLOW}! 服务未在 5 秒内退出，强制终止...${NC}"
            kill -9 "$SERVER_PID" 2>/dev/null
        fi
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
if [ "$(uname -s)" = "Darwin" ]; then
  echo -e "${YELLOW}提示: 若双击 launcher.sh 打开的是文本编辑器，请双击${NC}"
  echo -e "${YELLOW}      同目录下的「启动印章对比工具.command」（双击即运行）${NC}"
  echo ""
fi

# ============================================================
# 1. 收集可用的 Python 3 候选（按版本偏好排序）
#    python3.13 ~ python3.8：PaddlePaddle 兼容性最好的版本段
#    python3 / /usr/bin/python3 / python：兜底
# ============================================================
CANDIDATES="python3.13 python3.12 python3.11 python3.10 python3.9 python3.8 python3 /usr/bin/python3 python"

# 1a. 优先寻找"依赖已就绪"的 Python（cv2 + paddle + paddleocr 均已安装）
#     找到就直接使用，免去 700MB 下载与 PEP 668 的一切麻烦
DEPS_PY=""
for cmd in $CANDIDATES; do
    if command -v "$cmd" &>/dev/null || [ -x "$cmd" ]; then
        if $cmd -c "import cv2, paddle, paddleocr" 2>/dev/null; then
            DEPS_PY="$cmd"
            break
        fi
    fi
done

if [ -n "$DEPS_PY" ]; then
    PYTHON="$DEPS_PY"
    echo -e "${GREEN}✓ 依赖已就绪${NC}: $($DEPS_PY --version 2>&1)（$DEPS_PY，无需安装）"
else

# 1b. 无依赖齐全的 Python → 按版本偏好选一个基础 Python
BASE_PYTHON=""
FALLBACK_PY=""
FALLBACK_VER=""
for cmd in $CANDIDATES; do
    if command -v "$cmd" &>/dev/null || [ -x "$cmd" ]; then
        ver=$("$cmd" -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')" 2>/dev/null || echo "0.0")
        major=$(echo "$ver" | cut -d. -f1)
        minor=$(echo "$ver" | cut -d. -f2)
        if [ "$major" -eq 3 ] && [ "$minor" -ge 8 ] && [ "$minor" -le 13 ]; then
            BASE_PYTHON="$cmd"
            break
        fi
        # 记录第一个可用的 3.x 作为兜底（如 3.14）
        if [ -z "$FALLBACK_PY" ] && [ "$major" -eq 3 ]; then
            FALLBACK_PY="$cmd"
            FALLBACK_VER="$ver"
        fi
    fi
done

if [ -z "$BASE_PYTHON" ] && [ -n "$FALLBACK_PY" ]; then
    echo -e "${YELLOW}! 未找到 Python 3.8-3.13，将使用 $FALLBACK_PY ($FALLBACK_VER)${NC}"
    echo -e "${YELLOW}! 注意：PaddlePaddle 对过新的 Python（3.14+）可能没有预编译安装包${NC}"
    BASE_PYTHON="$FALLBACK_PY"
fi

if [ -z "$BASE_PYTHON" ]; then
    echo -e "${RED}✗ 未找到 Python 3${NC}"
    echo ""
    echo "  请先安装 Python 3.8+："
    echo "    macOS  : brew install python@3.12"
    echo "    Ubuntu : sudo apt install python3 python3-pip"
    echo "    官网   : https://www.python.org/downloads/"
    echo ""
    read -p "  是否尝试自动安装？(y/N) " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        if command -v brew &>/dev/null; then
            echo -e "${BLUE}→ brew install python@3.12 ...${NC}"
            brew install python@3.12
        elif command -v apt-get &>/dev/null; then
            echo -e "${BLUE}→ sudo apt install python3 python3-pip ...${NC}"
            sudo apt-get update && sudo apt-get install -y python3 python3-pip
        else
            echo -e "${RED}  无法自动安装，请手动安装 Python 3${NC}"
            exit 1
        fi
        if command -v python3.12 &>/dev/null; then
            BASE_PYTHON="python3.12"
        else
            BASE_PYTHON="python3"
        fi
    else
        exit 1
    fi
fi
echo -e "${GREEN}✓ Python${NC}: $($BASE_PYTHON --version 2>&1)"

# ============================================================
# 2. 虚拟环境（PEP 668 兼容）
#    Homebrew / Ubuntu / uv 管理的 Python 是"外部管理环境"，
#    直接 pip install 会被拒绝。方案：在项目内创建 .venv。
# ============================================================
VENV_DIR="$SCRIPT_DIR/.venv"
VENV_PYTHON="$VENV_DIR/bin/python"
VENV_MANAGER="none"   # standard | uv | none

# 已有可用的 .venv？
if [ -x "$VENV_PYTHON" ] && $VENV_PYTHON -c "import cv2, paddle, paddleocr" 2>/dev/null; then
    PYTHON="$VENV_PYTHON"
    echo -e "${GREEN}✓ 使用虚拟环境 Python${NC}: $($VENV_PYTHON --version 2>&1)（依赖已就绪）"
elif [ "$SKIP_INSTALL" -eq 1 ]; then
    # --no-install：不创建 venv，直接用基础 Python（依赖缺失时 OCR 不可用）
    PYTHON="$BASE_PYTHON"
    echo -e "${YELLOW}! 已指定 --no-install，跳过依赖检查与安装（依赖缺失时 OCR 不可用）${NC}"
else
    # 创建 .venv（多级兜底）
    echo -e "${BLUE}→ 创建项目虚拟环境 .venv（依赖将安装到其中，避免 PEP 668 限制）...${NC}"
    # 尝试 1：标准 venv
    if $BASE_PYTHON -m venv "$VENV_DIR" 2>/tmp/stamp_venv_err.txt && [ -x "$VENV_PYTHON" ]; then
        PYTHON="$VENV_PYTHON"
        VENV_MANAGER="standard"
        echo -e "${GREEN}✓ 虚拟环境已创建${NC}: $($VENV_PYTHON --version 2>&1)"
    else
        # 尝试 2：uv venv（uv 管理的 Python 常缺 ensurepip，标准 venv 会失败）
        if command -v uv &>/dev/null 2>&1; then
            echo -e "${YELLOW}! 标准 venv 失败（$(head -1 /tmp/stamp_venv_err.txt 2>/dev/null)），尝试 uv venv...${NC}"
            rm -rf "$VENV_DIR"
            if uv venv "$VENV_DIR" >/tmp/stamp_venv_err.txt 2>&1 && [ -x "$VENV_PYTHON" ]; then
                PYTHON="$VENV_PYTHON"
                VENV_MANAGER="uv"
                echo -e "${GREEN}✓ 虚拟环境已创建（uv venv）${NC}"
            fi
        fi
        # 尝试 3：--without-pip 模式 + 手动补装 pip
        if [ -z "$PYTHON" ]; then
            echo -e "${YELLOW}! 尝试无 pip 模式创建 venv 并手动补装 pip...${NC}"
            rm -rf "$VENV_DIR"
            if $BASE_PYTHON -m venv --without-pip "$VENV_DIR" 2>/tmp/stamp_venv_err.txt && [ -x "$VENV_PYTHON" ]; then
                if curl -sS https://bootstrap.pypa.io/get-pip.py | "$VENV_PYTHON" >/tmp/stamp_venv_err.txt 2>&1 && "$VENV_PYTHON" -m pip --version &>/dev/null 2>&1; then
                    PYTHON="$VENV_PYTHON"
                    VENV_MANAGER="standard"
                    echo -e "${GREEN}✓ 虚拟环境已创建（无 pip 模式 + 手动补装 pip）${NC}"
                fi
            fi
        fi
        # 全部失败 → 回退系统 Python
        if [ -z "$PYTHON" ]; then
            echo -e "${YELLOW}! 虚拟环境创建失败（$(head -1 /tmp/stamp_venv_err.txt 2>/dev/null)）${NC}"
            echo -e "${YELLOW}! 改用系统 Python（安装时可能需要 --break-system-packages）${NC}"
            PYTHON="$BASE_PYTHON"
        fi
    fi
fi
fi

# ============================================================
# 3. 检查 pip
# ============================================================
if [ "$VENV_MANAGER" != "uv" ] && ! $PYTHON -m pip --version &>/dev/null 2>&1; then
    echo -e "${YELLOW}! pip 未安装，正在安装...${NC}"
    $PYTHON -m ensurepip --upgrade 2>/dev/null || {
        curl -sS https://bootstrap.pypa.io/get-pip.py | $PYTHON
    }
fi

# ============================================================
# 4. 检查并安装依赖（PaddlePaddle / PaddleOCR / OpenCV）
# ============================================================
if [ "$SKIP_INSTALL" -eq 1 ]; then
    : # 已在上方提示，跳过
else
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
    if [ "$VENV_MANAGER" != "none" ]; then
        echo ""
        echo -e "${GREEN}  将安装到虚拟环境: ${NC}$VENV_DIR"
    fi
    echo ""
    read -p "是否立即安装？(Y/n) " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Nn]$ ]]; then
        echo -e "${YELLOW}已取消安装。下次运行时会再次提示。${NC}"
        exit 0
    fi

    # 安装助手：镜像源失败自动回退官方 PyPI；PEP 668 环境自动 --break-system-packages
    MIRROR="-i https://mirror.baidu.com/pypi/simple --trusted-host mirror.baidu.com"
    pip_exec() {
        if [ "$VENV_MANAGER" = "uv" ] && command -v uv &>/dev/null 2>&1; then
            uv pip install --python "$PYTHON" "$@"
        else
            if ! $PYTHON -m pip install "$@"; then
                echo -e "${YELLOW}! 常规安装失败（PEP 668 外部管理环境限制？），改用 --break-system-packages 重试...${NC}"
                $PYTHON -m pip install --break-system-packages "$@"
            fi
        fi
    }
    pip_install() {
        if ! pip_exec "$1" $MIRROR; then
            echo -e "${YELLOW}! 镜像源安装失败，改用官方 PyPI 重试...${NC}"
            pip_exec "$1"
        fi
    }

    echo ""
    echo -e "${BLUE}[1/3] 安装 PaddlePaddle (CPU 版)...${NC}"
    if ! pip_install paddlepaddle; then
        echo -e "${RED}✗ PaddlePaddle 安装失败${NC}"
        echo "  常见原因：当前 Python 版本过新（3.14+），PaddlePaddle 尚无对应安装包，"
        echo "  或镜像源缺少对应平台安装包。"
        echo "  请安装 Python 3.9-3.12 后重试："
        echo "    brew install python@3.12"
        echo "  然后重新运行本启动器（会自动选择可用版本或创建 .venv）。"
        exit 1
    fi
    echo ""
    echo -e "${BLUE}[2/3] 安装 PaddleOCR...${NC}"
    pip_install paddleocr
    echo ""
    echo -e "${BLUE}[3/3] 安装 OpenCV...${NC}"
    pip_install opencv-python
    echo ""
    echo -e "${GREEN}${BOLD}✓ 所有依赖安装完成${NC}"
    echo ""
fi
fi

# ============================================================
# 5. 检查核心文件
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
# 6. 检查端口是否被占用
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
# 7. 启动服务
# ============================================================
echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${BLUE}正在启动 OCR 服务（首次加载模型约需 15 秒）...${NC}"

$PYTHON stamp_app.py --port $PORT --no-browser &
SERVER_PID=$!

# 等待服务就绪（最多 60 秒）
READY=0
health_check() {
    if command -v curl &>/dev/null; then
        curl -s "http://127.0.0.1:$PORT/health" &>/dev/null
    else
        $PYTHON -c "import urllib.request,sys; sys.exit(0 if urllib.request.urlopen('http://127.0.0.1:$PORT/health', timeout=2).status == 200 else 1)" &>/dev/null
    fi
}
for i in $(seq 1 60); do
    if health_check; then
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
# 8. 打开浏览器
# ============================================================
URL="http://127.0.0.1:$PORT/"
if [ "$NO_BROWSER" -eq 1 ]; then
    echo -e "${YELLOW}! 已指定 --no-browser，不自动打开浏览器${NC}"
else
    echo -e "${BLUE}正在打开浏览器...${NC}"
    open "$URL" 2>/dev/null || xdg-open "$URL" 2>/dev/null || echo -e "${YELLOW}请手动打开: $URL${NC}"
fi

echo ""
echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${GREEN}${BOLD}  印章对比工具已启动${NC}"
echo -e "  浏览器访问: ${BOLD}$URL${NC}"
echo -e "  停止服务  : 按 ${BOLD}Ctrl+C${NC} 或关闭此窗口"
echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo ""

# 保持脚本运行（等待服务进程结束或用户中断）
wait "$SERVER_PID" 2>/dev/null

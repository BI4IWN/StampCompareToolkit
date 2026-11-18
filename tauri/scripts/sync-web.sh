#!/bin/sh
# 把仓库里的单文件前端同步进 Tauri 的 web 目录，并追加调试钩子（只在预览版生效）
set -e
here="$(cd "$(dirname "$0")" && pwd)"
repo="${STAMP_REPO:-$HOME/Downloads/印章比对/StampCompareToolkit}"
src="$repo/stamp-toolkit/src/stamp-compare.html"
dst="$here/../web/index.html"
python3 - "$src" "$dst" "$here/../web/desktop-shim.html" "$here/../web/debug-hook.html" <<'PY'
import sys
src, dst, hook, dbg = sys.argv[1:5]
html = open(src, encoding='utf-8').read()
def read(f):
    try:
        return open(f, encoding='utf-8').read()
    except FileNotFoundError:
        return ''
# shim 必须在前端脚本之前定义 window.stampDesktop，否则桌面桥不会注入顶栏按钮
shim = read(hook)
if shim:
    i = html.find('<body')
    j = html.find('>', i) + 1
    html = html[:j] + shim + html[j:]
extra = read(dbg)
if extra:
    html = html.replace('</body>', extra + '</body>', 1)
open(dst, 'w', encoding='utf-8').write(html)
print(f'synced {len(html)} chars -> {dst}')
PY

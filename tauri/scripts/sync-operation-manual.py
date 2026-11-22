#!/usr/bin/env python3
# 把仓库里的正式操作手册同步进 Tauri web 目录，并写入页面内嵌副本。
# 使用说明随版本打包、离线可读，不从网络拉取。
from __future__ import annotations

import argparse
import re
import shutil
import sys
from pathlib import Path

SCRIPT_RE = re.compile(
    r'(<script type="text/plain" id="bundled-operation-manual">)(.*?)(</script>)',
    re.S,
)


def escape_for_script(md: str) -> str:
    # 避免手册正文误关 script 标签
    return md.replace("</script>", "<\\/script>")


def inject(html: str, md: str) -> str:
    payload = "\n" + escape_for_script(md).rstrip() + "\n"
    if not SCRIPT_RE.search(html):
        raise SystemExit("missing <script type=\"text/plain\" id=\"bundled-operation-manual\"> in HTML")
    return SCRIPT_RE.sub(r"\1" + payload + r"\3", html, count=1)


def write_if_changed(path: Path, text: str) -> bool:
    old = path.read_text(encoding="utf-8") if path.exists() else None
    if old == text:
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return True


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--web-dir", required=True, help="tauri/web 目录")
    ap.add_argument("--html", action="append", default=[], help="要写入内嵌手册的 HTML（可重复）")
    args = ap.parse_args()

    repo = Path(args.repo)
    # 页面内嵌的是「使用说明 + 技巧」专用文档（不含版本记录/更新日志）；
    # 完整的 operation-manual.md 仍作为仓库文档存在，不进 App。
    src = repo / "stamp-toolkit" / "docs" / "usage-guide.md"
    if not src.is_file():
        raise SystemExit(f"usage-guide.md not found: {src}")
    md = src.read_text(encoding="utf-8")
    if "正式桌面版为 **Tauri + Rust**" not in md:
        raise SystemExit("usage-guide.md 缺少正式线口径，拒绝同步")
    for bad in ("版本记录", "更新日志", "变更记录"):
        if bad in md:
            raise SystemExit(f"usage-guide.md 不应包含「{bad}」段落")

    web = Path(args.web_dir)
    dest_md = web / "usage-guide.md"
    copied = write_if_changed(dest_md, md if md.endswith("\n") else md + "\n")
    print(f"{'updated' if copied else 'unchanged'} {dest_md}")

    for html_path in args.html:
        p = Path(html_path)
        if not p.is_file():
            raise SystemExit(f"html not found: {p}")
        new = inject(p.read_text(encoding="utf-8"), md)
        changed = write_if_changed(p, new)
        print(f"{'updated' if changed else 'unchanged'} {p}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

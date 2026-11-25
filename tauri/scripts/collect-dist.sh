#!/bin/sh
# 把构建出的安装包统一归位到仓库的 desktop/dist/（只保留当前版本的产物）
set -e
here="$(cd "$(dirname "$0")" && pwd)"
repo="${STAMP_REPO:-$(git -C "$here/../.." rev-parse --show-toplevel 2>/dev/null || true)}"
[ -n "$repo" ] || repo="$HOME/Downloads/印章比对/StampCompareToolkit"
out="$repo/desktop/dist"
bundle="$here/../src-tauri/target/release/bundle"
ver="$(node -p "require('$here/../package.json').version" 2>/dev/null || echo 0)"
mkdir -p "$out"
rm -f "$out"/*
# macOS dmg
for f in "$bundle"/dmg/*.dmg; do
  [ -e "$f" ] || continue
  cp -f "$f" "$out/StampCompareToolkit-Tauri-${ver}-mac-arm64.dmg"
done
# Windows 安装包（CI 产物命名保持原样）
for f in "$bundle"/*.exe "$bundle"/nsis/*.exe; do
  [ -e "$f" ] || continue
  base="$(basename "$f")"
  case "$base" in
    StampCompareToolkit-*) cp -f "$f" "$out/$base" ;;
    *) cp -f "$f" "$out/StampCompareToolkit-Tauri-${ver}-win-x64-setup.exe" ;;
  esac
done
ls -lh "$out"

#!/usr/bin/env bash
# @thunder
# name: inspect_image
# description: 检查本地图片元数据（格式、文件大小、分辨率）供多模态模型参考
# parameters:
#   type: object
#   properties:
#     path:
#       type: string
#       description: 本地图片文件路径
#   required: [path]
# @end

set -euo pipefail

payload=$(cat)
path=$(echo "$payload" | python3 -c 'import sys, json; print(json.load(sys.stdin).get("path", ""))' 2>/dev/null || echo "")

if [ -z "$path" ]; then
    echo "Error: missing 'path' parameter" >&2
    exit 1
fi

cwd="${THUNDER_CWD:-$(pwd)}"
if [[ "$path" = /* ]]; then
    target="$path"
else
    target="${cwd}/${path}"
fi

if [ ! -f "$target" ]; then
    echo "Error: Image file not found: $target" >&2
    exit 2
fi

bytes=$(wc -c < "$target" | tr -d ' ')
mime="application/octet-stream"
if command -v file >/dev/null 2>&1; then
    mime=$(file -b --mime-type "$target" 2>/dev/null || echo "unknown")
fi

dimensions="unknown"
if command -v sips >/dev/null 2>&1; then
    width=$(sips -g pixelWidth "$target" 2>/dev/null | awk '/pixelWidth:/ {print $2}')
    height=$(sips -g pixelHeight "$target" 2>/dev/null | awk '/pixelHeight:/ {print $2}')
    if [ -n "$width" ] && [ -n "$height" ]; then
        dimensions="${width}x${height}"
    fi
fi

cat <<EOF
{
  "path": "$path",
  "resolved_path": "$target",
  "size_bytes": $bytes,
  "mime_type": "$mime",
  "dimensions": "$dimensions"
}
EOF

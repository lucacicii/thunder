#!/usr/bin/env bash
#
# 开发模式：编译 → 用固定身份重签名 → 重启守护进程。
#
#   ./scripts/dev.sh              # debug 构建（快，日常改代码用这个）
#   ./scripts/dev.sh --release    # release 构建
#   ./scripts/dev.sh --no-restart # 只编译+签名，不动守护进程
#
# 为什么每次都要签名：
#   cargo 产出的二进制只有 ld 的 ad-hoc 签名，identifier 和 cdhash 都由内容派生，
#   每次编译都变。macOS TCC 用 designated requirement 匹配屏幕录制授权，
#   ad-hoc 的 DR 实质就是 cdhash —— 内容一变授权就“不再匹配”，于是反复弹框。
#   用固定的本地自签名证书签名后 DR 变成
#     identifier "com.lucas.thunder-memory" and certificate root = H"<证书指纹>"
#   与内容无关，所以每编译一次就重签一次，授权永远不会失效（耗时 ~1s）。
#
set -euo pipefail

SIGN_ID="${THUNDER_SIGN_ID:-Thunder Local Signing}"
MAIN_ID="com.lucas.thunder-memory"
HELPER_ID="com.lucas.thunder-vision-ocr"
AGENT_LABEL="com.thunder.memory"
INSTALL_PATH="$HOME/.cargo/bin/thunder-memory"
HELPER_PATH="$HOME/.thunder/bin/thunder-vision-ocr"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PROFILE="debug"
RESTART=1
for arg in "$@"; do
  case "$arg" in
    --release|-r) PROFILE="release" ;;
    --no-restart) RESTART=0 ;;
    -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "未知参数: $arg" >&2; exit 64 ;;
  esac
done

info() { printf '\033[36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[!]\033[0m %s\n' "$*"; }
die()  { printf '\033[31m[x]\033[0m %s\n' "$*" >&2; exit 1; }

if ! security find-identity -p codesigning 2>/dev/null | grep -q "\"$SIGN_ID\""; then
  die "找不到签名身份 \"$SIGN_ID\"，见 scripts/install-release.sh 顶部的证书创建步骤"
fi

cd "$REPO_ROOT"

info "cargo build ($PROFILE)"
if [ "$PROFILE" = "release" ]; then
  cargo build --release
  BIN="$REPO_ROOT/../../target/release/thunder-memory"
else
  cargo build
  BIN="$REPO_ROOT/../../target/debug/thunder-memory"
fi
[ -f "$BIN" ] || die "构建产物不存在: $BIN"

# 停掉守护进程再覆盖，避免覆盖正在运行的二进制页面
if [ "$RESTART" = 1 ] && launchctl print "gui/$(id -u)/$AGENT_LABEL" >/dev/null 2>&1; then
  info "卸载 launchd 守护进程"
  launchctl bootout "gui/$(id -u)/$AGENT_LABEL" >/dev/null 2>&1 || true
fi

sign() {
  local path="$1" ident="$2"
  codesign --force --sign "$SIGN_ID" --identifier "$ident" --timestamp=none "$path" >/dev/null 2>&1 \
    || die "签名失败: $path"
}

# 先把源码产物签好（软链接模式 / cargo run 都直接用它）
sign "$BIN" "$MAIN_ID"
info "已签名 $BIN"

# 再看 ~/.cargo/bin/thunder-memory 是软链接还是实体副本，分别处理
if [ -L "$INSTALL_PATH" ]; then
  RESOLVED="$(cd "$(dirname "$INSTALL_PATH")" && readlink "$INSTALL_PATH")"
  info "安装路径是软链接 → $RESOLVED（无需拷贝）"
else
  info "覆盖安装到 $INSTALL_PATH"
  mkdir -p "$(dirname "$INSTALL_PATH")"
  cp "$BIN" "$INSTALL_PATH"
  sign "$INSTALL_PATH" "$MAIN_ID"
fi

if [ -f "$HELPER_PATH" ]; then
  sign "$HELPER_PATH" "$HELPER_ID" || warn "OCR 助手签名失败（TCC 归因给父进程，不影响授权）"
fi

if [ "$RESTART" = 1 ]; then
  if launchctl print "gui/$(id -u)/$AGENT_LABEL" >/dev/null 2>&1 \
     || [ -f "$HOME/Library/LaunchAgents/$AGENT_LABEL.plist" ]; then
    launchctl print "gui/$(id -u)/$AGENT_LABEL" >/dev/null 2>&1 \
      || launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/$AGENT_LABEL.plist"
    launchctl kickstart -k "gui/$(id -u)/$AGENT_LABEL"
    info "守护进程已重启（launchd）"
  else
    "$INSTALL_PATH" restart >/dev/null 2>&1 || { "$INSTALL_PATH" stop >/dev/null 2>&1 || true; "$INSTALL_PATH" start; }
    info "守护进程已重启（start/stop）"
  fi
fi

DR="$(codesign -d -r- "$(readlink -f "$INSTALL_PATH" 2>/dev/null || echo "$INSTALL_PATH")" 2>&1 | grep 'designated =>' || true)"
info "DR: ${DR#designated => }"
case "$DR" in
  *cdhash*) warn "DR 里还有 cdhash —— 授权会继续失效，检查签名是否真的生效" ;;
esac

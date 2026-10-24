#!/usr/bin/env bash
#
# 构建并安装正式版 thunder-memory，并使用固定的本地代码签名身份重新签名。
#
# 为什么需要重新签名：
#   `cargo build` 产出的二进制只有 ld 生成的 ad-hoc (linker-signed) 签名，
#   其 code signing identifier 与 cdhash 都由二进制内容派生——每次重新编译都会变。
#   macOS TCC（屏幕录制权限）用「designated requirement」来匹配已授权的条目，
#   ad-hoc 签名的 DR 基本等同于 cdhash，因此一旦重新编译，
#   系统设置里那条已经打开的授权记录就会「不再匹配」，
#   于是每次截图都重新弹「thunder-memory 想要录制屏幕」的对话框。
#   用一个固定的本地自签名证书签名后，DR 变为
#     identifier "com.lucas.thunder-memory" and certificate root = H"<证书指纹>"
#   与二进制内容无关，重新编译/覆盖安装都不会让授权失效。
#
set -euo pipefail

SIGN_ID="${THUNDER_SIGN_ID:-Thunder Local Signing}"
MAIN_ID="com.lucas.thunder-memory"
HELPER_ID="com.lucas.thunder-vision-ocr"
INSTALL_DIR="$HOME/.cargo/bin"
HELPER_PATH="$HOME/.thunder/bin/thunder-vision-ocr"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

info() { printf '\033[36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[!]\033[0m %s\n' "$*"; }
die()  { printf '\033[31m[x]\033[0m %s\n' "$*" >&2; exit 1; }

require_identity() {
  if ! security find-identity -p codesigning 2>/dev/null | grep -q "\"$SIGN_ID\""; then
    cat >&2 <<EOF
[x] 找不到代码签名身份 "$SIGN_ID"。

创建一个本地自签名证书（一次性，只做一次）：

  mkdir -p /tmp/thunder-sign && cd /tmp/thunder-sign
  openssl req -x509 -newkey rsa:2048 -nodes -keyout key.pem -out cert.pem -days 3650 \\
    -subj "/CN=$SIGN_ID/O=Local" \\
    -addext "keyUsage=critical,digitalSignature" \\
    -addext "extendedKeyUsage=critical,codeSigning" \\
    -addext "basicConstraints=critical,CA:false"
  openssl pkcs12 -export -inkey key.pem -in cert.pem -out id.p12 \\
    -passout pass:thunder -name "$SIGN_ID"
  security import id.p12 -k "$HOME/Library/Keychains/login.keychain-db" \\
    -P thunder -T /usr/bin/codesign -A

（证书显示为 "CSSMERR_TP_NOT_TRUSTED" 是正常的，codesign 与 TCC 都不要求它被信任。）
EOF
    exit 1
  fi
}

sign() {
  local path="$1" ident="$2"
  [ -f "$path" ] || die "目标不存在: $path"
  codesign --force --sign "$SIGN_ID" --identifier "$ident" --timestamp=none "$path" >/dev/null
  info "已签名 $path ($ident)"
}

info "构建 release 二进制"
cd "$REPO_ROOT"
cargo build --release

require_identity

# 如果装了 LaunchAgent，就交给 launchd 重启，否则会出现「launchd 一份 + start 一份」双守护进程。
AGENT_LABEL="com.thunder.memory"
if launchctl print "gui/$(id -u)/$AGENT_LABEL" >/dev/null 2>&1; then
  USE_LAUNCHD=1
  info "卸载 launchd 守护进程（避免运行中的二进制被覆盖）"
  launchctl bootout "gui/$(id -u)/$AGENT_LABEL" >/dev/null 2>&1 || true
else
  USE_LAUNCHD=0
  info "停止后台守护进程（忽略未运行的情况）"
  "$INSTALL_DIR/thunder-memory" stop >/dev/null 2>&1 || true
fi
sleep 1

info "安装到 $INSTALL_DIR/thunder-memory"
mkdir -p "$INSTALL_DIR"
cp "$REPO_ROOT/../../target/release/thunder-memory" "$INSTALL_DIR/thunder-memory"
sign "$INSTALL_DIR/thunder-memory" "$MAIN_ID"

# 助手由 thunder-memory 首次运行时用 swiftc 现编译，签名同样会随内容变化。
# 它作为子进程调用时 TCC 归因给父进程，所以签名只是锦上添花，不必强制成功。
if [ -f "$HELPER_PATH" ]; then
  sign "$HELPER_PATH" "$HELPER_ID" || warn "OCR 助手签名失败（不影响屏幕录制授权）"
fi

if [ "$USE_LAUNCHD" = 1 ]; then
  info "重新加载 launchd 守护进程"
  launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/$AGENT_LABEL.plist"
  sleep 2
  launchctl list | grep "$AGENT_LABEL" || true
else
  info "启动并检查状态"
  "$INSTALL_DIR/thunder-memory" start
  sleep 2
fi
"$INSTALL_DIR/thunder-memory" status

cat <<EOF

签名校验：
  codesign -d -r- -vv "$INSTALL_DIR/thunder-memory" 2>&1 | grep designated
期望输出：
  designated => identifier "$MAIN_ID" and certificate root = H"..."
只要这里没有出现 cdhash，重新编译后就不会再弹屏幕录制授权框。
EOF

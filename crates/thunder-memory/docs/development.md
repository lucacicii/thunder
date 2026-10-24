# 🛠️ Thunder Memory 开发者指南 (Development Guide)

本文档面向 `thunder-memory` 的功能开发者，系统性说明**本地开发调试**与**系统常驻正式版本**之间的隔离机制、开发工作流、数据库管理与发布更新规范。

---

## 一、开发版本 vs 正式版本：隔离机制

`thunder-memory` 作为系统级常驻的认知守护进程，与普通命令行工具有所不同。为了保证你**在日常使用正式版本的同时，能够安全、互不冲突地迭代新功能**，本项目在设计上采用了双重隔离机制：

### 1. 二进制寻址隔离（Binary Resolution）
- **正式常驻版本**：存放在 `~/.cargo/bin/thunder-memory`，被系统的 LaunchAgent、全局 CLI 以及 Thunder 的 MCP 协议调用。
- **本地开发版本**：位于仓库根目录的 `target/debug/thunder-memory` 或 `target/release/thunder-memory`。

### 2. 数据层隔离（Database Isolation）
- **正式生产数据**：默认保存在 `~/.thunder/memory.db`（记录日常真实屏幕与长期记忆，包含 WAL 索引与元数据）。
- **开发测试数据**：单元测试默认使用 SQLite 纯内存模式（In-Memory），绝不触碰生产库；调试实验支持命令行参数重定向到临时文件。

---

## 二、双轨开发工作流（两种开发模式）

根据你的开发目标，可以选择以下两种模式之一：

### 模式 A：安全隔离开发（推荐：重构、实验性特性、改动数据库 Schema）

适用于：开发全新感知模块、修改 SQLite 表结构、调整小模型 Prompt 算法等场景。此模式下**完全不影响**正在后台跑着的正式常驻进程。

```bash
cd /Users/lucas/wz/GitHub/thunder/crates/thunder-memory

# 1. 运行所有单元测试（全部在内存中进行，100% 隔离）
cargo test

# 2. 诊断单次 OCR（仅打印到终端，不入库）
cargo run -- ocr-once

# 3. 指定独立的临时数据库进行入库测试与提炼演练
cargo run -- ocr-once --save
cargo run -- synthesize
cargo run -- status
```

---

### 模式 B：边改边用模式（推荐：日常快速修 Bug、增加实用 CLI、微调功能）

如果你希望自己写出的最新代码**立即生效**并服务于整个系统的 Thunder 和桌面日常，可以使用**符号软链接（Symlink）**：

#### 1. 建立软链接（一次性配置）
```bash
# 停掉旧的常驻进程
thunder-memory stop

# 将系统路径软链接到源码的 debug 产物
ln -sf /Users/lucas/wz/GitHub/thunder/target/debug/thunder-memory ~/.cargo/bin/thunder-memory
```

#### 2. 日常修改与热生效
在此配置下，你在仓库中每次执行：
```bash
./scripts/dev.sh            # = cargo build + 重签名 + 重启守护进程
./scripts/dev.sh --release  # 想让常驻进程跑 release 时
./scripts/dev.sh --no-restart  # 只编译+签名，不动守护进程
```
几秒后，系统中的 `thunder-memory`、Thunder Agent 的 MCP 工具调用，都会**自动跑在你刚刚编译出来的最新代码上**。

> ⚠️ `tmd` 请在普通 Terminal/iTerm 里跑。若在 Agent Resume 面板内置终端里执行，守护进程会继承
> 面板的 TCC 责任归因，屏幕录制授权无法落盘，会无限弹「要记录屏幕」的授权框
> （详见 README 安装节）。已用 `install-launchagent` 托管时，`tmd --no-restart` 编译签名后
> 由 launchd 自动拉起，不受此影响。

> ⚠️ **不要直接 `cargo build` 之后手工拷贝/重链**。cargo 产出的二进制只有 ad-hoc 签名，
> cdhash 由内容派生，内容一变 TCC 就判定“不是同一个程序”，屏幕录制授权失效并开始弹框。
> `dev.sh` 每次编译后都会用固定证书重签一次（耗时 ~70ms），DR 恒为
> `identifier "com.lucas.thunder-memory" and certificate root = H"<证书指纹>"`，
> 所以你一天编译一百次也不会再弹授权框。
>
> 建议加个别名，防止忘了走脚本：
> ```bash
> echo "alias tmd='/Users/lucas/wz/GitHub/thunder/crates/thunder-memory/scripts/dev.sh'" >> ~/.zshrc
> ```
> 另：用 `cargo run` 直接跑源码（`target/debug/thunder-memory`）与
> `~/.cargo/bin/thunder-memory` 是**两个不同的 TCC 主体（路径）**，两边都要各自授权一次；
> 但签名身份相同，所以各自只需授权一次，编译不影响。
> 只想稳定日常使用、不频繁改代码时，请用 `./scripts/install-release.sh`。

---

## 三、常用调试与开发命令

### 1. 屏幕捕获与 Vision OCR 调试
Swift 原生 OCR 引擎位于 `swift/thunder_vision_ocr.swift`。
- 如果修改了 Swift 源码，可删除缓存二进制触发 Rust 自动重新编译：
  ```bash
  rm -f ~/.thunder/bin/thunder-vision-ocr
  cargo run -- ocr-once
  ```
- 也可以手动独立编译测试 Swift 脚本：
  ```bash
  swiftc -O swift/thunder_vision_ocr.swift -o /tmp/test-ocr
  /tmp/test-ocr
  ```

### 2. 小模型提炼（Synthesis Engine）调试
- 手动触发一次合成周期，观察控制台输出的日志级别（`RUST_LOG=debug`）：
  ```bash
  RUST_LOG=debug cargo run -- synthesize
  ```
- 检查长期记忆表新增内容：
  ```bash
  cargo run -- list-memories 5
  ```

### 3. Composer 数据同步与微调集导出调试
- 增量同步测试：
  ```bash
  cargo run -- sync-composer
  ```
- 导出指定格式训练集并检查样本结构：
  ```bash
  cargo run -- export-training openai /tmp/test-sft.jsonl
  head -n 2 /tmp/test-sft.jsonl
  ```

### 4. MCP 服务交互调试
通过管道直接向 MCP Server 发送 JSON-RPC 消息：
```bash
# 测试工具枚举
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | cargo run -- mcp

# 测试记忆状态调用
echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_stats","arguments":{}}}' | cargo run -- mcp
```

---

## 四、正式发布与升级部署流程

当某个新功能或优化测试完毕，准备作为日常正式版固化时：

```bash
# 一条命令完成：构建 → 停止守护进程 → 覆盖安装 → 重新签名 → 启动 → 校验
./scripts/install-release.sh
```

**不要手工 `cp` / 软链接替换二进制**（旧版文档里的做法）。`cargo build` 产出的二进制
只有 ld 生成的 ad-hoc 签名，其 identifier 与 cdhash 都由内容派生，每次编译都会变；
macOS 的屏幕录制授权是按 designated requirement 匹配的，内容一变授权就“不再匹配”，
于是每次截图都重新弹授权框。`install-release.sh` 会用固定的本地自签名身份重新签名，
把 DR 固定成 `identifier "com.lucas.thunder-memory" and certificate root = H"<证书指纹>"`，
与二进制内容无关，之后无论重编译多少次都不会再弹框。

手工步骤（等价）：

```bash
# 1. 优雅停止后台常驻进程
thunder-memory stop

# 2. 编译最高优化的 Release 二进制
cd /Users/lucas/wz/GitHub/thunder/crates/thunder-memory
cargo build --release

# 3. 替换系统正式二进制并用固定身份重签名
cp ../../target/release/thunder-memory ~/.cargo/bin/thunder-memory
codesign --force --sign "Thunder Local Signing" \
  --identifier com.lucas.thunder-memory --timestamp=none ~/.cargo/bin/thunder-memory

# 4. 重新启动守护进程并验证运行状态
thunder-memory start
sleep 2
thunder-memory status
```

---

## 五、常见问题与注意事项 (FAQ)

1. **macOS 屏幕录制权限反复弹框**：
   - `thunder-memory` 通过自带的 Swift 助手（`~/.thunder/bin/thunder-vision-ocr`）调用
     `/usr/sbin/screencapture`，TCC 会把这次录屏请求归因到负责进程（responsible process），
     也就是 `thunder-memory` 本体，所以授权列表里出现的是 `thunder-memory` 而不是终端。
   - **典型症状**：系统设置里 `屏幕与系统录制` 中 `thunder-memory` 的开关明明是打开的、
     却每次截图都重新弹框。原因不是开关没生效，而是 TCC 存的授权条目里记录的
     designated requirement 与当前二进制对不上了。`log show --predicate 'process == "tccd"'`
     里能看到关键行：
     `Failed to match existing code requirement for subject /Users/lucas/.cargo/bin/thunder-memory and service kTCCServiceScreenCapture`。
   - 只有 ad-hoc 签名（`codesign -dvvv` 显示 `adhoc,linker-signed`、`Internal requirements=none`）
     的二进制才会这样，因为它的 DR 实质是 cdhash，重新编译即失效。
     用 `./scripts/install-release.sh` 安装即彻底解决；自签名证书的创建方法见脚本内注释。
   - 已经装了 ad-hoc 版本、列表里留下旧条目的情况：在 `系统设置 -> 隐私与安全性 ->
     屏幕与系统录制` 里用 `−` 删掉 `thunder-memory` 那条旧记录，然后重启守护进程触发出**一次**
     授权框，打开开关即可（此后不再弹）。
   - 检查当前二进制的 DR 是否已稳定：
     `codesign -d -r- -vv ~/.cargo/bin/thunder-memory 2>&1 | grep designated`，
     稳定版本输出里只有 `identifier` + `certificate root`，**不应出现 cdhash**。
2. **PID 锁残留**：
   - `run`（LaunchAgent 的入口）启动时会自己写入 `~/.thunder/thunder-memory.pid`、退出时清理，所以 `status` / `stop` 现在总能定位真实进程；若检测到另一个存活的 daemon，日志里会出现 `WARN Another thunder-memory daemon is already running`。
   - 若异常崩溃（SIGKILL / 断电）导致文件残留且旧进程已不存在，`thunder-memory start` 自会覆盖它；实在异常时手动清理：`rm -f ~/.thunder/thunder-memory.pid`。
3. **SQLite 锁冲突**：
   - 本项目已默认开启 `PRAGMA journal_mode = WAL;`，支持高并发读写，但多个进程尽量避免对同一文件执行长耗时排他事务。

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
cd /Users/lucas/wz/GitHub/thunder-memory

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
ln -sf /Users/lucas/wz/GitHub/thunder-memory/target/debug/thunder-memory ~/.cargo/bin/thunder-memory
```

#### 2. 日常修改与热生效
在此配置下，你在仓库中每次执行：
```bash
cargo build
```
编译完成后，系统中的 `thunder-memory`、Thunder Agent 的 MCP 工具调用，都会**立即自动跑在你刚刚编译出来的最新代码上**，无需反复拷贝二进制。

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
# 1. 优雅停止后台常驻进程
thunder-memory stop

# 2. 编译最高优化的 Release 二进制
cd /Users/lucas/wz/GitHub/thunder-memory
cargo build --release

# 3. 替换系统正式二进制（如果之前用了软链接可重新固化，或链接到 release）
cp target/release/thunder-memory ~/.cargo/bin/thunder-memory
# 或者：ln -sf $(pwd)/target/release/thunder-memory ~/.cargo/bin/thunder-memory

# 4. 重新启动守护进程并验证运行状态
thunder-memory start
sleep 2
thunder-memory status
```

---

## 五、常见问题与注意事项 (FAQ)

1. **macOS 屏幕录制权限提示**：
   - `thunder-memory` 默认调用系统自带的 `/usr/sbin/screencapture`，它继承终端（Terminal / iTerm / Cursor）的“屏幕与系统录制”权限。如果截图为空或全黑，请检查 macOS `系统设置 -> 隐私与安全性 -> 屏幕与系统录制` 中是否允许了你当前运行的宿主终端。
2. **PID 锁残留**：
   - 若异常崩溃导致 `thunder-memory start` 报错已有实例运行，可检查并清理 PID 文件：`rm -f ~/.thunder/thunder-memory.pid`。
3. **SQLite 锁冲突**：
   - 本项目已默认开启 `PRAGMA journal_mode = WAL;`，支持高并发读写，但多个进程尽量避免对同一文件执行长耗时排他事务。

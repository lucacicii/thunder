# Thunder 工作区

[English](WORKSPACE_en.md) | [简体中文](WORKSPACE.md)

本文件讲**代码怎么组织、依赖往哪边走、日常怎么验证**。架构细节在 [`docs/architecture.md`](docs/architecture.md)。

---

## 1. 单一仓库

- GitHub：<https://github.com/lucacicii/thunder>
- 单一 Git Monorepo + 单一 Cargo workspace（根 `Cargo.toml` 治理 12 个成员，共享一个 `Cargo.lock`）。
- git 根目录就是工作区根；不要在子 crate 里 `git init`。
- 子 crate 不再保留各自的 `LICENSE` / `.gitignore` 副本：它们由根级资产与 `[workspace.package]` 继承统一治理。

```bash
git clone https://github.com/lucacicii/thunder.git
cd thunder
```

---

## 2. 目录布局

```text
thunder/
├── README.md / README_en.md            # 主页
├── WORKSPACE.md / WORKSPACE_en.md      # 本文件
├── Cargo.toml                          # workspace 定义（12 成员）+ 共享依赖
├── rust-toolchain.toml                 # 固定 1.98.0（与 CI 同步）
├── run.sh / daemon.sh / test.sh        # TUI / sidecar / 测试
├── scripts/check-version.py            # 版本一致性门禁
├── docs/
│   ├── architecture.md                 # 架构与模块地图
│   ├── permission-structure.md         # 权限、审批、四档模式
│   └── migration-0.6.md                # 0.5 → 0.6 迁移
├── .github/workflows/                  # CI（fmt / clippy / version / test）与 release
├── .thunder/                           # 项目级配置（用户级 ~/.thunder 与之同构）
├── .arp/                               # 归 agent-resume-panel，thunder 不读取
├── examples/electron/                  # Electron 侧车集成示例
├── thunder-agent-types/                # 纯 wire 类型
├── thunder-agent-loop/                 # 内核
├── thunder-agent-policy/               # 策略层（工具漏斗 / gate / UI / 只读判据）
├── thunder-agent-pack-code/            # 参考能力包
├── thunder-agent-providers/            # 模型目录 + 进程内流式传输
├── thunder-agent-skills/               # Skill 解析与注入
├── thunder-agent-mcp/                  # MCP 客户端 / 传输 / 桥接
├── thunder-agent-root/                 # 宿主装配
├── thunder-agent-daemon/               # NDJSON sidecar
├── thunder-agent-runtime/              # JSONL 无头运行时
└── thunder-agent-core/
    ├── conversation/                   # 会话存储（thunder-conversation）
    └── tui/                            # 终端界面（thunder-tui）
```

---

## 3. 依赖方向

内部依赖一律用 path，不允许 git URL 交叉引用。

| Crate | 依赖的内部 crate |
|---|---|
| `thunder-agent-types` | — |
| `thunder-agent-loop` | `types`（`policy` 仅在 `[dev-dependencies]`） |
| `thunder-agent-policy` | `types`、`loop` |
| `thunder-agent-pack-code` | `types`、`policy`、`loop` |
| `thunder-agent-providers` | `types`、`policy`、`loop` |
| `thunder-agent-skills` | `types`、`policy`、`loop` |
| `thunder-agent-mcp` | `types`、`policy`、`loop` |
| `thunder-conversation` | `loop` |
| `thunder-agent-runtime` | `types`、`policy`、`loop`、`providers` |
| `thunder-agent-root` | `types`、`policy`、`loop`、`pack-code`、`providers` |
| `thunder-agent-daemon` | `types`、`policy`、`loop`、`providers`、`skills`、`mcp`、`conversation`、`root` |
| `thunder-tui` | `types`、`policy`、`loop`、`pack-code`、`providers`、`skills`、`mcp`、`conversation`、`root` |

> `root` 的 `conversation` / `skills` / `mcp` 是**可选 feature**（默认全开），所以上表只列必需依赖；关掉对应 feature 时 `StandardHostBuilder` 不会装那一项。

**分层结论**：`types` 是叶子；`loop` 只认 `types` 与自己的接缝；`policy` 实现内核的 `Tools` 接缝（方向是 policy → loop，不是反向）；`root` 是装配层；`tui` / `daemon` / `runtime` 是并列的最上层二进制。

### 3.1 五条设计原则

1. **内核不持有策略**：`thunder-agent-loop` 里没有 gate、没有 UI、没有审批、没有 MCP。它只调用 `Tools` 与 `LLMClientTrait`，并保证取消、超时、配对不变量。
2. **宿主显式装配**：一次运行的贡献是 `PromptSection` / `ToolPack` / `Observer` / `Prepared` 四个列表；注册即激活，`plugins.disabled` 是唯一减法。
3. **一条工具漏斗**：`ToolExecutor::run_tool` 是唯一入口（守卫 → 模式 → 弹窗 → 拦截器 → 注册表 → trace）。模型调用与插件反向 RPC 都走它。
4. **并发写安全**：写文件按物理路径走 `FILE_MUTATION_LOCKS`（`pack-code/src/builtin/fs.rs`）；内核不持锁。
5. **配置归 `.thunder/`，不碰 `.arp/`**：`.arp/` 属于 agent-resume-panel。用户级数据根可用 `THUNDER_CONFIG_DIR`（或 `--config-dir`）整体重定向，解析集中在 `thunder-agent-loop/src/core/paths.rs`。

---

## 4. 运行与测试

要求 Rust 1.80+（`rust-version`），仓库固定 1.98.0；不需要 Node.js。

```bash
./run.sh          # thunder-tui
./daemon.sh       # thunder-agent-daemon
./test.sh         # 全工作区测试

cargo test -p thunder-agent-loop
cargo test -p thunder-conversation
cargo check --workspace
cargo build --workspace --release
```

三道门禁（与 CI 一致）：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features thunder-agent-daemon/testing-mock -- -D warnings
python3 scripts/check-version.py
```

---

## 5. 提交规范

- 所有改动在工作区根提交，不在子目录建仓库。
- Conventional Commits，英文动词：`feat(root): …`、`fix(policy): …`、`refactor(loop): …`、`docs: …`、`test(loop): …`。
- 提交前本地跑一遍 §4 的三道门禁与 `cargo test --workspace`。

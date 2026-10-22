# 🛠️ Thunder 开发者指南 (Development Guide)

本文档面向 `thunder` 的功能开发者，说明**本地开发版本**与**正式安装版本**之间的切换机制、日常开发工作流、热重启、发布固化，以及与姊妹项目 `thunder-memory` 的刻意差异。

> 一句话：**`thunderctl` 管三种运行态** —— `dev`（跑仓库里刚编译的代码）、`prod`（跑 `~/.cargo/bin` 里固化的安装版）、`bundled`（交还给 Agent Resume 自带版）。改完代码想立刻生效，用 `thunderctl reload`，不用重启 App。

---

## 一、三态总览

| 模式 | 命令 | Desktop 会 spawn | 典型用途 |
|---|---|---|---|
| **dev** | `thunderctl dev` | `<repo>/target/tui-dev/thunder-daemon`（`thunder-tui` 同理） | 改代码时边改边用 |
| **prod** | `thunderctl prod` | `~/.cargo/bin/thunder-daemon` | 跑自己固化发布的版本 |
| **bundled** | `thunderctl bundled` | Agent Resume.app 内 `Resources/thunder/bin/thunder-daemon` | 回归「跟随 App 升级」，零漂移 |

三种模式**只改一处**：`~/.agent-resume-panel/settings.desktop.json` 的 `thunder` 对象。

```jsonc
{
  "thunder": {
    "repoPath":   "/Users/you/wz/GitHub/thunder",
    "daemonPath": "/Users/you/wz/GitHub/thunder/target/tui-dev/thunder-daemon",
    "tuiPath":    "/Users/you/wz/GitHub/thunder/target/tui-dev/thunder-tui"
  }
}
```

`bundled` 模式会把这个对象**整个删掉**（不是写成空字符串），让解析链回落到下一优先级。

写入细节：只改 `thunder` 这一个键，其他键（`providers`、`workbench`、`notes`…）原样保留；序列化会按字典序重排键顺序（`serde_json` 默认行为，Desktop 侧 `JSON.parse` 不关心顺序）；保留原文件的权限位（配置里有 provider API key，不会把 0600 变成 0644）；写入前的备份是 `settings.desktop.json.thunderctl.bak`；写入本身是临时文件 + rename 的原子替换。

查看当前处于哪一态、以及为什么：

```bash
thunderctl status
```

---

## 二、Desktop 是怎么选二进制的（以及为什么你的构建可能没生效）

Agent Resume 的 `daemonResolver` 按下面的顺序取第一个存在的候选，**高优先级一旦命中就结束**：

```
1. THUNDER_DAEMON_BIN            环境变量
2. settings.thunder.daemonPath   ← thunderctl dev/prod 写的就是这里
3. THUNDER_PATH                  环境变量（仓库根目录）
4. settings.thunder.repoPath     仓库根目录
5. bundled                       App 内 Resources/thunder/bin/   ⚠ 排在仓库构建产物之前
6. cwd / $HOME 下的 checkout 猜测（~/wz/GitHub/thunder 等）
```

两个高频踩坑点：

1. **bundle 排在第 5 位、却排在所有 `$HOME` 猜测之前。** 只要你没设 `daemonPath`，App 自带的 sidecar 就永远赢，你在仓库里 `cargo build` 出什么都没用。`thunderctl status` 会把这条链原样打印出来，第一行带 `✓` 的就是 Desktop 下次 spawn 的二进制。
2. **`thunderctl dev` 用的是 `target/tui-dev`。** Desktop 的自动探测只认 `target/release` 和 `target/debug`，所以 dev 模式必须靠显式 `daemonPath` 生效 —— 这正是 `dev` 命令替你做的事。

---

## 三、日常配方

### 3.1 在终端 TUI 里开发（最快循环）

不碰 Desktop，改完直接重跑：

```bash
./run.sh                      # 等同 cargo build --profile release + exec thunder-tui
THUNDER_TUI_PROFILE=tui-dev ./run.sh   # 增量编译、无 LTO，日常推荐
```

`run.sh` 会先构建再 `exec`，所以每次运行都保证跑的是最新代码。

### 3.2 在 Desktop 里开发（边改边用）

```bash
# 一次性切换（会先构建，再把 Desktop 指向 target/tui-dev）
thunderctl dev

# 之后每次改完代码
cargo build --profile tui-dev -p thunder-std -p thunder-tui -p thunder-daemon
thunderctl reload            # 常驻 daemon 原地换二进制，pid 不变
```

TUI 的代码改动**不需要** `reload`：Desktop 里新开一个 thunder TUI 会话就会用新二进制。

如果只想切配置不构建（比如二进制已经构建好了）：

```bash
thunderctl dev --no-build
thunderctl dev --profile debug      # 用普通 debug 构建
thunderctl dev --dry-run            # 只打印将要写入的配置
```

### 3.3 发布固化（prod）

```bash
thunderctl install           # cargo build --profile release + 拷贝 5 个二进制到 ~/.cargo/bin
thunderctl prod              # Desktop 指向 ~/.cargo/bin
thunderctl status            # 核对
```

`install` 会一并安装 `thunder`（一次性任务 CLI）、`thunder-std`（核心 MCP 工具进程）、`thunder-tui`、`thunder-daemon`，以及 `thunderctl` 自己。四个运行期二进制必须在同一目录：`thunder-daemon` / `thunder-tui` / `thunder` 通过 `current_exe` 的**兄弟目录**找 `thunder-std`。

拷贝是「临时文件 + rename」的原子写，正在运行的 daemon 不会看到半个二进制。安装位文件的 mtime 保持**构建时间**（macOS 的 `copyfile` 语义），所以它能直接和 App 自带版比新旧 —— `status` 的「回退风险」就是靠这个算的。

---

## 四、热重启：`thunderctl reload`

### 原理

```
thunderctl reload
  → ps 找到常驻 thunder-daemon
  → 读 <data_root>/state/daemon-<pid>.json 确认它声明了 "hot_reload": true
  → kill -USR1 <pid>
daemon 收到 SIGUSR1：
  → active_tasks 非空？拒绝（日志 warn），继续跑旧二进制
  → 校验 current_exe 存在且可执行
  → 等 120ms 让 NDJSON writer 把在途帧 flush 完
  → execv(自己, 原 argv)
```

因为用的是 `execv` 而不是「退出 + 重新拉起」：

- **pid 不变**；
- **stdin/stdout 管道不变** → Desktop 的 readline 连接原样保留，不会断开；
- **环境变量、argv、config dir 全部继承**；
- 新进程重新写一次 `state/daemon-<pid>.json`（`started_at_ms` 变化），`thunderctl reload` 就靠这个字段确认换心成功，3 秒没变化就报「可能有任务在跑」。

### 什么时候会被拒绝

| 情况 | 行为 |
|---|---|
| 有任务在跑（`active_tasks` 非空） | 拒绝换心，日志 warn，旧进程继续服务 |
| `current_exe` 对应的文件消失 / 没有执行位 | 拒绝，日志 error（不会 exec 失败到死掉） |
| daemon 版本过旧、没有 `state/daemon-*.json` | `thunderctl` 根本不发信号 —— `SIGUSR1` 的默认动作是**终止进程**，绝不能盲发 |
| 磁盘上的二进制没更新 | 照常换心，等于原地重启一次（无害） |

### 什么时候别用 reload

- 改的是 daemon **启动期**逻辑（参数解析、`--config-dir`、信号注册）：能换心，但启动期行为只在下次被 Desktop 真正 spawn 时体现，建议直接重启 App。
- 跨协议版本的不兼容改动：换心后旧 Desktop 可能不认识新帧。`thunderctl status` 会打印协议版本（当前 v3）。

---

## 五、数据层：三态共用正式环境

`dev` / `prod` / `bundled` **只隔离二进制，不隔离数据**。三者都读写同一个数据根：

```
~/.thunder/
  conversations/      每个会话一个目录（真实使用数据，不要手动删）
  state/              thunder-daemon 的运行时状态标记（daemon-<pid>.json）
  scratchpad/         工具执行的大输出落盘
  logs/               TUI / thunder-memory 日志
  models.json  auth.json  permissions.json
  config.toml         全局配置（可被工作区的 .thunder/config.toml 覆盖）
```

这带来的**唯一一条硬规则**：

> 改动**落盘格式**（`conversations/` 的序列化字段、`permissions.json` 结构、任何持久化 schema）时，先用独立数据根验证一遍，确认没问题再切回正式环境。

```bash
THUNDER_CONFIG_DIR=$PWD/target/dev-home ./run.sh
THUNDER_CONFIG_DIR=$PWD/target/dev-home cargo run -p thunder-daemon --bin thunder-daemon
```

`THUNDER_CONFIG_DIR` 会同时搬走 `conversations/`、`state/`、`scratchpad/`、`logs/`，以及 `models.json` / `auth.json` 的查找根（注意：新目录里没有凭证，需要先拷一份或用环境变量提供 key）。`thunder-daemon` 也支持等价命令 `--config-dir <path>`。

除此之外的场景（改工具、改 UI、调 prompt、修 bug）**直接用正式环境即可**，不用起沙箱。

---

## 六、profile 怎么选

| profile | 产物目录 | 说明 |
|---|---|---|
| `release` | `target/release` | 最高优化、`lto = "fat"`。发布用；构建最慢 |
| `tui-dev` | `target/tui-dev` | 继承 release 优化但关 LTO、开 incremental、保留调试符号。**日常开发推荐** |
| `dev` | `target/debug` | 无优化。除非要单步调试，否则不建议 |

为什么 `tui-dev` 是默认：TUI 的渲染路径开销集中在依赖上（pulldown-cmark、syntect、ratatui 行布局），未优化时约为优化后的 7 倍。工作区已为 `dev` profile 单独把依赖设为 `opt-level = 2`，但 `tui-dev` 仍然更快、更平滑。

`target/tui-dev` 不在 Desktop 的自动探测列表里（它只认 `release` / `debug`），所以只有 `thunderctl dev` 写下的显式 `daemonPath` 才会指到它。

---

## 七、与 `thunder-memory` 的对齐与刻意差异

两个项目的开发模式共享同一套心智模型（**双轨：隔离开发 + 边改边用**），但落地手段因为形态不同而不同。

### 对齐的部分

| 能力 | thunder-memory | thunder |
|---|---|---|
| 双轨心智模型 | 仓库构建 vs `~/.cargo/bin` 安装版 | 同左（`dev` / `prod`） |
| 正式安装位 | `~/.cargo/bin/thunder-memory` | `~/.cargo/bin/thunder-{daemon,tui,std}` + `thunder` |
| 数据隔离开关 | 单元测试走内存 SQLite；可重定向数据库 | 单元测试全临时目录；`THUNDER_CONFIG_DIR` 重定向整个数据根 |
| 一键切换 / 状态诊断 | 手工 `stop` / `start` / `status` | `thunderctl dev/prod/bundled` + `status`（多一个解析链诊断） |
| 质量门禁 | 本地 `cargo test` | `scripts/ci.sh` + GitHub Actions（fmt / clippy -D warnings / test） |

### 刻意不一样的部分

| 差异 | 为什么 |
|---|---|
| **不用 symlink**：`dev` / `prod` 都写 Desktop 配置，而不是把 `~/.cargo/bin/x` 软链到 `target/x` | ① Desktop 有自己完整的解析链，软链无法表达「App 自带版」这一态；② daemon 热重启靠 `current_exe`，软链会让它指向解析后的真实路径，行为不确定；③ 4 个二进制的软链维护成本高于一次 JSON 写入 |
| **没有「改完不用管」的自动生效**：必须 `thunderctl reload` 或重启 App | daemon 是 Electron 的常驻子进程，二进制替换后旧进程仍在内存里跑。`reload` 用 `execv` 解决，但它是一次显式动作，不是自动的 |
| **数据默认不隔离**（共享 `~/.thunder`） | 按需求直接跑正式环境；只在改落盘 schema 时用 `THUNDER_CONFIG_DIR` |
| **多一层「回退风险」提示** | 一旦 `daemonPath` 指走，App 升级自带的 sidecar 新版本就会被永久忽略。`status` 会对比三方 mtime 并告警，`bundled` 提供显式出口 |
| **不覆盖 App bundle** | Agent Resume 是 adhoc 签名 + `Sealed Resources`，替换 `Contents/Resources/thunder/bin/*` 会破坏签名校验。`daemonPath` 是唯一正确的杠杆 |

---

## 八、文件与环境变量速查

### `thunderctl` 读取 / 写入的位置

| 路径 | 作用 |
|---|---|
| `~/.agent-resume-panel/settings.desktop.json` | 唯一被改写的文件（仅 `thunder` 对象）；写入前备份为 `settings.desktop.json.thunderctl.bak` |
| `~/.agent-resume-panel/settings.json` | 只读，用于识别被 `panelHome` 重定位后的配置目录 |
| `<data_root>/state/daemon-<pid>.json` | daemon 启动时写：pid / exe / version / protocol_version / hot_reload / started_at_ms |
| `~/.cargo/bin/thunder*` | `install` 的发布目标 |

### 环境变量

| 变量 | 作用 |
|---|---|
| `THUNDER_PATH` | 指定仓库根目录（Desktop 与 `thunderctl` 都读） |
| `THUNDER_DAEMON_BIN` / `THUNDER_TUI_BIN` | 直接指定二进制，优先级最高（临时试用时很方便） |
| `THUNDER_CONFIG_DIR` | 数据根目录（默认 `~/.thunder`） |
| `THUNDER_INSTALL_DIR` | 安装位（默认 `~/.cargo/bin`，测试用） |
| `THUNDER_APP_BUNDLE` | 覆盖 App 路径探测（默认扫 `/Applications/Agent Resume.app`） |
| `CARGO_TARGET_DIR` | 构建产物目录（`thunderctl` 会跟随） |
| `RUST_LOG` | daemon 日志级别，Desktop 默认注入 `info,thunder_daemon=debug,...` |

---

## 九、故障排查

| 症状 | 原因 | 处理 |
|---|---|---|
| 改了代码，Desktop 里行为没变 | 解析链命中的是 bundled；或者 daemon 还是旧进程 | `thunderctl status` 看 `✓` 那一行；再 `thunderctl reload` |
| `thunderctl status` 显示「回退风险」 | `daemonPath` 指向安装版，但 App 自带的 sidecar 更新 | 想跟随 App：`thunderctl bundled`；想用自己版本：`thunderctl install && thunderctl prod` |
| `thunderctl reload` 说「没有找到常驻 daemon」 | Desktop 只在需要时 spawn sidecar | 在 Desktop 里随便发一条 Thunder 会话，或重启 App |
| `thunderctl reload` 说「版本过旧，不支持热重启」 | 运行中的 daemon 是旧构建，没有状态文件 | 完全退出并重开 Agent Resume |
| `reload` 后 3 秒无变化 | 有任务在跑，daemon 拒绝了换心 | 等任务结束再 `reload`，或直接重启 App |
| `thunderctl prod` 报「还没有安装版二进制」 | 没跑过 `install` | `thunderctl install` |
| `thunderctl dev` 报「构建产物缺失」 | 用了 `--no-build` 但没产物 | 去掉 `--no-build` |
| TUI 卡顿 / 帧率低 | 跑的是 `debug` 构建 | 用 `tui-dev` 或 `release`：`./run.sh` 或 `THUNDER_TUI_PROFILE=tui-dev ./run.sh` |
| `thunder-daemon` 起来就报 `--config-dir requires a path` | 参数缺值 | 补上路径，或改用 `THUNDER_CONFIG_DIR` |

---

## 十、质量门禁

```bash
bash scripts/ci.sh      # 与 CI 完全一致：fmt --check → clippy -D warnings → test --workspace
```

单项手跑：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI 见 [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)，在 `ubuntu-latest` 上跑同一套命令（工作区只有 `#[cfg(unix)]` 分支，没有 macOS 专属依赖）。

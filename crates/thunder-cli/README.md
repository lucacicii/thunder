# 🎛️ thunder-cli

`thunderctl` 是 Thunder 的运行时管理入口（bin: `thunderctl`），负责在三种运行态之间切换，并诊断「为什么我改的代码没生效」。

它不参与 Agent 推理，只做四件事：**构建**、**发布**、**写 Desktop 配置**、**给常驻 daemon 发热重启信号**。

---

## 命令

```bash
thunderctl status                     # 只读诊断：解析链 / 运行中进程 / 产物 / 漂移提示
thunderctl install [--profile P]      # cargo build --profile P + 原子发布到 ~/.cargo/bin
thunderctl dev     [--profile P]      # 构建 + Desktop 指向本仓库 target/<P>
thunderctl prod                       # Desktop 指向 ~/.cargo/bin
thunderctl bundled                    # 清空 thunder.* 配置，交还 App 自带版本
thunderctl reload                     # 常驻 daemon 原地 exec 新二进制（pid 不变）
```

通用选项：`--profile release|tui-dev|dev`、`--repo <path>`、`--no-build`、`--dry-run`、`-h`。

默认 profile：`install` → `release`，`dev` → `tui-dev`。

---

## 三态

| 模式 | Desktop spawn 的 daemon | 语义 |
|---|---|---|
| `dev` | `<repo>/target/tui-dev/thunder-daemon` | 跑正在改的代码 |
| `prod` | `~/.cargo/bin/thunder-daemon` | 跑自己固化安装的版本 |
| `bundled` | `<App>.app/Contents/Resources/thunder/bin/thunder-daemon` | 跟随 App 升级，零漂移 |

三态都只改 `~/.agent-resume-panel/settings.desktop.json` 的 `thunder` 对象（写入前备份 `.thunderctl.bak`，原子 rename）。

---

## `status` 输出的解析链

复刻 Agent Resume `daemonResolver` 的优先级，逐条打 `✓` / `·` / `✗`：

```
1. THUNDER_DAEMON_BIN
2. settings.thunder.daemonPath
3. THUNDER_PATH
4. settings.thunder.repoPath
5. bundled（App 内 Resources/thunder/bin）   ← 排在所有 $HOME 猜测之前
6. 当前 checkout / ~/wz/GitHub/thunder 等
```

`✓` 就是 Desktop 下次 spawn 会用的二进制。末尾的「提示」段落会对比**运行中 / 磁盘安装位 / App bundle** 三方 mtime，报出漂移与回退风险。

---

## `reload` 的安全边界

`SIGUSR1` 的默认动作是**终止进程**，所以 `thunderctl` 只在 daemon 的运行时状态文件（`<data_root>/state/daemon-<pid>.json`）声明了 `"hot_reload": true` 时才发信号。

daemon 侧收到信号后：有任务在跑 → 拒绝；二进制缺失/无执行位 → 拒绝；否则 `execv(自己, 原 argv)`，pid 与 stdio 管道都不变，Desktop 无感知。`thunderctl` 用 `started_at_ms` 变化确认换心成功。

---

## 相关文档

- 完整开发者指南：[`docs/development.md`](../../docs/development.md)
- daemon 侧协议与信号：[`crates/thunder-daemon/README.md`](../thunder-daemon/README.md)

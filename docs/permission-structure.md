# thunder 权限结构图

> 源码事实图。所有结论都对应到具体文件与函数。
> 唯一判定入口：`SessionPolicy::decide`（`thunder-agent-loop/src/types/policy.rs:484`）

---

## 0. 一句话总览

```
                  ┌──────────────────────────────────────────────┐
                  │  SessionPolicy  ← 权限的唯一"大脑"            │
                  │  { tier, role_tier, mode, rules }            │
                  │  decide() → Allow | Deny | Ask               │
                  └──────────────────────────────────────────────┘
                                    ▲
             判定的三个输入来自：档位 / 模式 / 已记住的规则
```

---

## 1. 全景图：五层防线

```mermaid
flowchart TB
    subgraph SRC["① 配置来源（声明式，非代码）"]
        R3["宿主 / run_task 请求<br/>permission 档位"]
        R4["面板 SetPermissionMode<br/>/ GetPermissionState"]
    end

    R3 --> POL
    R4 --> POL

    subgraph POL["② SessionPolicy（唯一判定点）"]
        P1["tier ＝ 能力天花板<br/>Read ⊂ Write ⊂ Bash"]
        P2["mode ＝ 什么时候问人<br/>Plan/Ask/AcceptEdits/Manual/Yolo"]
        P3["rules ＝ 本会话已同意<br/>Vec&lt;AllowRule&gt;"]
        JUDGE["decide(call, caller) → Verdict"]
    end

    P1 --> JUDGE
    P2 --> JUDGE
    P3 --> JUDGE

    JUDGE --> L1

    subgraph PIPE["③ 洋葱 Pipeline：outer → inner"]
        L1["PermissionGuardMiddleware<br/>唯一判定 + 执行审批弹窗"]
        L2["SecurityGuardMiddleware<br/>多根路径 jail + 禁用命令"]
        L3["ResourceGuardMiddleware<br/>10MB 读取上限 / 取消"]
        L4["TransactionMiddleware<br/>临时文件影子写入 / 回滚"]
        L5["OutputPostProcessor<br/>输出裁剪"]
        L6["ToolRegistry（终态）"]
        L1 --> L2 --> L3 --> L4 --> L5 --> L6
    end

    subgraph HOST["④ 宿主注册门（更外层，更早）"]
        H1["host.rs:509<br/>按 tier 决定注册哪些内置工具<br/>被禁工具 → 模型 Prompt 中物理不可见"]
    end

    HOST -.->|更早生效| L1

    subgraph PLG["⑤ 插件侧：合成为工具调用，走同一条 pipeline"]
        TS["TS 插件 ctx.exec / ctx.fs.*"]
        INV["types/invoke.rs:184<br/>caller = Some(plugin_id)"]
        TS --> INV --> L1
    end

    L6 --> RES["ToolExecutionResult<br/>拒绝时带 SystemNotice 回灌模型"]

    classDef src fill:#e8f0fe,stroke:#4285f4
    classDef pol fill:#fef7e0,stroke:#f9ab00,stroke-width:2px
    classDef pipe fill:#e6f4ea,stroke:#34a853
    classDef gate fill:#fce8e6,stroke:#ea4335,stroke-width:2px
    class R1,R2,R3,R4 src
    class P1,P2,P3,JUDGE pol
    class L1,L2,L3,L4,L5,L6 pipe
    class H1,TS,INV gate
```

---

## 2. 正交的两根轴：tier（档位）× mode（审批模式）

这是整套权限设计的核心。**tier 决定"什么根本不可能"（能力天花板），mode 决定"什么时候向人类确认"（审批策略）**。两轴完全独立，绝不反向耦合。

```mermaid
flowchart LR
    subgraph T["tier ＝ 能力天花板（硬上限，物理不可越权）"]
        direction LR
        T1["read<br/>fs_write=off, bash=off"]
        T2["write<br/>fs_write=on, bash=off"]
        T3["bash<br/>fs_write=on, bash=on"]
        T1 <-->|⊂| T2 <-->|⊂| T3
    end

    subgraph M["mode ＝ 审批策略（纯粹决定何时弹窗）"]
        direction LR
        M1["never / yolo<br/>0 弹窗直接放行"]
        M2["shell_only<br/>仅 Shell 弹窗"]
        M3["mutations / ask<br/>改写与 Shell 弹窗"]
        M4["always / manual<br/>每步全量弹窗"]
    end

    T3 --- P["SessionPolicy"]
    M1 --- P
```

### 2.1 模式 × 效果 决策矩阵

`ToolEffect::of(tool)` 按工具名分类（`policy.rs`）：

| ToolEffect | 匹配工具 | required_tier | prompt_worthy |
|---|---|---|---|
| `Read` | `read_file` `grep` `find` `ls` `list_dir` `read` `glob` | `Read` | ❌ |
| `Write` | `write_file` `edit` `write` `apply_patch` `notebook_edit` | `Write` | ✅ |
| `Exec` | `bash` `shell` `powershell` 以及名称含 `exec/terminal/cmd` 的工具 | `Bash` | ✅ |
| `Other` | **其他一切（普通插件 / MCP 工具）** | `Write` | ✅ |

`decide()` 的输出矩阵（纯净正交，无任何死状态）：

| tier ＼ mode | never (yolo) | shell_only | mutations (ask) | always (manual) |
|---|---|---|---|---|
| **read** | 读 Allow<br>写/执行 Deny | 读 Allow<br>写/执行 Deny | 读 Allow<br>写/执行 Deny | 读 Ask<br>写/执行 Deny |
| **write** | 读写 Allow<br>执行 Deny | 读写 Allow<br>执行 Deny | 读 Allow / 写 Ask<br>执行 Deny | 读写 Ask<br>执行 Deny |
| **bash** | 读写执行 Allow | 读写 Allow<br>执行 Ask | 读 Allow<br>写执行 Ask | 全 Ask |

> 提示：在只读角色（`Permission::Read`）下，由于写和执行均已在第一步物理硬拒绝，前端自动锁定为 `never`（免审放行），彻底消除了“只读却问人”的冗余状态。

---

## 3. decide() 的判定顺序 —— 这就是安全属性本身

```mermaid
flowchart TB
    IN["ToolCall + Caller<br/>(Model | Plugin(id))"] --> S0

    S0["<b>Step 0</b> 取消检查<br/>cancelled → 直接拒绝，不弹窗"]
    S0 --> S1

    S1["<b>Step 1 · 档位</b>　tier_allows(tier, effect)?<br/>❌ → Verdict::Deny（硬拒，无 UI）"]
    S1 -->|通过| S2

    S2["<b>Step 2 · 已记住规则</b>　rules.any(matches)<br/>✅ → Verdict::Allow"]
    S2 -->|无匹配| S3

    S3{"<b>Step 3 · 模式</b>　needs_ask?"}
    S3 -->|"Manual → 总是要问"| ASK
    S3 -->|"Ask / Plan → effect.prompt_worthy()"| ASK
    S3 -->|"AcceptEdits → 仅 Exec"| ASK
    S3 -->|"Yolo → false"| ALLOW

    ASK["Verdict::Ask(ApprovalRequest)"]
    ALLOW["Verdict::Allow"]

    S1 -.->|"顺序承重"| NOTE["<b>为什么档位必须最先查：</b><br/>『总是允许』是一次同意的记录，<br/>而同意扛不过天花板被收紧。<br/>若先查规则，read-only run 里<br/>旧的 bash(git status) 规则会继续生效。"]

    style S1 fill:#fce8e6,stroke:#ea4335,stroke-width:2px
    style NOTE fill:#fff4e5,stroke:#f9ab00
```

---

## 4. 审批弹窗：fail-closed 的交互闭环

```mermaid
flowchart TB
    A["Verdict::Ask"] --> L{"prompt_lock<br/>串行化"}
    L -->|"与 cancellation<br/>竞速"| CANC["cancelled → Refuse<br/>（取消的 run 不会<br/>占住整批调用）"]
    L -->|拿到锁| R["AllowRule::for_call(tool, args)<br/>推导最窄的『总是允许』规则"]

    R --> FILTER{"rule.is_some()?"}
    FILTER -->|否| O1["选项裁掉『总是允许』<br/>（插件工具：参数无作用域）"]
    FILTER -->|是| O2["保留四个选项"]
    O1 --> TITLE
    O2 --> TITLE["标题标注调用方<br/>插件请求 → 『插件 X 请求：…』<br/>（绝不让插件伪装成助手发起）"]
    TITLE --> DIALOG["HostUi.request(Select)<br/>①允许一次 ②总是允许 ③拒绝 ④拒绝并说明原因"]

    DIALOG --> ANS{"答案"}
    ANS -->|"① 允许一次"| A1["执行本次"]
    ANS -->|"② 总是允许"| A2["policy.remember(rule)<br/>（下次同规则直接 Allow）"]
    ANS -->|"③ 拒绝"| A3["Refuse: the user declined"]
    ANS -->|"④ 拒绝+原因"| A4["追问 Input → 原因原文回灌模型"]
    ANS -->|"无答案（超时/关闭/无面板）"| F1["Refuse: no answer<br/>== 拒绝，fail-closed"]
    ANS -->|"答案不在选项集内"| F2["Refuse: unrecognised answer<br/>意外值绝不扩大权限"]

    A3 --> REJ
    A4 --> REJ
    F1 --> REJ
    F2 --> REJ
    REJ["SystemNotice 拒绝结果<br/>『The call never ran.<br/>The workspace is unchanged.』<br/>+ 引导：不要原样重试、<br/>别找别的工具绕过、去问用户"]
    REJ --> MODEL["回灌模型（ToolExecutionResult）<br/>不是抛异常 → 模型停止重试"]

    style F1,F2 fill:#fce8e6,stroke:#ea4335
    style REJ fill:#fce8e6,stroke:#ea4335
    style A2 fill:#e6f4ea,stroke:#34a853
```

### 4.1 『总是允许』规则的窄化

`AllowRule::for_call`（`policy.rs:255`）—— 规则绝不授予"一个工具类"：

| 工具类型 | 规则形式 | 匹配逻辑 |
|---|---|---|
| `Exec`（bash） | 命令**到第一个链接符为止**的前缀 | `strip_prefix(prefix)` 后余下部分**不得含** `; & \| \` > < \n $` |
| 路径类（write_file / read_file） | `path` 参数（文件或其下整个目录） | `path == prefix \|\| path.starts_with(prefix + "/")` |
| 其他 | `None` → **不提供**该选项 | — |

> `git status` 永远不会授权 `git status && rm -rf /`。
> `SHELL_CHAINING = [';','&','|','`','>','<','\n','$']`

**审批绑定**：`call_hash(tool, args)` 把批准绑定到具体 `(tool, 参数)`，参数一变即失效，无法重放到别的调用上。

---

## 5. tier 之外的空间权限：多根路径 jail

tier 管"能不能写"，jail 管"能写哪"。后者由 `SecurityGuardMiddleware` 独立执行（`middleware/security.rs`）。

```mermaid
flowchart LR
    subgraph ROOTS["allowed_roots（读写同权）"]
        R0["[0] 主工作区<br/>相对路径基准"]
        R1["[n] extra_roots<br/>任务引用的共享仓库"]
    end
    R0 --> J["check_path(target)"]
    R1 --> J
    J --> N1["① 相对路径 → 挂到 workspace_root"]
    N1 --> N2["② resolve_for_check<br/>解符号链接（macOS /var→/private/var）<br/>不存在的尾部按最深存在祖先解析"]
    N2 --> N3{"③ normalize 后<br/>starts_with 任一 root?"}
    N3 -->|否| E1["❌ Path traversal detected!<br/>列出全部 allowed_roots<br/>→ 模型学到合法目标"]
    N3 -->|是| E2["✅ 通过"]

    SH["check_command(cmd)"] --> F1["禁用命令表<br/>rm -rf / · rm -rf /*<br/>:(){ :|:&amp; };: · mkfs · dd if="]
    SH --> F2["extract_write_targets<br/>重定向 &gt; &gt;&gt; 2&gt;<br/>rm/tee/mkdir/touch/truncate…<br/>cp/mv/ln/install（末操作数）<br/>chmod/chown（首操作数后）<br/>sed -i"]
    F2 --> F3["check_static_target<br/>仅绝对路径 + 可静态解析<br/>含 $ ` ~ → 跳过（无法解析）<br/>/dev/null 等设备白名单放行"]
    F3 --> E1

    style E1 fill:#fce8e6,stroke:#ea4335
    style E2 fill:#e6f4ea,stroke:#34a853
```

---

## 6. 插件权限：与模型完全同一条路径

```mermaid
flowchart TB
    P["TS 插件<br/>ctx.exec('ls') / ctx.fs.write(...)"] --> SYN["合成为工具调用<br/>bash / write_file / read_file"]
    SYN --> CTX["ToolExecutionContext {<br/>  caller: Some(plugin_id),  ← types/invoke.rs:184<br/>  route: session_id<br/>}"]
    CTX --> PIPE["execute_with_context<br/>（不是 execute_one —— 后者会<br/>重建 ctx 并丢掉 caller）"]
    PIPE --> G1["PermissionGuardMiddleware"]
    G1 --> D{"tier 检查<br/>Other → 需要 Write"}
    D -->|不通过| E1["拒绝，并在错误里点名插件"]
    D -->|通过| D2["rules 检查（与模型同一套）"]
    D2 --> D3["mode 决定是否弹窗"]
    D3 --> DLG["弹窗标题：『插件 X 请求：执行 bash』<br/>用户批准的是插件的请求，<br/>不是助手请求的"]

    subgraph SEC["协议层防伪（daemon README）"]
        S1["ui_request.request_id 由 daemon 签发<br/>（进程号+计数器+随机后缀）<br/>→ 插件无法自选，无法伪造/重放审批"]
        S2["source: host ｜ plugin<br/>host 必须用插件无法伪造的专用样式<br/>plugin-sourced 答复不得视为授权依据"]
        S3["插件的 UI 请求一律 fail-closed<br/>select/input/editor → null，confirm → false"]
    end

    style E1 fill:#fce8e6,stroke:#ea4335
    style DLG fill:#fef7e0,stroke:#f9ab00
```

> 代价：插件的写入现在也走事务层（产生 `.arp/tmp` 影子文件、保留原文件权限）——比之前手写版本更严格，与模型的 `write_file` 行为一致。

---

## 7. 类结构总览

```mermaid
classDiagram
    class SessionPolicy {
        +Mutex~SessionInner~ inner
        +new(tier, mode) Arc
        +decide(call, caller) Verdict  «唯一判定»
        +set_mode(mode)  «下次调用即生效»
        +set_tier(tier)  «被 mode 裁剪»
        +remember(rule)
        +clear_rules()
        +rules() Vec
        +tier() Permission
        +mode() PermissionMode
    }

    class SessionInner {
        -tier        «有效天花板：mode.effective(role_tier)»
        -role_tier   «宿主设定的原始档位»
        -mode
        -rules
    }

    class Permission {
        <<enumeration>>
        Read
        Write
        Bash
        +allows_read() true
        +allows_write() Write|Bash
        +allows_exec() Bash
        +allows_builtin(name)  «未知名 → true，由其它层管»
        +min()  «Read ⊂ Write ⊂ Bash»
    }

    class PermissionMode {
        <<enumeration>>
        Plan        «ceiling = Read，唯一压低档位»
        Ask
        AcceptEdits
        Manual
        Yolo        «default，不提问»
        +ceiling() Option~Permission~
        +effective(tier) Permission
        +next() / parse() / ALL
    }

    class ToolEffect {
        <<enumeration>>
        Read
        Write
        Exec
        Other  «= Write，fail-safe»
        +of(tool_name)
        +required_tier()
        +is_prompt_worthy()
    }

    class Verdict {
        <<enumeration>>
        Allow
        Deny { reason }
        Ask(ApprovalRequest)
    }

    class AllowRule {
        +tool
        +arg_prefix Option~String~
        +for_call(tool, args)
        +matches(tool, args) bool
    }

    class ApprovalRequest {
        +tool
        +title
        +detail
        +options
        +call_hash
        +caller Caller
    }

    class Caller {
        <<enumeration>>
        Model
        Plugin(String)
    }

    class PermissionGuardMiddleware {
        +NAME
        -policy Arc~SessionPolicy~
        -ui Arc~dyn HostUi~
        -prompt_lock Mutex  «弹窗串行化»
        -timeout Duration
        -workspace_roots
        -tier_hint TierHint
        +new(policy, ui)
        +new_tier_only(tier, roots)
        +with_workspace_roots()
        +with_timeout()
    }

    class SecurityGuardMiddleware {
        -workspace_root
        -allowed_roots
        -forbidden_commands
        +check_path(target)
        +check_command(cmd)
        +with_extra_roots()
    }

    SessionPolicy *-- SessionInner
    SessionPolicy ..> Verdict : decide 返回
    SessionPolicy ..> AllowRule : 记住
    Verdict ..> ApprovalRequest : Ask 携带
    ApprovalRequest *-- Caller
    PermissionMode ..> Permission : ceiling/effective
    ToolEffect ..> Permission : required_tier
    PermissionGuardMiddleware --> SessionPolicy : 询问
    PermissionGuardMiddleware --> ApprovalRequest : 弹窗
    PermissionGuardMiddleware --> SecurityGuardMiddleware : onion 内层
```

---

## 8. 关键设计不变量（都有测试钉住）

| # | 不变量 | 实现位置 | 测试 |
|---|---|---|---|
| 1 | `decide()` 是**唯一**判定入口，其它层只照做 | `policy.rs` 模块文档 | `permission_layer_test.rs` |
| 2 | 判定顺序：tier → rules → mode | `policy.rs:484` | `permission_mode_test.rs`（"nothing above ever produces a tier the run did not grant"） |
| 3 | mode 永不抬升 tier；`{"permission":"read","mode":"yolo"}` 仍只读 | `PermissionMode::ceiling()` | `tool_tier_test.rs`、`permission_mode_test.rs` |
| 4 | 档位被拒的调用**走不到弹窗**（用户无法批准绕过档位） | Pipeline 中 Guard 最外层 | `permission_layer_test.rs` |
| 5 | 被拒的调用**不创建临时文件**（拒绝在事务层之外） | Guard 在 Transaction 外层 | `permission_layer_test.rs` |
| 6 | 审批 fail-closed：无 UI / 超时 / 关闭 / 取消 → 拒绝 | `PermissionGuardMiddleware::prompt` | `policy.rs` 单测 |
| 7 | `yolo` 不问，但 tier 仍生效 | `needs_ask` 分支 | `policy.rs` 单测："yolo must not lift the tier for a plugin" |
| 8 | 未知工具按 `Write` 对待（需 `Write` 而非 `Bash`） | `ToolEffect::of` / `required_tier` | `policy.rs` 单测 |
| 9 | 模式切换后恢复原档位（保留 `role_tier`） | `SessionPolicy::set_mode` | `permission_mode_test.rs` |
| 10 | 弹窗串行，避免并发批叠框批准错对象 | `prompt_lock` | 契约点 5 |
| 11 | 拒绝回灌为工具结果 + `SystemNotice`，非异常 | `PermissionGuardMiddleware::refuse` | — |
| 12 | extra_roots 与主工作区**读写同权** | `with_extra_roots` | `multi_root_jail_test.rs` |

---

## 9. 三个值得注意的边界

### 9.1 默认 `yolo` 是刻意的产品决策
`policy.rs:73-95` 写得很直白：审批门 fail-closed，无面板宿主上每个弹窗都判为"取消＝拒绝"。若默认 `ask`，升级后所有无面板用户的**每一次写文件、每一条 shell、每一个插件工具**都会被静默拒绝——"一个没人要求的安全特性，在第一天就把产品搞坏，结果只会被整个关掉"。所以审批是 **opt-in**。

### 9.2 TUI 目前不接审批面板
`thunder-agent-core/tui/src/app.rs:2413-2416` 硬编码 `mode: Some(PermissionMode::Yolo)`、`ui: None`，注释说明：有面板的话 gate 只能拒绝（安全但无用），所以交互式路径交给 daemon。**后果**：TUI 里的 `/permission` 只能压档位，无法开启审批。

### 9.3 角色层已移除
`roles.jsonl` / `RoleRegistry` / `RoleSpec`、面板角色编辑器与 `list_roles` / `set_role` RPC 均已删除：daemon 固定以 `Permission::Bash` + `ApprovalMode::Never` 运行，由模型按意图（Ask / Plan / Write-Edit）自行约束。`SessionPolicy` 的档位 / 模式 / 规则机制仍保留，供宿主（如 TUI `/permission`）显式使用。

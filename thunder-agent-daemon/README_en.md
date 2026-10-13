# Thunder Daemon (Sidecar for Electron & Desktop UI)

> **Lightweight STDIO Sidecar Daemon for Electron, Desktop Applications & External Hosts**

[English](README_en.md) | [简体中文](README.md)

`thunder-daemon` is a long-running sidecar process specifically designed for desktop clients. Communicating over standard input/output (STDIO) via high-performance NDJSON framing, it provides complete agent execution, tool dispatch, dynamic plugin assembly, and multi-turn session persistence.

---

## 🚀 Architecture & Key Features

- **Zero Port Conflicts & Natural Lifecycle Binding**: Pure STDIO pipe communication avoids TCP/HTTP ports, eliminating port collisions or firewall prompts. When the parent Electron process terminates or crashes, STDIN automatically reaches EOF and `thunder-daemon` exits cleanly within milliseconds, leaving no zombie processes.
- **Concurrency Semaphore & Backpressured Scheduling**: Employs an internal async task semaphore (tunable via the `THUNDER_MAX_CONCURRENT_TASKS` environment variable, default `8`) to bound parallel reasoning tasks, preventing local compute exhaustion or provider rate-limit spikes.
- **Asynchronous STDOUT Actor (Zero Line Interleaving)**: All outgoing events are serialized via a dedicated tokio STDOUT actor over a bounded MPSC channel. Under concurrent multi-task streaming, **every NDJSON line is guaranteed atomic and never interleaved**.
- **Authoritative History Reload & Tiered Trace Retention**: Reloads the latest authoritative conversation from disk before task launch to keep multi-client state consistent. In-memory traces use tiered retention — high-frequency micro-deltas (`TokenDelta` / `ReasoningDelta` / `ToolCallChunk`) are streamed live over stdout but **not** kept in the trace: they are **counted** in `dropped_deltas` (dropping is reported, never silent), preventing unbounded growth in long-lived daemons. A trace file also carries `tool_traces` (cap 2000) and `tool_decisions` (what the caps pushed out, keeping the decision itself).
- **Cooperative Pausing & User Question Bubbles**:
  - `pause_task` pauses execution safely at the next tool dispatch boundary without disrupting in-flight disk writes; `resume_task` resumes execution.
  - `ask_user_question` allows agents to pose blocking questions rendered as interactive question bubbles in the desktop UI, resumed via `answer_question`.
- **Log Isolation**: All diagnostic and runtime traces are directed to `STDERR`, guaranteeing that `STDOUT` remains 100% valid NDJSON protocol frames.

---

## 🛠️ Launch & Testing

```bash
# 1. Launch daemon
./daemon.sh

# 2. Send ping command
echo '{"method":"ping","id":"1"}' | ./daemon.sh

# 3. List available models
echo '{"method":"list_models","id":"2"}' | ./daemon.sh
```

---

## 📡 Protocol Specification (NDJSON over STDIN/STDOUT)

### 1. Inbound Host Requests (STDIN)

One valid JSON line per command:

```json
{"method":"ping","id":"req-1"}
{"method":"list_models","id":"req-2"}
{"method":"run_task","id":"req-4","task_id":"task-1","prompt":"Check repo git status","session_id":"sess-001"}
{"method":"cancel_task","id":"req-5","task_id":"task-1"}
{"method":"pause_task","id":"req-6","task_id":"task-1"}
{"method":"resume_task","id":"req-7","task_id":"task-1"}
{"method":"answer_question","id":"req-8","question_id":"task-1:q1","answers":{"Which component?":"renderer"}}
```

### 2. Outbound Daemon Responses (STDOUT)

- **Response Confirmation**: `{"type":"response","id":"req-1","success":true,"data":{...}}`
- **Streaming Event**: `{"type":"observed_event","task_id":"task-1","event":{...}}`
- **User Question (Awaiting Input)**: `{"type":"user_question","task_id":"task-1","question_id":"task-1:q1","questions":[{"question":"...","options":[{"label":"..."}]}]}`
- **Task Paused**: `{"type":"task_paused","task_id":"task-1","reason":"..."}`
- **Task Completed**: `{"type":"task_completed","task_id":"task-1","session_id":"...","final_content":"...","finish_reason":"Done"}`
- **Task Failed**: `{"type":"task_failed","task_id":"task-1","error":"..."}`

---

## 🔒 Approval Modes

As of 0.4 the daemon has **no tiers**. Tools are always registered in full, and
whether a call runs is decided by the **mode** — which lives in an unloadable
host plugin (`ModePlugin`); the kernel only offers a `ToolGate` hook.

| `mode` | reads | writes in scope | writes out of scope | shell | Notes |
| :---: | :---: | :---: | :---: | :---: | :--- |
| **`plan`** | ✅ | ❌ | ❌ | ask | Read-only research plus one plan file; `write_plan` / `exit_plan_mode` are the only way out |
| **`ask`** | ✅ | ask | ask | ask | Reads run unprompted; everything else asks |
| **`acceptEdits`** | ✅ | ✅ | ask | ask | In-workspace edits run unprompted; reaching out still asks |
| **`yolo`** (default) | ✅ | ✅ | ✅ | ✅ | Fully autonomous, matching 0.3's `never` |

Old names still parse: `ask` ← `default`/`manual`/`mutations`/`writes`/`always`/`all`;
`acceptEdits` ← `accept_edits`/`shell_only`/`ask_shell`;
`yolo` ← `never`/`none`/`bypass`/`bypass_permissions`. **`plan` no longer maps to
`never`** — that 0.3 mapping was wrong.

Three rules that hold in every mode:

1. **The guard cannot be argued with.** Dangerous commands (`mkfs*`, `dd if=`,
   `rm -rf /`, fork bombs) and critical paths (the credential store, `.git`,
   system directories) are refused in *every* mode, `yolo` included. Changing the
   mode does not change that.
2. **Path classification only ever relaxes.** A scope can turn an "ask" into an
   "allow"; it can never turn an "allow" into a "refuse" — so a new rule can cost
   you an extra prompt, never a silent refusal.
3. **Asking fails closed.** No panel, a timeout, a dismissal, or an answer outside
   the offered set is a refusal. It comes back as a *tool result* carrying
   "nothing happened", not as an exception, so the model does not read it as
   success and retry.

### Setting the mode

```json
{"method":"set_mode","id":"req-14","session_id":"sess-001","mode":"plan"}
```

- The response echoes `{"session_id":"...","mode":"plan"}`; an unknown mode returns
  `success:false` with `unknown mode '…': expected one of plan | ask | acceptEdits | yolo`
  — it never falls back to the default silently.
- The mode is remembered per `session_id` (oldest evicted first) and applies to the
  **very next** call. `run_task` also accepts an optional `mode`; it writes the
  session's mode too, so later runs of that session keep it rather than reverting.
- `get_permission_state` reports the current mode and the remembered "always allow" rules.

### Where it takes effect

There is one decision point, and it sits inside the one tool-execution funnel
(`ToolExecutor::run_tool`): cancellation → scope → guard → gate → dialog →
execute → trace. **With no gate installed, everything but the guard runs** — the
red line embedding hosts rely on.

Plugin-initiated calls take the same path: a Rust plugin's
`ctx.tool_slot().invoke(...)` builds a context with `caller = plugin id` and calls
the same `run_tool`, so it is guarded, judged, prompted (with the plugin named in
the title) and traced like any other call.

---

## 🔄 State Machine & Cross-Behavior Rules

```text
               ┌──────────┐
               │ Running  │
               └────┬─────┘
          pause_task│   ▲ resume_task
     (tool boundary)│   │
                    ▼   │
               ┌──────────┐
               │  Paused  │
               └──────────┘
                    ▲
                    │ Answered and pause was requested
                    │
         ┌─────────────────────┐
         │ WaitingUserInput    │ ◄── Agent calls ask_user_question
         │ (user_question wait)│ ──► answer_question resumes Running
         └─────────────────────┘
```

1. **Tool Boundary Pausing**: `pause_task` takes effect before the next tool is dispatched. If a write is currently in progress, it finishes completely before the agent suspends.
2. **Pausing During User Question**: The question continues to await user input; when answered, the tool completes and the task immediately freezes in `Paused`.
3. **Cancellation Priority**: `cancel_task` overrides any state immediately, waking waiters and aborting the task.

---

## 📄 License

This project is licensed under the [Apache License 2.0](../LICENSE).

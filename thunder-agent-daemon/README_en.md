# Thunder Daemon (Sidecar for Electron & Desktop UI)

> **Lightweight STDIO Sidecar Daemon for Electron, Desktop Applications & External Hosts**

[English](README_en.md) | [简体中文](README.md)

`thunder-daemon` is a long-running sidecar process specifically designed for desktop clients. Communicating over standard input/output (STDIO) via high-performance NDJSON framing, it provides complete agent execution, tool dispatch, dynamic plugin assembly, and multi-turn session persistence.

---

## 🚀 Architecture & Key Features

- **Zero Port Conflicts & Natural Lifecycle Binding**: Pure STDIO pipe communication avoids TCP/HTTP ports, eliminating port collisions or firewall prompts. When the parent Electron process terminates or crashes, STDIN automatically reaches EOF and `thunder-daemon` exits cleanly within milliseconds, leaving no zombie processes.
- **Concurrency Semaphore & Backpressured Scheduling**: Employs an internal async task semaphore (tunable via the `THUNDER_MAX_CONCURRENT_TASKS` environment variable, default `8`) to bound parallel reasoning tasks, preventing local compute exhaustion or provider rate-limit spikes.
- **Asynchronous STDOUT Actor (Zero Line Interleaving)**: All outgoing events are serialized via a dedicated tokio STDOUT actor over a bounded MPSC channel. Under concurrent multi-task streaming, **every NDJSON line is guaranteed atomic and never interleaved**.
- **Authoritative History Reload & Tiered Trace Retention**: Reloads the latest authoritative conversation from disk before task launch to keep multi-client state consistent. In-memory traces use tiered retention — high-frequency micro-deltas (`TokenDelta` / `ReasoningDelta` / `ToolCallChunk`) are streamed live over stdout but excluded from trace memory, preventing unbounded growth in long-lived daemons.
- **Role & Permission Hard Isolation**: Loads roles from `~/.thunder/roles.jsonl` and `<workspace>/.arp/roles.jsonl`. A role's permission tier (`Read` ⊂ `Write` ⊂ `Bash`) determines tool registration. Blocked tools are **physically invisible** to the LLM.
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
{"method":"list_roles","id":"req-3","workspace_dir":"/path/to/repo"}
{"method":"run_task","id":"req-4","task_id":"task-1","prompt":"Check repo git status","session_id":"sess-001","role":"plan"}
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

## 🔒 Permission Tiers

| Tier | `read_file` | `write_file` | `bash` | Description |
| :---: | :---: | :---: | :---: | :--- |
| **`read`** | ✅ | ❌ | ❌ | Read-only inspection; file writes and shell execution tools are omitted |
| **`write`** | ✅ | ✅ | ❌ | Editing mode; file reading and writing permitted, shell commands forbidden |
| **`bash`** (default) | ✅ | ✅ | ✅ | Full authorization; all builtin and system tools available |

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

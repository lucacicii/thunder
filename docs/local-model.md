# 🧠 本地模型 (Local Model) 使用与集成指南

本文档说明 `thunder-memory` **内置的进程内小模型**：怎么装、怎么单独提问、怎么把它变成一个 OpenAI 兼容的 HTTP endpoint、怎么接入别的 Agent，以及踩过的坑。

> 相关文档：[architecture.md](architecture.md)（三层认知架构）、[development.md](development.md)（开发/正式版本隔离与发布）。

---

## 一、两套推理面：进程内 vs HTTP

内置模型是 **Qwen2.5-0.5B-Instruct (Q4_K_M, GGUF)**，由 **Candle** 在 **Apple Silicon Metal GPU** 上做**进程内**推理——不是 Ollama，也不是外部服务。它有两个使用面：

| 使用面 | 入口 | 形态 | 用途 |
|---|---|---|---|
| 进程内引擎 | `LocalModelEngine` | 库调用（`src/local_model.rs`） | 守护进程的认知提炼（synthesis） |
| 一次性 CLI | `thunder-memory ask` | 进程启动→加载→生成→退出 | 手工验证、脚本里偶尔用一次 |
| HTTP 服务 | `thunder-memory serve` | 常驻进程，OpenAI 兼容 API | 给别的 Agent / 客户端当本地小模型 |

**调用优先级**（`SynthesisEngine::extract_insight`，由高到低）：

```
1. 进程内本地模型 (LocalModelEngine)      ← 默认走这条
2. 配置的 OpenAI 兼容端点 (llm_endpoint)  ← 兜底
3. 纯规则启发式 (heuristic_extract)       ← 最终兜底
```

> 注意：MCP 工具（`memory_query` / `memory_current_screen` 等）走的是**数据库检索**，**不会**调用这个模型；MCP 是 stdio JSON-RPC，不是 HTTP。

### 代码位置

| 文件 | 内容 |
|---|---|
| `src/local_model.rs` | 模型加载（GGUF + tokenizer）、`generate` / `generate_with`、采样、ChatML 模板、JSON 容错解析、`download_default_model` |
| `src/server.rs` | OpenAI 兼容 HTTP 服务（手搓 HTTP/1.1 on `tokio::TcpListener`） |
| `src/synthesis.rs` | 三层调度、Episode 聚类、prompt 组装 |
| `src/config.rs` | `model_dir` / `local_model_file` / `local_tokenizer_file` / `enable_local_model` / `llm_endpoint` |

---

## 二、安装模型

```bash
thunder-memory download-model
```

从 `hf-mirror.com` 下载两个文件到默认目录 `~/.thunder/models/`：

| 文件 | 大小 | 来源 |
|---|---|---|
| `qwen2.5-0.5b-instruct-q4_k_m.gguf` | ≈ 469 MB | `Qwen/Qwen2.5-0.5B-Instruct-GGUF` |
| `tokenizer.json` | ≈ 6.7 MB | `Qwen/Qwen2.5-0.5B-Instruct` |

已有文件会跳过（可断点重跑）。

**自检**：

```bash
thunder-memory status
#   🧠 In-Process Local AI: Ready [Candle Metal GPU] (~/.thunder/models/qwen2.5-0.5b-instruct-q4_k_m.gguf)
#   🔬 Edge Small Model:   In-Process qwen2.5-0.5b-instruct-q4_k_m.gguf [Candle Metal] (local)
```

- `In-Process Local AI` 只反映**磁盘上模型文件是否齐全**。
- 真正在用的档位看 `Edge Small Model`：模型就绪时它显示 `In-Process ...`，否则回落到 `llm_endpoint` 或 `Heuristic Rule Engine`。

**配置覆盖**（`~/.thunder/config.toml` 或环境变量）：

```toml
model_dir = "/Users/you/.thunder/models"
local_model_file = "qwen2.5-0.5b-instruct-q4_k_m.gguf"
local_tokenizer_file = "tokenizer.json"
enable_local_model = true
```

> 换模型只需把任意 **Qwen2 架构**的 GGUF + 对应 tokenizer.json 放进 `model_dir` 并改 `local_model_file`。非 Qwen2 架构（Llama、Gemma 等）当前不支持——`quantized_qwen2` 是硬编码的。

---

## 三、一次性提问：`ask`

```bash
thunder-memory ask "用一句话解释 SQLite 的 WAL 模式"
thunder-memory ask "把这段报错归类：ERROR: database is locked" --greedy --max-tokens 32
thunder-memory ask "..." --temperature 0.3 --max-tokens 128
```

| 参数 | 默认 | 说明 |
|---|---|---|
| `--max-tokens N` | 256 | 生成上限 |
| `--temperature T` | 0.7 | 采样温度 |
| `--greedy` | — | 等价 `--temperature 0`，确定性输出（适合测试/抽取） |

每次调用会 **完整加载一次模型（≈3.3s）+ 生成 + 退出**，实测 64 token 全程约 8.7s（其中大部分是加载与首次预热）。适合验证和脚本；高频调用请用 `serve`。

---

## 四、HTTP endpoint：`serve`

```bash
thunder-memory serve                            # 127.0.0.1:8787
thunder-memory serve --port 9000                # 换端口
thunder-memory serve --host 0.0.0.0             # 局域网/容器可访问（注意无鉴权！）
thunder-memory serve --max-tokens-cap 2048      # 放宽单次生成上限（默认 1024）
```

启动后模型常驻内存，**RSS ≈ 1.1 GB**。

### 路由表

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/health` | 存活探测 + 模型/后端信息 |
| `GET` | `/v1/models` | OpenAI 模型列表（`id` = GGUF 文件名去后缀） |
| `POST` | `/v1/chat/completions` | OpenAI Chat Completions，支持 `stream` |
| `GET` | `/` | 自描述信息 |
| `OPTIONS` | 任意 | CORS 预检，`204` |

别名：`/models`、`/chat/completions`。
所有响应带 `Access-Control-Allow-Origin: *`，浏览器页面可直接调用。

### 示例

```bash
curl -s localhost:8787/health
# {"backend":"candle-metal","model":"qwen2.5-0.5b-instruct-q4_k_m","status":"ok"}

curl -s localhost:8787/v1/models
# {"data":[{"created":1792737484,"id":"qwen2.5-0.5b-instruct-q4_k_m",
#           "object":"model","owned_by":"thunder-memory"}],"object":"list"}

curl -s localhost:8787/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"local","messages":[{"role":"user","content":"用一句话说明什么是 KV cache"}],
       "max_tokens":48,"temperature":0.3}'
# {"choices":[{"finish_reason":"stop","index":0,
#   "message":{"content":"KV（键值对）缓存是一种在内存中存储和快速检索数据的机制。","role":"assistant"}}],
#  "created":1792737489,"id":"chatcmpl-...","model":"qwen2.5-0.5b-instruct-q4_k_m",
#  "object":"chat.completion",
#  "usage":{"completion_tokens":20,"prompt_tokens":14,"total_tokens":34}}

# 流式（SSE）
curl -sN localhost:8787/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"messages":[{"role":"user","content":"说点什么"}],"max_tokens":16,"stream":true}'
# data: {"object":"chat.completion.chunk","choices":[{"delta":{"content":"...","role":"assistant"},...}]}
# data: {"object":"chat.completion.chunk","choices":[{"delta":{},"finish_reason":"stop"}],"usage":{...}}
# data: [DONE]

# stop 序列（命中即截断）
curl -s localhost:8787/v1/chat/completions \
  -d '{"messages":[{"role":"user","content":"数 1 到 10"}],"max_tokens":64,"temperature":0,"stop":["5"]}'
```

### 请求字段

| 字段 | 默认 | 说明 |
|---|---|---|
| `messages` | 必填 | `[{role, content}]`，`content` 支持 **string 或 content parts 数组** |
| `max_tokens` / `max_completion_tokens` | 256 | `clamp(1, --max-tokens-cap)` |
| `temperature` | 0.7 | `clamp(0, 2)`；`0` = 贪心 |
| `top_p` | 0.95 | 核采样阈值 |
| `stop` | 空 | string 或 string[]，命中即截断 |
| `stream` | false | `true` 返回 SSE |
| `model` | 忽略 | 只有一个模型，字段仅为兼容 |

### 响应与错误

- 非流式：标准 `chat.completion`，含 `usage`（token 数由 tokenizer 实算）。
- 流式：因为引擎是**同步整段生成**，所以只发**一个 delta 块** + `finish_reason: stop` + `[DONE]`。OpenAI SDK / Agent CLI 都能吃。
- 错误：`{"error":{"message":"...","type":"invalid_request_error"}}`，状态码 `400`（body/字段不合法）、`404`（路由未知）、`413`（body > 8 MB）、`431`（header > 64 KB）、`500`（推理失败）。

### 并发

模型被单个 `Mutex` 串行化，推理跑在 `spawn_blocking`（不堵 reactor）。并发请求**不报错但排队**——不要指望它扛并发。

---

## 五、采样与解码行为

| 机制 | 值 | 为什么 |
|---|---|---|
| Repetition penalty | `1.15`，窗口 64 token | 0.5B 量化模型贪心解码会无限重复同一句 OCR 短语 |
| `temperature = 0` | 纯 argmax | 确定性，抽取/测试用 |
| `temperature > 0` | softmax + top-p 核采样 | xorshift64* PRNG（不引 `rand`），`seed=0` 用时钟 |
| ChatML | `<|im_start|>role\n content <|im_end|>\n...<|im_start|>assistant\n` | Qwen2.5 instruct 必须带 ChatML，直接喂裸文本质量会明显变差 |
| EOS | `<|im_end|>` (151645) / `<|endoftext|>` (151643) | — |

**合成专用 profile**（`GenOptions::synthesis()`，`extract_insight` 用）额外有三条约束：

1. **贪心 + JSON 早停**：生成到第一个闭合 `}` 就停，否则模型会一路刷满 `max_tokens`。
2. **prompt 预算**：OCR 文本 ≤ 1200 字符、单行 ≤ 120 字符、最多 40 行。
3. **JSON 容错解析**：容忍 `_title` / `__summary__` 这类前缀键、字符串化分数（`"5.3"`）、单引号数组（`'key_entities': ['a']`）、空字段回落。

---

## 六、接入别的 Agent / 工具

```bash
# ① 直接 curl / 任何 OpenAI 兼容客户端
OPENAI_BASE_URL=http://127.0.0.1:8787/v1 OPENAI_API_KEY=dummy <your-client>

# ② 让 thunder-memory 自己的兜底档指向它（自指，方便切换本地/远程模型对比）
export THUNDER_MEMORY_LLM_ENDPOINT=http://127.0.0.1:8787/v1
export THUNDER_MEMORY_LLM_MODEL=qwen2.5-0.5b-instruct-q4_k_m

# ③ 指到真正的 Ollama（当本地模型文件缺失时的兜底）
export THUNDER_MEMORY_LLM_ENDPOINT=http://localhost:11434/v1
export THUNDER_MEMORY_LLM_MODEL=qwen2.5-coder
```

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8787/v1", api_key="dummy")
r = client.chat.completions.create(
    model="qwen2.5-0.5b-instruct-q4_k_m",
    messages=[{"role": "user", "content": "把这句话缩成 5 个字：SQLite 的预写日志模式"}],
    max_tokens=32,
    temperature=0.2,
)
print(r.choices[0].message.content)
```

### 资源与性能（本机实测，M1/Metal）

| 指标 | 数值 |
|---|---|
| 模型加载（`ask` 每次都要付） | ≈ 3.3 s |
| 稳态生成速度 | **≈ 200 token/s**（200 token / 0.93 s，0.5B Q4_K_M，`serve` 单并发） |
| 首个请求延迟 | 明显更慢（Metal kernel / 着色器预热，可达数秒），之后稳定 |
| `serve` 常驻 RSS | ≈ 1.1 GB |
| 与 daemon 的关系 | 两个进程**各自持有一份模型**；`ask`/`serve` 的启停不影响 daemon 的感知与提炼 |

> 因此：偶发单次用 `ask`（付 3.3 s 加载），高频或长驻用 `serve`（只付一次加载 + 一次预热）。

> 结论：0.5B 适合**分类、抽取、摘要、格式化**；不要用它替代云端大模型做推理。

---

## 七、故障排查

### 1. `Metal error no metal implementation for rms-norm`

**症状**：`status` 显示 `Ready [Candle Metal GPU]`，模型也能加载，但每次提炼都 `WARN In-process local model inference failed: ... no metal implementation for rms-norm`，永远走启发式兜底。

**根因**：`candle-nn` 的 `RmsNorm` / `LayerNorm` 是 CustomOp，其 `metal_fwd` 实现被 `#[cfg(feature = "metal")]` 包住。只在 `candle-core` 上开 `metal` 而 **没有**在 `candle-transformers`（转发到 `candle-nn`）上开，就会走 CustomOp 默认实现，直接返回 `no metal implementation for {op}`。

**正确写法**（`Cargo.toml`，macOS target）：

```toml
[target.'cfg(target_os = "macos")'.dependencies]
candle-core         = { version = "0.11", features = ["metal"] }
candle-transformers = { version = "0.11", features = ["metal"] }   # ← 关键：转发到 candle-nn/metal
```

**排查手法**：错误字符串出自 `candle-core/src/custom_op.rs` 的默认 `metal_fwd`；去 `candle-nn/src/ops.rs` 看该 op 是否有 `#[cfg(feature = "metal")]` 分支即可确认。CUDA 同理（`cuda` feature 也要开在 `candle-transformers` 上）。

### 2. 输出无限重复 / 满屏刷同一句

0.5B 贪心解码的典型退化。检查是否用的是 `GenOptions::synthesis()`（带 penalty + JSON 早停）；自写调用时至少设置 `repetition_penalty = 1.15`。

### 3. `serve` 报 `local model not found`

先 `thunder-memory download-model`，或检查 `model_dir` / `local_model_file` 配置。

### 4. Metal 不可用

`LocalModelEngine::load` 会尝试 `Device::new_metal(0)`，失败则 `debug` 日志提示并回退 `Device::Cpu`——此时仍能工作，只是明显变慢。

### 5. `status` 说 `Not ready` 但文件明明在

`is_local_model_ready()` 校验的是 `model_dir/local_model_file` **且** `local_tokenizer_file` 都存在。缺 tokenizer.json 也算未就绪。

### 6. 装二进制后屏幕录制权限反复弹框

**不要**直接用 `cargo install`（ad-hoc 签名，cdhash 随编译变化 → TCC 授权条目失效）：

```bash
./scripts/install-release.sh     # 用固定本地证书重签，DR 与二进制内容无关
```

详见 [development.md](development.md) 第五节。

### 7. 两个 daemon 同时跑 / `stop` 杀不掉

历史 bug：`run`（LaunchAgent 的入口）从不写 PID 文件，重启后 `status` 报旧 PID、`stop` 杀错进程。现在 `run` 启动时会**自己持有** `~/.thunder/thunder-memory.pid`，退出时清理，并在发现另一个存活 daemon 时打 `WARN`。仍残留时手动处理：

```bash
pgrep -fl "thunder-memory run"
kill <多余的那个 PID>
```

---

## 八、开发与验证

```bash
cargo test                       # 30 个单测（含 7 个 HTTP 层测试）
cargo run -- ask "hello" --greedy --max-tokens 8   # 不安装也能验证模型
cargo run -- serve --port 8788                     # 不占用常驻端口试新代码
```

HTTP 层的可测试部分是纯函数：`parse_request_head`、`ChatRequest::from_json`、`message_text`、`apply_stop_sequences`——改协议时先补这几个的用例。

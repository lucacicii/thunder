# 🧠 Thunder Memory 架构与算法设计白皮书

本文档系统性阐述 `thunder-memory` 模块的设计架构、核心实现机制，以及对业界主流开源 Memory 算法的深度调研与技术选型分析。

---

## 一、系统定位与设计目标

`thunder-memory` 是 Thunder 生态中的**自主认知与长期记忆侧车（Cognitive Sidecar & Persistent Memory Engine）**。它具备以下核心特性：

1. **完全解耦与独立后台常驻**：可作为独立系统守护进程（Daemon）在 macOS 后台静默运行，无需随 Thunder Agent 启动或退出，开机即跑，随时可查。
2. **低开销感知流捕获（Perception Stream）**：周期性截取桌面屏幕，借助 macOS 原生 Apple Vision 深度学习框架进行离线 OCR，并提取当前前台活跃应用（Frontmost App）与窗口标题（Window Title）。
3. **感知差分去重（Perceptual Diffing）**：基于内容哈希与相似度过滤静态画面，防止短时间内对未变动屏幕重复 OCR 和数据库写入，极大降低能耗与磁盘占用。
4. **两级分层存储（Tiered Memory SQLite）**：
   - **短期感知流表 (`screen_captures`)**：记录原始感知流（时间戳、应用、窗口、完整 OCR 文本、置信度、处理状态）。
   - **长期情境与语义记忆表 (`long_term_memories`)**：记录经过小模型加工后的结构化事实、任务进度、重要性评分与实体标签。
5. **周期性小模型提炼（Small Model Synthesis）**：定时从短期流中批量聚类未处理记录，调用本地小模型（如 Ollama / 本地 OpenAI 兼容端点 / Thunder 内部轻量模型）提取结构化关键信息与反思（Reflection）。
6. **用户意图蒸馏与训练集沉淀（Composer Sends Ingestion）**：无缝打通桌面端历史交互数据源（`~/.agent-resume-panel/.desktop/desktop.db` 中的 `workbench_composer_sends` 表），提取用户意图、实体与质量评分，导出规范的 SFT/DPO JSONL 训练集。

---

## 二、市面主流开源 Memory 算法与系统调研分析

针对大模型与 Agent 系统的长期记忆，当前业界主要存在以下几种代表性开源架构与算法范式：

| 项目 / 算法范式 | 核心思想 | 优势 | 劣势 | 对屏幕感知场景适用性 |
|---|---|---|---|---|
| **Mem0** (前身 Embedchain) | 基于 LLM 的事实提取与冲突决断 (`ADD` / `UPDATE` / `DELETE` / `NOOP`) | 事实粒度细，避免上下文冗余，状态更新精准 | 实时性单条调用 LLM 开销过大，需要语义批处理 | ⭐⭐⭐⭐（适合长期语义层） |
| **Screenpipe** | 24/7 本地录屏/录音 + 原生 OCR (Vision) + SQLite FTS5 | 低 CPU/GPU 占用，纯本地离线，工程实现极强 | 缺乏深度语义反思与自动认知压缩，检索偏词法 | ⭐⭐⭐⭐⭐（最佳感知捕获层） |
| **Letta / MemGPT** | 仿操作系统分层（Working/Core Memory + Recall + Archival），Agent 主动工具自编辑 | Agent 拥有显式自我意识和记忆管理工具 | 严重依赖大模型 Tool Calling，System Prompt 占用大 | ⭐⭐⭐（偏通用推理交互） |
| **Generative Agents** (Stanford Park et al.) | 记忆流 (Memory Stream) + 周期反思 (Reflection) + 综合检索评分 | 拟人化认知衰减与反思机制，检索排序机制成熟 | 原版偏学术模拟，无生产级差分去重与数据库优化 | ⭐⭐⭐⭐⭐（最佳反思与检索模型） |
| **Zep / Graphiti** | 时序知识图谱 (Temporal Knowledge Graph) | 能精确刻画事实的时序有效窗口 `[t_start, t_end]` | 图谱构建和推理计算复杂度较高，依赖重 | ⭐⭐⭐（图谱在桌面端稍重） |
| **LangMem / LangGraph Memory** | 语义记忆 (Semantic) / 情境记忆 (Episodic) / 程序记忆 (Procedural) 三元分离 | 理论结构清晰，便于模块化插拔 | 偏重 Agent 框架绑定，缺乏独立系统感知 | ⭐⭐⭐⭐（概念架构值得借鉴） |

### 1. Mem0 的核心算法细节
Mem0 的核心算法是**动态认知冲突解决（Cognitive Conflict Resolution）**：
- 当产生新的文本或交互时，LLM 先从中抽取出 candidate facts（如 `["User is developing thunder-memory in Rust", "User prefers SQLite for local storage"]`）。
- 检索向量库中与该 candidate 最相关的现有记忆 top-k。
- 构造 LLM 裁决 Prompt，针对每个候选事实输出操作指令：
  - `ADD`：完全崭新的事实，直接插入；
  - `UPDATE`：已有事实但属性变更（例如用户升级了版本或完成了某个模块）；
  - `DELETE`：与现有事实直接冲突，或者已被用户显式废弃；
  - `NOOP`：现有记忆库已包含该内容，无需任何操作。

### 2. Screenpipe 的工程落地精髓
Screenpipe 采用 Rust 编写，其核心成功经验在于：
- **原生 API 接入**：在 macOS 上直接调用 Apple Vision Framework (`VNRecognizeTextRequest`)，在 Windows 上调用 Windows Media OCR，完全绕过 Tesseract 等古老且低效的方案，利用系统 GPU/NPU 硬件加速，毫秒级完成多语言识别。
- **SQLite + FTS5**：利用 SQLite 嵌入式零运维的优势，搭配 FTS5 全文索引，支持千亿级字符的高速模糊词检索。

### 3. Stanford Generative Agents 的认知反思与检索公式
斯坦福 Generative Agents 提出了极具影响力的三维检索评分公式：
$$\text{Score}(m) = w_{\text{recency}} \cdot \text{Recency}(m) + w_{\text{importance}} \cdot \text{Importance}(m) + w_{\text{relevance}} \cdot \text{Relevance}(m, q)$$
其中：
- **Recency（时间衰减）**：指数衰减函数 $e^{-\lambda \cdot (t_{\text{now}} - t_{m})}$，越近发生的事权重越高；
- **Importance（重要性评分）**：由认知模型给记忆打分（1.0 - 10.0）。琐碎的页面滚动给 1-2 分，关键代码编写或错误解决给 7-9 分；
- **Relevance（查询相关度）**：当前查询与记忆文本的余弦相似度或词法匹配度。

---

## 三、Thunder Memory 的混合认知架构设计

结合上述调研，`thunder-memory` 创新性地提出了 **"Screenpipe 感知流 + Stanford 记忆反思 + Mem0 状态更新"** 的三层混合架构：

```
                ┌───────────────────────────────────┐
                │ macOS 桌面环境 (Display / Retina)   │
                └─────────────────┬─────────────────┘
                                  │ 周期性采样 (3s - 10s)
                                  ▼
                ┌───────────────────────────────────┐
                │ Perceptual Diffing (感知差分去重)   │
                │  - 计算哈希/变动检测，静态画面跳过  │
                └─────────────────┬─────────────────┘
                                  │ 有变动画面
                                  ▼
                ┌───────────────────────────────────┐
                │ macOS Native Vision OCR (Swift)   │
                │  - Apple Neural Engine / Metal    │
                │  - 捕获 frontmostApp & windowTitle │
                └─────────────────┬─────────────────┘
                                  │
                                  ▼
       ┌─────────────────────────────────────────────────────┐
       │ Tier 1: 短期感知流表 (SQLite: screen_captures)       │
       │  - captured_at_ms, app_name, window_title, ocr_text │
       │  - confidence, content_hash, processed=0            │
       └──────────────────────────┬──────────────────────────┘
                                  │
                                  │ 周期调度 / 达到批量阈值
                                  ▼
       ┌─────────────────────────────────────────────────────┐
       │ Tier 2: 小模型认知提炼引擎 (Synthesis Engine)       │
       │  0. 外部 LLM 端点 (Ollama / OpenAI 兼容，可换可共享)│
       │  1. 时间窗会话聚类 (Episode Clustering)             │
       │  2. 小模型提炼任务意图、关键技术实体、重要度打分    │
       │  3. 记忆状态决断 (ADD 开启新任务 vs UPDATE 续写进度)│
       └──────────────────────────┬──────────────────────────┘
                                  │
                                  ▼
       ┌─────────────────────────────────────────────────────┐
       │ Tier 3: 长期情境与语义记忆 (long_term_memories)      │
       │  - title, summary, key_entities, importance_score   │
       │  - time_window, source_captures, status             │
       └──────────────────────────┬──────────────────────────┘
                                  │
                                  │ 桌面静止 15min 且距上次 ≥6h（可配）
                                  ▼
       ┌─────────────────────────────────────────────────────┐
       │ Tier 3.5: 空闲深度整理 (Deep Consolidation)         │
       │  - 大模型（可与即时档同源或异地，如云端 30B）       │
       │  - 按 category 分组，合并讲同一件事的记忆碎片      │
       │  - 新记忆继承时间窗与 capture id；源记录 superseded│
       │  - 失败不丢数据：源保持 active，下个空闲窗口重试    │
       └─────────────────────────────────────────────────────┘
                                  ▲
                                  │ 关联对齐
       ┌──────────────────────────┴──────────────────────────┐
       │ Tier 4: 用户指令学习池 (composer_training_samples)   │
       │  - 同步 ~/.agent-resume-panel/.../composer_sends    │
       │  - 提取用户意图、规范化 Prompt、质量打分            │
       │  - 导出 Alpaca / ShareGPT / OpenAI 格式训练集       │
       └─────────────────────────────────────────────────────┘
```

---

## 四、核心算法与数据结构

### 1. 短期感知捕获与 OCR
- 原生 Swift 编译二进制 `bin/thunder-vision-ocr`，利用 `VNRecognizeTextRequest` 并启用 `zh-Hans`, `zh-Hant`, `en-US` 联合识别与语言修正。
- 通过 `NSWorkspace.shared.frontmostApplication` 与 `CGWindowListCopyWindowInfo` 零权限提示获取当前焦点应用名与窗口标题。
- 文本指纹计算：计算规范化 OCR 文本的哈希值，当两帧连续指纹一致时直接复用，不重复落库。

### 2. 小模型认知合成算法（Synthesis Algorithm）

> 推理后端：**配置的 OpenAI 兼容端点 (`llm_endpoint`，推荐 Ollama)** → 纯规则启发式。thunder-memory 自身不加载任何模型：早期内置的进程内 Qwen2.5-0.5B（Candle Metal）及其 `serve` / `ask` 入口已整体移除，模型进程由外部服务持有，可随时换模型、与其他 Agent 共享同一实例。
1. **聚类提取**：从 `screen_captures` 查询 `processed = 0` 的连续样本（按应用和时间间隙聚类，同应用且间隔 < 5 分钟划归为一个 Episode）。
2. **提示词工程与认知抽取**：
   - 输入：时间段、焦点应用、窗口标题、主要文本段落。
   - 输出结构化 JSON：
     - `category`: 活动分类（`code_development`, `debugging`, `research`, `documentation`, `communication` 等）；
     - `title`: 活动标题（简明扼要）；
     - `summary`: 2-3 句话阐述用户在这个时间段内的工作内容、遇到的问题与推进成果；
     - `key_entities`: 识别出的文件路径、核心函数、依赖库、错误代码或术语；
     - `importance_score`: 1.0 - 10.0 的认知重要性分值。
3. **Mem0 风格状态合并**：
   - 检查 `long_term_memories` 中最新一条同一应用/同类别的记忆；
   - 若时间间隔在阈值内且主题连续，触发 `UPDATE`，追加进度并延长时间窗口；
   - 若发生工作场景切换或重要阶段性产出，触发 `ADD` 产生新的长期记忆节点。
4. **事务回写**：将对应的原始捕获标记为 `processed = 1`。

### 3. Composer 数据提炼与训练集构建
- 读取 `workbench_composer_sends` 表。
- 过滤无效单字符或测试指令，按启发式规则与代码密度评估 `quality_score` (0.0 ~ 1.0)。
- 结构化归一化为指令微调数据对（Instruction Tuning Pair）：
  - `system`: 系统提示词（专家级研发 Agent 上下文）；
  - `user`: 经过清洗的用户真实输入；
  - `metadata`: 工作区路径、所属 Session 等上下文特征。
- 支持直接导出为通用 JSONL 格式，供 Llama-Factory, Unsloth, Axolotl 等训练框架直接加载微调。

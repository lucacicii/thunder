# 🧠 thunder-memory

Autonomous Screen OCR Ingestion, Episodic & Long-Term Memory Engine, and Composer Training Data Pipeline.

`thunder-memory` 是一个独立常驻运行的系统级后台认知底座（Cognitive Substrate Daemon），24/7 默默在 macOS 后台收集全桌面屏幕感知流、本地离线 OCR 识别、提取长期结构化情境记忆，并自动采集、清洗和导出用户真实交互指令作为模型微调（SFT / DPO）训练集。

它通过标准 **Model Context Protocol (MCP)** 以及本地 SQLite 为各类 AI Agent（如 Thunder、Agent Resume Panel、Cursor、Claude Desktop）提供外部海马体级别的长期记忆能力。

---

## 🌟 核心特性

1. **完全解耦的独立后台常驻守护进程**：
   - 支持 `run`（前台模式）、`start`（后台守护进程）、`stop`（优雅停止）、`status`（状态查看）。
   - 内置 macOS LaunchAgent 一键安装/卸载，支持开机自动启动、异常崩溃自动重启。
2. **零隐私泄露、极速的原生感知流**：
   - 直接调用 macOS 官方 Apple Vision 深度学习框架 (`VNRecognizeTextRequest`)，利用 Apple Neural Engine / Metal 硬件加速毫秒级多语言识别（简体中文、繁体中文、英文）。
   - 自动获取前台焦点应用名（`appName`，如 Cursor、Terminal、Chrome、WeChat）及活动窗口标题（`windowTitle`）。
   - **感知差分去重（Perceptual Diffing）**：基于内容哈希过滤静止画面，连续无变动的屏幕自动跳过，不产生多余 OCR 与磁盘占用。
3. **三级认知金字塔与激进冷热隔离（Aggressive Cold Archiving）**：
   - **L1 短期感知流表 (`screen_captures`)**：48 小时极短 TTL，一旦被 L2 提炼或超期后物理清理，避免无意义原始 OCR 文本膨胀。
   - **L2 长期情境与语义记忆表 (`long_term_memories`)**：记录经过小模型提炼的任务进展、重要度评分（1.0 ~ 10.0）、关键实体与进展总结。未被频繁调用的历史记录在 30 天后自动归档至 `~/.thunder/memory_archive.db`。
   - **L3 确定性规则与偏好表 (`agent_interactions`)**：按 `global` 与 `project` 严格双域隔离，记录 Agent 写入的红线、架构决议与反向约束，通过确定性 SQL 状态机（`active` vs `superseded`）无损流转。
   - **动态感知心跳与认知噪音掩码（Noise Masking）**：自适应心跳机制在检测到活跃 UI 变更时自动加速（至 2s），在静止或抑制窗口时减速（至 60s）；内置音视频播放器与通讯噪音过滤掩码。
4. **小模型认知提炼与反思引擎 (Synthesis Engine)**：
   - 启发于 **Stanford Generative Agents** 与 **Mem0** 架构：
   - 自动将会话按时间窗口与应用聚类（Episode Clustering）。
   - 提取活动类别（`code_development`, `debugging`, `research`, `documentation`, `communication` 等）。
   - Mem0 状态更新机制：连续任务触发 `UPDATE` 续写进展；切换场景或关键突破触发 `ADD` 生成新记忆。
   - 支持配置本地 Ollama、Apple MLX 或 OpenAI 兼容端点，离线状态下无缝回退至内置启发式规则抽取器。
5. **用户交互训练数据同步与导出 (Training Data Pipeline)**：
   - 自动检测并对接 `~/.agent-resume-panel/.desktop/desktop.db` 中的 `workbench_composer_sends` 表。
   - 自动根据增量水印（Watermark）同步用户真实提示词。
   - 分析意图类型（代码生成、错误修复、架构咨询、反向约束、执行确认等）。
   - 过滤噪音并计算质量评分（Quality Score）。
   - 一键导出为标准 **OpenAI**、**Alpaca** 或 **ShareGPT** 格式的 JSONL 文件，直接用于 SFT / LoRA / DPO 微调。
6. **四维加权认知检索与效用反馈飞轮（Feedback-Driven Retrieval）**：
   - 基于斯坦福 Generative Agents 认知检索公式与强化学习效用打分：
     $$\text{Score} = w_{\text{recency}} \cdot \text{Recency} + w_{\text{importance}} \cdot \text{Importance} + w_{\text{relevance}} \cdot \text{Relevance} + w_{\text{utility}} \cdot \text{Utility}$$
   - 每次 Agent 调用记忆自动记录访问频次与时间戳；支持 `memory_feedback` 接口对高价值记忆予以正反馈强化（强化排序权重）、对失效记忆予以负反馈降权。
7. **标准 MCP (Model Context Protocol) 服务**：
   - 提供 `thunder-memory mcp` 命令，原生提供 STDIO JSON-RPC 2.0 MCP 接口。
   - 暴露 8 项标准工具：
     - `memory_record`：跨 Agent 确定性持久化规则、偏好与约束（支持 global/project 隔离）。
     - `memory_get_preferences`：精准拉取当前项目 workspace 与全局生效的活跃约束红线。
     - `memory_query`：基于四维认知检索公式查询长期记忆与认知记录。
     - `memory_recent`：罗列近期高价值记忆与 Agent 指南。
     - `memory_current_screen`：获取当前活动屏幕感知上下文与 OCR 文本。
     - `memory_stats`：获取屏幕捕获、长期记忆、交互规则与噪音抑制命中率。
     - `memory_feedback`：对检索出的记忆进行效用强化（+delta）或惩罚（-delta）。
     - `memory_compact`：手动或定时触发 48h L1 物理修剪与 30d L2 冷归档。

---

## 🛠️ CLI 命令一览

```bash
# 1. 守护进程管理
thunder-memory run                    # 前台运行（带实时日志）
thunder-memory start                  # 后台守护进程启动（PID写至 ~/.thunder/thunder-memory.pid）
thunder-memory stop                   # 停止后台守护进程
thunder-memory status                 # 查看运行状态与数据库各项指标统计

# 2. 屏幕感知与 OCR 诊断
thunder-memory ocr-once               # 立即截屏并执行 Apple Vision OCR，终端输出识别结果
thunder-memory ocr-once --save        # 立即截屏并入库 SQLite

# 3. 记忆提炼与认知查询
thunder-memory synthesize             # 手动触发一次小模型认知提炼
thunder-memory query "SQLite WAL"     # 按四维认知评分检索长期记忆与 Agent 指南
thunder-memory list-memories 10       # 罗列最近 10 条长期记忆

# 4. 偏好规则与反向约束管理
thunder-memory preferences            # 显示当前全局约束与开发偏好
thunder-memory preferences /path/to   # 显示指定工作区的针对性约束
thunder-memory record constraint "No Lombok" "Never use lombok" --scope project --project /path/to

# 5. 存储压缩与冷归档 (Aggressive Cold Archiving)
thunder-memory compact                # 物理清理 48h 过期感知流，冷归档 30d 未激活记忆至 memory_archive.db
thunder-memory compact --days 1 --archive-days 14

# 6. 认知噪音掩码 (Noise Suppression)
thunder-memory masks                  # 查看活跃认知噪音抑制规则及拦截统计
thunder-memory mask add com.apple.Music "*" "Apple Music window has no text value"

# 7. 用户交互训练集同步与导出
thunder-memory sync-composer          # 同步 ~/.agent-resume-panel/.../workbench_composer_sends
thunder-memory export-training openai # 导出 OpenAI 格式微调数据集到 ~/.thunder/training_dataset.jsonl
thunder-memory export-training alpaca /path/to/dataset.jsonl

# 8. 作为 MCP Server 运行
thunder-memory mcp                    # 运行标准 STDIO MCP 2.0 服务（暴露 8 大认知工具）

# 9. 系统开机自启
thunder-memory install-launchagent    # 安装 macOS LaunchAgent 开启 24/7 常驻
thunder-memory uninstall-launchagent  # 卸载 macOS LaunchAgent
```

---

## 🔌 接入各类 Agent 客户端

### 1. 接入 Thunder
在 `~/.thunder/config.json` 中配置：
```json
{
  "mcp_servers": {
    "memory": {
      "command": "thunder-memory",
      "args": ["mcp"]
    }
  }
}
```

### 2. 接入 Claude Desktop / Cursor / Agent Resume Panel
在相应的 MCP 配置文件（如 `claude_desktop_config.json` 或 `settings.json`）中添加：
```json
{
  "mcpServers": {
    "thunder-memory": {
      "command": "/Users/lucas/.cargo/bin/thunder-memory",
      "args": ["mcp"]
    }
  }
}
```

---

## 📐 算法调研与白皮书

- **核心算法设计白皮书**：[docs/architecture.md](docs/architecture.md)（深度调研对比 Mem0、Screenpipe、Letta、Stanford Generative Agents 等，并阐述三层混合认知架构）。
- **开发者指南与隔离规范**：[docs/development.md](docs/development.md)（本地开发与系统正式常驻版本的隔离、双轨工作流与升级发布说明）。

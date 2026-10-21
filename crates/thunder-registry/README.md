# 📚 thunder-registry

`thunder-registry` 是 Thunder 的动态工具注册表与运行时热加载引擎。

它基于无锁双缓冲并发模型，负责静态扫描单文件脚本元数据，并在运行时动态构建版本化工具快照。

---

## 核心职责

1. **TSP v1 脚本 Frontmatter 静态解析 (`frontmatter.rs`)**：
   * 仅读取脚本文件（Shell, Python, Node.js 等）前 40 行文本。
   * 定位 `# @thunder`（或 `// @thunder`、`-- @thunder`）至 `# @end` 之间的 YAML 块，零执行开销提取 `name`、`description` 和 `parameters` JSON Schema。
   * 自动确保脚本在 Unix 环境下拥有可执行权限（`0o755`）。

2. **无锁双缓冲快照与优先级遮蔽 (`registry.rs`)**：
   * `CatalogSnapshot`：
     * `version: u64`：自增单调版本号。
     * `descriptors: BTreeMap<String, ToolDescriptor>`：以 `BTreeMap` 保证序列化字典序 100% 确定，保护 LLM Prompt Cache。
     * `routing_table`：支持按暴露名（`script__*`）、URI（`script://*`）或逻辑名快速寻址 `ToolProvider`。
   * `ToolRegistry`：
     * 内部持有 `Arc<ArcSwap<CatalogSnapshot>>`，推理轮次开始时执行无锁读取，单轮内工具视图完全冻结。
     * **命名空间覆盖规则 (Shadowing)**：
       $$\text{Local Script (script://)} > \text{External MCP (mcp://)} > \text{Core (thunder-std)}$$
       当高优先级工具与低优先级工具同名时，自动遮蔽低优先级实现并记录告警。

3. **目录监听 Actor (`watcher.rs`)**：
   * 集成 `notify-debouncer-mini`，以 **200ms 防抖窗口** 异步监听 `.thunder/tools/` 目录。
   * 当文件发生新增、修改或删除时，在后台线程重新扫描头部元数据，构建全新不可变 `CatalogSnapshot` 并通过原子替换指针发布更新，发射 `ThunderEvent::RegistryUpdated` 事件。

# 从 0.5 迁移到 0.6

> 0.6 的核心变化只有一句：**内核不再持有策略，宿主不再有"插件"。**
> 本文按"你会看到什么编译错误"组织，每条给出替换写法。

---

## 1. 宿主：`with_plugin` 没了

**0.5**

```rust
root = root.with_plugin(ConversationPlugin::new(store));
root = root.with_plugin(MemoryPlugin::new());
root = root.with_plugin(SkillsPlugin::default());
```

**0.6**：一个组件对运行的全部贡献拆成四件事，按贡献方式注册：

```rust
// 提示词片段
root = root.with_section(Arc::clone(&conversation) as Arc<dyn PromptSection>);
// 按运行准备
root = root.with_prepared(Arc::clone(&conversation) as Arc<dyn Prepared>);
// 事件观察者
root = root.with_observer(conversation as Arc<dyn Observer>);
// 工具
root = root.with_pack(ToolPack::new("skills").with_tools(skills.tools()));
```

一个组件通常注册多次（section + prepared + pack/observer），因为它的四件事在不同的时机被调用。

`StandardHostBuilder` 已经把基线集合装好，**大多数宿主不需要手写上面这些**；只有宿主专属能力才需要在 `build` 之后追加：

```rust
let tool = AskUserQuestionTool::new(...);
root = root.with_pack(ToolPack::new("ask_user").with_tool(Arc::new(tool)));
```

## 2. `RootRunOptions.forced_plugins` 没了

```rust
// 0.5
RootRunOptions { forced_plugins: Some(vec!["skills".into(), "mcp".into()]), .. }
// 0.6
RootRunOptions { .. }   // 注册即激活；唯一的减法是 .thunder/config.json 的 plugins.disabled
```

## 3. `RootRunHandle.selection` / `RootRunResult.selection` 没了

```rust
// 0.5
println!("{:?}", handle.selection.active_plugin_ids);
// 0.6
println!("{:?}", root.component_ids());      // 运行前就能读
println!("{:?}", handle.assembly.ids());      // 本次运行实际装配的
```

daemon 的 `TaskCompleted.active_plugins` 字段**名字没变、语义变了**：现在装的是 `assembly.ids()`（section / pack / observer 的并集），不再是旧的插件 id 集合。按旧 id 断言的客户端需要同步。

## 4. 内核：不再 `register_tool`，改为给 `Tools`

**0.5**：工具注册进内核的 `ToolRegistry`，策略（gate / scope / ui / trace）也从内核的层里配。

**0.6**：内核只认 `Tools` 接缝；宿主持有 registry + executor，并把它们交进去：

```rust
let mut registry = ToolRegistry::new(max_output_bytes, timeout);
for tool in assembly.tools() { registry.register(tool); }
for plugin in assembly.tool_plugins() { registry.register_plugin(plugin); }

let executor = ToolExecutor::new(registry.clone())
    .with_tool_scope(Arc::new(scope) as Arc<dyn ToolScope>)
    .with_tool_gate(Some(mode_plugin.clone() as Arc<dyn ToolGate>))
    .with_host_ui(ui)
    .with_trace_ids(session_id, route);

agent = agent.with_tools_factory(Arc::new(StaticToolsFactory::new(registry, executor)));
```

简单的宿主可以直接 `agent.with_tools(plain_tools(vec![...], max_bytes, timeout))`。

## 5. `ThunderPlugin::tool_gate()` 没了

gate 不再是"插件声明的"，而是宿主在构造 executor 时**显式安装**的：

```rust
ToolExecutor::new(registry).with_tool_gate(Some(gate))
```

没有"声明了但没人读"的路径。

## 6. 生命周期钩子改名

| 0.5 | 0.6 |
|---|---|
| `on_init(&PluginContext)` | `Prepared::prepare(&RunEnv)` |
| `on_run_ready(&PluginContext)` | **删除**（`RunEnv` 已带运行期需要的全部信息） |
| `on_event(&ObservedEvent, &PluginContext)` | `Observer::on_event(&AgentEvent, &RunEnv)`，**异步** |
| `on_finish(&AgentRunResult, &PluginContext)` | `Observer::on_finish(&AgentRunResult, &RunEnv)`，无返回值 |
| `system_prompt_contribution()` | `PromptSection::render()` |

`PluginContext` 仍然存在（`root/context.rs`），但只服务**工具执行期**：session / route / ui / mode / tool-slot。

## 7. 事件流类型变了

`AgentHandle::take_events()` 现在返回 `EventStream` 而不是 `tokio::sync::mpsc::Receiver<ObservedEvent>`：

```rust
let mut events = handle.take_events().unwrap();
while let Some(event) = events.recv().await { /* … */ }
```

区别不只是类型：每 `recv` 一次会归还字节配额。**不要绕过 `EventStream` 直接持有底层通道**，否则配额永不释放，生产端会在 5 秒后开始丢事件。

## 8. 新配置项（都有默认值，不必须改）

| 配置 | 默认 | 作用 |
|---|---|---|
| `AgentConfig::max_event_bytes` | 8 MiB | 单次运行的事件队列字节上限 |
| `ContextPruningConfig::max_concurrent_summaries` | 2 | 共享限流器下允许的在途摘要数 |
| `McpServerConfig::max_frame_bytes` | 4 MiB | 单帧响应上限（读取过程中截断） |

## 9. 行为变化（不改变 API，但会改变你看到的结果）

1. **plan 模式不再提供 "always allow"**：plan 永不查规则，提供这个选项只会让下一次同样调用再问一遍。
2. **只读命令的名单更宽**：`uname` / `whoami` / `id` / `tree` / `git config --get` 等不再弹窗；`awk` / `jq` / `sed` / `python -c` / `node -e` 仍然会问。
3. **trace sink 的生命周期绑定到运行**：`RegistryTools` 在 `Drop` 时清除 executor 的 trace 槽位。此前槽位从不清除，导致事件通道永不关闭、等待排空的宿主永久挂起。
4. **事件超限是丢弃而不是截断**：单个事件大于整个预算时直接丢弃并计 `structural`；micro delta 在队列满时丢弃并计 `micro_deltas`。

## 10. 检查清单

```bash
cargo check --workspace --all-targets          # 编译期能抓到的都在这
rg "ThunderPlugin|PluginManifest|PluginRegistry|PluginSelector|with_plugin|forced_plugins|TriggerSpec" .
cargo test --workspace
```

如果只用了 `StandardHostBuilder` + `RootRunOptions`，通常只需要改两处：删掉 `forced_plugins`，以及把 `handle.selection` 换成 `handle.assembly.ids()`。

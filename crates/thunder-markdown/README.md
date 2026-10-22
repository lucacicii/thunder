# 🎨 thunder-markdown

`thunder-markdown` 是 Thunder 的终端 Markdown 渲染层。

它**不自己实现 Markdown 解析器**，把解析、表格、GFM 告警、语法高亮委托给
[`tui-markdown`](https://crates.io/crates/tui-markdown)（pulldown-cmark + syntect），
自己只负责 LLM 输出里那些通用解析器处理不好的部分：

1. **Thunder 配色**（`MarkdownTheme` 实现 `tui_markdown::StyleSheet`）；
2. **流式容错**：未闭合的代码围栏合成闭合，保证流式期间代码块不闪成正文；
3. **`markdown` 围栏解包**：模型把整篇文档包进 ```` ```markdown ```` 时按 Markdown 渲染，
   而不是印出一堆 `###` 和 `| :--- |`（围栏按长度配对，```` 块里的 ``` 不会提前关掉它）；
4. **Mermaid 图表**：` ```mermaid ` 渲染成 Unicode 方框线图（见下）。

---

## Mermaid

````markdown
```mermaid
flowchart LR
    A[Client] -->|Submit| B(Scheduler Leader)
    B -->[(Metadata DB)]
```
````

渲染为：

```
▸ mermaid diagram
┌────────┐  ╭──────────────────╮  ╭─────────────╮
│ Client │  │ Scheduler Leader │  │ Metadata DB │
└┬───────┘  ╰──────────────────╯  ╰─────────────╯
 │Submit
```

底层是 [`thunder-mermaid`](../thunder-mermaid)，从 `openai/codex` 的 `codex-mermaid`
vendored 而来（Apache-2.0，见其 NOTICE）。

**它的契约是"要么画对、要么报错"**：语法不支持、或图形超出当前列宽预算，都返回错误，
由 `thunder-markdown` 回退成原始代码块。**绝不输出被截断的图** —— 一张缺了节点的图比源码更误导。

当前支持：flowchart / sequenceDiagram / stateDiagram-v2 / classDiagram / erDiagram，
节点上限 16、边上限 24、标签 40 字符。

---

## 依赖

* `ratatui` 0.30
* `tui-markdown` 0.3
* `thunder-mermaid`（本地 crate，仅依赖 `unicode-width`）

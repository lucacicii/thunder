# 🎨 thunder-markdown

`thunder-markdown` 是 Thunder 的终端 Markdown 渲染层。

它**不自己实现 Markdown 解析器**，而是把解析、表格、GFM 告警、语法高亮全部委托给久经考验的
[`tui-markdown`](https://crates.io/crates/tui-markdown) 引擎（基于 `pulldown-cmark` + `syntect`），
自己只负责两件事：

1. **Thunder 的配色主题**（`MarkdownTheme`）；
2. **流式容错预处理**（LLM 打字时未闭合的代码围栏）。

---

## 为什么用 `tui-markdown` 而不是自研解析器

自研解析器曾经在 LLM 流式输出中踩过一个致命问题：遇到 `#include <stdio.h>`、`#define`、`#tag`
这类以 `#` 开头但不是标题的行时，段落循环没有推进索引，直接**死循环把 TUI 卡死**。

通用解析器把这类边界情况交给上游维护，Thunder 只补上 LLM 场景真正需要的那一层：
**未闭合代码围栏的合成闭合**。

```text
LLM 正在输出：              close_unterminated_fences 之后：
```rust                      ```rust
fn main() {                  fn main() {
    println!("hi")               println!("hi")
                             ```
                             ← 自动补一个围栏结束符
```

没有这一步，`pulldown-cmark` 会把围栏之后的所有文字吞进段落，屏幕上就会出现
“代码块一闪变成普通正文” 的抽搐。

---

## 主题

`MarkdownTheme` 实现 `tui_markdown::StyleSheet`，提供 Thunder 的深色开发者配色：

| 元素 | 样式 |
|---|---|
| H1 | 天蓝 `#38bdf8` + 加粗 |
| H2 | 亮白 `#f8fafc` + 加粗 |
| H3–H6 | 柔和灰蓝 `#94a3b8` + 加粗 |
| 行内 code / 代码块 | 青蓝 `#7dd3fc` on 深板岩 `#1e293b` |
| 链接 | 天蓝 + 下划线 |
| 引用块 | 灰蓝 + 斜体 |
| 告警 NOTE / TIP / IMPORTANT / WARNING / CAUTION | 蓝 / 绿 / 紫 / 黄 / 红 |
| 表格表头 / 边框 | 亮白加粗 / 深灰 |
| 列表标记 | 天蓝 |

---

## 使用

```rust
use thunder_markdown::{render_markdown, render_markdown_default, parse_inline};

// 整段 Markdown → Ratatui 行（可直接塞进 Paragraph）
let lines = render_markdown_default("## 标题\n\n- 列表\n\n```rust\nfn a() {}\n```", 80);

// 自定义主题
let theme = thunder_markdown::MarkdownTheme::default();
let lines = render_markdown(source, width, &theme);

// 单行内联样式（用于用户输入的原文回显，不走块级解析）
let spans = parse_inline("修改 `src/lib.rs` 里的 **MAX_VAL**");
```

---

## 依赖

* `ratatui` 0.30（`tui-markdown` 0.3 基于 `ratatui-core`，与 0.30 类型一致）
* `tui-markdown` 0.3

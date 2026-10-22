# 🎨 thunder-markdown

`thunder-markdown` 是专为终端 TUI 与大模型对话流式输出打造的高美感 Markdown 渲染引擎（以独立 Crate 发布）。

它不依赖任何具体的 Agent 业务逻辑或内核组件，**只依赖 `ratatui` 和 `unicode-width`**，输出原生的 `Vec<ratatui::text::Line<'static>>`。

---

## 核心设计与视觉亮点

1. **流式未闭合悲观容错（Streaming Lenient）**：
   * 大模型打字时代码块可能停在未闭合状态（如 ````rust\nfn main() { ` 尚未输出末尾的 ` ``` `）；
   * `thunder-markdown` 自动将未闭合的代码块识别为流式进行中（渲染 `(streaming…)` 卡片底边），**绝不闪烁或跳回普通纯文本**。

2. **语言徽章代码卡片（Syntax Highlighted Code Cards）**：
   * 代码块渲染为带有圆角边框、语言标识（如 `[rust]`, `[python]`, `[bash]`）和行号的独立卡片；
   * 内置零分配多语言语法高亮：关键字（紫色加粗）、字符串（翠绿色）、数值（橙色）、注释（暗灰斜体）、类型（青色加粗）。

3. **Callout / Alert 警告框原生支持**：
   * 自动解析 GitHub 规范的 Callout 语法：
     * `> [!TIP]` $\to$ 绿色提示框 `💡 TIP`
     * `> [!NOTE]` $\to$ 蓝色通知框 `ℹ️ NOTE`
     * `> [!WARNING]` $\to$ 黄色警告框 `⚠️ WARNING`
     * `> [!IMPORTANT]` $\to$ 红色警示框 `🚨 IMPORTANT`

4. **对齐优化与丰富行内元素**：
   * **标题**：`H1`（带色块标记 `█ `）、`H2`（带左侧竖线 `│ `）、`H3`（带原点 `• `）；
   * **行内代码**：`` `code` `` 带有深色背景底衬与青色高亮；
   * **任务列表**：`- [x]` 渲染为绿色勾选 `✔`，`- [ ]` 渲染为暗灰色圆圈 `○`；
   * **表格**：基于 `unicode-width` 精准计算中英文混合宽度，绘制规整的 Unicode 栅格边框；
   * **引用**：带有高对比度琥珀色引用条 `▎ `。

---

## 使用示例

```rust
use thunder_markdown::{render_markdown, render_markdown_default, MarkdownTheme};

// 1. 使用默认主题直接渲染
let lines = render_markdown_default("# Hello\n```rust\nfn main() {}\n```", 80);

// 2. 自定义主题与样式
let theme = MarkdownTheme::default();
let lines = render_markdown(source_text, term_width, &theme);
```

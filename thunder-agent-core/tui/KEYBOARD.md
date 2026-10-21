[English](KEYBOARD_en.md) | [简体中文](KEYBOARD.md)

# Thunder TUI 按键参考

只列出 `src/app.rs`、`src/picker.rs` 实际处理过的按键。未在代码中出现的组合键不做承诺。

## 优先级

`App::handle_key`（`src/app.rs`）按以下顺序消费按键，命中即返回：

1. 提问弹窗打开 → `handle_question_key`
2. picker 打开 → `PickerState::handle_key`
3. 帮助视图（`ViewMode::Help`）→ 滚动键；仅 `Esc` 与全局 `Ctrl+` 组合穿透到下一步
4. 全局快捷键（表格 A）
5. 当前焦点面板：`Input` / `Chat` / `Sidebar` / `Monitor`

## A. 全局快捷键

| 按键 | 动作 | 位置 |
|---|---|---|
| `Ctrl+C` | 依次尝试：复制选中文本 → 清空输入框 → 取消重命名 → 停止运行 → 退出程序；编辑器相关步骤仅在 `Input` 焦点下生效 | `App::handle_interrupt` |
| `Ctrl+Q` | 退出 | `App::handle_key` |
| `Ctrl+N` | 新建会话（运行中会拒绝） | `App::new_session` |
| `Ctrl+P` | 切换执行模式 `AutoRouter` ⇄ `SingleAgent` | `App::handle_key` |
| `Ctrl+B` | 显示 / 隐藏会话侧栏 | `App::handle_key` |
| `Ctrl+O` | 展开 / 折叠思考与工具细节 | `App::toggle_details` |
| `Ctrl+L` | 打开「链接与文件」picker | `App::open_links_picker` |
| `Ctrl+H` | 打开 / 关闭帮助视图 | `App::handle_key` |
| `Ctrl+T` | 显示 / 隐藏轮次导轨 | `App::toggle_timeline` |
| `PageUp` | 对话流上滚 10 行 | `App::scroll_up` |
| `PageDown` | 对话流下滚 10 行 | `App::scroll_down` |
| `Ctrl+Home`（或输入框为空时的 `Home`） | 滚到顶部 | `App::scroll_to_top` |
| `Ctrl+End`（或输入框为空时的 `End`） | 滚到底部 | `App::scroll_to_bottom` |
| `Shift+↑` / `Alt+↑` | 对话流上滚 1 行（优先于历史记录） | `App::scroll_up` |
| `Shift+↓` / `Alt+↓` | 对话流下滚 1 行 | `App::scroll_down` |
| `Tab` | 输入框内：循环命令第一参数 → 补全命令名；否则切换焦点 | `App::handle_key` |
| `Shift+Tab`（`BackTab`） | 输入框内：反向循环命令参数；无参数可循环时切到下一个审批模式 | `App::handle_key` |
| `Esc` | 见表格 B | `App::handle_key` |

`Tab` 的焦点顺序（`App::cycle_focus`）：`Input → Chat → Sidebar → Input`（侧栏显示时），或 `Input → Chat → Monitor → Input`（侧栏隐藏且导轨显示时），否则 `Input → Chat → Input`。重命名进行中时 `Tab` 不生效。

## B. `Esc` 的分支

| 前置状态 | 动作 |
|---|---|
| 帮助视图 / 会话列表视图 | 回到对话视图，焦点回输入框 |
| 重命名模式 | 取消重命名（不停止运行） |
| 焦点在轮次导轨 | 焦点回输入框（不取消运行） |
| 输入框有选区 | 清除选区 |
| 输入以 `/` 开头 | 清空输入与命令弹窗索引 |
| 其他 | 停止当前运行（`App::stop_run`） |

## C. 输入框（`FocusPane::Input`）

| 按键 | 动作 | 位置 |
|---|---|---|
| `Enter` | `rename_mode` 时提交新名字；命令名未写完时补全；运行中则作为 steer 消息入队；否则执行斜杠命令或提交提示 | `App::handle_input_key` |
| `Alt+Enter` | 运行中：作为 follow-up 入队 | `App::enqueue_for_run` |
| `Shift+Enter` | 插入换行（需要终端支持键盘增强协议） | `App::handle_input_key` |
| `Ctrl+J` | 插入换行（通用回退） | 同上 |
| `←` / `→` | 光标左右移动 | `App::cursor_left` / `cursor_right` |
| `Alt+←` / `Alt+→` | 按词移动 | `App::cursor_prev_word` / `cursor_next_word` |
| `Super+←` / `Super+→`（`Cmd+←/→`） | 移到行首 / 行尾 | `App::cursor_home` / `cursor_end` |
| `Home` / `End`（输入非空） | 移到行首 / 行尾 | 同上 |
| `Shift+←` / `Shift+→` | 扩展选区 | `App::extend_left` / `extend_right` |
| `Shift+Home` / `Shift+End`（输入非空） | 选区扩展到行首 / 行尾 | `App::extend_home` / `extend_end` |
| `Ctrl+A` | 全选输入内容 | `App::select_all` |
| `Ctrl+E` | 光标到行尾 | `App::cursor_end` |
| `Backspace` / `Delete` | 删除前一个 / 后一个字符 | `App::backspace` / `delete_forward` |
| `↑` | 命令参数反向循环 → 命令列表上移 → 历史记录上一条 → 输入为空时上滚 1 行 | `App::handle_input_key` |
| `↓` | 命令参数正向循环 → 命令列表下移 → 历史记录下一条 → 输入为空时下滚 1 行 | 同上 |
| 其他字符 | 插入到光标处 | `App::insert_char` |

括号粘贴（bracketed paste）走 `App::handle_paste`，不触发提交；模态打开时忽略粘贴。

## D. 对话流（`FocusPane::Chat`）

| 按键 | 动作 |
|---|---|
| `↑` / `k` | 上滚 1 行 |
| `↓` / `j` | 下滚 1 行 |
| `PageUp` / `PageDown` | 上 / 下滚 10 行 |
| `Home` | 滚到顶部 |
| `End` | 滚到底部 |

## E. 会话侧栏（`FocusPane::Sidebar`）

| 按键 | 动作 |
|---|---|
| `↑` / `k` | 选中上一行 |
| `↓` / `j` | 选中下一行 |
| `Enter` | 加载选中会话（运行中由 `LoadSession` 路径统一拒绝） |
| `d` / `Delete` | 删除选中会话并刷新列表 |

## F. 轮次导轨（`FocusPane::Monitor`）

| 按键 | 动作 |
|---|---|
| `↑` / `k` | 选中上一轮 |
| `↓` / `j` | 选中下一轮 |
| `Home` / `g` | 选第一轮 |
| `End` / `G` | 选最后一轮 |
| `Enter` | 跳到选中轮次（把该轮滚到顶部） |

## G. 提问弹窗（`PendingQuestion` 存在时）

| 按键 | 动作 |
|---|---|
| `Esc` | 以空答案放弃，agent 继续自行假设 |
| `↑` / `↓` | 在选项间移动（仅当该问题有选项） |
| `Space` | 多选问题中切换当前选项（仅当 `multi_select`） |
| `Enter` | 提交当前答案；最后一个问题回答后整组答案回传 |
| `Backspace` | 自由输入问题中删除一个字符 |
| 其他字符 | 自由输入问题中追加文本 |

## H. Picker（`PickerState::is_open`）

| 按键 | 动作 |
|---|---|
| `Esc` | 关闭 picker |
| `↑` / `k` | 上移（`k` 仅在过滤文本为空时） |
| `↓` / `j` | 下移（同上） |
| `PageUp` / `PageDown` | 上 / 下移 5 项 |
| `Home` / `End` | 首项 / 末项 |
| `Enter` | 选中当前项并关闭 |
| `Backspace` | 删除过滤文本最后一个字符 |
| 其他字符 | 追加过滤文本 |

## I. 帮助视图（`ViewMode::Help`）

| 按键 | 动作 |
|---|---|
| `↑` / `k` | 上滚 1 行 |
| `↓` / `j` | 下滚 1 行 |
| `PageUp` | 上滚 10 行 |
| `PageDown` / `Space` | 下滚 10 行 |
| `Home` / `g` | 到顶 |
| `End` / `G` | 到底 |

## J. 鼠标

| 事件 | 动作 |
|---|---|
| 左键点击导轨刻度 | 跳到该轮 |
| 左键点击链接 / 路径 | 打开 URL 或在文件管理器中显示文件；未点中时提示该行链接起始列 |
| 左键点击（picker / 提问弹窗 / 帮助视图打开时） | 忽略 |
| 指针移动 | 更新导轨悬停提示 |
| 滚轮上 / 下 | picker 打开时移动选择，否则滚动 3 行 |

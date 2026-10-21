[English](KEYBOARD_en.md) | [简体中文](KEYBOARD.md)

# Thunder TUI keyboard reference

Only keys actually handled in `src/app.rs` and `src/picker.rs` are listed. Combinations that appear nowhere in the code are not promised here.

## Precedence

`App::handle_key` (`src/app.rs`) consumes keys in this order and returns on the first match:

1. Question modal open → `handle_question_key`
2. Picker open → `PickerState::handle_key`
3. Help view (`ViewMode::Help`) → scroll keys; only `Esc` and the global `Ctrl+` combinations fall through
4. Global shortcuts (table A)
5. The focused pane: `Input` / `Chat` / `Sidebar` / `Monitor`

## A. Global shortcuts

| Key | Action | Where |
|---|---|---|
| `Ctrl+C` | In order: copy the selection → clear the prompt → cancel a rename → stop the run → quit. The editor steps apply only while `Input` is focused | `App::handle_interrupt` |
| `Ctrl+Q` | Quit | `App::handle_key` |
| `Ctrl+N` | New session (refused during a run) | `App::new_session` |
| `Ctrl+P` | Cycle execution mode `AutoRouter` ⇄ `SingleAgent` | `App::handle_key` |
| `Ctrl+B` | Show / hide the session sidebar | `App::handle_key` |
| `Ctrl+O` | Expand / collapse thinking and tool details | `App::toggle_details` |
| `Ctrl+L` | Open the links-and-files picker | `App::open_links_picker` |
| `Ctrl+H` | Open / close the help view | `App::handle_key` |
| `Ctrl+T` | Show / hide the turn rail | `App::toggle_timeline` |
| `PageUp` | Scroll the transcript up 10 lines | `App::scroll_up` |
| `PageDown` | Scroll the transcript down 10 lines | `App::scroll_down` |
| `Ctrl+Home` (or `Home` with an empty prompt) | Scroll to top | `App::scroll_to_top` |
| `Ctrl+End` (or `End` with an empty prompt) | Scroll to bottom | `App::scroll_to_bottom` |
| `Shift+↑` / `Alt+↑` | Scroll up 1 line (wins over history) | `App::scroll_up` |
| `Shift+↓` / `Alt+↓` | Scroll down 1 line | `App::scroll_down` |
| `Tab` | In the prompt: cycle a command's first argument → complete the command name; otherwise cycle focus | `App::handle_key` |
| `Shift+Tab` (`BackTab`) | In the prompt: cycle arguments backwards; with nothing to cycle, switch to the next approval mode | `App::handle_key` |
| `Esc` | See table B | `App::handle_key` |

`Tab` focus order (`App::cycle_focus`): `Input → Chat → Sidebar → Input` when the sidebar is shown, `Input → Chat → Monitor → Input` when the sidebar is hidden and the rail is visible, otherwise `Input → Chat → Input`. `Tab` does nothing while renaming.

## B. What `Esc` does

| State | Action |
|---|---|
| Help view / session-list view | Back to the chat view, focus returns to the prompt |
| Rename mode | Cancel the rename (the run keeps going) |
| Focus on the turn rail | Focus returns to the prompt (the run keeps going) |
| Prompt has a selection | Clear the selection |
| Prompt starts with `/` | Clear the prompt and the command-popup index |
| Anything else | Stop the current run (`App::stop_run`) |

## C. Prompt box (`FocusPane::Input`)

| Key | Action | Where |
|---|---|---|
| `Enter` | With `rename_mode`: save the new name. With an incomplete command name: complete it. During a run: enqueue as a steer message. Otherwise: run the slash command or submit the prompt | `App::handle_input_key` |
| `Alt+Enter` | During a run: enqueue as a follow-up | `App::enqueue_for_run` |
| `Shift+Enter` | Insert a newline (needs terminal keyboard-enhancement support) | `App::handle_input_key` |
| `Ctrl+J` | Insert a newline (universal fallback) | same |
| `←` / `→` | Move the caret by one character | `App::cursor_left` / `cursor_right` |
| `Alt+←` / `Alt+→` | Move by one word | `App::cursor_prev_word` / `cursor_next_word` |
| `Super+←` / `Super+→` (`Cmd+←/→`) | Caret to start / end | `App::cursor_home` / `cursor_end` |
| `Home` / `End` (prompt non-empty) | Caret to start / end | same |
| `Shift+←` / `Shift+→` | Extend the selection | `App::extend_left` / `extend_right` |
| `Shift+Home` / `Shift+End` (prompt non-empty) | Extend the selection to start / end | `App::extend_home` / `extend_end` |
| `Ctrl+A` | Select the whole prompt | `App::select_all` |
| `Ctrl+E` | Caret to end | `App::cursor_end` |
| `Backspace` / `Delete` | Delete the previous / next character | `App::backspace` / `delete_forward` |
| `↑` | Cycle arguments backwards → move up the command list → previous history entry → scroll up 1 line when the prompt is empty | `App::handle_input_key` |
| `↓` | Cycle arguments forwards → move down the command list → next history entry → scroll down 1 line when the prompt is empty | same |
| Any other character | Insert at the caret | `App::insert_char` |

Bracketed paste goes through `App::handle_paste`, never submitting; it is ignored while a modal owns the screen.

## D. Transcript (`FocusPane::Chat`)

| Key | Action |
|---|---|
| `↑` / `k` | Scroll up 1 line |
| `↓` / `j` | Scroll down 1 line |
| `PageUp` / `PageDown` | Scroll up / down 10 lines |
| `Home` | Scroll to top |
| `End` | Scroll to bottom |

## E. Session sidebar (`FocusPane::Sidebar`)

| Key | Action |
|---|---|
| `↑` / `k` | Select the previous row |
| `↓` / `j` | Select the next row |
| `Enter` | Load the selected session (refused during a run by the shared `LoadSession` path) |
| `d` / `Delete` | Delete the selected session and refresh the list |

## F. Turn rail (`FocusPane::Monitor`)

| Key | Action |
|---|---|
| `↑` / `k` | Select the previous turn |
| `↓` / `j` | Select the next turn |
| `Home` / `g` | Select the first turn |
| `End` / `G` | Select the last turn |
| `Enter` | Jump to the selected turn (scrolls it to the top) |

## G. Question modal (a `PendingQuestion` is present)

| Key | Action |
|---|---|
| `Esc` | Dismiss with an empty answer; the agent proceeds on assumptions |
| `↑` / `↓` | Move between options (only when the question has options) |
| `Space` | Toggle the current option in a multi-select question (`multi_select` only) |
| `Enter` | Submit the current answer; the whole set is returned once the last question is answered |
| `Backspace` | Remove one character in a free-form question |
| Any other character | Append text in a free-form question |

## H. Picker (`PickerState::is_open`)

| Key | Action |
|---|---|
| `Esc` | Close the picker |
| `↑` / `k` | Move up (`k` only while the filter text is empty) |
| `↓` / `j` | Move down (same) |
| `PageUp` / `PageDown` | Move up / down 5 items |
| `Home` / `End` | First / last item |
| `Enter` | Select the current item and close |
| `Backspace` | Drop the last filter character |
| Any other character | Append filter text |

## I. Help view (`ViewMode::Help`)

| Key | Action |
|---|---|
| `↑` / `k` | Scroll up 1 line |
| `↓` / `j` | Scroll down 1 line |
| `PageUp` | Scroll up 10 lines |
| `PageDown` / `Space` | Scroll down 10 lines |
| `Home` / `g` | Top |
| `End` / `G` | Bottom |

## J. Mouse

| Event | Action |
|---|---|
| Left click on a rail tick | Jump to that turn |
| Left click on a link / path | Open the URL, or reveal the file in the file manager; a near miss reports the line's link start column |
| Left click while a picker / question modal / help view is open | Ignored |
| Pointer movement | Update the rail hover hint |
| Scroll wheel up / down | Move the picker selection when open, otherwise scroll 3 lines |

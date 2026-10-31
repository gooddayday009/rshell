# 终端常用快捷键

参考 [Xshell 官方快捷键表](https://netsarang.atlassian.net/wiki/spaces/EN7/pages/419957476/Pre-defined%2BShortcut%2BKey)。用户补充的清单来源为 CSDN 作者 Fighting_Orz 的[原文](https://blog.csdn.net/flyaaa123/article/details/124430453)，原文声明 CC 4.0 BY-SA；本页按 RShell 实际行为重组，并依据 [GNU Bash 手册](https://www.gnu.org/s/bash/manual/bash.html) 修正部分描述。窗口动作由 RShell 处理；Shell 编辑动作通过控制字节或转义序列交给远端，不能保证所有 Shell 使用相同键位。

## Shell 按键

下列含义以 Bash/Readline 常用 Emacs 编辑模式为例；vi 模式、PowerShell、tmux/screen、Vim 可有不同含义。自定义应用快捷键若占用相同组合会优先处理。

| 按键 | 常用动作 |
| --- | --- |
| Ctrl+A / Ctrl+E | 行首 / 行尾 |
| Ctrl+B / Ctrl+F | 向左 / 向右移动一个字符 |
| Ctrl+← / Ctrl+→、Alt+B / Alt+F | 向左 / 向右移动一个单词；Ctrl+方向键发送 xterm 修饰键序列，需远端绑定支持 |
| Ctrl+X 后再按 Ctrl+X | 交换光标与标记位置，并非固定跳到行尾 |
| Ctrl+U / Ctrl+K | 删除光标前 / 后的行内容 |
| Ctrl+W / Alt+D | 删除前一个 / 后一个单词 |
| Alt+Backspace | 删除光标左侧单词 |
| Ctrl+H | 删除左侧字符 |
| Ctrl+Y / Ctrl+T | 恢复剪切内容 / 交换字符 |
| Alt+T | 交换单词 |
| Alt+. | 插入上一条历史命令的最后一个参数 |
| Ctrl+_、Ctrl+X 后按 Ctrl+U | 撤销编辑；常见键盘上 Ctrl+_ 为 Ctrl+Shift+- |
| Alt+R | 撤销当前行的全部编辑 |
| Ctrl+P / Ctrl+N | 上一条 / 下一条历史 |
| Ctrl+R / Ctrl+G | 反向搜索历史 / 取消搜索；搜索中 Enter 接受并执行，Esc 接受当前结果回到编辑 |
| Ctrl+O | 接受并执行当前行，再取历史中相对于当前行的下一条供编辑 |
| Ctrl+L | 请求 Shell 重绘屏幕 |
| Ctrl+C / Ctrl+Z | 中断 / 挂起前台程序 |
| Ctrl+D | 删除光标处字符；空行时可能退出 Shell |
| Ctrl+S / Ctrl+Q | 部分远端终端暂停 / 恢复输出 |
| Tab / Shift+Tab | 补全 / 反向 Tab，由远端解释 |
| Home/End、方向键、Delete、F1–F12 | 发送终端键序列 |

Alt+B/F/D/T 也可依次按 Esc，再按对应的小写字母；Esc、数字、动作键可给 Readline 传递重复次数，例如 Esc → 3 → b。Ctrl+A 后 Ctrl+K、或 Ctrl+E 后 Ctrl+U 可清空当前行；这些是顺序操作，不是同时按三个键。Ctrl+& 并非 GNU Readline 的标准撤销键。

`!!`、`!前缀`、`!-n`、`!$`、`!*`、`:p`、`^旧文本^新文本^` 属于 Bash 的历史展开语法，作为普通文本原样发送，由远端决定是否展开；RShell 不自行解释或执行。`^旧文本^^` 可用于删除上一条命令中的匹配文本；`$_` 是 Shell 特殊参数，不能无条件等同于历史展开 `!$`。这些语法依赖 Shell、历史记录和历史展开设置。

## RShell 操作

| 按键 | 动作 |
| --- | --- |
| Ctrl+Tab / Ctrl+Shift+Tab | 当前分屏下一个 / 上一个标签 |
| Ctrl+PageDown / Ctrl+PageUp | 下一个 / 上一个标签（可配置） |
| Ctrl+Alt+→ / Ctrl+Alt+← | 下一个 / 上一个标签备用键（可配置） |
| Shift+Alt+N | 新建本地 PowerShell（可配置） |
| Ctrl+Shift+F4 | 关闭标签（可配置，沿用关闭确认） |
| Ctrl+Shift+C | 复制选区（可配置） |
| Ctrl+Shift+F | 查找终端历史（可配置） |
| Ctrl+V / Shift+Insert / Ctrl+Shift+V | 粘贴；统一过滤控制字符，多行须确认 |
| Ctrl+Shift+A | 清除当前终端本地显示和历史，不向远端发送命令 |
| Shift+PageUp / Shift+PageDown | 历史上一页 / 下一页 |
| Shift+↑ / Shift+↓ | 历史上一行 / 下一行 |
| Shift+Home / Shift+End | 历史顶部 / 底部 |
| Ctrl+滚轮 | 当前终端字号缩放 |
| Ctrl+Shift+M | 切换菜单栏 |

Ctrl+A 不用于全选终端输出，Ctrl+C 不用于复制。不要将 Ctrl+A/E 等常用 Shell 键分配给应用动作。Ctrl+Insert 在当前窗口后端与 Ctrl+C 合并为同一种事件，尚未实现可靠的复制区分，请使用 Ctrl+Shift+C。

加载旧配置时，原 Ctrl+F 查找自动迁移为 Ctrl+Shift+F，之后随常规配置保存落盘。其他自定义键位保留；若 Ctrl+Shift+F 已分配给其他动作，则不覆盖，请在快捷键设置中解决冲突。Alt 字母、Alt+. 和 Alt 数字支持仅按键事件或按键附带文本事件两种输入路径，附带文本时只发送一次。

默认不显示查找入口及查找栏，也不占用终端高度。使用 Ctrl+Shift+F（可配置）或「编辑 → 查找…」打开终端右上角查找栏，按钮与输入框使用 11px 字号，点击右侧「关闭查找」收起并恢复终端输入焦点，保留搜索词。鼠标从查找框、AI 面板等区域移回终端后恢复输入焦点；多分屏时移到哪个终端区域，就激活哪个分屏，无需点击。拖选文字、拖动标签或分隔条期间不因经过其他分屏而切换；确认弹窗、多行粘贴确认及未就绪本地终端仍阻止输入。鼠标停留不动不会持续抢走查找框焦点。

## 验证边界

已用终端 GUI 控件测试验证清单中的 Ctrl 字母、Ctrl+方向键、Ctrl+X 前缀、撤销、Esc 数字序列、Alt 编辑键两种事件路径，以及 Ctrl+Shift+F 只打开查找；还验证失焦清理组合输入后控制键恢复，以及历史首尾跳转不写入远端。项目检查通过不代表真实 Linux/OpenSSH 或真实输入法验收完成。Xshell 的脚本快捷键、快捷命令编号、透明窗口、所有面板焦点与全屏快捷键未完整移植。

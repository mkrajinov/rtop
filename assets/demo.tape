# Demo GIF for the README. Recorded inside a Linux container, never on a personal machine:
# the header, USER and COMMAND columns show whatever machine rtop runs on.
#
#   make demo
#
# (puts a static Linux rtop on PATH in ghcr.io/charmbracelet/vhs and runs this tape there)
Output assets/demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 1800
Set Height 900
Set Padding 16
Set TypingSpeed 60ms
Set Theme "Catppuccin Mocha"
Set Framerate 10 # low enough for the container to keep up, so the GIF plays in real time

Hide
Type "export PS1='$ ' && clear"
Enter
Show

# 1. Every number explains itself: focus RSS, open the explain panel.
Type "rtop"
Sleep 300ms
Enter
Sleep 2.5s
Right 3
Sleep 800ms
Type "e"
Sleep 6s
Type "q"
Sleep 500ms

# 2. A lab: two readers, same reads, one from disk and one from the page cache.
Type "rtop lab pagecache"
Sleep 300ms
Enter
Sleep 6s
Type "v"
Sleep 4s
Type "n"
Sleep 3s
Type "q"
Sleep 1s

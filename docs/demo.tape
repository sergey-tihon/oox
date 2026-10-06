# Regenerate docs/demo.gif from the repo root:
#   cargo build --release --locked && vhs docs/demo.tape
Output docs/demo.gif

Set Shell "bash"
Set FontSize 16
Set Width 1280
Set Height 760
Set Padding 12
Set Theme "Catppuccin Mocha"
Set TypingSpeed 70ms

Hide
Type "export PATH=$PWD/target/release:$PATH && clear"
Enter
Show

Type "oox data/sample.pptx"
Sleep 400ms
Enter
Sleep 4s

# Expand the whole package tree
Type "E"
Sleep 2.5s

# Filter to a slide and preview its XML
Type "/"
Sleep 300ms
Type "slides/slide1"
Sleep 800ms
Enter
Sleep 300ms
Enter
Sleep 3s

# Embedded image preview
Escape
Type "/"
Sleep 300ms
Type "image1"
Sleep 800ms
Enter
Sleep 300ms
Enter
Sleep 3s

# Document summary
Type "s"
Sleep 3s
Type "s"

# Key help
Type "?"
Sleep 3s
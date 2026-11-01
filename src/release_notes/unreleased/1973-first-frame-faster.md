fix: croft's first frame is up in a fraction of the time: building the editor no longer compiles the secret-masking and agent rules or reads the bundled extension list five times, and the highlight prewarm, file index, git status, language servers, downloads and icon bakes wait until the frame is on screen instead of competing with it for the CPU.
fix: Every frame reaches the terminal in one write and shows up whole, so the first frame no longer paints in from the top.
fix: A launch from Croft.app no longer runs the shell's startup files beside croft's own startup; the PATH refresh for the next launch waits until the first frame is up.
fix: A Croft.app built from a Homebrew croft starts the version `brew upgrade` installed, not the one it was built from.
feature: Every launch writes `~/.cache/croft/startup-trace.txt`: how long each startup step took and how much of it was waiting, and when the terminal that started croft began, so a slow launch says where its time went.
fix: Running a task, a runnable code block or an agent lane in an idle bash pane no longer rings the bell, and the status bar keeps saying "Running …" instead of "Bell in terminal".

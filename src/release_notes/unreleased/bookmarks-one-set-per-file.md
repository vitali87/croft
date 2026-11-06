fix: A file open in two tabs or two splits keeps one set of bookmarks: setting or clearing one in a tab does the same in the others.
fix: Closing the last tab of a file keeps its bookmarks: they come back when the file opens again and are saved with the session.
fix: The editor's caret is hidden while a list picker (View Bookmarks, Quick Fix, Select Channel and the rest) is open, instead of blinking behind it.
fix: Each workspace keeps its own saved session, in croft's state folder rather than a .session file in the folder croft was started from, and it is read after the first frame, so startup never waits for it.
fix: Bookmarks also come back on the next launch when hot exit brings back unsaved edits, each on the line it moved to with them.
fix: A bookmark saved with the session comes back on the line it marked even when the file changed since, such as lines added above it and saved before a crash, rather than on its old line number.

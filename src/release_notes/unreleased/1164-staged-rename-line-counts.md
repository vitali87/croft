fix: Source Control counts the lines a staged rename changed, so a renamed and edited file no longer shows +0 -0 and the Staged Changes total matches `git diff --cached --shortstat`.

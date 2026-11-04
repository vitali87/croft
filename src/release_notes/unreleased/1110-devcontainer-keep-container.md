fix: `croft devcontainer` keeps the container when devcontainer.json changes and says to run `--rebuild`, instead of deleting it and killing a croft still running inside with its unsaved edits.
fix: Comments and formatting in devcontainer.json no longer count as a config change.
fix: `croft devcontainer --rebuild` refuses while croft is still running in the container from another terminal.

fix: Migrate Changes restores the stash it made, even when a checkout hook pushes a stash of its own during the switch, instead of restoring the hook's and leaving your changes stashed.

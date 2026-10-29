fix: `croft record --stop` no longer terminates an unrelated process that was given the pid of a recorder stopped with Ctrl-C.
fix: Stopping `croft record` with Ctrl-C or `croft record --stop` exits 0, so `croft record --out x.cast && agg x.cast x.gif` goes on to make the GIF.
fix: A second `croft record` for a session that is already being recorded is refused, instead of leaving the first recorder impossible to stop.

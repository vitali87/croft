fix: connecting to an unreachable host from the in-app connect dialog now gives up after 10 seconds, as `croft remote` does, instead of waiting out the system's TCP timeout (#2796).

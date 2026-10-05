fix: A session host whose self-update could not start the new binary keeps ignoring SIGPIPE, so the next write to a client that disconnected no longer kills the whole session.

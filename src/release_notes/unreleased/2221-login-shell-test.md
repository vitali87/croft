refactor: The login shell's PATH probe now takes its deadline from the caller instead of fixing it inside, so its tests can allow for a busy machine (#2221). Nothing changes in use.

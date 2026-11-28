refactor: A workspace switch starts the language servers' state over in one call instead of resetting its fields one by one, so a field added later is reset too (#2062). Nothing changes in use.

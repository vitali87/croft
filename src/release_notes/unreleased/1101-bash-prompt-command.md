fix: Terminal command marks and the working directory keep working in bash when `PROMPT_COMMAND` ends in `;`, as zoxide's init leaves it, instead of every prompt printing a `;;` syntax error.

fix: Regex Replace no longer deletes a `$5`, `$9` or `${3}` that numbers no group in the pattern; it stays as typed, and `$10` with one group reads as `$1` then `0`, as in VS Code.

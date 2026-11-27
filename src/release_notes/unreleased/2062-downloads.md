refactor: every download croft reads into memory now goes through one capped reader that refuses past its cap, and every checksum through one rule (#2062). Nothing changes in use.

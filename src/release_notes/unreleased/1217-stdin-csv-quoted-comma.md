fix: `croft view -` opens piped CSV in the sheet grid even when a field holds a quoted comma ("Doe, John", "1,200"); the sniff now counts fields with CSV quoting instead of raw commas.

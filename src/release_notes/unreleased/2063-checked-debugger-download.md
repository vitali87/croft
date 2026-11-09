fix: The JavaScript and TypeScript debugger download is checked against its pinned release before croft unpacks and runs it, and a download that is too large is refused instead of being cut short and installed broken.
fix: A language server download over its size limit is refused with a message naming the limit, instead of being cut short.

fix: Accepting a completion for a name from another file adds its import: croft now asks TypeScript and JavaScript's vtsls for the import it sends only on request, and keeps the import Python's ty sends with the item, which was dropped.
fix: Undo takes back an accepted completion in one step when its server gave no edit range, instead of one step per character typed.

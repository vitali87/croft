feature: Completion opens as you begin a word in code, as VS Code's quick suggestions do, and stays out of strings, comments and Markdown. Set `disable_quick_suggestions` to keep it to Ctrl+Space and the trigger characters.
fix: Completion opens on the characters each language server asks for, such as `"` and `/` in a TypeScript import path, instead of only on `.`, and typing `.` with the list already open now lists the members.
feature: Trigger Suggest (Ctrl+Space) is in the Command Palette and can be bound in keybindings.json.

fix: The SQLite viewer shows NULL as a dimmed `NULL`, so it reads apart from an empty string.
fix: The SQLite viewer lists views beside the tables, labelled `(view)`, and a view whose table is gone says why instead of stopping the database from opening.
fix: Spreadsheet, CSV and SQLite cells show a newline as `⏎` and a tab as `→` instead of dropping them, so `line1⏎line2→tabbed` no longer reads `line1line2tabbed`.

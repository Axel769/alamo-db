# What's new

One section per version, the most recent at the top. Every version has its
`v<version>` tag on this repo, on the commit the build was made from.

The text of the section goes, as is, into the app's update window (plain
text, line breaks preserved): short lines, words for the people who use the
app, no Markdown beyond the dashes.

Before launching "Release (alpha)": bump `version` in
`app/src-tauri/tauri.conf.json` and rename "Unreleased" to
`[<version>] - <date>`. Without the version's section the workflow stops.

## [0.1.0-alpha.12] - 2026-11-02

- Grid: with a whole column selected, ⌘+click (Ctrl+click on Windows and Linux) on one of its cells takes that cell out of the selection, and the same click puts it back. Copy, typing a value and ⇧Del then skip it.
- macOS, table tabs: ⌘V on a selected cell pastes right away what was copied (⌘C from another cell or from elsewhere). Before, ⌘V on the grid did nothing.
- SQL editor: Tab inserts spaces at the cursor (with more lines selected it indents them, ⇧Tab takes the indent back). Before, Tab jumped to the result grid and moved its cell cursor.
- Connections window: ⌘F (Ctrl+F on Windows and Linux) goes to the profile filter, the up and down arrows move between profiles (also from the password field) and Enter connects the selected one. Typing in the filter selects the first match.
- Window: Alamo reopens with the size, position and maximized state it had when you quit, also across screens. The first time it opens maximized. Before, it always opened in a small window.
- Table tabs: after Save, the cell panel and the row card show the saved value of long values (JSON or text over 4 KB). Before, they went back to the value from before the save until the table was reloaded.

## [0.1.0-alpha.11] - 2026-10-29

- MySQL/MariaDB with few connections allowed: after the computer went to sleep, disconnecting and connecting again (or quitting and reopening the app) no longer leaves the old connections taking up seats on the server. Alamo remembers them and closes them at the next connection, only if they are idle and belong to the same user on the same server. Before, with 3 connections allowed, one sleep left one seat free and the next one none, for up to 8 hours.
- MySQL: running a new query while the previous one is paused ("scroll for more rows") starts right away. Before, the app first read the whole rest of the paused result: on a large table this took minutes, and ⌘↵ did nothing in the meantime.
- If ⌘↵, Commit or Rollback cannot start because the connection is busy, a message says which tab is using it, instead of nothing happening.
- Table tabs say what they are waiting for instead of a bare "loading…": the query running (with Cancel), another table tab of the same connection loading first, or the connection to the server.
- Table tabs: Reload no longer shows "another table tab is already loading" by mistake, and an error in one table tab no longer appears in the next one opened.
- Row card: in table tabs the fields can be edited, like in the grid. Double click a field (or Enter) to change it, Enter to keep it, Esc to undo; ⇧Del sets NULL. Changes are highlighted and written with Save. Long values (more lines, very long text) open in the cell panel below the grid, with the row card still open. On a foreign key only the → arrow next to the value opens the linked record: the value is edited like the others.
- Row card and new rows: "+ Row" and "Duplicate" with the row card open show the new row there, ready to fill in field by field (handy on tables with many columns). The cell panel at the bottom follows the new row too.
- Row card (the fields of the selected row, to the right of the grid): drag its left edge to make it wider, double click the edge to go back to the default width. Column names get more room as it widens.
- New app icon: a black poplar (the "álamo" of the name) rooted in a database.
- After a Rollback (or an automatic one) the open tables show the data again as it is on the server: before, a value saved from the grid and then rolled back stayed on screen.
- Manual commit on any connection: the "Auto-commit" button in the toolbar switches to "Manual commit", so changes wait for Commit or Rollback, grid edits included. Production connections are always in manual commit.
- Transaction log: the Commit button shows how many changes are waiting, and the transaction badge opens their list (each statement with its rows). On production connections Commit always shows the list first, so you see what you are confirming.
- UPDATE and DELETE without WHERE ask for confirmation on every connection, not only production ones, showing how many rows they would touch.
- Transactions left open: after 15 minutes without queries the app closes them with a ROLLBACK, on MySQL too (before, a forgotten transaction held its locks on the server for hours). In the last 2 minutes the transaction badge counts down, and a message says whether uncommitted changes were lost.
- Script with several queries: "Export all" next to the result tabs saves every result in one go, in CSV, TSV, JSON or INSERT. Either one file with a section per query (each introduced by its SQL), or a new folder with one file per query (01_select_….csv, 02_show_index_….csv), never overwriting an earlier export. INSERT puts each result into the table it comes from.

## [0.1.0-alpha.10] - 2026-10-27

- PostgreSQL: the tree lists every database of the server, like DBeaver (connection → databases → schemas → tables). Expanding a database opens a connection to it, with the profile's credentials and SSH tunnel; its tables and new queries work on that database. Disconnecting closes them all. Favorites are kept per database.
- PostgreSQL connection page: it starts from the server's databases. Double click one to see its schemas and tables; "‹ Databases" (or Esc) goes back, the name next to it switches to another database. The database marker in the tab bar and "New query on this database" open that database.
- PostgreSQL: after a restart or a reconnection, the tabs of another database go back to it when you switch to them (one connection per database, opened only when needed). If the database cannot be opened anymore, a message says why.
- PostgreSQL: the profile's "Show only databases" / "Hide databases" filter now applies to the server's databases, as on MySQL; schemas are no longer filtered. A filter written for schemas may now hide every database: tree and page say so, change it in Profile….
- "+" right after opening a table from the tree of another connection now opens the query on that table's connection.
- Table tab: foreign-key cells show a ↗ arrow, like links. Click it to open the linked record in the same tab (⌥← brings you back), ⌘+click (Ctrl+click on Windows and Linux) to open it in a new tab. A click on the value still just selects the cell.
- SQL editor and WHERE bar: picking a suggestion adds a space after it, ready for the next word ("user" → "username "). If you then type "." "," "(" ")" ";" or another space, it takes the space's place, so you get "username," or "count(" and never a double space.
- Table tab: "+ Row" and "Duplicate" put the new row right below the selected one, like DBeaver, instead of at the bottom where it got lost in tables with many rows. After Save the row stays where you added it and the cursor stays on it. Scrolling to the next pages no longer shows it a second time.
- Table tab, like DBeaver: click a column's name to select it, then type. The value goes to every loaded row of the column (to be written with Save); ⇧Del sets them all to NULL. The cursor stays on the row you were on.
- macOS: quotes and dashes stay as you type them. `"` no longer turns into curly quotes and `--` into a long dash in cells, SQL and filters.
- Tabs are kept together by database: inside a connection each database has one block, its queries first and then its tables, so a database's name no longer shows up twice in the bar. A new tab joins its database's block; open tabs never move. Drag a tab anywhere inside its connection's group.

## [0.1.0-alpha.9] - 2026-10-27

- Tabs take less room. The database is no longer written under every tab: a marker with its name opens each run of tabs on it, shortened to the part that tells the databases apart ("sales", "billing" instead of app_sales_test). The full name is in the tooltip, on the connection page and in the ▾ list.
- Click the database marker to open its tables on the connection page; right click for "New query on this database" and "Close the database's tabs".
- The connection's label, and beside it the marker of the database whose tabs pass under it, stay at the left edge while the bar scrolls.
- A new tab opens right after the last tab on the same database, so a database's tabs stay together; open tabs never move.
- When the bar fills up the tabs get narrower, with the name cut in the middle so its end stays readable ("…routes"); only then does the bar scroll. The ▾ button lists every tab with its full name. The active tab's name is no longer bold, so clicking a tab never shifts the others.
- Drag a tab to reorder it, inside its connection's group and among tabs of its kind.
- ⌘W (Ctrl+W on Windows and Linux) closes the active tab, asking first if it has unsaved changes. Pinned and running tabs stay open.
- Servers with thousands of tables: right after connecting, the tree and the connection page no longer wait for the editor's autocomplete (up to 30 s before); the table list of a database opens in a fraction of a second.
- Users with few connections (for example 3, with HeidiSQL open on the same user): the tree no longer fails, it shares the editor's connection like DBeaver; while a query runs it says so, and Retry loads it once the query ends.
- When the databases or the tables cannot be read, "unavailable" now says why and what to do, with Retry and the server's error one click away, ready to copy.
- Table tab: long or multi-line values of a new or duplicated row, not yet saved, are edited in the cell panel at the bottom, like those of the saved rows. Before they opened in a box over the cell that came out cut on the left in a small window.
- MySQL: BIT columns show their number (0 or 1) instead of raw bytes like \x00, and can be edited in the grid.

## [0.1.0-alpha.8] - 2026-10-23

- Grid: clicking a column header selects the whole column, like DBeaver (⌘/Ctrl+click adds a column, Shift+click extends); ⌘C copies all the downloaded rows of the selected columns. Sorting moved to the right end of the header: a "⇅" appears there on hover, click it to sort (⌘/Ctrl or Shift+click adds a sort column). "Sort ▾" and the right-click menu are unchanged.
- Database and table tree: typing in the filter no longer freezes while the tree redraws, even with thousands of tables.
- Tree filter on a server with many databases: the table names are read with one request instead of one per database (before, over a slow connection, every database showed "…" for a long time). While it runs the tree says "searching the tables…".
- Connection page: favorites star on every database and table, and a "favorites only" button next to the filter, in both the database list and the table list. Same stars as the tree.
- A database that is not a favorite but holds favorite tables shows an empty star, with "Contains favorite tables" on hover: that is why it stays listed under "favorites only".
- Import connections: the dialog shows the required file format, with an example to copy; it opens by itself when a file is not accepted.
- Italian messages are written with the accents (è, può, più) instead of the apostrophe (e', puo', piu').

## [0.1.0-alpha.7] - 2026-10-23

- Long values with line breaks (for example a JSON over 4 KB) show their beginning in the grid cell, not just the first line.
- Long values and values on more lines (JSON) are edited in the panel under the grid, with room to write and JSON colors, also beyond 4 KB (before they could not be edited); short values are still edited in the cell. Shift+Enter edits any cell in the panel, Esc undoes, ⌘↵ goes back to the grid.
- Selecting a cell in the last visible rows no longer hides it under the panel that opens below the grid.

## [0.1.0-alpha.6] - 2026-10-23

- With the app in English, the error messages of Alamo DB itself and the Mac menu bar are in English too (before they were always in Italian).
- Windows: installing an update no longer stops with "Failed to kill Alamo DB. Please close it first then try again".

## [0.1.0-alpha.5] - 2026-10-22

- Mac: the Keychain no longer asks for the password at every update. With this version it asks one last time for every saved connection (enter the Mac password and choose "Always Allow"), then never again. The same goes for the permission on the Documents folder, if an SSH key lives there.
- Right-clicking the grid no longer loses the rows selected from the row number: "Copy selection as" (TSV, CSV, JSON, INSERT) copies the whole rows with all their values.
- Tab bar: the open tab is recognizable at a glance even with many tabs.
- Database and table tree: row count and favorites star no longer spill past the panel's right edge.
- The left panel widens or narrows by dragging its right edge.
- A table's Structure and DDL reload with ⟳ or F5.
- With "favorites only" on, favorite tables are shown even if their database is not among the favorites.

## [0.1.0-alpha.4] - 2026-10-21

- Fixed a sudden crash of the app with MySQL/MariaDB after the Mac went to sleep or the network was slow to come back: the half-open connection is now reopened instead of being reused.
- Favorite tables and databases, as in HeidiSQL: the star on the tree row marks them, the star button next to the filter shows only those.
- The panel below the grid with the cell value (useful for long JSON) resizes by dragging its top edge; double-click to go back to the normal height.
- When the connection has dropped (VPN, network, Mac asleep) and Alamo DB is reopening it, you can now see it: a "Connecting to …" bar with the seconds, the Run button and the status bar saying "Connecting…", a spinner on "Refresh".
- "Reconnect" from the right-click menu on the connection (left bar) and in the connection page: after a dropped VPN it immediately reopens SSH tunnels and connections, and reloads databases and tables. Old connections left hanging on the server are closed.
- INSERT export of a query (MySQL/MariaDB): the table name is the one the data comes from instead of "result", if all the columns come from the same table.

## [0.1.0-alpha.3] - 2026-10-21

- MySQL/MariaDB users with few connections: Alamo DB uses at most 2 and always leaves one free for "Cancel" and for other programs (e.g. HeidiSQL). Tree, tables and statistics share a connection when needed.
- After the PC went to sleep, a network change or a dropped VPN, Alamo DB closes the connections left hanging on the server instead of letting them occupy slots for hours.
- Clearer message if the user has run out of connections anyway: what is occupying them and what to ask the administrator.

## [0.1.0-alpha.2] - 2026-10-20

- Update window at startup, like DBeaver: release notes, "Skip this version", "Later", "Install and restart".
- Import/export of connections to a JSON file, with encrypted passwords if you want.
- Clear message when the database user exceeds its connection limit on the server (MySQL).
- Security fixes in dependencies (rustls).

## [0.1.0-alpha.1] - 2026-10-19

- Alpha build for Windows.

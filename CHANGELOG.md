# What's new

One section per version, the most recent at the top. Every version has its
`v<version>` tag on this repo, on the commit the build was made from.

The text of the section goes, as is, into the app's update window (plain
text, line breaks preserved): short lines, words for the people who use the
app, no Markdown beyond the dashes.

Before launching "Release (alpha)": bump `version` in
`app/src-tauri/tauri.conf.json` and rename "Unreleased" to
`[<version>] - <date>`. Without the version's section the workflow stops.

## Unreleased

- New app icon: a poplar under an arch on green hills, in the Dock, the taskbar, the window and the connections dialog.

## [0.1.0-alpha.14] - 2026-11-06

- Tab bar: a tab with a short name ("SQL 3") is as wide as its name, with the × right after it, instead of the fixed width that left a gap between the two. Long names keep their minimum width and the "…" cut when the bar is full.
- Settings → Shortcuts: every keyboard shortcut of the app can be changed. The actions are listed by where they work (everywhere, SQL editor, table tab, grid); click the key, press the new one, and it is saved; a key used by two actions is flagged in red; "Remove" leaves an action without a key and a changed action goes back to the preset with one click. Two presets, DBeaver and DataGrip, give their keys to the actions Alamo has in common with them (run, run the whole script, commit and rollback, close tab, add or delete a row, and the others). Tooltips, menus, the Run button and the command palette show the key in force. On Windows and Linux the hints now read Ctrl+… instead of the Mac glyphs.
- Settings → Updates: the check at startup can be turned off, "Check now" asks at any time (a newer version opens the update window; a development build says the check is off instead of a false "up to date"), "Copy diagnostics" puts in the clipboard what a bug report needs (version, system, webview, engines with server version, changed settings, where settings.json is; no hosts, profiles or passwords) and "Open settings folder" shows settings.json in the file manager.
- Settings → Connections: a connection timeout (15 s by default) on the database server and on the SSH bastion, where before a server that accepted and stayed silent hung the dialog; the TCP keep-alive after idle and the SSH keep-alive (both 30 s); automatic reconnection on or off (off: a lost connection is reported with a Reconnect button); and the passwords policy ("save in the keychain when asked" or "always ask, never save", which hides the save box). They apply to the next connections.
- Development build: deleting a profile or forgetting an SSH credential now removes the entry of the test keychain service, not the real app's.
- Settings → Export: the format the export dialog proposes, the CSV delimiter (comma or semicolon), a UTF-8 byte order mark for Excel on Windows, LF or CRLF line endings and the rows per INSERT statement (was fixed at 500). The dialog starts from them and each export can still differ; the clipboard never gets the byte order mark.
- Settings → Copy: the separator ⌘C (Ctrl+C) puts between the cells (tab, comma or semicolon), the column names as first line when more than one cell is copied, and quotes around every value or only where needed. "Copy selection as" follows the header and quote choices too.
- Settings → Data: the word shown for NULL, dates and times in ISO or in the language's format, timestamps with time zone in UTC or in this computer's zone, a thousands separator in numbers, how many characters a cell shows before "…" (was fixed at 500) and binary values as hexadecimal or base64. They change what the grid draws, at once; copying, exporting and editing keep the value as the database gives it.
- Per-connection overrides of the execution settings now survive a change in the settings window: a connection with its own pause or page size kept the global value until a reconnect. The table page size, the idle-rollback message and the DDL confirmation of a connection follow its own values, and the "use the default" line of the connection dialog shows the global value in force, not the built-in one. An appearance or editor change no longer touches the open connections.
- First start, before any profile: the connections dialog introduces the app with its icon and name in place of the empty list, three pieces in a short cascade; the form on the right is usable at once.
- Tooltips are the app's own: same look on every platform, after half a second, at once on the next button of a toolbar, hidden on a click or a key; they also open on keyboard focus. The text is the one the hover showed before, now in full lines.
- Dark theme: red text and borders (errors, Delete, failed statements) are brighter and readable on the dark background.
- Settings can be opened from the connections dialog (gear in its title): at the first start, before any connection, language and theme are one click away; the settings window sits above the dialog.
- Same look for every control: one focus ring on every text field and select (settings search, settings values, connection page filter, export and import fields had none or a different one), soft borders everywhere, two shadows for the whole app (menus and windows), 12 px corners on every window, a small dip when a button is pressed, hover on the section tabs of the connection dialog and on the segmented switches, settings categories laid out like the profile list.
- Tree: a schema with thousands of tables opens in a fraction of the time (3000 tables: 0.2 s instead of 1.2 s) and filters faster, because only the rows in view are drawn; scrolling and expanding a table work as before.
- A profile name is at most 120 characters: the dialog stops there and an import cuts there.
- Tree, connections panel and tab bar: a name cut with "…" is whole in its tooltip (table, schema, column, connection); the column tooltip shows the type and a short default, not a 2 KB one; MySQL views no longer show "VIEW" as their comment; a tab title in Japanese or Chinese keeps a readable tail; names written right to left are cut on the right side.
- A long server error (a trigger's message of a few KB) no longer runs past the results pane: it scrolls inside it and leaves half of the pane to the grid. The cell panel cuts a 64-character column name with "…" (whole in the tooltip) and keeps its buttons in sight in a narrow pane. The "all tabs" list no longer widens to most of the window with long names.
- With "reduce motion" on in the system, buttons and tabs keep their short colour fade on hover and focus; only movement is dropped.
- Dialogs (connection, settings, export, update and the others) open with a short fade instead of appearing at once; closing stays instant. The command palette and the side panel do not animate. With "reduce motion" on in the system, only the fade remains.
- While a query runs, the moving thread under the query bar no longer costs the main thread anything: the rows arriving are not slowed down by it, and it moves at a constant speed.
- A connection profile can override the execution settings for itself (new "Settings" tab in the connection dialog): each one is "use the default (value)" or a value of its own. The overrides travel with the connection in export and import. On connect, an override this build cannot use (unknown or not overridable) is reported once, and the global settings stay in force.
- Settings → Execution: a query timeout (seconds, 0 = none). The server stops a statement that runs longer: on PostgreSQL and MariaDB every statement, on MySQL only SELECT. It applies from the next query and the error says the limit came from the settings.
- Settings → Execution: rows fetched before a SQL tab pauses (was fixed at 200), rows per page of a table tab (500), memory per result set (384 MB), manual commit as the default of new connections, the idle time after which an open transaction is rolled back (15 minutes), and an extra confirmation of schema changes on production. The limits reach the open connections at once, also a fetch already running.
- Settings → Editor: autocomplete on/off and its delay, keywords in upper case as you type (never inside strings, comments or quoted names), indent width, wrapping of long lines, what ⌘↵ (Ctrl+Enter) runs (statement at the cursor, selection or the whole script; the Run button says which), the confirmation of UPDATE and DELETE without WHERE on non-production profiles, and the size of the query history. All apply to the open editor at once.
- Settings → Appearance: font and text size of the SQL editor and of the grid, grid density (compact rows of 26 px, or comfortable of 30 px) and high-contrast grid lines. They apply at once to the open tabs. The default monospace is now the system one on every platform (SF Mono, Consolas, DejaVu Sans Mono).
- Settings window: categories on the left (Appearance, Updates), a search box that finds a setting by its name in Italian or English, every value that differs from the default marked with the default next to it, and "Restore defaults" per section. It opens from the gear, the command palette, "Settings…" in the macOS menu (⌘,) or Ctrl+, on Windows and Linux.
- Settings: language and theme change at once, without reloading the app. They are saved in settings.json in the app's configuration folder (with the skipped update version), no longer in the browser storage: the choices made so far are carried over at the first start.

## [0.1.0-alpha.13] - 2026-11-05

- Updates and downloads now come from github.com/Axel769/alamo-db, the public source repository. The old alamo-db-releases repository only keeps a pointer to this version, so that installations up to alpha.12 find the update.
- Table tabs on production (or with manual commit): Save works while a SQL tab of the same connection shows a query paused at its first rows ("scroll for more rows"). The paused query stops, its rows stay. Before, Save failed with "running on the main connection: wait for it to finish".
- Table tabs: ⌘S (Ctrl+S on Windows and Linux) with a cell still being edited saves that cell too, right away. Before, the cell had to be closed with Enter first, or ⌘S saved nothing.
- Tabs: the × of a tab with a short title (SQL 3, a short table name) sits at the right edge of the tab, like on the longer ones. Before, it followed the title and left an empty stretch after it.
- Security section of a connection: the TLS modes are listed in words, each with one line on what it protects and a padlock (none, "if possible", required, verified against the CA, verified with the host name — recommended), the technical name next to it. A new production profile starts from "Verify certificate and host name", and the warning on a weaker choice names it and offers the fix. The root CA has "Browse…" and, once chosen, shows the certificate's subject, issuer and validity, or why the file is wrong (missing, not a PEM certificate). "Test connection" and the connection page now show what was really negotiated: TLS version, cipher, whether the server was verified, its certificate and expiry; a datum the engine cannot give reads "not available" (MySQL and MariaDB do not expose the certificate's subject). Before, the section was a list of codes (prefer, require, verify-full), the CA a bare path, and after connecting only "verified / not verified" was known.
- SSH profiles have their own window (from the SSH tab of a connection, "Edit…" and "Manage…", or "SSH profiles…" in the command palette): the list on the left with New, Duplicate and Delete, the bastion's details on the right, how many connections use it, the host key confirmed and "Test tunnel", which logs into the bastion without opening a database. Delete asks for confirmation and says which connections go back to a direct connection. The SSH tab of a connection now only picks a profile. Before, the bastion was edited inside the connection and deleted with a two-click button, without saying what it changed elsewhere.

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

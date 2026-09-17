# Server coordination

`sekai` never touches the server process. Pausing writes around a backup
belongs to the caller or the administrator — never to the tool:

1. `save-off` (stop the server from writing region files),
2. `save-all` (flush pending writes to disk),
3. run `sekai backup`,
4. `save-on` (resume).

Backing up a world that is being written concurrently can capture torn
sectors. Reads tolerate trailing partial sectors the way vanilla does,
but genuinely corrupt runs fail loudly instead of being backed up
silently. Rollback and export read only from the store, so they need no
server coordination beyond stopping the server before you let players
back in — rollback overwrites live region files.

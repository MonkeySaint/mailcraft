# MailCraft control protocol

The running app can be driven over a local socket: tests, scripts and agents run commands, query state,
press keys, resize the window and take screenshots. Implementation: `crates/ui-egui/src/control.rs`.

## Enabling it

- The listener binds **127.0.0.1 only**, on the port given by `--control-port N` or the environment variable
  `MAILCRAFT_CONTROL_PORT` (the shell stores it in `UiState::control_port` and calls `control::frame` each
  frame). Port `0` picks a free port (useful in tests; `Control::port()` reports it).
- Without a port the channel is off. If the port cannot be bound the app logs an error and runs without it.
- There is no authentication: any local process can connect. Do not enable it on shared machines.

## Framing

- One TCP connection carries any number of requests. Each request is **one line of JSON** (a single object,
  UTF-8, terminated by `\n`); each gets exactly **one response line**, in order.
- Lines longer than 1 MiB are rejected with an error response (the connection stays open).
- Several connections may be open at once; requests are executed one at a time on the UI thread, between
  frames. A response that the UI does not produce within 30 s becomes a timeout error.

## Requests and responses

Every request has `cmd` and may carry an `id` (any JSON value), echoed back in the response:

```json
{"id": 1, "cmd": "run", "command": "mail.reply", "params": {}}
```

Responses:

```json
{"id": 1, "ok": true, "result": {"compose": 3}}
{"id": 1, "ok": false, "error": "not available: select a message first"}
```

A line that is not JSON, or not an object, gets `{"id": null, "ok": false, "error": "…"}`.

### `ping`

`{"cmd": "ping"}` → `{"pong": true}`.

### `run` — run a command

`{"cmd": "run", "command": "<command id>", "params": {…}}` runs a registry command through `App::run`,
exactly as a ribbon button or menu entry would (undo, journaling and enabled checks included). `params`
defaults to `{}`. The result is the command's own JSON result; failures return the command's error text.
Command ids, parameters and enabled state: `{"cmd": "query", "what": "commands"}`.

### `query` — read state

`{"cmd": "query", "what": "<name>", …}`:

| `what` | Result |
|---|---|
| `list_rows` | Message list rows (`queries::list_rows`): `{"Group": {label, count, collapsed}}` or `{"Item": {id, who, subject, time, preview, unread, selected, …}}` |
| `folder_rows` | Folder pane rows (`queries::folder_rows`) |
| `status` | `{items, unread, module, folder, folderName, view, undo, redo}` — status bar counts, current module/folder, the whole `ViewState`, undo/redo depth |
| `selection` | `{folder, items, focus}` |
| `composes` | Open compose windows / inline responses: `[{id, title, kind, inline, to, cc, bcc, subject, dirty, source, draft, attachments}]` |
| `compose` | One compose state in full; needs `"compose": id` |
| `reading_header` | Reading pane header of `"item": id` (default: the focused item) |
| `ui` | Transient UI state: `{ribbonTab, backstage, dialogs, readWindows, searchActive, searchText, singlePaneReading, toast, keytips, calendarView}` |
| `commands` | Every registry command: `[{id, label, location, shortcut, params, enabled}]` |

### `key` — press keys

`{"cmd": "key", "keys": "Mod+R"}` applies keys through the same bindings as the keyboard (`keys.rs`), as if
pressed in the window with no text field focused. Optional `"surface"`: `"main"` (default),
`{"compose": id}` or `{"read": itemId}`.

- Chords use the portable form: `Mod` (Ctrl, ⌘ on macOS), `Shift`, `Alt`, then a key: letters, digits,
  `F1`…`F12`, `Delete`, `Insert`, `Enter`, `Escape`, `Up`/`Down`/`Left`/`Right`, `Home`, `End`, `=`, `.`,
  `,`, and the shifted forms `>`, `<`, `*`, `+` (e.g. `Mod+Shift+>`).
- Several keys are separated by spaces and applied in order: `"Alt H R P"` shows keytips, picks the Home tab,
  then runs the control with keytip `R P`. A bare `Alt` toggles keytip mode; while it is on, tokens are keytip
  letters (`Escape` steps back a level).
- The result describes what happened, e.g. `{"action": "run", "command": "mail.reply", "ok": true, "result":
  {…}}`, `{"action": "escape", "closed": "dialog"}`, `{"key": "Mod+Alt+Q", "action": null}` (unbound), or an
  array for several keys.

### `resize`

`{"cmd": "resize", "w": 1280, "h": 800}` asks the window for that inner size (logical points, 1–16384).
Result: `{"w": …, "h": …}`. The resize happens on a later frame.

### `screenshot`

`{"cmd": "screenshot", "path": "/tmp/shot.png"}` captures the main window on the next frame the integration
renders and writes it as PNG. Result: `{"path": …, "w": …, "h": …}` (without `path`, only the size). Needs a
real window: in headless test contexts the request waits and times out.

## Example

```sh
exec 3<>/dev/tcp/127.0.0.1/7341
echo '{"id":1,"cmd":"run","command":"nav.selectAll","params":{}}' >&3; read -r line <&3; echo "$line"
echo '{"id":2,"cmd":"key","keys":"Mod+Q"}' >&3; read -r line <&3; echo "$line"
echo '{"id":3,"cmd":"query","what":"status"}' >&3; read -r line <&3; echo "$line"
```

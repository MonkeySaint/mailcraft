# MailCraft MCP server

`mailcraft-cli mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io) server on stdin/stdout,
so an agent (Claude Code, Claude Desktop, any MCP client) can read and drive MailCraft. It goes through the
same command registry as the ribbon, menus and shortcuts: whatever a person can click, an agent can run by id.

Implementation: `crates/mcp` (layer L5). Transport: JSON-RPC 2.0, one message per line, UTF-8 (the MCP stdio
transport). Protocol revisions: `2025-06-18` (preferred), `2025-03-26`, `2024-11-05`.

## Running it

### Headless

The server owns its own session; no window is involved.

```sh
mailcraft-cli mcp                       # the built-in sample mailbox (synthetic mail, nothing saved)
mailcraft-cli mcp --sample              # same, explicitly
mailcraft-cli mcp --profile ~/mail/test # a MailCraft data folder (created when missing); changes are saved
```

Use the sample mailbox to try things. With `--profile`, commands change real data the same way the app would
(`app.undo` undoes the last change).

### Bridged to a running app

Start the app with its control channel, then point the server at it:

```sh
mailcraft --sample --control-port 7982
mailcraft-cli mcp --connect 127.0.0.1:7982
```

Tool calls then act on the open window: the person watching sees folders change, messages get selected and
compose windows open. The control channel listens on 127.0.0.1 only and has no authentication
(`docs/control-protocol.md`); don't enable it on a shared machine.

Differences in bridged mode:
- `list_folders` with `all: true` lists only folders visible in the folder pane (collapsed subfolders are
  missing); folder ids still work everywhere.
- `read_message` returns `body: null` with a note when the app does not answer the control query `body`.

## Client configuration

Claude Code (project `.mcp.json`, or `claude mcp add mailcraft -- mailcraft-cli mcp --sample`):

```json
{
  "mcpServers": {
    "mailcraft": {
      "command": "mailcraft-cli",
      "args": ["mcp", "--sample"]
    }
  }
}
```

Bridged to a running app:

```json
{
  "mcpServers": {
    "mailcraft": {
      "command": "mailcraft-cli",
      "args": ["mcp", "--connect", "127.0.0.1:7982"]
    }
  }
}
```

From a source checkout, use `"command": "cargo", "args": ["run", "-q", "-p", "mailcraft-cli", "--", "mcp",
"--sample"]` (slower to start; `cargo build -p mailcraft-cli` first). Claude Desktop and other clients take
the same `command`/`args` pair in their own config files.

## Tools

Every tool returns its result as JSON in one text content block. A failed call (unknown command, bad
parameters, disabled command, missing message) returns `isError: true` with the reason as text, so the model
can correct itself; protocol mistakes (unknown tool, malformed request) are JSON-RPC errors.

| Tool | Arguments | Result |
|---|---|---|
| `list_commands` | `filter?`: string (substring of id, label or location), `enabled_only?`: bool | `{count, commands: [{id, label, location, shortcut, params, enabled}]}` |
| `run_command` | `id`: string, `params?`: object (default `{}`) | `{command, result, notices?}`: the command's own result; `notices` lists dialogs and info bars it asked the UI to show (headless only) |
| `list_folders` | `all?`: bool | folder pane rows `{rows: [{id, kind, name, depth, badge, selected, …}]}`; with `all`, `{count, folders: [{id, name, path, kind}]}` |
| `list_messages` | `folder?`: id or name/path, `offset?`, `limit?` (1–1000, default 100) | `{folder, folderName, search, total_rows, items, offset, next_offset, rows}`; rows are `{type: "group", label, count, collapsed}` or `{type: "item", id, who, subject, time, preview, unread, flagged, has_attachments, categories, …}` |
| `read_message` | `id`: number, `max_chars?` (default 20000) | `{id, header: {subject, from, to, cc, date, info, attachments, categories}, body, truncated}` |
| `select` | `folder?`: id or name/path, `item?`: number | `{folder, items, focus}` |
| `search` | `query`: string (`""` clears), `limit?` | `{search: {query, scope, results}, rows, …}` as in `list_messages` |
| `status` | none | `{items, unread, module, folder, folderName, view, undo, redo}` |
| `parity` | none | `{total_rows, not_applicable, mapped_rows, implemented_rows, percent, by_area, missing_ids}` |

Notes:
- Folder names are matched case-insensitively against paths first (`Inbox/Projects` or `Inbox\Projects`),
  then names; mail folders win over calendars or contact folders with the same name, the default account over
  others. A number is always a folder id.
- `list_messages` with `folder` and `select` change the selection, as clicking would. `read_message` does not
  mark the message read; run `mail.markRead` for that.
- Most commands act on the selection when they get no `ids`/`folder` params: `select` first, then
  `run_command`. Each command's `params` text (from `list_commands`) describes what it accepts.

## Limits and failure behaviour

- Request lines over 4 MiB get an "Invalid Request" error and are skipped; the server keeps reading.
- Lines that are not JSON get a "Parse error" (id `null`); notifications (no `id`) get no response; batches
  (JSON arrays) are answered with an array.
- Nothing a client sends can crash the server: commands run inside the engine's panic guard, and tool
  arguments are validated before use.
- Bridged requests time out after 35 s (the app gives up after 30 s itself). A dropped connection is
  re-established on the next call.

## Example exchange

Client → server (one line each):

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"example","version":"1.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_messages","arguments":{"folder":"Inbox","limit":2}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"select","arguments":{"item":23}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"run_command","arguments":{"id":"mail.flag","params":{"kind":"today"}}}}
```

Server → client (sample mailbox; text content shortened):

```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"mailcraft","title":"MailCraft","version":"0.1.0"},"instructions":"MailCraft mail client (headless: sample mailbox (in memory)). …"}}
{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\n  \"folder\": 1,\n  \"folderName\": \"Inbox\",\n  \"items\": 20,\n  \"next_offset\": 2,\n  \"rows\": [{\"type\": \"group\", \"label\": \"Today\", \"count\": 3, …}, {\"type\": \"item\", \"id\": 23, \"who\": \"Grace Kim\", \"subject\": \"Slides for Thursday\", \"time\": \"11:45 AM\", \"unread\": true, …}],\n  \"total_rows\": 29 …}"}],"isError":false}}
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\n  \"focus\": 23,\n  \"folder\": 1,\n  \"items\": [23]\n}"}],"isError":false}}
{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"{\n  \"command\": \"mail.flag\",\n  \"result\": {\"changed\": 1, \"ids\": [23]}\n}"}],"isError":false}}
```

A command that cannot run comes back as a tool error, not a protocol error:

```json
{"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"mail.markRead: not available: No message is selected."}],"isError":true}}
```

Try it by hand:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"status","arguments":{}}}' \
  | mailcraft-cli mcp --sample
```

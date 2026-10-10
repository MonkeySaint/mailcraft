//! MailCraft MCP server (layer L5): the Model Context Protocol over stdio (JSON-RPC 2.0, one message per
//! line), so agents can read and drive MailCraft through the same command registry as the ribbon.
//!
//! Two backends:
//! - [`HeadlessBackend`] owns a [`mailcraft_engine::Session`]: the sample mailbox, or a profile folder.
//! - [`BridgedBackend`] drives a running app over its control channel (`--control-port N`,
//!   `docs/control-protocol.md`), so an agent and a person share one window.
//!
//! Tools: `list_commands`, `run_command`, `list_folders`, `list_messages`, `read_message`, `select`, `search`,
//! `status`, `parity` (see `docs/mcp.md`). Nothing here panics on bad input: malformed lines get JSON-RPC
//! errors, bad tool arguments get `isError` results, and lines over [`MAX_REQUEST_BYTES`] are skipped.

mod backend;
mod protocol;
pub mod tools;

pub use backend::{
    Backend, BridgedBackend, FolderInfo, HeadlessBackend, MAX_RESPONSE_BYTES, folders_from_rows,
    resolve_folder, session_folders, session_query,
};
pub use protocol::{
    LineRead, MAX_REQUEST_BYTES, PROTOCOL_VERSION, RpcError, SUPPORTED_VERSIONS, Server, code,
    read_line_capped, serve, serve_stdio,
};

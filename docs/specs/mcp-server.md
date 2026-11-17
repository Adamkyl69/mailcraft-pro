# Design: MailCraft as a local MCP server

Status: **approved by the owner**; being built.

## Goal

Local AI apps (Claude Desktop, Claude Code, any MCP client on this computer) can read, search
and organize mail in the MailCraft that's running, and send mail with the owner's approval,
without compromising security:

- no network port, nothing reachable from another computer
- a token per app, created in Settings, with its own permissions, and revocable
- a send outside the safe rules needs a 6-digit code from the owner's authenticator app

**Out of scope:** remote access (claude.ai in the cloud, tunnels), HTTP transport, the browser
app (it has no socket), and MCP resources and prompts (only tools are provided).

## How it fits together

```
AI app ──stdin/stdout──▶ mailcraft-cli mcp ──Unix socket (key)──▶ MailCraft app ──▶ Session (engine)
        newline JSON-RPC  (the bridge, holds no data)  one line each way  (owns the data)  pure, no I/O
```

1. **The AI app launches `mailcraft-cli mcp`.** Its config passes the token in `MAILCRAFT_MCP_TOKEN`.
2. **The bridge relays each JSON-RPC line** to the running app over the single-instance socket
   (`apps/mailcraft/src/startup/single_instance.rs`). That socket is already in a folder only the
   user can reach and requires the per-run random key from `.key`. The request is a new kind:
   `{"key": …, "mcp": {"token": …, "request": {…}}}`, and the answer is one JSON line.
3. **The app hands it to the window's thread**, the way handed-over `mailto:` links are handled
   today (`request_repaint`). That thread runs it on the `Session` and replies.
4. **If MailCraft isn't running,** the bridge answers with an error saying so. It never opens the
   data folder itself, because the running app owns the cache (`store/src/packed.rs`).

There are two independent locks. The socket key proves the request comes from the same user on
the same computer. The MCP token proves which app is asking and what it's allowed to do. Both
are required.

## Where the code goes

| Piece | Place | Notes |
|---|---|---|
| JSON-RPC and MCP protocol, tool table, permission check, send rules, TOTP check | `crates/engine/src/mcp.rs` + `mcp/` (`protocol.rs`, `tools.rs`, `rules.rs`, `approval.rs`, `tests.rs`) | Pure and wasm-safe. The time is passed in; the engine never reads the clock. |
| Socket request kind, hand-off to the window thread | `apps/mailcraft/src/startup/single_instance.rs` (+ `single_instance/mcp.rs`) | |
| Socket naming, shared with the CLI | Moved out of `single_instance.rs` to a place both apps reach | A move commit of its own |
| The bridge | `apps/mailcraft-cli/src/commands/mcp.rs`, `mcp` in `help.txt` | |
| Settings card | `crates/ui-egui/src/panes/backstage/options/connected.rs` | Strings in every i18n catalog |
| QR code | `qrcodegen` crate (MIT, on the allowlist), drawn as egui rectangles | Checked with `cargo deny` |
| Secrets | TOTP secret in the system keychain (`Credentials`, extended with an app-level secret not tied to an account) | Token hashes go in the settings, as SHA-256 of 256 random bits |

Everything is a command, so the Settings card calls `mcp.setEnabled`, `mcp.createToken`,
`mcp.revokeToken`, `mcp.setupApproval`, `mcp.confirmApproval`, `mcp.resetApproval`,
`mcp.unlockApproval` and `mcp.setRules`. **None of them is ever reachable through `mail_run`.**

## Protocol

- **Version:** MCP `2025-06-18`, plus whatever older versions Claude Desktop and Claude Code still
  negotiate. **Methods:** `initialize`, `notifications/initialized`, `ping`, `tools/list` and
  `tools/call`. Anything else returns JSON-RPC "method not found".
- **Hostile input:** a line is at most 1 MiB, JSON nesting at most 64 deep, and every string and
  list parameter is capped. Bad input gets a JSON-RPC error and never a panic.
- **Throttling:** one call at a time per bridge, and at most 10 calls a second per token (beyond
  that: a "slow down" error).

## Tokens

- **Format:** `mcp_` followed by 32 bytes from the system's secure random source, in base64url.
  The token is shown **once** at creation, together with config snippets for Claude Desktop
  (`claude_desktop_config.json`) and Claude Code
  (`claude mcp add mailcraft --env MAILCRAFT_MCP_TOKEN=… -- mailcraft-cli mcp`).
- **What's stored:** the name, SHA-256 of the token, the permissions, the creation time and the
  last-use time. The token itself is never stored or logged.
- **Revoking** takes effect on the next call. Turning MCP off rejects every call. MCP is off until
  the first token is created.

## Permissions and tools

| Tool | Permission | What it does |
|---|---|---|
| `mail_accounts` | read | Accounts, their folders and unread counts |
| `mail_list` | read | Messages in a folder: key, from, to, subject, date, flags, has attachments (limit at most 200, with an offset) |
| `mail_search` | read | The app's search syntax (`from:`, `subject:`, `has:attachment`…), limit at most 200 |
| `mail_read` | read | Text body (HTML converted to text, never remote content) and an attachment list. With `headers: true`: the raw header block (capped at 64 KiB), the `Authentication-Results` lines (SPF, DKIM, DMARC) and MailCraft's own look-alike sender check |
| `mail_commands` | read | The commands **this token** may run, with labels and their params docs |
| `monitor_log` | read | WARN and ERROR lines and panic reports from this run's log; the last N (at most 500), optionally since a time |
| `mail_sync` | organize | Sync all accounts or one. Waits up to 120 s, then reports what's done and what's still running |
| `mail_run` | organize | Runs one command from the **allowlist** |
| `mail_send` | send | See *Sending* |

**The `mail_run` allowlist** denies everything by default. Only commands that are easy to undo
are on it: mark read/unread, flag/unflag, categorize, pin, move to a folder, snooze (the exact
ids are picked from `crates/engine/src/cmd/*.rs`). **Never** on it: delete, purge, empty folder,
rules and forwarding (a forwarding rule would send mail out), accounts, options, signatures,
export, backup, data files, keys and certificates, `ui.*` and `mcp.*`.

## Sending

`mail_send` takes `to`, `cc`, `subject`, `body`, `attachments` (file paths), `from`
(an account or one of its addresses), `in_reply_to` (a message key), and later `pending_id` with
`approval_code`. It returns one of three outcomes:

1. **Sent**, when the message passes **every** *no code needed* rule:
   - it's a reply (`in_reply_to`) in a thread the owner has written in, from the account that
     received it
   - every recipient is someone the owner has sent mail to before, or is in their contacts (so
     the person who wrote a malicious email can't receive an automatic reply that leaks mail)
   - it has no attachments
   - MailCraft's own send checks raise nothing (look-alike address, many people shown each other)
   - it's under the daily limit (default 20)
2. **Needs a code:** the message is held as a pending send for 10 minutes and the call returns
   its `pending_id` and a MailCraft-written summary (recipients, subject, size, attachments,
   any warnings). Calling again with that `pending_id` and a valid code sends **that exact
   message**. Pending sends also appear in the app with **Send** and **Discard**, so approving at
   the desktop still works.
3. **Refused:** more than 10 recipients, approval not set up and the message needs a code, sending
   locked, or the permission is missing.

**Authenticator codes (RFC 6238):** SHA-1, 6 digits, a 30 s step, one step either way accepted.
- **Setup:** File › Options shows the QR code (`otpauth://totp/MailCraft?secret=…&issuer=MailCraft`)
  and the secret as text. Approval only becomes active once a code from the phone is entered and
  checks out.
- **No reuse:** a step that was used once is never accepted again.
- **Lockout:** 5 wrong codes in a row lock sending until **Unlock** is clicked in the app. Every
  approval, failure and lockout is logged.
- **A lost phone:** **Reset** in the app makes a new secret, and the old one stops working.

The owner can turn the *no code needed* rules off, so every send needs a code, and change the
daily limit.

## Logging and privacy

Log target `mailcraft::mcp`: each call's token **name**, tool and outcome, and how long it took.
Never a token, a code, a message's content or an address.

## Tests

- **Protocol:** JSON-RPC parsing (valid, malformed, oversized, too deeply nested), version
  negotiation, unknown methods.
- **Tools:** every tool against the sample mailbox. Junk params through every tool (alongside
  `every_command_survives_junk_params`).
- **Permissions:** every tool × read/organize/send; a revoked token; MCP off; every denied command
  through `mail_run`.
- **Sending:** each rule on its own; a reply to a stranger needs a code; the code works only for
  that `pending_id`; a pending send expires.
- **TOTP:** RFC 6238 Appendix B test values; ±1 step; a replayed step refused; lockout after 5 and
  unlock.
- **Socket:** a key-less request refused; an MCP request round trip; the bridge's answer when the
  app isn't running.
- **End to end:** a scripted MCP client (initialize → tools/list → tools/call) through
  `mailcraft-cli mcp` to a running app on `--sample`.
- **Settings card:** a headless frame in `crates/ui-egui/src/tests.rs`.
- **Fuzz:** a target for the MCP request parser in `fuzz/`.

**Can't be tested here:** Claude Desktop itself, Windows and macOS sockets, and their keychains.

## Commits, in order

Each commit passes `cargo xtask ci`.

1. Move socket naming to a shared place (moves only).
2. Engine: protocol, tools and permissions, with tests and the fuzz target.
3. Engine: send rules and the TOTP approval, with tests.
4. App socket request kind and the `mailcraft-cli mcp` bridge, with tests.
5. Settings card, the QR code (adds `qrcodegen`), and translations.
6. Docs: a README section, the `CHANGELOG.md` line, `docs/architecture.md`.

## Owner decisions taken

Local clients only; stdio, no port; authenticator code plus rules; the QR crate is added;
the *no code needed* rules for replies stay on by default.

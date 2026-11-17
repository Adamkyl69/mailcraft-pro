<h1 align="center">MailCraft</h1>

<p align="center">
  <b>Local-first email: an open-source, clean-room alternative to Microsoft Outlook, written in Rust.</b><br>
  <i>By Zinadin Zidan and the MailCraft contributors.</i>
</p>

<p align="center">
  <a href="https://github.com/ZIDAN44/mailcraft/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/ZIDAN44/mailcraft/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-0e7c86?style=flat-square">
</p>

<p align="center">
  <a href="https://mailcraft.dev">mailcraft.dev</a> · <a href="https://mailcraft.dev/#download">Download</a> · <a href="CHANGELOG.md">Changelog</a> · <a href="ROADMAP.md">Roadmap</a>
</p>

<p align="center">
  <img src="docs/images/reading.png" alt="MailCraft with the sample mailbox: folder pane, ribbon, the Inbox grouped by date and the reading pane" width="100%">
</p>

<table>
<tr>
<td width="50%"><img src="docs/images/search-dark.png" alt="Dark mode with a search across all mailboxes" width="100%"></td>
<td width="50%"><img src="docs/images/compose.png" alt="Writing a reply in the reading pane" width="100%"></td>
</tr>
</table>

MailCraft gives you the everyday Outlook mail workflow with your own mail servers. It keeps a copy
of your mail on your computer, so it works offline.

## Features

- **Familiar:** three panes, a ribbon and the usual Outlook shortcuts. Drag messages onto folders.
- **Accounts:** IMAP or POP with SMTP, JMAP, and Exchange Online (Graph) or on-premises (EWS). Sign in with Gmail or Microsoft 365, see *All Inboxes*, and open or export `.pst` files.
- **Works offline:** every folder is kept on disk and synced in the background. Changes you make offline go to the server when you reconnect.
- **Reading and writing:** conversations, formatted HTML, picture thumbnails, rich-text replies, signatures, drafts, an Outbox, **Print** and **Save as PDF**.
- **Organising:** archive, flag, pin, snooze, categories, Focused and Other, rules, and Undo.
- **Search:** works offline, with `from:`, `subject:`, `has:attachment`, `is:unread` and more.
- **Calendar and people:** day, week and month views, meeting invitations, tasks, a To-Do Bar beside the mail, an address book, and CalDAV/CardDAV, with a mail account or on their own, from any such server (Nextcloud, Baïkal, SOGo, Radicale and others).
- **Private and secure:** MailCraft talks only to your servers, always over TLS. Passwords stay in the system keychain. Remote pictures load only when you ask. S/MIME and OpenPGP sign and encrypt, and the message you're writing says whether everyone in it can receive it encrypted.
- **Everywhere:** Windows, macOS, Linux and the browser (JMAP accounts only), in 16 interface languages.
- **Scriptable:** every action is a command with an id. The ribbon, shortcuts, the CLI and `--cmd` all run the same commands.

## Install

Download an installer from [mailcraft.dev](https://mailcraft.dev/#download), which offers the file for your system, or from the [Releases](https://github.com/ZIDAN44/mailcraft/releases) page:

- **Windows:** `.msi`, or a portable zip.
- **macOS:** `.dmg`.
- **Linux:** AppImage, `.deb`, `.rpm`, Flatpak or `.tar.gz`.
- **FreeBSD:** a tarball.

To run MailCraft portably, put a `portable.txt` file beside the app. It then keeps its data in a `MailCraftData` folder next to it.

## Build and run

```bash
cargo run --release -p mailcraft -- --sample   # local sample mailbox; drop --sample to add an account
cargo run -p mailcraft-cli -- commands         # CLI: add, sync, list, read, search, send, run
cd apps/mailcraft-web && trunk serve           # browser: http://127.0.0.1:8774/?sample
```

- The CLI reads your password from `MAILCRAFT_PASSWORD`, or asks for it. It never stores it.
- MailCraft keeps its data in `%APPDATA%\MailCraft` (Windows), `~/Library/Application Support/MailCraft` (macOS) or `~/.local/share/mailcraft` (Linux). To use another folder, pass `--data DIR` or set `MAILCRAFT_DATA`.
- To put the web build on your own site, see [hosting the web build](packaging/web/README.md).

## Use MailCraft from an AI app

MailCraft can answer AI apps on your computer, such as Claude Desktop and Claude Code, through a local [MCP](https://modelcontextprotocol.io) server. It works only while MailCraft is running, and nothing is reachable from another computer: the app starts `mailcraft-cli mcp`, which passes each request to the running MailCraft over its private local socket. It isn't offered in the browser build.

1. In MailCraft, open **File › Options › Connected apps** and make a token for the app, with the permissions it needs.
2. Copy the token (it's shown once) and paste the snippet beside it into Claude Desktop's `claude_desktop_config.json`, or run the `claude mcp add` line. Both set `MAILCRAFT_MCP_TOKEN` for the bridge.
3. Turn on **Allow connected apps**. Revoke a token from the same card at any time.

A token has up to three permissions. **Read** is always on; **Organize** adds the commands below that change mail, and **Send** adds `mail_send`.

| Tool | Permission | What it does |
|---|---|---|
| `mail_accounts` | Read | Accounts, folders and counts |
| `mail_list` | Read | Messages in a folder |
| `mail_search` | Read | Search all mail, as the search box does |
| `mail_read` | Read | A message's text, without marking it read or loading pictures |
| `mail_commands` | Read | The commands `mail_run` accepts |
| `monitor_log` | Read | MailCraft's warnings and errors from this run |
| `mail_sync` | Organize | Fetch new mail now |
| `mail_run` | Organize | Mark read or unread, flag, categorize, pin, move (not to Trash or Junk), snooze; each can be undone |
| `mail_send` | Send | Send a new message or a reply |

**Sending.** A reply to people you've written to, in a conversation you've written in, with no attachments, goes at once, within a daily limit; you can turn this off or change the limit in the card. Anything else is held. The app asks you for a 6-digit code from an authenticator app (set up in the card by scanning a QR code), or you send or discard the held message in the card. Five wrong codes lock sending until you press **Unlock**; **Reset** sets the authenticator up again after a lost phone.

**What this protects against.** The protection is for an AI being tricked by the mail it reads: it can only do what its token allows, and it can't send outside the rules without a code you hold. An AI app that can run shell commands as you can get around any protection on your own computer, since it can read MailCraft's files as you can. Prefer an app without a shell, or keep its shell commands behind its permission prompts.

## Documentation

- [Setting up your email account](https://mailcraft.dev/setup)
- [Switching from Outlook](https://mailcraft.dev/switch-from-outlook)
- [Opening a `.pst` file without Outlook](https://mailcraft.dev/open-pst-without-outlook)
- [Running MailCraft from a USB stick](https://mailcraft.dev/portable)
- [What each version added](CHANGELOG.md)
- [What comes next](ROADMAP.md)
- [What's still missing](docs/gaps.md)
- [How MailCraft is built](docs/architecture.md)
- [How sync works](docs/sync.md)
- [How test builds update themselves](docs/updates.md)
- [Microsoft and Google sign-in](docs/oauth.md)
- [Hosting the web build](packaging/web/README.md)

## Contribute

- Start with [how MailCraft is built](docs/architecture.md).
- Follow the project rules in [CLAUDE.md](CLAUDE.md): no crashes, no Microsoft assets, clean room.
- Run `cargo xtask ci` before every commit.
- To fuzz the parsers, see [fuzzing](fuzz/README.md).
- The installer scripts live in [`packaging/`](packaging). The `Release` workflow (`release.yml`) runs them and publishes a release.

## License and credits

MailCraft is dual-licensed under MIT OR Apache-2.0.

It is built on [`mail-parser`](https://github.com/stalwartlabs/mail-parser), [`mail-builder`](https://github.com/stalwartlabs/mail-builder), [`imap-proto`](https://github.com/djc/tokio-imap), [`lettre`](https://github.com/lettre/lettre), [`rustls`](https://github.com/rustls/rustls), [`html2text`](https://github.com/jugglerchris/rust-html2text), [`hayro`](https://github.com/LaurenzV/hayro) and [`egui`](https://github.com/emilk/egui). `.pst` files go through MailCraft's own crate, written from Microsoft's published specification.

Microsoft Outlook is a trademark of Microsoft. MailCraft is not affiliated with Microsoft and uses none of its code, icons or assets.

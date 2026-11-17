//! The tools MCP clients are offered: their names, schemas and permissions, the commands
//! `mail_run` may run, and the checks every argument passes before anything runs.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::{MAX_LIST, MAX_LOG, MAX_STRING, Permission};

/// The commands `mail_run` may run: easy to undo, and about the messages given. Everything else
/// is denied (`mail_run` also narrows `mail.move` and `mail.categorize`).
pub const RUNNABLE: [&str; 9] =
    ["mail.markRead", "mail.markUnread", "mail.toggleRead", "mail.flag", "mail.categorize", "mail.pin", "mail.move", "mail.snooze", "mail.unsnooze"];

/// One tool.
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub permission: Permission,
    pub read_only: bool,
    pub idempotent: bool,
    input: fn() -> Value,
    output: fn() -> Value,
}

/// Every tool, in the order they're listed.
pub const TOOLS: [Tool; 8] = [
    Tool {
        name: "mail_accounts",
        title: "List accounts and folders",
        description: "The mail accounts in MailCraft, their addresses, and each one's folders with unread and total counts.",
        permission: Permission::Read,
        read_only: true,
        idempotent: true,
        input: no_arguments,
        output: accounts_output,
    },
    Tool {
        name: "mail_list",
        title: "List the messages in a folder",
        description: "Messages in one folder, newest first. Each has a `key` to give to mail_read or mail_run.",
        permission: Permission::Read,
        read_only: true,
        idempotent: true,
        input: list_input,
        output: list_output,
    },
    Tool {
        name: "mail_search",
        title: "Search all mail",
        description: "Search every account's mail as MailCraft's search box does (words, from:, to:, subject:, is:unread, dates...). Newest first.",
        permission: Permission::Read,
        read_only: true,
        idempotent: true,
        input: search_input,
        output: search_output,
    },
    Tool {
        name: "mail_read",
        title: "Read a message",
        description: "A message's text, its sender, recipients and attachment names. Reading it here doesn't mark it read, and no pictures are fetched.",
        permission: Permission::Read,
        read_only: true,
        idempotent: true,
        input: read_input,
        output: read_output,
    },
    Tool {
        name: "mail_commands",
        title: "List the commands mail_run may run",
        description: "The commands mail_run accepts, with what parameters each takes (none when this token may not organize).",
        permission: Permission::Read,
        read_only: true,
        idempotent: true,
        input: no_arguments,
        output: commands_output,
    },
    Tool {
        name: "monitor_log",
        title: "Read MailCraft's warnings and errors",
        description: "The warnings and errors MailCraft logged during this run, oldest first.",
        permission: Permission::Read,
        read_only: true,
        idempotent: true,
        input: log_input,
        output: log_output,
    },
    Tool {
        name: "mail_sync",
        title: "Fetch new mail",
        description: "Ask the servers for new mail now (one account, or all that check on their own) and wait until they've answered.",
        permission: Permission::Organize,
        read_only: false,
        idempotent: true,
        input: sync_input,
        output: sync_output,
    },
    Tool {
        name: "mail_run",
        title: "Organize messages",
        description: "Run one of the commands from mail_commands on messages of one folder: mark read or unread, flag, categorize, pin, move (not to Trash or Junk), snooze. Each can be undone in MailCraft.",
        permission: Permission::Organize,
        read_only: false,
        idempotent: false,
        input: run_input,
        output: run_output,
    },
];

/// The tool called `name`.
pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

/// The `tools/list` entries a token with `perms` is offered. `structured`: the protocol version
/// has output schemas (2025-06-18).
pub fn listing(perms: &BTreeSet<Permission>, structured: bool) -> Value {
    Value::Array(
        TOOLS
            .iter()
            .filter(|t| perms.contains(&t.permission))
            .map(|t| {
                let mut entry = json!({
                    "name": t.name,
                    "title": t.title,
                    "description": t.description,
                    "inputSchema": (t.input)(),
                    "annotations": {
                        "title": t.title,
                        "readOnlyHint": t.read_only,
                        "destructiveHint": false,
                        "idempotentHint": t.idempotent,
                        "openWorldHint": false,
                    },
                });
                if structured {
                    entry["outputSchema"] = (t.output)();
                }
                entry
            })
            .collect(),
    )
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn no_arguments() -> Value {
    schema(json!({}), &[])
}

fn list_input() -> Value {
    schema(
        json!({
            "account": {"type": "string", "description": "An account id from mail_accounts."},
            "folder": {"type": "string", "description": "A folder path from mail_accounts."},
            "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIST, "default": 50},
            "offset": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0},
            "unread_only": {"type": "boolean"},
        }),
        &["account", "folder"],
    )
}

fn search_input() -> Value {
    schema(
        json!({
            "query": {"type": "string", "maxLength": 1024},
            "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIST, "default": 50},
        }),
        &["query"],
    )
}

fn read_input() -> Value {
    schema(
        json!({
            "key": {"type": "string", "description": "A message key from mail_list or mail_search."},
            "headers": {"type": "boolean", "description": "Also the raw header block and the sender checks."},
        }),
        &["key"],
    )
}

fn log_input() -> Value {
    schema(
        json!({
            "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LOG, "default": 100},
            "since": {"type": "integer", "description": "Unix seconds: only lines from then on."},
            "errors_only": {"type": "boolean"},
        }),
        &[],
    )
}

fn sync_input() -> Value {
    schema(json!({"account": {"type": "string", "description": "One account id; else every account that checks on its own."}}), &[])
}

fn run_input() -> Value {
    schema(
        json!({
            "command": {"type": "string", "enum": RUNNABLE},
            "keys": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 100, "description": "Keys of messages of one folder."},
            "params": {"type": "object", "description": "The command's parameters (see mail_commands)."},
        }),
        &["command", "keys"],
    )
}

fn output(properties: Value) -> Value {
    json!({"type": "object", "properties": properties})
}

fn accounts_output() -> Value {
    output(json!({"accounts": {"type": "array", "items": {"type": "object"}}}))
}

fn list_output() -> Value {
    output(
        json!({"total": {"type": "integer"}, "offset": {"type": "integer"}, "busy": {"type": "boolean"}, "messages": {"type": "array", "items": {"type": "object"}}}),
    )
}

fn search_output() -> Value {
    output(json!({"found": {"type": "integer"}, "messages": {"type": "array", "items": {"type": "object"}}}))
}

fn read_output() -> Value {
    output(json!({
        "key": {"type": "string"},
        "from": {"type": "string"},
        "to": {"type": "string"},
        "cc": {"type": "string"},
        "date": {"type": "integer"},
        "subject": {"type": "string"},
        "text": {"type": ["string", "null"]},
        "truncated": {"type": "boolean"},
        "encrypted": {"type": "boolean"},
        "attachments": {"type": "array", "items": {"type": "object"}},
        "external": {"type": "boolean"},
        "warning": {"type": ["object", "null"]},
        "headers": {"type": "object"},
    }))
}

fn commands_output() -> Value {
    output(json!({"commands": {"type": "array", "items": {"type": "object"}}}))
}

fn log_output() -> Value {
    output(json!({"lines": {"type": "array", "items": {"type": "object"}}}))
}

fn sync_output() -> Value {
    output(
        json!({"done": {"type": "array", "items": {"type": "string"}}, "running": {"type": "array", "items": {"type": "string"}}, "failed": {"type": "array", "items": {"type": "object"}}}),
    )
}

fn run_output() -> Value {
    output(json!({"result": {}}))
}

/// A tool's arguments, checked: the names are those the tool takes, and each is read with its
/// limits. Every error names the argument.
#[derive(Debug)]
pub struct Args<'a> {
    map: &'a Map<String, Value>,
}

impl<'a> Args<'a> {
    /// `v` must be an object whose names are all in `allowed`.
    pub fn new(v: &'a Value, allowed: &[&str]) -> Result<Args<'a>, String> {
        let map = v.as_object().ok_or_else(|| "the arguments must be an object".to_string())?;
        if let Some(unknown) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
            let shown: String = unknown.chars().take(40).collect();
            return Err(format!("unknown argument `{shown}`"));
        }
        Ok(Args { map })
    }

    /// The names given.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    pub fn has(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    /// A string of at most `max` bytes; `None` when not given.
    pub fn str(&self, name: &str, max: usize) -> Result<Option<&'a str>, String> {
        match self.map.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.len() <= max => Ok(Some(s)),
            Some(Value::String(_)) => Err(format!("`{name}` is too long (at most {max} bytes)")),
            Some(_) => Err(format!("`{name}` must be a string")),
        }
    }

    /// [`Args::str`] when it must be given (an empty one isn't a value).
    pub fn req_str(&self, name: &str, max: usize) -> Result<&'a str, String> {
        self.str(name, max)?.ok_or_else(|| format!("`{name}` is required"))
    }

    /// A whole number from `lo` to `hi`.
    pub fn int(&self, name: &str, lo: i64, hi: i64) -> Result<Option<i64>, String> {
        match self.map.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => match v.as_i64() {
                Some(n) if (lo..=hi).contains(&n) => Ok(Some(n)),
                Some(_) => Err(format!("`{name}` must be from {lo} to {hi}")),
                None => Err(format!("`{name}` must be a whole number")),
            },
        }
    }

    pub fn bool(&self, name: &str) -> Result<Option<bool>, String> {
        match self.map.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(format!("`{name}` must be true or false")),
        }
    }

    /// A list of strings: from `min` to `max` of them, each of at most `MAX_STRING` bytes.
    pub fn strings(&self, name: &str, min: usize, max: usize) -> Result<Option<Vec<&'a str>>, String> {
        let Some(v) = self.map.get(name).filter(|v| !v.is_null()) else { return Ok(None) };
        let list = v.as_array().ok_or_else(|| format!("`{name}` must be a list"))?;
        if list.len() < min || list.len() > max {
            return Err(format!("`{name}` must hold from {min} to {max} items"));
        }
        list.iter()
            .map(|x| x.as_str().filter(|s| s.len() <= MAX_STRING).ok_or_else(|| format!("`{name}` must hold strings of at most {MAX_STRING} bytes")))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// An object (`params`).
    pub fn object(&self, name: &str) -> Result<Option<&'a Value>, String> {
        match self.map.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(v @ Value::Object(_)) => Ok(Some(v)),
            Some(_) => Err(format!("`{name}` must be an object")),
        }
    }
}

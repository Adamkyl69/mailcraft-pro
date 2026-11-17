//! Requests from local AI apps: JSON-RPC lines read, then each request that parses handed to a
//! session with a token that may read, organize and send, as the app's socket hands it. The
//! tools' arguments are the hostile input: every one must give an answer, never a panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use mailcraft_engine::mcp::Permission;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = mailcraft_engine::mcp::protocol::parse_line(&text);
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) {
        let mut s = mailcraft_engine::Session::blank();
        let token = mailcraft_engine::mcp::tokens::make_token(&[7u8; 32]).unwrap_or_default();
        s.mcp_seed_token(&token, &[Permission::Read, Permission::Organize, Permission::Send]);
        let _ = s.mcp_handle(&token, &v, 0);
        let _ = s.mcp_finished(1_000_000);
    }
});

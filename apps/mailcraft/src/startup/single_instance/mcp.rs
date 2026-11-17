//! MCP calls on the single-instance socket: a local AI app's request, relayed by
//! `mailcraft-cli mcp`, answered on one line (`docs/specs/mcp-server-build.md`).
//!
//! Two threads are never made to wait for each other. The socket's accept loop hands each MCP
//! connection to a thread of its own ([`hand_to_thread`]: at most [`MAX_IN_FLIGHT`] at once, so
//! a launch is never kept waiting behind a call), which queues the call for the window and waits
//! for its answer ([`serve_one`]). The window's thread ([`Desk::serve`], every frame) runs the
//! calls on the session and hands each answer back with `try_send`: it never waits on a socket,
//! and a call it finds past its deadline is dropped without being run. The socket's thread never
//! touches the session.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use mailcraft_engine::mcp::protocol::{self, INTERNAL_ERROR, Id, SLOW_DOWN};
use mailcraft_engine::{McpOutcome, Session};
use mailcraft_net::instance::{TIMEOUT, UnixStream};
use serde_json::Value;

use super::{Asks, Server};

/// MCP connections being answered at once; the next is told MailCraft is busy.
pub(super) const MAX_IN_FLIGHT: usize = 4;
/// Calls waiting for the window; the next is told MailCraft is busy.
pub(super) const MAX_QUEUED: usize = 16;
/// How long a connection's thread waits for the window (a sync waits 120 s at most).
pub(super) const WAIT: Duration = Duration::from_secs(150);
/// How soon the window looks again while a call waits for the network.
const LOOK_AGAIN: Duration = Duration::from_millis(250);

const BUSY: &str = "MailCraft is busy with other calls";
const TOO_SLOW: &str = "MailCraft didn't answer in time";
const CLOSING: &str = "MailCraft is closing";

/// A call waiting for the window's thread.
pub struct Asked {
    pub token: String,
    pub request: Value,
    /// Where the answer goes (one line, no newline; `""` for a notification).
    pub reply: SyncSender<String>,
    /// When the connection's thread stops waiting: past it, the call isn't run.
    pub deadline: Instant,
}

/// Counts a connection's thread in [`Asks::in_flight`] until it ends, however it ends.
struct InFlight(Arc<Asks>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The request's id, for an answer that names it (`None` when it has none, or one that can't be
/// answered to).
fn id_of(request: &Value) -> Option<Id> {
    match request.get("id")? {
        Value::Number(n) => n.as_i64().map(Id::Num),
        Value::String(s) if s.len() <= 256 => Some(Id::Str(s.clone())),
        _ => None,
    }
}

/// One line, then a newline, with the write timeout the socket's other answers have. A reader
/// that has gone is passed over.
fn write_line(stream: &UnixStream, line: &str) {
    use std::io::Write;
    let _ = stream.set_write_timeout(Some(TIMEOUT));
    let mut text = String::with_capacity(line.len() + 1);
    text.push_str(line);
    text.push('\n');
    let mut writer = stream;
    let _ = writer.write_all(text.as_bytes());
}

/// Answer an MCP connection on a thread of its own, or tell it MailCraft is busy (too many
/// answering already, or no thread to be had). Returns at once.
pub(super) fn hand_to_thread(stream: UnixStream, asks: &Arc<Asks>, token: String, request: Value) {
    hand_to_thread_within(stream, asks, token, request, WAIT);
}

/// [`hand_to_thread`] with the wait for the window given (tests wait less than [`WAIT`]).
pub(super) fn hand_to_thread_within(stream: UnixStream, asks: &Arc<Asks>, token: String, request: Value, wait: Duration) {
    let id = id_of(&request);
    let room = asks.in_flight.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_IN_FLIGHT).then_some(n + 1)).is_ok();
    if !room {
        write_line(&stream, &protocol::error(id.as_ref(), SLOW_DOWN, BUSY));
        return;
    }
    // From here the count goes down however this ends: the guard is dropped with the thread's
    // closure, or with this frame when there is no thread.
    let guard = InFlight(Arc::clone(asks));
    let Ok(spare) = stream.try_clone() else {
        write_line(&stream, &protocol::error(id.as_ref(), SLOW_DOWN, BUSY));
        return;
    };
    let started = std::thread::Builder::new().name("mcp-call".into()).spawn(move || {
        let guard = guard;
        serve_one(stream, &guard.0, token, request, wait);
    });
    if started.is_err() {
        write_line(&spare, &protocol::error(id.as_ref(), SLOW_DOWN, BUSY));
    }
}

/// Queue one call for the window, wait up to `wait` for its answer and write it: the window's
/// line, or an error when it didn't answer in time or is closing.
pub(super) fn serve_one(stream: UnixStream, asks: &Asks, token: String, request: Value, wait: Duration) {
    let id = id_of(&request);
    let (tx, rx) = sync_channel(1);
    let queued = {
        let mut queue = asks.mcp.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if queue.len() >= MAX_QUEUED {
            false
        } else {
            queue.push_back(Asked { token, request, reply: tx, deadline: Instant::now() + wait });
            true
        }
    };
    let line = if queued {
        if let Some(ctx) = asks.wake.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
            ctx.request_repaint();
        }
        match rx.recv_timeout(wait) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => protocol::error(id.as_ref(), INTERNAL_ERROR, TOO_SLOW),
            Err(RecvTimeoutError::Disconnected) => protocol::error(id.as_ref(), INTERNAL_ERROR, CLOSING),
        }
    } else {
        protocol::error(id.as_ref(), SLOW_DOWN, BUSY)
    };
    write_line(&stream, &line);
}

/// Now in Unix milliseconds: the engine never reads the clock.
fn now_ms() -> i64 {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    i64::try_from(ms).unwrap_or(0)
}

/// The window thread's side: runs the calls that came on the session, and keeps the ones that
/// wait for the network until the session has their answer.
#[derive(Default)]
pub struct Desk {
    /// Calls the session answers later: its ticket, where the answer goes, and when the
    /// connection's thread stops waiting.
    waiting: Vec<(u64, SyncSender<String>, Instant)>,
}

impl Desk {
    /// Run what came since the last frame and hand back what finished. Called every frame, the
    /// window minimized or hidden too; never waits on a socket.
    pub fn serve(&mut self, server: Option<&Server>, session: &mut Session, ctx: &egui::Context) {
        let asked = server.map(Server::take_mcp).unwrap_or_default();
        if asked.is_empty() && self.waiting.is_empty() {
            return;
        }
        self.run(asked, session, now_ms(), Instant::now());
        // Calls that wait for the network finish while nothing else draws.
        if session.mcp_waiting() || !self.waiting.is_empty() {
            ctx.request_repaint_after(LOOK_AGAIN);
        }
    }

    /// [`Desk::serve`] at a time given: `now_ms` for the session, `now` for the deadlines.
    fn run(&mut self, asked: Vec<Asked>, session: &mut Session, now_ms: i64, now: Instant) {
        for a in asked {
            // The connection's thread has answered "too slow" already.
            if now >= a.deadline {
                continue;
            }
            match session.mcp_handle(&a.token, &a.request, now_ms) {
                // `try_send`: a connection that gave up has dropped its end, and the window never waits.
                McpOutcome::Reply(line) => {
                    let _ = a.reply.try_send(line);
                }
                McpOutcome::Later(ticket) => self.waiting.push((ticket, a.reply, a.deadline)),
            }
        }
        if self.waiting.is_empty() {
            return;
        }
        for (ticket, line) in session.mcp_finished(now_ms) {
            if let Some(i) = self.waiting.iter().position(|(t, ..)| *t == ticket) {
                let (_, reply, _) = self.waiting.swap_remove(i);
                let _ = reply.try_send(line);
            }
        }
        // Past the deadline the connection's thread has given up: the session lets go of the call.
        self.waiting.retain(|(ticket, _, deadline)| {
            let keep = now < *deadline;
            if !keep {
                session.mcp_forget(*ticket);
            }
            keep
        });
    }
}

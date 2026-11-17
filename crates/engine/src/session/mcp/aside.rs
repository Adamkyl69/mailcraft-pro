//! Running something over the mail without touching what the user sees.

use super::*;

/// What the user sees, put aside (every `Session` field that is "what's on screen": a field added
/// to the session that belongs here goes in this list too, and in the test that checks it).
struct Aside {
    view: View,
    filter: ListFilter,
    search: Option<(String, SearchScope)>,
    search_folder: Option<String>,
    server_search: Option<ServerSearch>,
    showing_more: Option<(String, String)>,
    list: Vec<SharedSummary>,
    threads: Vec<Thread>,
    nav: mailboxes::ListNav,
    selection: Vec<MessageKey>,
    focus: Option<MessageKey>,
    anchor: Option<MessageKey>,
    reading: Option<Reading>,
    focus_other: bool,
    read_due: Option<(MessageKey, i64)>,
    read_on_leave: Option<MessageKey>,
    compose: Option<Compose>,
    shown: searching::ShownSearch,
}

impl Session {
    /// Run `f` with what the user sees put aside (the view and its list, the search on screen,
    /// the selection, the message open, the message being written) and put back after, whatever
    /// `f` did. It starts from nothing shown. `changed`: `f` changed mail (flags, moves), so the
    /// user's list is built again once it's back. A panic in `f` is caught, counted and logged,
    /// and the user's state comes back all the same: the answer is then an error.
    pub(crate) fn aside<R>(&mut self, changed: bool, f: impl FnOnce(&mut Session) -> R) -> Result<R, String> {
        // The user's pending body is finished first, so the body lane holds nothing of theirs.
        self.finish_pending_body();
        let mine = Aside {
            view: std::mem::replace(&mut self.view, View::Empty),
            filter: std::mem::take(&mut self.filter),
            search: self.search.take(),
            search_folder: self.search_folder.take(),
            server_search: self.server_search.take(),
            showing_more: self.showing_more.take(),
            list: std::mem::take(&mut self.list),
            threads: std::mem::take(&mut self.threads),
            nav: std::mem::take(&mut self.nav),
            selection: std::mem::take(&mut self.selection),
            focus: self.focus.take(),
            anchor: self.anchor.take(),
            reading: self.reading.take(),
            focus_other: std::mem::take(&mut self.focus_other),
            read_due: self.read_due.take(),
            read_on_leave: self.read_on_leave.take(),
            compose: self.compose.take(),
            shown: self.search_state.take_shown(),
        };
        let deferring = std::mem::replace(&mut self.body.defer, false);
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        // Whatever `f` showed goes: its search, its list, the folders its search kept in memory.
        self.drop_search();
        self.stop_server_search();
        self.body.defer = deferring;
        let Aside {
            view,
            filter,
            search,
            search_folder,
            server_search,
            showing_more,
            list,
            threads,
            nav,
            selection,
            focus,
            anchor,
            reading,
            focus_other,
            read_due,
            read_on_leave,
            compose,
            shown,
        } = mine;
        self.view = view;
        self.filter = filter;
        self.search = search;
        self.search_folder = search_folder;
        self.server_search = server_search;
        self.showing_more = showing_more;
        self.list = list;
        self.threads = threads;
        self.nav = nav;
        self.selection = selection;
        self.focus = focus;
        self.anchor = anchor;
        self.reading = reading;
        self.focus_other = focus_other;
        self.read_due = read_due;
        self.read_on_leave = read_on_leave;
        self.compose = compose;
        self.search_state.put_shown(shown);
        if changed {
            self.refresh_list();
            self.revision = self.revision.wrapping_add(1);
        }
        ran.map_err(|_| {
            log::error!(target: "mailcraft::mcp", "an MCP call failed unexpectedly");
            PANICS_CAUGHT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            "that call failed unexpectedly".to_string()
        })
    }
}

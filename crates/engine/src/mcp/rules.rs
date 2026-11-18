//! When a send may go without a code: every condition has to hold, and each that doesn't is said.
//! Pure: the session gathers the facts (`session/mcp/facts.rs`).

use serde::Serialize;

/// What the session found out about a message an AI app wants to send.
#[derive(Clone, Debug, Default)]
pub struct RuleFacts {
    /// `SendRules::no_code_replies`.
    pub rules_on: bool,
    pub is_reply: bool,
    /// The owner has written in the conversation (a message of its own in the Sent folder).
    pub owner_wrote_in_thread: bool,
    /// It's from the account the original came to, and the address the reply chose.
    pub from_receiving_account: bool,
    /// Recipients the owner has never written to and has no saved contact for, lower-cased.
    /// Never logged.
    pub unknown_recipients: Vec<String>,
    /// Recipients of a reply who aren't already in the message it answers (its From, Reply-To, To
    /// and Cc, less the owner's own addresses), lower-cased. Never logged.
    pub outside_conversation: Vec<String>,
    pub attachments: usize,
    /// How many things `Session::send_checks` found to look at.
    pub checks: usize,
    pub sent_today: u32,
    pub daily_limit: u32,
}

/// A rule a message didn't pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "rule", rename_all = "camelCase")]
pub enum RuleMiss {
    RulesOff,
    NotAReply,
    OwnerNotInThread,
    OtherAccount,
    UnknownRecipients { count: usize },
    OutsideConversation { count: usize },
    HasAttachments,
    SendChecks,
    DailyLimit,
}

/// The rules the message missed: none means it's sent without a code.
pub fn misses(f: &RuleFacts) -> Vec<RuleMiss> {
    if !f.rules_on {
        return vec![RuleMiss::RulesOff];
    }
    let mut out = Vec::new();
    if !f.is_reply {
        out.push(RuleMiss::NotAReply);
    } else {
        if !f.owner_wrote_in_thread {
            out.push(RuleMiss::OwnerNotInThread);
        }
        if !f.from_receiving_account {
            out.push(RuleMiss::OtherAccount);
        }
    }
    if !f.unknown_recipients.is_empty() {
        out.push(RuleMiss::UnknownRecipients { count: f.unknown_recipients.len() });
    }
    if f.is_reply && !f.outside_conversation.is_empty() {
        out.push(RuleMiss::OutsideConversation { count: f.outside_conversation.len() });
    }
    if f.attachments > 0 {
        out.push(RuleMiss::HasAttachments);
    }
    if f.checks > 0 {
        out.push(RuleMiss::SendChecks);
    }
    if f.sent_today >= f.daily_limit {
        out.push(RuleMiss::DailyLimit);
    }
    out
}

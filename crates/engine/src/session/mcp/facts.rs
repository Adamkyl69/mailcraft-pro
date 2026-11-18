//! What a send's rules are decided from: whether the owner wrote in the conversation, and whom
//! they've written to before. Cheap scans of what's in memory.
//!
//! "Written to before" is the Sent folders of every account, and the saved contacts: not the
//! address book `ContactBook` counts, which counts any message whose From *claims* an address of
//! the owner's, in any folder, so a message forged in the Inbox could make its recipient known.

use super::*;

impl Session {
    /// The owner has written in the conversation of message `key`: a message of its own in the
    /// same account's Sent folder is one it answers (or answered by), or `key` is in that folder.
    pub(super) fn owner_wrote_in_thread(&mut self, key: &MessageKey) -> bool {
        let Some(sent) = self.account(&key.account).and_then(|a| a.folder(FolderRole::Sent)).map(|f| f.path.clone()) else { return false };
        if key.folder == sent {
            return true;
        }
        let Some(original) = self.folder_state(&key.account, &key.folder).messages.iter().find(|m| m.key == *key).cloned() else { return false };
        let mut ids: HashSet<&str> = original.references.iter().map(String::as_str).collect();
        ids.insert(&original.in_reply_to);
        ids.insert(&original.message_id);
        ids.remove("");
        let mine = &original.message_id;
        self.folder_state(&key.account, &sent).messages.iter().any(|m| {
            (!m.message_id.is_empty() && ids.contains(m.message_id.as_str()))
                || (!mine.is_empty() && (m.in_reply_to == *mine || m.references.iter().any(|r| r == mine)))
        })
    }

    /// Of `emails` (lower case), those who aren't already in message `key`: not its From,
    /// Reply-To, To or Cc, or an address of the owner's (a reply may go to people already in the
    /// conversation without a code, and to no one else). A message that can't be read has no one.
    pub(super) fn outside_message(&mut self, key: &MessageKey, emails: &[String]) -> Vec<String> {
        let in_it: HashSet<String> = self
            .body_raw(key)
            .and_then(|raw| mailcraft_mime::parse(&raw))
            .map(|p| {
                std::iter::once(&p.from)
                    .chain(&p.reply_to)
                    .chain(&p.to)
                    .chain(&p.cc)
                    .map(|a| a.email.trim().to_lowercase())
                    .filter(|e| !self.accounts.iter().any(|a| a.config.has_address(e)))
                    .collect()
            })
            .unwrap_or_default();
        let mut outside: Vec<String> = Vec::new();
        for e in emails {
            if !in_it.contains(e) && !outside.contains(e) {
                outside.push(e.clone());
            }
        }
        outside
    }

    /// Of `emails` (lower case), those the owner has never written to and has no card for.
    pub(super) fn written_to_before(&mut self, emails: &[String]) -> Vec<String> {
        let owner: HashSet<String> = self.accounts.iter().flat_map(|a| a.config.addresses().map(str::to_lowercase).collect::<Vec<_>>()).collect();
        let sent: Vec<(String, String)> =
            self.accounts.iter().filter_map(|a| a.folder(FolderRole::Sent).map(|f| (a.config.id.clone(), f.path.clone()))).collect();
        let mut known: HashSet<String> = HashSet::new();
        for (account, folder) in sent {
            for m in &self.folder_state(&account, &folder).messages {
                if owner.contains(&m.from.email.trim().to_lowercase()) {
                    known.extend(m.to.iter().chain(&m.cc).map(|a| a.email.trim().to_lowercase()));
                }
            }
        }
        for pim in self.pim.values() {
            known.extend(pim.cards.iter().flat_map(|c| c.card.emails.iter()).map(|e| e.value.trim().to_lowercase()));
        }
        let mut unknown: Vec<String> = Vec::new();
        for e in emails {
            if !known.contains(e) && !unknown.contains(e) {
                unknown.push(e.clone());
            }
        }
        unknown
    }
}

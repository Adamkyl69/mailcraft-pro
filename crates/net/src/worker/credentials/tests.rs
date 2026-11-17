use std::sync::Arc;

use mailcraft_model::job::{Event, Job, Secret};
use mailcraft_store::Store;

use super::super::{Auth, Pool, SyncOptions, Worker};
use super::*;

/// Found in review: the password just given was kept for signing in only when the account's lock
/// on it could be had whole, unlike everywhere else it's taken: after a thread panicked holding
/// it, the account signed in with no password, and nothing said why.
#[test]
fn a_password_given_is_kept_after_a_thread_panicked_holding_the_lock() {
    let store = Store::memory();
    let pool = Pool::new(store.clone(), Arc::new(MemoryCredentials::default()));
    let acc = AccountConfig { id: "a1".into(), ..Default::default() };
    let mut w = Worker::new(acc, Auth::shared(""), store, pool.sink.clone(), SyncOptions::default());
    w.creds = Some(Arc::new(MemoryCredentials::default()));
    w.keychain = Some(Some("pw".into()));
    let held = w.auth.clone();
    let _ = std::thread::spawn(move || {
        let _held = held.lock();
        panic!("a thread panicking with the lock held");
    })
    .join();
    assert!(w.auth.is_poisoned());
    assert!(w.settle_password());
    assert_eq!(w.auth.lock().unwrap_or_else(std::sync::PoisonError::into_inner).secret, "pw");
}

/// The approval secret of connected apps is kept apart from every account's: kept, read and deleted
/// through the pool on a thread of its own, and answered as events.
#[test]
fn an_app_secret_is_kept_read_and_deleted_through_the_pool() {
    let creds = Arc::new(MemoryCredentials::default());
    let mut pool = Pool::new(Store::memory(), creds.clone());
    let answer = |pool: &mut Pool, what: &str, ended: fn(&Event) -> bool| -> Event {
        let mut found = None;
        crate::waiting::eventually(what, || {
            for e in pool.poll() {
                if found.is_none() && ended(&e) {
                    found = Some(e);
                }
            }
            found.is_some()
        });
        found.unwrap()
    };
    pool.submit(Job::KeepAppSecret { what: "mcp-approval".into(), secret: Some(Secret("MZXW6YTB".into())) });
    let kept = answer(&mut pool, "the secret to be kept", |e| matches!(e, Event::AppSecretKept { .. }));
    assert_eq!(kept, Event::AppSecretKept { what: "mcp-approval".into(), error: None });
    assert_eq!(creds.get_app_secret("mcp-approval").as_deref(), Some("MZXW6YTB"));
    // Not an account's: an account of any id has none under that name.
    assert_eq!(creds.get_secret(&AccountConfig { id: "mcp-approval".into(), ..Default::default() }, "mcp-approval"), None);
    pool.submit(Job::ReadAppSecret { what: "mcp-approval".into() });
    let read = answer(&mut pool, "the secret to be read", |e| matches!(e, Event::AppSecretRead { .. }));
    assert_eq!(read, Event::AppSecretRead { what: "mcp-approval".into(), secret: Some(Secret("MZXW6YTB".into())) });
    pool.submit(Job::KeepAppSecret { what: "mcp-approval".into(), secret: None });
    answer(&mut pool, "the secret to be deleted", |e| matches!(e, Event::AppSecretKept { error: None, .. }));
    assert_eq!(creds.get_app_secret("mcp-approval"), None);
    pool.submit(Job::ReadAppSecret { what: "mcp-approval".into() });
    let read = answer(&mut pool, "the answer for none", |e| matches!(e, Event::AppSecretRead { .. }));
    assert_eq!(read, Event::AppSecretRead { what: "mcp-approval".into(), secret: None });
}

/// A store with no keychain (the trait's defaults) keeps none, and says why.
#[test]
fn a_store_without_app_secrets_refuses_to_keep_one() {
    struct Bare;
    impl Credentials for Bare {
        fn get(&self, _: &AccountConfig) -> Option<String> {
            None
        }
        fn set(&self, _: &AccountConfig, _: &str) -> std::result::Result<(), String> {
            Ok(())
        }
        fn delete(&self, _: &AccountConfig) {}
        fn get_smtp(&self, _: &AccountConfig) -> Option<String> {
            None
        }
        fn set_smtp(&self, _: &AccountConfig, _: &str) -> std::result::Result<(), String> {
            Ok(())
        }
        fn delete_smtp(&self, _: &AccountConfig) {}
    }
    assert_eq!(Bare.get_app_secret("x"), None);
    assert_eq!(Bare.set_app_secret("x", "s"), Err("there's no keychain to keep it in".to_string()));
    Bare.delete_app_secret("x");
}

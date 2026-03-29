//! Process registry — maps actor names to their typed mailboxes.
//!
//! Callers register a mailbox under a static name:
//! ```ignore
//! registry::register("dummy", dummy_inbox.clone());
//! ```
//! Any code that knows the message and info types can retrieve it:
//! ```ignore
//! let inbox = registry::get::<DummyMsg, DummyInfo>("dummy");
//! ```
//! For generic info queries (no inner message type needed):
//! ```ignore
//! if let Some(status) = registry::ask_info("dummy").await {
//!     println!("name: {}  running: {}", status.name, status.running);
//! }
//! ```

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use crate::spin_mutex::SpinMutex as Mutex;
use lazy_static::lazy_static;
use super::mailbox::{ActorMsg, ActorStatus, ErasedInfo, Mailbox, Reply};

// ---------------------------------------------------------------------------
// Informable — type-erased handle for generic ErasedInfo queries

/// Implemented by any `Mailbox<ActorMsg<M, I>>` so that the registry can
/// forward type-erased `ErasedInfo` queries without knowing `M` or `I`.
pub trait Informable: Send + Sync {
    fn send_info(&self, reply: Reply<ActorStatus<ErasedInfo>>);
}

impl<M: Send, I: Send + 'static> Informable for Mailbox<ActorMsg<M, I>> {
    fn send_info(&self, reply: Reply<ActorStatus<ErasedInfo>>) {
        self.send(ActorMsg::ErasedInfo(reply));
    }
}

// ---------------------------------------------------------------------------
// Registry internals

struct Entry {
    name:       &'static str,
    /// Type-erased `Arc<Mailbox<ActorMsg<M, I>>>` for typed downcasting.
    mailbox:    Arc<dyn Any + Send + Sync>,
    /// Type-erased handle for sending `ErasedInfo` without knowing `M` or `I`.
    informable: Arc<dyn Informable>,
}

lazy_static! {
    static ref REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
}

// ---------------------------------------------------------------------------
// Public API

/// Register an actor's mailbox under `name`.
///
/// If an entry already exists for that name it is replaced, so callers can
/// re-register after a driver restart.
pub fn register<M: Send + 'static, I: Send + 'static>(
    name: &'static str,
    inbox: Arc<Mailbox<ActorMsg<M, I>>>,
) {
    let mailbox:    Arc<dyn Any + Send + Sync> = inbox.clone();
    let informable: Arc<dyn Informable>        = inbox;
    let mut reg = REGISTRY.lock();
    if let Some(e) = reg.iter_mut().find(|e| e.name == name) {
        e.mailbox    = mailbox;
        e.informable = informable;
        return;
    }
    reg.push(Entry { name, mailbox, informable });
}

/// Look up the typed mailbox registered under `name`.
///
/// Returns `None` if no entry exists or the stored type does not match
/// `(M, I)`.
pub fn get<M: Send + 'static, I: Send + 'static>(
    name: &str,
) -> Option<Arc<Mailbox<ActorMsg<M, I>>>> {
    let reg = REGISTRY.lock();
    reg.iter()
        .find(|e| e.name == name)
        .and_then(|e| e.mailbox.clone().downcast::<Mailbox<ActorMsg<M, I>>>().ok())
}

/// Send a type-erased `ErasedInfo` query to the actor registered under `name`
/// and await the response.
///
/// Returns `None` if no entry exists for the name, or if the actor dropped the
/// reply without responding (e.g. it was stopped mid-flight).
pub async fn ask_info(name: &str) -> Option<ActorStatus<ErasedInfo>> {
    // Clone the trait object while holding the lock, then drop the lock before
    // awaiting so we don't hold a mutex across an await point.
    let informable = {
        let reg = REGISTRY.lock();
        reg.iter().find(|e| e.name == name).map(|e| e.informable.clone())
    }?;
    let (reply, rx) = Reply::new();
    informable.send_info(reply);
    rx.recv().await
}

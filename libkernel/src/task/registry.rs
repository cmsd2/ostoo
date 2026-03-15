//! Process registry — maps actor names to their typed mailboxes.
//!
//! Callers register a mailbox under a static name:
//! ```ignore
//! registry::register("dummy", dummy_inbox.clone());
//! ```
//! Any code that knows the message type can retrieve it:
//! ```ignore
//! let inbox = registry::get::<DummyMsg>("dummy"); // Option<Arc<Mailbox<ActorMsg<DummyMsg>>>>
//! ```
//! For generic info queries (no inner message type needed):
//! ```ignore
//! if let Some(info) = registry::ask_info("dummy").await {
//!     println!("name: {}", info.name);
//! }
//! ```

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;
use lazy_static::lazy_static;
use super::mailbox::{ActorInfo, ActorMsg, Mailbox, Reply};

// ---------------------------------------------------------------------------
// Informable — type-erased handle for generic Info queries

/// Implemented by any `Mailbox<ActorMsg<M>>` so that the registry can forward
/// `Info` queries without knowing `M`.
pub trait Informable: Send + Sync {
    fn send_info(&self, reply: Reply<ActorInfo>);
}

impl<M: Send> Informable for Mailbox<ActorMsg<M>> {
    fn send_info(&self, reply: Reply<ActorInfo>) {
        self.send(ActorMsg::Info(reply));
    }
}

// ---------------------------------------------------------------------------
// Registry internals

struct Entry {
    name:       &'static str,
    /// Type-erased `Arc<Mailbox<ActorMsg<M>>>` for typed downcasting.
    mailbox:    Arc<dyn Any + Send + Sync>,
    /// Type-erased handle for sending `Info` without knowing `M`.
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
pub fn register<M: Send + 'static>(name: &'static str, inbox: Arc<Mailbox<ActorMsg<M>>>) {
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
/// Returns `None` if no entry exists or the stored inner message type does
/// not match `M`.
pub fn get<M: Send + 'static>(name: &str) -> Option<Arc<Mailbox<ActorMsg<M>>>> {
    let reg = REGISTRY.lock();
    reg.iter()
        .find(|e| e.name == name)
        .and_then(|e| e.mailbox.clone().downcast::<Mailbox<ActorMsg<M>>>().ok())
}

/// Send a generic `Info` query to the actor registered under `name` and await
/// the response.
///
/// Returns `None` if no entry exists for the name, or if the actor dropped the
/// reply without responding (e.g. it was stopped mid-flight).
pub async fn ask_info(name: &str) -> Option<ActorInfo> {
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

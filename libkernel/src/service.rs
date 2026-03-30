//! Kernel-global service name registry.
//!
//! Processes can register an fd under a well-known name so that other
//! processes can look it up by name — a minimal service discovery
//! mechanism that avoids filesystem entanglement.

use alloc::collections::BTreeMap;
use alloc::string::String;
use crate::file::FdObject;
use crate::spin_mutex::SpinMutex;

/// Maximum length of a service name (bytes, excluding null terminator).
pub const MAX_SERVICE_NAME_LEN: usize = 128;

static SERVICE_REGISTRY: SpinMutex<BTreeMap<String, FdObject>> =
    SpinMutex::new(BTreeMap::new());

/// Register an fd object under `name`.
///
/// Returns `Ok(())` on success, `Err(())` if the name is already taken.
pub fn register(name: String, obj: FdObject) -> Result<(), ()> {
    let mut reg = SERVICE_REGISTRY.lock();
    if reg.contains_key(&name) {
        return Err(());
    }
    reg.insert(name, obj);
    Ok(())
}

/// Look up a service by name.
///
/// Returns a clone of the registered `FdObject`, or `None` if not found.
pub fn lookup(name: &str) -> Option<FdObject> {
    let reg = SERVICE_REGISTRY.lock();
    reg.get(name).cloned()
}

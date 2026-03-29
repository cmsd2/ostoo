use alloc::string::String;

pub(super) fn generate() -> String {
    alloc::format!(
        "current thread: {}  context switches: {}\n",
        libkernel::task::scheduler::current_thread_idx(),
        libkernel::task::scheduler::context_switches()
    )
}

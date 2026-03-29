use alloc::string::String;

pub(super) fn generate() -> String {
    let ready   = libkernel::task::executor::ready_count();
    let waiting = libkernel::task::executor::wait_count();
    alloc::format!("ready: {}  waiting: {}\n", ready, waiting)
}

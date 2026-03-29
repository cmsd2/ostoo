use alloc::string::String;

pub(super) fn generate() -> String {
    let secs = libkernel::task::timer::ticks() / libkernel::task::timer::TICKS_PER_SECOND;
    alloc::format!("{}s\n", secs)
}

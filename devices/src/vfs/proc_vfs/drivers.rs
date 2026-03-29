use alloc::string::String;

pub(super) fn generate() -> String {
    let mut s = String::new();
    crate::driver::with_drivers(|name, state| {
        s.push_str(name);
        s.push_str("  ");
        s.push_str(state.as_str());
        s.push('\n');
    });
    s
}

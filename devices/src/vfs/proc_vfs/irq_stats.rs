pub(super) fn generate() -> alloc::string::String {
    libkernel::irq_handle::format_irq_stats()
}

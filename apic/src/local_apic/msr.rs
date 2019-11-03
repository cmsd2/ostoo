
bitflags! {
    pub struct ApicBaseMsrFlags: u64 {
        const BSP           = 0b0001_00000000;
        const GLOBAL_ENABLE = 0b1000_00000000;
        const APIC_BASE_AND_RESERVED = 0xffffffff_fffff000;
    }
}

impl ApicBaseMsrFlags {

}
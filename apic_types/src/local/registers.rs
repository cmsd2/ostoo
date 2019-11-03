use super::LocalApic;

#[repr(u32)]
pub enum LocalApicRegisterIndex {
    Id = 0x20,
    Version = 0x30,
    TaskPriority = 0x80,
    ArbitrationPriority = 0x90,
    ProcessPriority = 0xa0,
    EndOfInterrupt = 0xb0,
    RemoteRead = 0xc0,
    LocalDestination = 0xd0,
    DestinationFormat = 0xe0,
    SpuriousInterrupt = 0xf0,
    InterruptCommandLow = 0x300,
    InterruptCommandHigh = 0x310,
    // ...
}

impl LocalApicRegisterIndex {
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    pub fn as_u64(self) -> u64 {
        u64::from(self.as_u32())
    }
}

pub trait LocalApicRegister {
    type Value;

    unsafe fn read(&self, apic: &dyn LocalApic) -> Self::Value;
    unsafe fn write(&self, apic: &dyn LocalApic, value: Self::Value);
}
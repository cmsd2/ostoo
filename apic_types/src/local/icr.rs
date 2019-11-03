use core::convert::TryFrom;
use core::result::Result;

bitflags! {
    pub struct InterruptCommandFlags: u64 {
        const VECTOR = 0xff;
        const DELIVERY_MODE = 0x700;
        const DELIVERY_MODE_LOWEST_PRIORITY = 0x100;
        const DELIVERY_MODE_SMI = 0x200;
        const DELIVERY_MODE_RESERVED = 0x300;
        const DELIVERY_MODE_NMI = 0x400;
        const DELIVERY_MODE_INIT = 0x500;
        const DELIVERY_MODE_START_UP = 0x600;
        const DELIVERY_MODE_RESERVED2 = 0x700;
        const DESTINATION_MODE = 0x800;
        const DELIVERY_STATUS = 0x1000;
        const RESERVED = 0x2000;
        const LEVEL = 0x4000;
        const TRIGGER_MODE = 0x8000;
        const RESERVED2 = 0x30000;
        const DESTINATION_SHORTHAND = 0xc0000;
        const RESERVED3 = 0x00ffffff_fff00000;
        const DESTINATION = 0xff000000_00000000;
    }
}

impl InterruptCommandFlags {
    pub fn delivery_mode(&self) -> DeliveryMode {
        let bits = (*self & InterruptCommandFlags::DELIVERY_MODE).bits() >> 8;
        DeliveryMode::try_from(bits as u8).expect("delivery mode")
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u8)]
pub enum DeliveryMode {
    Fixed = 0x0,
    LowestPriority,
    SMI,
    Reserved,
    NMI,
    INIT,
    StartUp,
    Reserved2,
}

impl DeliveryMode {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn as_u64(self) -> u64 {
        self.as_u8() as u64
    }

    pub fn as_flags(self) -> InterruptCommandFlags {
        InterruptCommandFlags::from_bits(self.as_u64() << 8).unwrap()
    }
}

impl TryFrom<u8> for DeliveryMode {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x0 => Ok(DeliveryMode::Fixed),
            0x1 => Ok(DeliveryMode::LowestPriority),
            0x2 => Ok(DeliveryMode::SMI),
            0x3 => Ok(DeliveryMode::Reserved),
            0x4 => Ok(DeliveryMode::NMI),
            0x5 => Ok(DeliveryMode::INIT),
            0x6 => Ok(DeliveryMode::StartUp),
            0x7 => Ok(DeliveryMode::Reserved2),
            _ => Err("invalid delivery mode")
        }
    }
}

use x86_64::VirtAddr;

bitflags! {
    pub struct RedirectionEntry: u64 {
        const VECTOR           = 0b00000000_11111111;
        const DELIVERY_MODE    = 0b00000111_00000000;
        const DESTINATION_MODE = 0b00001000_00000000;
        const DELIERY_STATUS   = 0b00010000_00000000;
        const PIN_POLARITY     = 0b00100000_00000000;
        const REMOTE_IRR       = 0b01000000_00000000;
        const TRIGGER_MODE     = 0b10000000_00000000;
        const MASK             = 0x00000000_00010000;
        const RESERVED         = 0x00ffffff_fffe0000;
        const DESTINATION      = 0xff000000_00000000;
    }
}

#[repr(u8)]
pub enum IoApicReg {
    Id = 0x0,
    Version = 0x1,
}

impl IoApicReg {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

pub struct IoApic {
    pub id: u8,
    pub version: u8,
    pub max_redir_entry: u8,
    pub base_interrupt: u32,
    pub registers: IoApicRegisters,
}

impl IoApic {
    pub unsafe fn new(base: VirtAddr, base_interrupt: u32) -> Self {
        let mut registers = IoApicRegisters::new(base);
        IoApic {
            id: registers.id(),
            version: registers.version(),
            max_redir_entry: registers.max_redir_entry(),
            base_interrupt: base_interrupt,
            registers: registers,
        }
    }
}

pub struct IoApicRegisters {
    base_addr: VirtAddr,
}

impl IoApicRegisters {
    pub fn new(base_addr: VirtAddr) -> Self {
        IoApicRegisters {
            base_addr: base_addr,
        }
    }

    pub fn reg_sel_addr(&self) -> VirtAddr {
        self.base_addr
    }

    pub fn reg_win_addr(&self) -> VirtAddr {
        self.base_addr + 0x10 as u64
    }

    pub unsafe fn write_reg_32(&mut self, index: u8, val: u32) {
        let ptr = self.reg_sel_addr().as_mut_ptr();
        *ptr = index as u32;

        let ptr = self.reg_win_addr().as_mut_ptr();
        *ptr = val;
    }

    pub unsafe fn read_reg_32(&mut self, index: u8) -> u32 {
        let ptr = self.reg_sel_addr().as_mut_ptr();
        *ptr = index as u32;

        let ptr = self.reg_win_addr().as_ptr();
        *ptr
    }

    pub unsafe fn id(&mut self) -> u8 {
        ((self.read_reg_32(IoApicReg::Id.as_u8()) >> 24) & 0xf0) as u8
    }

    pub unsafe fn version(&mut self) -> u8 {
        self.read_reg_32(IoApicReg::Version.as_u8()) as u8
    }

    pub unsafe fn max_redir_entry(&mut self) -> u8 {
        (self.read_reg_32(IoApicReg::Version.as_u8()) >> 16 + 1) as u8
    }

    pub unsafe fn read_redir_entry(&mut self, index: u8) -> RedirectionEntry {
        let lower = self.read_reg_32(0x10 + index * 2);
        let higher = self.read_reg_32(0x10 + index * 2 + 1);

        let entry = lower as u64 + (higher as u64) << 32;

        RedirectionEntry::from_bits(entry).unwrap()
    }

    pub unsafe fn write_redir_entry(&mut self, index: u8, entry: RedirectionEntry) {
        let entry = entry.bits();

        self.write_reg_32(0x10 + index * 2, entry as u32);
        self.write_reg_32(0x10 + index * 2 + 1, (entry >> 32 & 0xffff) as u32);
    }
}
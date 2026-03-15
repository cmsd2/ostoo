use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::instructions::port::Port;

#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus:         u8,
    pub device:      u8,
    pub function:    u8,
    pub vendor_id:   u16,
    pub device_id:   u16,
    pub class:       u8,
    pub subclass:    u8,
    pub prog_if:     u8,
    pub revision:    u8,
    pub header_type: u8,
}

lazy_static! {
    pub static ref PCI_DEVICES: Mutex<Vec<PciDevice>> = Mutex::new(Vec::new());
}

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA:    u16 = 0xCFC;

fn read_config_u32(bus: u8, device: u8, func: u8, offset: u8) -> u32 {
    let addr: u32 = (1 << 31)
        | ((bus    as u32) << 16)
        | (((device as u32) & 0x1F) << 11)
        | (((func   as u32) & 0x07) << 8)
        | ((offset  as u32) & 0xFC);
    unsafe {
        Port::new(CONFIG_ADDRESS).write(addr);
        Port::new(CONFIG_DATA).read()
    }
}

fn read_config_u8(bus: u8, device: u8, func: u8, offset: u8) -> u8 {
    let word = read_config_u32(bus, device, func, offset & !3);
    (word >> ((offset & 3) * 8)) as u8
}

pub fn init() {
    let mut devices = PCI_DEVICES.lock();
    scan_bus(0, &mut devices);
    info!("[pci] found {} device(s)", devices.len());
}

fn scan_bus(bus: u8, out: &mut Vec<PciDevice>) {
    for device in 0u8..32 {
        scan_device(bus, device, out);
    }
}

fn scan_device(bus: u8, device: u8, out: &mut Vec<PciDevice>) {
    let w0 = read_config_u32(bus, device, 0, 0x00);
    if w0 & 0xFFFF == 0xFFFF { return; }
    let multifunction = read_config_u8(bus, device, 0, 0x0E) & 0x80 != 0;
    let n_funcs: u8   = if multifunction { 8 } else { 1 };
    for func in 0..n_funcs {
        scan_function(bus, device, func, out);
    }
}

fn scan_function(bus: u8, device: u8, func: u8, out: &mut Vec<PciDevice>) {
    let w0 = read_config_u32(bus, device, func, 0x00);
    let vendor_id = (w0 & 0xFFFF) as u16;
    if vendor_id == 0xFFFF { return; }
    let device_id   = (w0 >> 16) as u16;
    let w2          = read_config_u32(bus, device, func, 0x08);
    let revision    = (w2       & 0xFF) as u8;
    let prog_if     = (w2 >>  8 & 0xFF) as u8;
    let subclass    = (w2 >> 16 & 0xFF) as u8;
    let class       = (w2 >> 24 & 0xFF) as u8;
    let header_type = read_config_u8(bus, device, func, 0x0E) & 0x7F;

    if header_type == 0x01 {
        let secondary = (read_config_u32(bus, device, func, 0x18) >> 8 & 0xFF) as u8;
        scan_bus(secondary, out);
    }

    out.push(PciDevice { bus, device, function: func,
        vendor_id, device_id, class, subclass, prog_if, revision, header_type });
}

pub fn class_name(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x01, 0x01) => "IDE controller",
        (0x01, 0x06) => "SATA controller",
        (0x01, 0x08) => "NVMe controller",
        (0x02, 0x00) => "Ethernet controller",
        (0x03, 0x00) => "VGA controller",
        (0x04, 0x01) => "Audio controller",
        (0x06, 0x00) => "Host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI-PCI bridge",
        (0x0C, 0x03) => "USB controller",
        (0x0C, 0x05) => "SMBus",
        _            => "Unknown",
    }
}

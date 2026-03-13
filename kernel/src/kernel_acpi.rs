use core::ptr::NonNull;
use x86_64::VirtAddr;
use acpi::{AcpiError, AcpiTables, Handle, PhysicalMapping};
use acpi::platform::interrupt::InterruptModel;
use acpi::rsdp::Rsdp;

/// ACPI handler backed by the bootloader's complete physical-memory map.
///
/// The bootloader (with `map_physical_memory` feature) maps all physical RAM at a
/// fixed offset: `virt = phys + physical_memory_offset`. No dynamic page mapping is
/// required. This works for ACPI table parsing because all ACPI tables live in
/// physical RAM. MMIO regions (APIC registers etc.) are mapped separately by the
/// apic crate via `mapper.map_to()`.
#[derive(Clone)]
pub struct KernelAcpiHandler {
    physical_memory_offset: u64,
}

impl KernelAcpiHandler {
    fn phys_to_virt(&self, phys: usize) -> usize {
        phys + self.physical_memory_offset as usize
    }
}

impl acpi::Handler for KernelAcpiHandler {
    unsafe fn map_physical_region<T>(&self, physical_address: usize, size: usize) -> PhysicalMapping<Self, T> {
        let virt = self.phys_to_virt(physical_address);
        PhysicalMapping {
            physical_start: physical_address,
            virtual_start: NonNull::new_unchecked(virt as *mut T),
            region_length: size,
            mapped_length: size,
            handler: self.clone(),
        }
    }

    fn unmap_physical_region<T>(_region: &PhysicalMapping<Self, T>) {
        // Nothing to do: physical memory is permanently mapped by the bootloader.
    }

    fn read_u8(&self, address: usize) -> u8 {
        unsafe { *(self.phys_to_virt(address) as *const u8) }
    }
    fn read_u16(&self, address: usize) -> u16 {
        unsafe { *(self.phys_to_virt(address) as *const u16) }
    }
    fn read_u32(&self, address: usize) -> u32 {
        unsafe { *(self.phys_to_virt(address) as *const u32) }
    }
    fn read_u64(&self, address: usize) -> u64 {
        unsafe { *(self.phys_to_virt(address) as *const u64) }
    }

    fn write_u8(&self, address: usize, value: u8) {
        unsafe { *(self.phys_to_virt(address) as *mut u8) = value }
    }
    fn write_u16(&self, address: usize, value: u16) {
        unsafe { *(self.phys_to_virt(address) as *mut u16) = value }
    }
    fn write_u32(&self, address: usize, value: u32) {
        unsafe { *(self.phys_to_virt(address) as *mut u32) = value }
    }
    fn write_u64(&self, address: usize, value: u64) {
        unsafe { *(self.phys_to_virt(address) as *mut u64) = value }
    }

    fn read_io_u8(&self, port: u16) -> u8 {
        unsafe { x86_64::instructions::port::PortReadOnly::new(port).read() }
    }
    fn read_io_u16(&self, port: u16) -> u16 {
        unsafe { x86_64::instructions::port::PortReadOnly::new(port).read() }
    }
    fn read_io_u32(&self, port: u16) -> u32 {
        unsafe { x86_64::instructions::port::PortReadOnly::new(port).read() }
    }

    fn write_io_u8(&self, port: u16, value: u8) {
        unsafe { x86_64::instructions::port::PortWriteOnly::new(port).write(value) }
    }
    fn write_io_u16(&self, port: u16, value: u16) {
        unsafe { x86_64::instructions::port::PortWriteOnly::new(port).write(value) }
    }
    fn write_io_u32(&self, port: u16, value: u32) {
        unsafe { x86_64::instructions::port::PortWriteOnly::new(port).write(value) }
    }

    fn read_pci_u8(&self, _address: acpi::PciAddress, _offset: u16) -> u8 {
        unimplemented!("PCI config space access not implemented")
    }
    fn read_pci_u16(&self, _address: acpi::PciAddress, _offset: u16) -> u16 {
        unimplemented!("PCI config space access not implemented")
    }
    fn read_pci_u32(&self, _address: acpi::PciAddress, _offset: u16) -> u32 {
        unimplemented!("PCI config space access not implemented")
    }
    fn write_pci_u8(&self, _address: acpi::PciAddress, _offset: u16, _value: u8) {
        unimplemented!("PCI config space access not implemented")
    }
    fn write_pci_u16(&self, _address: acpi::PciAddress, _offset: u16, _value: u16) {
        unimplemented!("PCI config space access not implemented")
    }
    fn write_pci_u32(&self, _address: acpi::PciAddress, _offset: u16, _value: u32) {
        unimplemented!("PCI config space access not implemented")
    }

    fn nanos_since_boot(&self) -> u64 {
        0
    }

    fn stall(&self, microseconds: u64) {
        for _ in 0..microseconds * 100 {
            core::hint::spin_loop();
        }
    }

    fn sleep(&self, milliseconds: u64) {
        self.stall(milliseconds * 1000);
    }

    fn create_mutex(&self) -> Handle {
        unimplemented!("AML mutex support not implemented")
    }

    fn acquire(&self, _mutex: Handle, _timeout: u16) -> Result<(), acpi::aml::AmlError> {
        unimplemented!("AML mutex support not implemented")
    }

    fn release(&self, _mutex: Handle) {
        unimplemented!("AML mutex support not implemented")
    }
}

pub unsafe fn read_acpi(physical_memory_offset: VirtAddr) -> Result<InterruptModel, AcpiError> {
    let handler = KernelAcpiHandler {
        physical_memory_offset: physical_memory_offset.as_u64(),
    };

    let rsdp_mapping = Rsdp::search_for_on_bios(handler.clone())?;
    let rsdp_phys_addr = rsdp_mapping.physical_start;
    drop(rsdp_mapping);

    let tables = AcpiTables::from_rsdp(handler, rsdp_phys_addr)?;
    let (interrupt_model, _processor_info) = InterruptModel::new(&tables)?;

    Ok(interrupt_model)
}

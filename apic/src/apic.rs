use libkernel::{println};
use raw_cpuid::CpuId;
use spin::Mutex;
use lazy_static::lazy_static;
use x86_64::registers::model_specific::Msr;
use x86_64::{VirtAddr, PhysAddr};


use raw_cpuid::CpuId;
use core::result;

#[derive(Clone, Debug)]
pub enum CpuidError {
    Unsupported,
    Unknown
}

pub type Result<T> = result::Result<T,CpuidError>;

pub fn family() -> Result<u32> {
    let features = CpuId::new().get_feature_info().ok_or(CpuidError::Unsupported)?;

    let family = features.family_id() as u32;
    let family_ext = features.extended_family_id() as u32;

    Ok((family_ext << 4) | family)
}

pub fn model() -> Result<u32> {
    let features = CpuId::new().get_feature_info().ok_or(CpuidError::Unsupported)?;

    let model = features.model_id() as u32;
    let model_ext = features.extended_model_id() as u32;

    Ok((model_ext << 4) | model)
}

pub fn is_p4_or_xeon_or_later() -> Result<bool> {
    Ok(family()? >= 0xf || model()? >= 0xf)
}

pub fn stepping() -> Result<u32> {
    let features = CpuId::new().get_feature_info().ok_or(CpuidError::Unsupported)?;
    Ok(features.stepping_id() as u32)
}

/// Copy the CPU vendor string (up to 12 bytes) into `buf`. Returns the filled length.
pub fn vendor_into(buf: &mut [u8; 12]) -> usize {
    match CpuId::new().get_vendor_info() {
        Some(vi) => {
            let bytes = vi.as_string().as_bytes();
            let len = bytes.len().min(12);
            buf[..len].copy_from_slice(&bytes[..len]);
            len
        }
        None => 0,
    }
}

pub fn init() {
    let features = CpuId::new().get_feature_info();
    info!("[cpuid] init {:?}", features);
    info!("[cpuid] init family={:?} model={:?} stepping={:?}", family(), model(), features.map(|f| f.stepping_id()));
}
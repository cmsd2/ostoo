mod msr;
mod mapped;

pub use mapped::*;

#[derive(Debug)]
pub enum LocalApiError {
    MissingCpuidFeatures
}

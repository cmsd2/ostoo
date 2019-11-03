use log::{Log, Metadata, Record, Level, SetLoggerError, LevelFilter};
use crate::serial_println;
use core::result::Result;

static LOGGER: Logger = Logger;

pub fn init() -> Result<(), SetLoggerError> {
    log::set_logger(&LOGGER)
        .map(|()| log::set_max_level(LevelFilter::Info))
}

pub struct Logger;

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            serial_println!("{} - {}", record.level(), record.args());
        }
    }
    
    fn flush(&self) {
    }
}
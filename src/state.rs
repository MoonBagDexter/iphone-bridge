use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

pub struct CaptureCtl {
    pub selected_device: Mutex<Option<String>>,
    pub restart: AtomicBool,
}

impl CaptureCtl {
    pub fn new(initial: Option<String>) -> Self {
        Self {
            selected_device: Mutex::new(initial),
            restart: AtomicBool::new(false),
        }
    }
}

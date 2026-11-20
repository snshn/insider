//! NVIDIA GPU details nvidia-smi doesn't print: the CUDA core count, from
//! the NVIDIA Management Library (NVML, `libnvidia-ml.so.1`, installed with
//! the driver).
//!
//! Linux only, loaded at run time like the other FFI here: no driver, no
//! library, no error, just no core count. `nvmlDeviceGetNumGpuCores` needs
//! a recent driver (R520+); older ones simply lack the symbol.

use std::ffi::{c_char, c_int, c_void, CString};

const RTLD_NOW: c_int = 2;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

type Device = *mut c_void;

pub struct Nvml {
    by_bus_id: unsafe extern "C" fn(*const c_char, *mut Device) -> c_int,
    num_cores: unsafe extern "C" fn(Device, *mut u32) -> c_int,
    shutdown: unsafe extern "C" fn() -> c_int,
}

// Only used from the prober thread that owns it.
unsafe impl Send for Nvml {}

unsafe fn sym<T: Copy>(lib: *mut c_void, name: &[u8]) -> Option<T> {
    let p = dlsym(lib, name.as_ptr().cast());
    (!p.is_null()).then(|| std::mem::transmute_copy(&p))
}

impl Nvml {
    pub fn open() -> Option<Nvml> {
        unsafe {
            let lib = dlopen(b"libnvidia-ml.so.1\0".as_ptr().cast(), RTLD_NOW);
            if lib.is_null() {
                return None;
            }
            let init: unsafe extern "C" fn() -> c_int = sym(lib, b"nvmlInit_v2\0")?;
            let n = Nvml {
                by_bus_id: sym(lib, b"nvmlDeviceGetHandleByPciBusId_v2\0")?,
                num_cores: sym(lib, b"nvmlDeviceGetNumGpuCores\0")?,
                shutdown: sym(lib, b"nvmlShutdown\0")?,
            };
            (init() == 0).then_some(n)
        }
    }

    /// CUDA cores of the GPU at a PCI address ("0000:01:00.0").
    pub fn cuda_cores(&self, pci: &str) -> Option<u32> {
        let id = CString::new(pci).ok()?;
        let mut dev: Device = std::ptr::null_mut();
        let mut cores = 0u32;
        unsafe {
            if (self.by_bus_id)(id.as_ptr(), &mut dev) != 0 || dev.is_null() {
                return None;
            }
            ((self.num_cores)(dev, &mut cores) == 0 && cores > 0).then_some(cores)
        }
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
        }
    }
}

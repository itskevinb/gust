//! Read-only access to the Apple SMC through IOKit.
//!
//! There is deliberately no write path in this module. Writing fan keys
//! needs root and will live in a separate privileged helper later.

use std::ffi::c_void;
use std::fmt;

type KernReturn = i32;
type MachPort = u32;
type IoObject = MachPort;
type IoConnect = MachPort;

const KERN_SUCCESS: KernReturn = 0;
const KERNEL_INDEX_SMC: u32 = 2;
const CMD_READ_BYTES: u8 = 5;
const CMD_READ_INDEX: u8 = 8;
const CMD_READ_KEYINFO: u8 = 9;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const i8) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: MachPort, matching: *mut c_void) -> IoObject;
    fn IOServiceOpen(service: IoObject, owning_task: MachPort, kind: u32, conn: *mut IoConnect) -> KernReturn;
    fn IOServiceClose(conn: IoConnect) -> KernReturn;
    fn IOObjectRelease(obj: IoObject) -> KernReturn;
    fn IOConnectCallStructMethod(
        conn: IoConnect,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> KernReturn;
}

unsafe extern "C" {
    static mach_task_self_: MachPort;
}

// Mirrors the kernel's SMCKeyData_t (80 bytes). Field order and padding matter.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Vers {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PLimit {
    version: u16,
    length: u16,
    cpu: u32,
    gpu: u32,
    mem: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct KeyInfo {
    data_size: u32,
    data_type: u32,
    attributes: u8,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct KeyData {
    key: u32,
    vers: Vers,
    p_limit: PLimit,
    key_info: KeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<KeyData>() == 80);

pub fn fourcc(s: &str) -> u32 {
    let b = s.as_bytes();
    assert!(b.len() == 4, "SMC keys are 4 characters: {s:?}");
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

pub fn fourcc_str(v: u32) -> String {
    v.to_be_bytes().iter().map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { '?' }).collect()
}

#[derive(Debug)]
pub enum Error {
    NoSmc,
    Open(KernReturn),
    Call(KernReturn),
    Smc { key: String, code: u8 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::NoSmc => write!(f, "AppleSMC service not found"),
            Error::Open(k) => write!(f, "IOServiceOpen failed: {k:#x}"),
            Error::Call(k) => write!(f, "IOConnectCallStructMethod failed: {k:#x}"),
            Error::Smc { key, code } => write!(f, "SMC error {code} on key {key}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug)]
pub struct Value {
    pub key: String,
    pub kind: String,
    pub bytes: Vec<u8>,
}

impl Value {
    /// Decode the common numeric SMC types. None for types we don't understand.
    pub fn as_f64(&self) -> Option<f64> {
        let b = &self.bytes;
        let be16 = |b: &[u8]| u16::from_be_bytes([b[0], b[1]]);
        Some(match (self.kind.as_str(), b.len()) {
            ("flt ", 4) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            ("fpe2", 2) => be16(b) as f64 / 4.0,
            ("sp78", 2) => be16(b) as i16 as f64 / 256.0,
            ("ui8 ", 1) | ("flag", 1) => b[0] as f64,
            ("si8 ", 1) => b[0] as i8 as f64,
            ("ui16", 2) => be16(b) as f64,
            ("si16", 2) => be16(b) as i16 as f64,
            ("ui32", 4) => u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64,
            _ => return None,
        })
    }
}

pub struct Smc {
    conn: IoConnect,
}

impl Smc {
    pub fn open() -> Result<Self, Error> {
        unsafe {
            let matching = IOServiceMatching(c"AppleSMC".as_ptr());
            let service = IOServiceGetMatchingService(0, matching);
            if service == 0 {
                return Err(Error::NoSmc);
            }
            let mut conn = 0;
            let kr = IOServiceOpen(service, mach_task_self_, 0, &mut conn);
            IOObjectRelease(service);
            if kr != KERN_SUCCESS {
                return Err(Error::Open(kr));
            }
            Ok(Smc { conn })
        }
    }

    fn call(&self, input: &KeyData) -> Result<KeyData, Error> {
        let mut out = KeyData::default();
        let mut out_size = std::mem::size_of::<KeyData>();
        let kr = unsafe {
            IOConnectCallStructMethod(
                self.conn,
                KERNEL_INDEX_SMC,
                input as *const _ as *const c_void,
                std::mem::size_of::<KeyData>(),
                &mut out as *mut _ as *mut c_void,
                &mut out_size,
            )
        };
        if kr != KERN_SUCCESS {
            return Err(Error::Call(kr));
        }
        if out.result != 0 {
            return Err(Error::Smc { key: fourcc_str(input.key), code: out.result });
        }
        Ok(out)
    }

    pub fn read(&self, key: &str) -> Result<Value, Error> {
        self.read_raw(fourcc(key))
    }

    fn read_raw(&self, key: u32) -> Result<Value, Error> {
        let info = self.call(&KeyData { key, data8: CMD_READ_KEYINFO, ..Default::default() })?.key_info;
        let mut req = KeyData { key, data8: CMD_READ_BYTES, ..Default::default() };
        req.key_info.data_size = info.data_size;
        let out = self.call(&req)?;
        let n = (info.data_size as usize).min(32);
        Ok(Value { key: fourcc_str(key), kind: fourcc_str(info.data_type), bytes: out.bytes[..n].to_vec() })
    }

    /// Every key the SMC knows about, in index order.
    pub fn keys(&self) -> Result<Vec<String>, Error> {
        let count = self.read("#KEY")?.as_f64().unwrap_or(0.0) as u32;
        let mut keys = Vec::with_capacity(count as usize);
        for i in 0..count {
            let out = self.call(&KeyData { data8: CMD_READ_INDEX, data32: i, ..Default::default() })?;
            keys.push(fourcc_str(out.key));
        }
        Ok(keys)
    }
}

impl Drop for Smc {
    fn drop(&mut self) {
        unsafe {
            IOServiceClose(self.conn);
        }
    }
}

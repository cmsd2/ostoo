//! High-level 9P2000.L client wrapping `VirtIO9p`.

use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;
use virtio_drivers::device::virtio_9p::VirtIO9p;
use virtio_drivers::transport::pci::PciTransport;

use super::KernelHal;
use super::p9_proto::*;

// ---------------------------------------------------------------------------
// Constants

const DEFAULT_MSIZE: u32 = 8192;
const VERSION_9P2000_L: &str = "9P2000.L";
const ROOT_FID: u32 = 0;
const NO_AFID: u32 = u32::MAX;
const TAG: u16 = 1;

// S_IFMT and type bits for mode decoding
const S_IFDIR: u32 = 0o040000;
const S_IFMT:  u32 = 0o170000;

// ---------------------------------------------------------------------------
// P9Client

pub struct P9Client {
    device: Mutex<VirtIO9p<KernelHal, PciTransport>>,
    msize:  u32,
    next_fid: Mutex<u32>,
}

// VirtIO9p contains raw pointers (DMA buffers). Access is serialised
// through the spin::Mutex so these impls are sound.
unsafe impl Send for P9Client {}
unsafe impl Sync for P9Client {}

impl P9Client {
    /// Create a new 9P client, performing the version + attach handshake.
    pub fn new(transport: PciTransport) -> Result<Self, P9Error> {
        let mut device = VirtIO9p::<KernelHal, PciTransport>::new(transport)
            .map_err(|_| P9Error::DeviceError)?;

        // Tversion
        let req = encode_tversion(DEFAULT_MSIZE, VERSION_9P2000_L);
        let mut resp = vec![0u8; DEFAULT_MSIZE as usize];
        let n = device.request(&req, &mut resp).map_err(|_| P9Error::DeviceError)?;
        let payload = check_response(&resp, n, RVERSION)?;
        let (msize, _version) = decode_rversion(payload)?;

        // Tattach — attach root fid
        let req = encode_tattach(TAG, ROOT_FID, NO_AFID, "", "");
        let n = device.request(&req, &mut resp).map_err(|_| P9Error::DeviceError)?;
        let payload = check_response(&resp, n, RATTACH)?;
        let _root_qid = decode_rattach(payload)?;

        Ok(P9Client {
            device: Mutex::new(device),
            msize,
            next_fid: Mutex::new(ROOT_FID + 1),
        })
    }

    fn alloc_fid(&self) -> u32 {
        let mut next = self.next_fid.lock();
        let fid = *next;
        *next = fid + 1;
        fid
    }

    /// Send a request and return the validated payload for `expected_type`.
    fn request(&self, req: &[u8], resp: &mut [u8], expected_type: u8) -> Result<Vec<u8>, P9Error> {
        let mut dev = self.device.lock();
        let n = dev.request(req, resp).map_err(|_| P9Error::DeviceError)?;
        let payload = check_response(resp, n, expected_type)?;
        Ok(Vec::from(payload))
    }

    /// Walk from root to `path`, returning the new fid.
    /// The caller must clunk the returned fid when done.
    fn walk(&self, path: &str) -> Result<u32, P9Error> {
        let newfid = self.alloc_fid();

        // Split path into components, filtering empty segments.
        let names: Vec<&str> = path.split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let req = encode_twalk(TAG, ROOT_FID, newfid, &names);
        let mut resp = vec![0u8; self.msize as usize];
        let payload = self.request(&req, &mut resp, RWALK)?;
        let qids = decode_rwalk(&payload)?;

        // The server must return exactly as many qids as name components,
        // unless names is empty (walk to root).
        if !names.is_empty() && qids.len() != names.len() {
            // Partial walk — path not found. Clunk the newfid.
            let _ = self.clunk(newfid);
            return Err(P9Error::ServerError(2)); // ENOENT
        }

        Ok(newfid)
    }

    fn lopen(&self, fid: u32, flags: u32) -> Result<(Qid, u32), P9Error> {
        let req = encode_tlopen(TAG, fid, flags);
        let mut resp = vec![0u8; self.msize as usize];
        let payload = self.request(&req, &mut resp, RLOPEN)?;
        decode_rlopen(&payload)
    }

    fn getattr(&self, fid: u32) -> Result<Stat9p, P9Error> {
        let req = encode_tgetattr(TAG, fid, P9_GETATTR_BASIC);
        let mut resp = vec![0u8; self.msize as usize];
        let payload = self.request(&req, &mut resp, RGETATTR)?;
        decode_rgetattr(&payload)
    }

    fn read_chunk(&self, fid: u32, offset: u64, count: u32) -> Result<Vec<u8>, P9Error> {
        let req = encode_tread(TAG, fid, offset, count);
        let mut resp = vec![0u8; self.msize as usize];
        let mut dev = self.device.lock();
        let n = dev.request(&req, &mut resp).map_err(|_| P9Error::DeviceError)?;
        let payload = check_response(&resp, n, RREAD)?;
        let data = decode_rread(payload)?;
        Ok(Vec::from(data))
    }

    fn readdir_chunk(&self, fid: u32, offset: u64, count: u32) -> Result<Vec<DirEntry9p>, P9Error> {
        let req = encode_treaddir(TAG, fid, offset, count);
        let mut resp = vec![0u8; self.msize as usize];
        let mut dev = self.device.lock();
        let n = dev.request(&req, &mut resp).map_err(|_| P9Error::DeviceError)?;
        let payload = check_response(&resp, n, RREADDIR)?;
        decode_rreaddir(payload)
    }

    fn clunk(&self, fid: u32) -> Result<(), P9Error> {
        let req = encode_tclunk(TAG, fid);
        let mut resp = vec![0u8; self.msize as usize];
        let _ = self.request(&req, &mut resp, RCLUNK);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Public API

    /// List directory entries at `path`.
    pub fn list_dir(&self, path: &str) -> Result<Vec<DirEntry9p>, P9Error> {
        let fid = self.walk(path)?;
        self.lopen(fid, L_O_RDONLY)?;

        let mut all_entries = Vec::new();
        let mut offset: u64 = 0;
        let read_count = self.msize - 64; // leave room for header overhead

        loop {
            let entries = self.readdir_chunk(fid, offset, read_count)?;
            if entries.is_empty() { break; }
            // The offset for the next readdir call is the offset field of the
            // last returned entry.
            offset = entries.last().unwrap().offset;
            all_entries.extend(entries);
        }

        self.clunk(fid)?;

        // Filter out "." and ".."
        all_entries.retain(|e| e.name != "." && e.name != "..");
        Ok(all_entries)
    }

    /// Read the entire contents of a file at `path`.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, P9Error> {
        let fid = self.walk(path)?;

        // Get file size first.
        let stat = self.getattr(fid)?;
        let file_size = stat.size;

        self.lopen(fid, L_O_RDONLY)?;

        let mut data = Vec::with_capacity(file_size as usize);
        let mut offset: u64 = 0;
        let chunk_size = self.msize - 64; // leave room for header overhead

        loop {
            let chunk = self.read_chunk(fid, offset, chunk_size)?;
            if chunk.is_empty() { break; }
            offset += chunk.len() as u64;
            data.extend_from_slice(&chunk);
        }

        self.clunk(fid)?;
        Ok(data)
    }

    /// Get file attributes (mode, size) for the given path.
    pub fn stat(&self, path: &str) -> Result<Stat9p, P9Error> {
        let fid = self.walk(path)?;
        let stat = self.getattr(fid)?;
        self.clunk(fid)?;
        Ok(stat)
    }

    /// Returns true if the stat mode indicates a directory.
    pub fn is_dir(mode: u32) -> bool {
        (mode & S_IFMT) == S_IFDIR
    }
}

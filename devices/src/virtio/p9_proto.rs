//! Minimal 9P2000.L wire protocol encoding/decoding.
//!
//! Only the subset needed for read-only host directory sharing is implemented:
//! version, attach, walk, lopen, read, readdir, getattr, clunk.

use alloc::string::String;
use alloc::vec::Vec;
use core::convert::TryInto;

// ---------------------------------------------------------------------------
// Error type

#[derive(Debug)]
pub enum P9Error {
    BufferTooSmall,
    InvalidResponse,
    ServerError(u32),
    Utf8Error,
    DeviceError,
}

// ---------------------------------------------------------------------------
// 9P2000.L message type constants

pub const RLERROR:  u8 = 7;
pub const TLOPEN:   u8 = 12;
pub const RLOPEN:   u8 = 13;
pub const TGETATTR: u8 = 24;
pub const RGETATTR: u8 = 25;
pub const TREADDIR: u8 = 40;
pub const RREADDIR: u8 = 41;
pub const TVERSION: u8 = 100;
pub const RVERSION: u8 = 101;
pub const TATTACH:  u8 = 104;
pub const RATTACH:  u8 = 105;
pub const TWALK:    u8 = 110;
pub const RWALK:    u8 = 111;
pub const TREAD:    u8 = 116;
pub const RREAD:    u8 = 117;
pub const TCLUNK:   u8 = 120;
pub const RCLUNK:   u8 = 121;

/// 9P2000.L open flags: read-only.
pub const L_O_RDONLY: u32 = 0;

/// getattr request mask: request mode + size.
pub const P9_GETATTR_MODE: u64 = 0x0000_0001;
pub const P9_GETATTR_SIZE: u64 = 0x0000_0200;
pub const P9_GETATTR_BASIC: u64 = P9_GETATTR_MODE | P9_GETATTR_SIZE;

// ---------------------------------------------------------------------------
// Wire types

#[derive(Debug, Clone, Copy)]
pub struct Qid {
    pub qid_type: u8,
    pub version:  u32,
    pub path:     u64,
}

#[derive(Debug, Clone)]
pub struct DirEntry9p {
    pub qid:    Qid,
    pub offset: u64,
    pub dtype:  u8,
    pub name:   String,
}

#[derive(Debug, Clone)]
pub struct Stat9p {
    pub mode: u32,
    pub size: u64,
    pub qid:  Qid,
}

// ---------------------------------------------------------------------------
// Header helpers

const HEADER_SIZE: usize = 7; // size[4] + type[1] + tag[2]

/// Begin a message of given type and tag, returning a buffer with the header
/// placeholder (size will be patched by `finish`).
fn begin(msg_type: u8, tag: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&[0u8; 4]); // size placeholder
    buf.push(msg_type);
    buf.extend_from_slice(&tag.to_le_bytes());
    buf
}

/// Patch the size field at the start of the buffer.
fn finish(buf: &mut Vec<u8>) {
    let size = buf.len() as u32;
    buf[0..4].copy_from_slice(&size.to_le_bytes());
}

fn put_u16(buf: &mut Vec<u8>, v: u16) { buf.extend_from_slice(&v.to_le_bytes()); }
fn put_u32(buf: &mut Vec<u8>, v: u32) { buf.extend_from_slice(&v.to_le_bytes()); }
fn put_u64(buf: &mut Vec<u8>, v: u64) { buf.extend_from_slice(&v.to_le_bytes()); }
fn put_str(buf: &mut Vec<u8>, s: &str) {
    put_u16(buf, s.len() as u16);
    buf.extend_from_slice(s.as_bytes());
}

fn get_u8(data: &[u8], off: &mut usize) -> Result<u8, P9Error> {
    if *off + 1 > data.len() { return Err(P9Error::BufferTooSmall); }
    let v = data[*off];
    *off += 1;
    Ok(v)
}

fn get_u16(data: &[u8], off: &mut usize) -> Result<u16, P9Error> {
    if *off + 2 > data.len() { return Err(P9Error::BufferTooSmall); }
    let v = u16::from_le_bytes(data[*off..*off + 2].try_into().unwrap());
    *off += 2;
    Ok(v)
}

fn get_u32(data: &[u8], off: &mut usize) -> Result<u32, P9Error> {
    if *off + 4 > data.len() { return Err(P9Error::BufferTooSmall); }
    let v = u32::from_le_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Ok(v)
}

fn get_u64(data: &[u8], off: &mut usize) -> Result<u64, P9Error> {
    if *off + 8 > data.len() { return Err(P9Error::BufferTooSmall); }
    let v = u64::from_le_bytes(data[*off..*off + 8].try_into().unwrap());
    *off += 8;
    Ok(v)
}

fn get_str(data: &[u8], off: &mut usize) -> Result<String, P9Error> {
    let len = get_u16(data, off)? as usize;
    if *off + len > data.len() { return Err(P9Error::BufferTooSmall); }
    let s = core::str::from_utf8(&data[*off..*off + len])
        .map_err(|_| P9Error::Utf8Error)?;
    *off += len;
    Ok(String::from(s))
}

fn get_qid(data: &[u8], off: &mut usize) -> Result<Qid, P9Error> {
    let qid_type = get_u8(data, off)?;
    let version  = get_u32(data, off)?;
    let path     = get_u64(data, off)?;
    Ok(Qid { qid_type, version, path })
}

/// Check the response header: verify type matches expected (or is Rlerror).
/// Returns the payload slice (after header).
pub fn check_response(resp: &[u8], resp_len: u32, expected_type: u8) -> Result<&[u8], P9Error> {
    let data = &resp[..resp_len as usize];
    if data.len() < HEADER_SIZE { return Err(P9Error::InvalidResponse); }
    let msg_type = data[4];
    if msg_type == RLERROR {
        let mut off = HEADER_SIZE;
        let ecode = get_u32(data, &mut off)?;
        return Err(P9Error::ServerError(ecode));
    }
    if msg_type != expected_type {
        return Err(P9Error::InvalidResponse);
    }
    Ok(&data[HEADER_SIZE..])
}

// ---------------------------------------------------------------------------
// Tversion / Rversion

pub fn encode_tversion(msize: u32, version: &str) -> Vec<u8> {
    let mut buf = begin(TVERSION, 0xFFFF); // NOTAG
    put_u32(&mut buf, msize);
    put_str(&mut buf, version);
    finish(&mut buf);
    buf
}

pub fn decode_rversion(payload: &[u8]) -> Result<(u32, String), P9Error> {
    let mut off = 0;
    let msize   = get_u32(payload, &mut off)?;
    let version = get_str(payload, &mut off)?;
    Ok((msize, version))
}

// ---------------------------------------------------------------------------
// Tattach / Rattach

pub fn encode_tattach(tag: u16, fid: u32, afid: u32, uname: &str, aname: &str) -> Vec<u8> {
    let mut buf = begin(TATTACH, tag);
    put_u32(&mut buf, fid);
    put_u32(&mut buf, afid);
    put_str(&mut buf, uname);
    put_str(&mut buf, aname);
    put_u32(&mut buf, u32::MAX); // n_uname = NONUNAME
    finish(&mut buf);
    buf
}

pub fn decode_rattach(payload: &[u8]) -> Result<Qid, P9Error> {
    let mut off = 0;
    get_qid(payload, &mut off)
}

// ---------------------------------------------------------------------------
// Twalk / Rwalk

pub fn encode_twalk(tag: u16, fid: u32, newfid: u32, names: &[&str]) -> Vec<u8> {
    let mut buf = begin(TWALK, tag);
    put_u32(&mut buf, fid);
    put_u32(&mut buf, newfid);
    put_u16(&mut buf, names.len() as u16);
    for name in names {
        put_str(&mut buf, name);
    }
    finish(&mut buf);
    buf
}

pub fn decode_rwalk(payload: &[u8]) -> Result<Vec<Qid>, P9Error> {
    let mut off = 0;
    let nwqid = get_u16(payload, &mut off)? as usize;
    let mut qids = Vec::with_capacity(nwqid);
    for _ in 0..nwqid {
        qids.push(get_qid(payload, &mut off)?);
    }
    Ok(qids)
}

// ---------------------------------------------------------------------------
// Tlopen / Rlopen

pub fn encode_tlopen(tag: u16, fid: u32, flags: u32) -> Vec<u8> {
    let mut buf = begin(TLOPEN, tag);
    put_u32(&mut buf, fid);
    put_u32(&mut buf, flags);
    finish(&mut buf);
    buf
}

pub fn decode_rlopen(payload: &[u8]) -> Result<(Qid, u32), P9Error> {
    let mut off = 0;
    let qid    = get_qid(payload, &mut off)?;
    let iounit = get_u32(payload, &mut off)?;
    Ok((qid, iounit))
}

// ---------------------------------------------------------------------------
// Tread / Rread

pub fn encode_tread(tag: u16, fid: u32, offset: u64, count: u32) -> Vec<u8> {
    let mut buf = begin(TREAD, tag);
    put_u32(&mut buf, fid);
    put_u64(&mut buf, offset);
    put_u32(&mut buf, count);
    finish(&mut buf);
    buf
}

/// Decode Rread: returns the data bytes.
pub fn decode_rread(payload: &[u8]) -> Result<&[u8], P9Error> {
    let mut off = 0;
    let count = get_u32(payload, &mut off)? as usize;
    if off + count > payload.len() { return Err(P9Error::BufferTooSmall); }
    Ok(&payload[off..off + count])
}

// ---------------------------------------------------------------------------
// Treaddir / Rreaddir

pub fn encode_treaddir(tag: u16, fid: u32, offset: u64, count: u32) -> Vec<u8> {
    let mut buf = begin(TREADDIR, tag);
    put_u32(&mut buf, fid);
    put_u64(&mut buf, offset);
    put_u32(&mut buf, count);
    finish(&mut buf);
    buf
}

/// Decode Rreaddir: returns a list of directory entries.
pub fn decode_rreaddir(payload: &[u8]) -> Result<Vec<DirEntry9p>, P9Error> {
    let mut off = 0;
    let count = get_u32(payload, &mut off)? as usize;
    let end = off + count;
    if end > payload.len() { return Err(P9Error::BufferTooSmall); }
    let mut entries = Vec::new();
    while off < end {
        let qid    = get_qid(payload, &mut off)?;
        let offset = get_u64(payload, &mut off)?;
        let dtype  = get_u8(payload, &mut off)?;
        let name   = get_str(payload, &mut off)?;
        entries.push(DirEntry9p { qid, offset, dtype, name });
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Tgetattr / Rgetattr

pub fn encode_tgetattr(tag: u16, fid: u32, request_mask: u64) -> Vec<u8> {
    let mut buf = begin(TGETATTR, tag);
    put_u32(&mut buf, fid);
    put_u64(&mut buf, request_mask);
    finish(&mut buf);
    buf
}

/// Decode Rgetattr: extract mode, size, and qid from the fixed-layout response.
pub fn decode_rgetattr(payload: &[u8]) -> Result<Stat9p, P9Error> {
    let mut off = 0;
    let _valid = get_u64(payload, &mut off)?;
    let qid    = get_qid(payload, &mut off)?;
    let mode   = get_u32(payload, &mut off)?;
    let _uid   = get_u32(payload, &mut off)?;
    let _gid   = get_u32(payload, &mut off)?;
    let _nlink = get_u64(payload, &mut off)?;
    let _rdev  = get_u64(payload, &mut off)?;
    let size   = get_u64(payload, &mut off)?;
    // Remaining fields (blksize, blocks, timestamps) are ignored.
    Ok(Stat9p { mode, size, qid })
}

// ---------------------------------------------------------------------------
// Tclunk / Rclunk

pub fn encode_tclunk(tag: u16, fid: u32) -> Vec<u8> {
    let mut buf = begin(TCLUNK, tag);
    put_u32(&mut buf, fid);
    finish(&mut buf);
    buf
}

pub fn decode_rclunk(_payload: &[u8]) -> Result<(), P9Error> {
    Ok(())
}

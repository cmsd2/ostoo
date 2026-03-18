use alloc::string::String;
use alloc::vec::Vec;
use alloc::sync::Arc;
use core::convert::TryInto;

use libkernel::task::mailbox::{ActorMsg, Mailbox};

use super::blk::{VirtioBlkMsg, VirtioBlkInfo};

// ---------------------------------------------------------------------------
// Type alias for the block device mailbox

pub type BlkInbox = Arc<Mailbox<ActorMsg<VirtioBlkMsg, VirtioBlkInfo>>>;

// ---------------------------------------------------------------------------
// Public error type

#[derive(Debug)]
pub enum ExfatError {
    NoDevice,
    IoError,
    NotExfat,
    UnknownPartitionLayout,
    PathNotFound,
    NotAFile,
    NotADirectory,
    FileTooLarge,
}

// ---------------------------------------------------------------------------
// Public types

/// A directory entry returned by `list_dir`.
pub struct DirEntry {
    pub name:   String,
    pub is_dir: bool,
    pub size:   u64,
    /// First data cluster — used internally for traversal; not part of the
    /// stable public API surface.
    pub(crate) first_cluster: u32,
    /// If true, clusters are contiguous from `first_cluster`; do not follow
    /// the FAT chain.
    pub(crate) no_fat_chain: bool,
}

/// Parsed exFAT volume state.
pub struct ExfatVol {
    /// Absolute LBA of the exFAT boot sector.
    pub lba_base:            u64,
    /// Sectors per cluster (always a power of two).
    pub sectors_per_cluster: u64,
    /// Absolute LBA of the FAT.
    pub fat_lba:             u64,
    /// Absolute LBA of the cluster heap (data region).
    pub cluster_heap_lba:    u64,
    /// First cluster of the root directory.
    pub root_cluster:        u32,
}

// ---------------------------------------------------------------------------
// GPT "Microsoft Basic Data" partition type GUID (mixed-endian on disk)
// EBD0A0A2-B9E5-4433-87C0-68B6B72699C7

const GPT_BASIC_DATA_GUID: [u8; 16] = [
    0xA2, 0xA0, 0xD0, 0xEB,
    0xE5, 0xB9,
    0x33, 0x44,
    0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];

// ---------------------------------------------------------------------------
// Internal: read a single 512-byte sector via the virtio-blk actor.

async fn read_sector(inbox: &BlkInbox, lba: u64) -> Result<Vec<u8>, ExfatError> {
    let result = inbox.ask(|reply| {
        ActorMsg::Inner(VirtioBlkMsg::Read(lba, reply))
    }).await;
    match result {
        Some(Ok(buf)) => Ok(buf),
        Some(Err(())) => Err(ExfatError::IoError),
        None          => Err(ExfatError::NoDevice),
    }
}

// ---------------------------------------------------------------------------
// Partition auto-detection

/// Open the exFAT volume on the given block device.
///
/// Detects bare exFAT, MBR-partitioned exFAT, and GPT-partitioned exFAT
/// automatically.
pub async fn open_exfat(inbox: &BlkInbox) -> Result<ExfatVol, ExfatError> {
    let sector0 = read_sector(inbox, 0).await?;

    // Bare exFAT — volume starts at LBA 0.
    if &sector0[3..11] == b"EXFAT   " {
        return parse_exfat_boot(&sector0, 0);
    }

    // Need a valid MBR/protective-MBR signature to proceed.
    if sector0[510] != 0x55 || sector0[511] != 0xAA {
        return Err(ExfatError::UnknownPartitionLayout);
    }

    // Read LBA 1 to distinguish GPT from MBR.
    let sector1 = read_sector(inbox, 1).await?;

    if &sector1[0..8] == b"EFI PART" {
        find_exfat_gpt(inbox, &sector1).await
    } else {
        find_exfat_mbr(inbox, &sector0).await
    }
}

// ---------------------------------------------------------------------------
// GPT layout

async fn find_exfat_gpt(inbox: &BlkInbox, gpt_header: &[u8]) -> Result<ExfatVol, ExfatError> {
    // GPT header field offsets (UEFI spec 2.x):
    //   72..80  PartitionEntryLBA       (u64 LE)
    //   80..84  NumberOfPartitionEntries (u32 LE)
    //   84..88  SizeOfPartitionEntry    (u32 LE)
    let entry_lba  = u64::from_le_bytes(gpt_header[72..80].try_into().unwrap());
    let num_parts  = u32::from_le_bytes(gpt_header[80..84].try_into().unwrap()) as usize;
    let entry_size = u32::from_le_bytes(gpt_header[84..88].try_into().unwrap()) as usize;

    if entry_size == 0 || entry_size > 512 {
        return Err(ExfatError::UnknownPartitionLayout);
    }

    let entries_per_sector = 512 / entry_size;
    let num_sectors = (num_parts + entries_per_sector - 1) / entries_per_sector;

    for sector_idx in 0..num_sectors {
        let lba = entry_lba + sector_idx as u64;
        let sector = read_sector(inbox, lba).await?;

        for entry_idx in 0..entries_per_sector {
            let off = entry_idx * entry_size;
            if off + entry_size > 512 { break; }

            let entry = &sector[off..off + entry_size];

            // Skip empty entries (type GUID all-zero).
            if entry[0..16].iter().all(|&b| b == 0) { continue; }

            // Match "Microsoft Basic Data" type GUID.
            if entry[0..16] == GPT_BASIC_DATA_GUID {
                // GPT partition entry layout:
                //   32..40  StartingLBA (u64 LE)
                let start_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
                let boot = read_sector(inbox, start_lba).await?;
                if &boot[3..11] == b"EXFAT   " {
                    return parse_exfat_boot(&boot, start_lba);
                }
            }
        }
    }

    Err(ExfatError::NotExfat)
}

// ---------------------------------------------------------------------------
// MBR layout

async fn find_exfat_mbr(inbox: &BlkInbox, mbr: &[u8]) -> Result<ExfatVol, ExfatError> {
    // MBR partition table: bytes 446..510, four 16-byte entries.
    for i in 0..4usize {
        let off   = 446 + i * 16;
        let ptype = mbr[off + 4];

        // exFAT and NTFS both use type 0x07 — verify by reading the volume.
        if ptype == 0x07 {
            let lba_start = u32::from_le_bytes(mbr[off + 8..off + 12].try_into().unwrap()) as u64;
            if lba_start == 0 { continue; }

            let boot = read_sector(inbox, lba_start).await?;
            if &boot[3..11] == b"EXFAT   " {
                return parse_exfat_boot(&boot, lba_start);
            }
        }
    }

    Err(ExfatError::NotExfat)
}

// ---------------------------------------------------------------------------
// Boot-sector parser

fn parse_exfat_boot(boot: &[u8], lba_base: u64) -> Result<ExfatVol, ExfatError> {
    if &boot[3..11] != b"EXFAT   " {
        return Err(ExfatError::NotExfat);
    }
    if boot[510] != 0x55 || boot[511] != 0xAA {
        return Err(ExfatError::NotExfat);
    }

    // exFAT boot sector fields (all offsets from spec):
    //   80..84   FatOffset              (u32 LE) sectors from volume start
    //   88..92   ClusterHeapOffset      (u32 LE) sectors from volume start
    //   96..100  FirstClusterOfRootDir  (u32 LE)
    //   109      SectorsPerClusterShift (u8)
    let fat_offset          = u32::from_le_bytes(boot[80..84].try_into().unwrap()) as u64;
    let cluster_heap_offset = u32::from_le_bytes(boot[88..92].try_into().unwrap()) as u64;
    let root_cluster        = u32::from_le_bytes(boot[96..100].try_into().unwrap());
    let spc_shift           = boot[109] as u64;
    let sectors_per_cluster = 1u64 << spc_shift;

    Ok(ExfatVol {
        lba_base,
        sectors_per_cluster,
        fat_lba:          lba_base + fat_offset,
        cluster_heap_lba: lba_base + cluster_heap_offset,
        root_cluster,
    })
}

// ---------------------------------------------------------------------------
// FAT entry

async fn read_fat_entry(vol: &ExfatVol, inbox: &BlkInbox, cluster: u32) -> Result<u32, ExfatError> {
    let byte_off    = cluster as u64 * 4;
    let sector_lba  = vol.fat_lba + byte_off / 512;
    let sector_off  = (byte_off % 512) as usize;

    let sector = read_sector(inbox, sector_lba).await?;
    Ok(u32::from_le_bytes(sector[sector_off..sector_off + 4].try_into().unwrap()))
}

// ---------------------------------------------------------------------------
// Directory scanning

/// Read all `DirEntry` values from the directory rooted at `cluster`.
///
/// Follows the FAT chain across multiple clusters if the directory spans more
/// than one.  Entry sets that span a cluster boundary are silently dropped
/// (very rare on well-formed images).
async fn scan_dir_cluster(
    vol: &ExfatVol,
    inbox: &BlkInbox,
    cluster: u32,
) -> Result<Vec<DirEntry>, ExfatError> {
    let mut entries = Vec::new();
    let mut current = cluster;
    const MAX_CLUSTERS: usize = 1_000;

    'chain: for _ in 0..MAX_CLUSTERS {
        if current < 2 || current >= 0xFFFF_FFF8 { break; }
        // Collect all sectors of this cluster into a flat buffer so that
        // entry sets crossing sector boundaries are handled naturally.
        let cluster_lba = vol.cluster_heap_lba + (current as u64 - 2) * vol.sectors_per_cluster;
        let mut cluster_data: Vec<u8> = Vec::new();
        for s in 0..vol.sectors_per_cluster {
            let sector = read_sector(inbox, cluster_lba + s).await?;
            cluster_data.extend_from_slice(&sector);
        }

        let mut i = 0usize;
        while i + 32 <= cluster_data.len() {
            let etype = cluster_data[i];

            if etype == 0x00 {
                // End-of-directory marker — stop completely.
                break 'chain;
            }

            if etype == 0x85 {
                // Primary "File" entry — begins an entry set.
                let secondary_count = cluster_data[i + 1] as usize;

                // Bounds-check: the whole set must fit in this cluster.
                if i + (1 + secondary_count) * 32 > cluster_data.len() {
                    // Entry set crosses a cluster boundary — skip gracefully.
                    i += 32;
                    continue;
                }

                let file_attrs = u16::from_le_bytes(
                    [cluster_data[i + 4], cluster_data[i + 5]]
                );
                let is_dir = file_attrs & (1 << 4) != 0;

                let mut data_length    = 0u64;
                let mut first_cluster  = 0u32;
                let mut no_fat_chain   = false;
                let mut name_chars: Vec<u16> = Vec::new();

                for j in 1..=secondary_count {
                    let off = i + j * 32;
                    let stype = cluster_data[off];

                    match stype {
                        0xC0 => {
                            // Stream Extension:
                            //   +1       GeneralSecondaryFlags (bit 1 = NoFatChain)
                            //   +8..+16  DataLength   (u64 LE)
                            //   +20..+24 FirstCluster (u32 LE)
                            let s = &cluster_data[off..off + 32];
                            no_fat_chain  = s[1] & 0x02 != 0;
                            data_length   = u64::from_le_bytes(s[8..16].try_into().unwrap());
                            first_cluster = u32::from_le_bytes(s[20..24].try_into().unwrap());
                        }
                        0xC1 => {
                            // File Name: 15 UTF-16LE code units at +2..+32.
                            let s = &cluster_data[off..off + 32];
                            for k in 0..15usize {
                                let w = u16::from_le_bytes([s[2 + k * 2], s[2 + k * 2 + 1]]);
                                if w == 0 { break; }
                                name_chars.push(w);
                            }
                        }
                        _ => {}
                    }
                }

                let name = utf16_to_string(&name_chars);
                if !name.is_empty() {
                    entries.push(DirEntry { name, is_dir, size: data_length, first_cluster, no_fat_chain });
                }

                i += (1 + secondary_count) * 32;
            } else if etype < 0x80 {
                // Unused / deleted entry — skip.
                i += 32;
            } else {
                // Other secondary entry we don't need — skip.
                i += 32;
            }
        }

        // Follow the FAT chain.
        let next = read_fat_entry(vol, inbox, current).await?;
        if next >= 0xFFFF_FFF8 {
            break;
        }
        current = next;
    }

    Ok(entries)
}

// ---------------------------------------------------------------------------
// UTF-16LE → String helper

fn utf16_to_string(chars: &[u16]) -> String {
    let mut s = String::new();
    for &c in chars {
        if c == 0 { break; }
        if c < 0x80 {
            s.push(c as u8 as char);
        } else {
            s.push('?');
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Path traversal

/// Walk a path (e.g. `"/"`, `"/docs"`, `"/docs/readme.txt"`) and return the
/// matching `DirEntry`, or an error.
async fn walk_path(vol: &ExfatVol, inbox: &BlkInbox, path: &str) -> Result<DirEntry, ExfatError> {
    let components: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    // Virtual root entry.
    let mut current = DirEntry {
        name:          String::new(),
        is_dir:        true,
        size:          0,
        first_cluster: vol.root_cluster,
        no_fat_chain:  false,
    };

    for component in &components {
        if !current.is_dir {
            return Err(ExfatError::NotADirectory);
        }
        let listing = scan_dir_cluster(vol, inbox, current.first_cluster).await?;
        let found = listing.into_iter().find(|e| e.name.eq_ignore_ascii_case(component));
        match found {
            Some(e) => current = e,
            None    => return Err(ExfatError::PathNotFound),
        }
    }

    Ok(current)
}

// ---------------------------------------------------------------------------
// Public API

/// List the directory at `path`.  Use `"/"` for the root.
pub async fn list_dir(
    vol:   &ExfatVol,
    inbox: &BlkInbox,
    path:  &str,
) -> Result<Vec<DirEntry>, ExfatError> {
    let dir = walk_path(vol, inbox, path).await?;
    if !dir.is_dir {
        return Err(ExfatError::NotADirectory);
    }
    scan_dir_cluster(vol, inbox, dir.first_cluster).await
}

/// Read a file into memory.  Capped at 256 KiB to protect the heap.
pub async fn read_file(
    vol:   &ExfatVol,
    inbox: &BlkInbox,
    path:  &str,
) -> Result<Vec<u8>, ExfatError> {
    const MAX_FILE_SIZE: u64 = 256 * 1024;

    let file = walk_path(vol, inbox, path).await?;
    if file.is_dir {
        return Err(ExfatError::NotAFile);
    }
    if file.size > MAX_FILE_SIZE {
        return Err(ExfatError::FileTooLarge);
    }

    let mut data: Vec<u8> = Vec::new();
    let mut cluster        = file.first_cluster;
    let mut bytes_left     = file.size;

    const MAX_CLUSTERS: usize = 1_000;

    for _ in 0..MAX_CLUSTERS {
        if bytes_left == 0 || cluster < 2 || cluster >= 0xFFFF_FFF8 { break; }

        let cluster_lba = vol.cluster_heap_lba + (cluster as u64 - 2) * vol.sectors_per_cluster;

        for s in 0..vol.sectors_per_cluster {
            if bytes_left == 0 { break; }
            let sector     = read_sector(inbox, cluster_lba + s).await?;
            let to_copy    = (bytes_left.min(512)) as usize;
            data.extend_from_slice(&sector[..to_copy]);
            bytes_left     = bytes_left.saturating_sub(to_copy as u64);
        }

        if file.no_fat_chain {
            // Contiguous allocation: clusters are sequential.
            cluster += 1;
        } else {
            let next = read_fat_entry(vol, inbox, cluster).await?;
            cluster = next;
        }
    }

    Ok(data)
}

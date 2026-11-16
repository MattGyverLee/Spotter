//! Which drives count as "local". Network drives are never queried (a disconnected one can block for
//! minutes), and cloud drives such as Google Drive, which present themselves as fixed disks, are left out.

use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;

/// Volume labels of virtual drives that stream files from the cloud.
const CLOUD_LABELS: [&str; 4] = ["google drive", "dropbox", "onedrive", "box"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Local,
    Removable,
    Network,
    Cloud,
    Other,
}

#[derive(Clone, Debug)]
pub struct Drive {
    /// `C:\`
    pub root: String,
    pub kind: Kind,
    pub label: String,
}

impl Drive {
    pub fn describe(&self) -> String {
        let kind = match self.kind {
            Kind::Local => "",
            Kind::Removable => " (removable)",
            Kind::Network => " (network)",
            Kind::Cloud => " (cloud)",
            Kind::Other => "",
        };
        if self.label.is_empty() { format!("{}{kind}", self.root) } else { format!("{}  {}{kind}", self.root, self.label) }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn label_of(root: &[u16]) -> Option<String> {
    let mut name = [0u16; 261];
    let ok = unsafe { GetVolumeInformationW(root.as_ptr(), name.as_mut_ptr(), name.len() as u32, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0) };
    let end = name.iter().position(|&c| c == 0).unwrap_or(0);
    (ok != 0).then(|| String::from_utf16_lossy(&name[..end]))
}

pub fn list() -> Vec<Drive> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| {
            let root = format!("{}:\\", (b'A' + i) as char);
            let w = wide(&root);
            let (kind, label) = match unsafe { GetDriveTypeW(w.as_ptr()) } {
                DRIVE_FIXED => {
                    let label = label_of(&w).unwrap_or_default();
                    let lower = label.to_lowercase();
                    (if CLOUD_LABELS.iter().any(|c| lower.contains(c)) { Kind::Cloud } else { Kind::Local }, label)
                }
                DRIVE_REMOVABLE => (Kind::Removable, label_of(&w).unwrap_or_default()),
                DRIVE_REMOTE => (Kind::Network, String::new()),
                DRIVE_CDROM => (Kind::Other, String::new()),
                _ => (Kind::Other, String::new()),
            };
            Drive { root, kind, label }
        })
        .collect()
}

/// The local drives as a `Look in` value: `C:\;D:\`.
pub fn local_roots(drives: &[Drive]) -> String {
    drives.iter().filter(|d| d.kind == Kind::Local).map(|d| d.root.as_str()).collect::<Vec<_>>().join(";")
}

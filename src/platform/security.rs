//! Token, ACL and security descriptor helpers.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
    GetNamedSecurityInfoW, GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT, SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{
    EqualSid, GetAce, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenUser, ACCESS_ALLOWED_ACE,
    ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SYSTEM_MANDATORY_LABEL_ACE, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::ipc::OwnedHandle;

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0x0;
const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;
const INHERIT_ONLY_ACE: u8 = 0x8;
const SECURITY_MANDATORY_MEDIUM_RID: u32 = 0x2000;

const FILE_WRITE_DATA: u32 = 0x2; // FILE_ADD_FILE on folders
const FILE_APPEND_DATA: u32 = 0x4; // FILE_ADD_SUBDIRECTORY on folders
const FILE_DELETE_CHILD: u32 = 0x40;
const DELETE: u32 = 0x1_0000;
const WRITE_DAC: u32 = 0x4_0000;
const WRITE_OWNER: u32 = 0x8_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

/// SYSTEM, Administrators and TrustedInstaller; the current user is added at runtime.
const TRUSTED_SIDS: [&str; 3] =
    ["S-1-5-18", "S-1-5-32-544", "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"];

/// A SID allocated by `ConvertStringSidToSidW`.
struct LocalSid(PSID);

impl LocalSid {
    fn parse(sid: &str) -> Option<LocalSid> {
        let mut out = PSID::default();
        unsafe { ConvertStringSidToSidW(&HSTRING::from(sid), &mut out) }.ok()?;
        Some(LocalSid(out))
    }
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.0 .0)));
        }
    }
}

/// A security descriptor allocated by the system with `LocalAlloc`.
struct LocalDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            unsafe {
                LocalFree(Some(HLOCAL(self.0 .0)));
            }
        }
    }
}

pub fn current_user_sid() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let token = OwnedHandle(token);
        let mut len = 0u32;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut len);
        // u64 storage keeps TOKEN_USER suitably aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        GetTokenInformation(token.0, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len).ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

/// Attributes for named kernel objects: full access for this user and SYSTEM only, with a
/// Medium integrity label so lower-integrity processes can't open them.
pub struct ObjectAttributes {
    _descriptor: LocalDescriptor,
    attributes: SECURITY_ATTRIBUTES,
}

impl ObjectAttributes {
    pub fn private() -> Option<ObjectAttributes> {
        let sid = current_user_sid()?;
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)S:(ML;;NWNRNX;;;ME)"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut descriptor, None).ok()?;
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        Some(ObjectAttributes { _descriptor: LocalDescriptor(descriptor), attributes })
    }

    pub fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attributes
    }
}

/// True if a named object we didn't create is owned by this user, SYSTEM or Administrators and
/// has no integrity label below Medium, so no other account or lower-integrity process made it
/// first. The handle needs READ_CONTROL.
pub fn existing_object_trusted(handle: HANDLE) -> bool {
    let Some(user) = current_user_sid() else { return false };
    let owners: Vec<LocalSid> =
        [user.as_str(), "S-1-5-18", "S-1-5-32-544"].into_iter().filter_map(LocalSid::parse).collect();
    unsafe {
        let mut owner = PSID::default();
        let mut sacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let err = GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            Some(&mut sacl),
            Some(&mut descriptor),
        );
        let _descriptor = LocalDescriptor(descriptor);
        if err != ERROR_SUCCESS {
            return false;
        }
        if owner.0.is_null() || !owners.iter().any(|o| EqualSid(o.0, owner).is_ok()) {
            return false;
        }
        if sacl.is_null() {
            return true; // No label means Medium.
        }
        for i in 0..u32::from((*sacl).AceCount) {
            let mut ace: *mut c_void = std::ptr::null_mut();
            if GetAce(sacl, i, &mut ace).is_err() {
                continue;
            }
            let header = &*(ace as *const ACE_HEADER);
            if header.AceType != SYSTEM_MANDATORY_LABEL_ACE_TYPE {
                continue;
            }
            let label = &*(ace as *const SYSTEM_MANDATORY_LABEL_ACE);
            let sid = PSID(&label.SidStart as *const u32 as *mut c_void);
            let count = *GetSidSubAuthorityCount(sid);
            if count == 0 {
                return false;
            }
            return *GetSidSubAuthority(sid, u32::from(count) - 1) >= SECURITY_MANDATORY_MEDIUM_RID;
        }
        true
    }
}

/// Returns the exe or the first parent folder that an account other than this user, SYSTEM,
/// Administrators or TrustedInstaller could change to replace the exe. None if the location
/// looks safe or can't be checked.
pub fn unsafe_location(exe: &Path) -> Option<PathBuf> {
    let user = current_user_sid()?;
    let trusted: Vec<LocalSid> =
        TRUSTED_SIDS.iter().copied().chain([user.as_str()]).filter_map(LocalSid::parse).collect();
    if trusted.len() != TRUSTED_SIDS.len() + 1 {
        return None;
    }

    let file_rights =
        FILE_WRITE_DATA | FILE_APPEND_DATA | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL;
    let folder_rights =
        FILE_WRITE_DATA | FILE_DELETE_CHILD | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL;
    // Higher up, only rights that allow renaming or replacing the folder chain matter.
    let ancestor_rights = FILE_DELETE_CHILD | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_ALL;

    if !path_is_private(exe, file_rights, &trusted)? {
        return Some(exe.to_path_buf());
    }
    let mut rights = folder_rights;
    for dir in exe.ancestors().skip(1) {
        if dir.as_os_str().is_empty() {
            break;
        }
        // The volume root can't be deleted or renamed.
        let mask = if dir.parent().is_none() { rights & !DELETE } else { rights };
        if !path_is_private(dir, mask, &trusted)? {
            return Some(dir.to_path_buf());
        }
        rights = ancestor_rights;
    }
    None
}

fn is_app_container_sid(sid: PSID) -> bool {
    unsafe {
        let mut text = PWSTR::null();
        if ConvertSidToStringSidW(sid, &mut text).is_err() {
            return false;
        }
        let matched = text.to_string().is_ok_and(|s| s.starts_with("S-1-15-"));
        LocalFree(Some(HLOCAL(text.0.cast())));
        matched
    }
}

/// Some(true) if no untrusted principal owns `path` or is granted any of `rights` on it.
fn path_is_private(path: &Path, rights: u32, trusted: &[LocalSid]) -> Option<bool> {
    let wide = HSTRING::from(path.as_os_str());
    unsafe {
        let mut owner = PSID::default();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let err = GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            &mut descriptor,
        );
        let _descriptor = LocalDescriptor(descriptor);
        if err != ERROR_SUCCESS {
            return None;
        }
        // AppContainer and capability SIDs (S-1-15-...) stand for sandboxed apps running as this
        // same user, not other accounts.
        let is_trusted = |sid: PSID| trusted.iter().any(|t| EqualSid(t.0, sid).is_ok()) || is_app_container_sid(sid);
        // An owner can always rewrite the DACL.
        if !owner.0.is_null() && !is_trusted(owner) {
            return Some(false);
        }
        if dacl.is_null() {
            return Some(false); // A NULL DACL grants everyone full access.
        }
        for i in 0..u32::from((*dacl).AceCount) {
            let mut ace: *mut c_void = std::ptr::null_mut();
            if GetAce(dacl, i, &mut ace).is_err() {
                continue;
            }
            let header = &*(ace as *const ACE_HEADER);
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            let allowed = &*(ace as *const ACCESS_ALLOWED_ACE);
            let sid = PSID(&allowed.SidStart as *const u32 as *mut c_void);
            if allowed.Mask & rights != 0 && !is_trusted(sid) {
                return Some(false);
            }
        }
        Some(true)
    }
}

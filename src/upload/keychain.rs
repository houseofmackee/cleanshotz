//! Secrets for uploads, stored as generic passwords in the login Keychain
//! (service "CleanShotZ").

use std::ptr;

use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFRetained, CFString, CFType};
use objc2_security::{
    SecItemAdd, SecItemCopyMatching, SecItemDelete, kSecAttrAccount, kSecAttrService, kSecClass,
    kSecClassGenericPassword, kSecMatchLimit, kSecMatchLimitOne, kSecReturnData, kSecValueData,
};

const SERVICE: &str = "CleanShotZ";
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

fn query(name: &str, extra: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let service = CFString::from_str(SERVICE);
    let account = CFString::from_str(name);
    // SAFETY: reading Security framework constants.
    let (class_key, class, service_key, account_key) = unsafe {
        (
            kSecClass,
            kSecClassGenericPassword,
            kSecAttrService,
            kSecAttrAccount,
        )
    };
    let mut keys: Vec<&CFString> = vec![class_key, service_key, account_key];
    let mut values: Vec<&CFType> = vec![class.as_ref(), service.as_ref(), account.as_ref()];
    for (k, v) in extra {
        keys.push(k);
        values.push(v);
    }
    CFDictionary::from_slices(&keys, &values)
}

pub fn get(name: &str) -> Option<String> {
    // SAFETY: reading Security framework constants.
    let (return_key, limit_key, one) =
        unsafe { (kSecReturnData, kSecMatchLimit, kSecMatchLimitOne) };
    let q = query(
        name,
        &[
            (return_key, CFBoolean::new(true).as_ref()),
            (limit_key, one.as_ref()),
        ],
    );
    let mut result: *const CFType = ptr::null();
    // SAFETY: a valid query; `result` receives a +1 reference on success.
    let status = unsafe { SecItemCopyMatching(q.as_opaque(), &mut result) };
    if status != 0 || result.is_null() {
        return None;
    }
    // SAFETY: with kSecReturnData the result is an owned CFData.
    let data: CFRetained<CFData> =
        unsafe { CFRetained::from_raw(std::ptr::NonNull::new(result as *mut CFData)?) };
    String::from_utf8(data.to_vec()).ok()
}

pub fn set(name: &str, value: &str) -> Result<(), String> {
    delete(name)?;
    let data = CFData::from_bytes(value.as_bytes());
    // SAFETY: reading a Security framework constant.
    let value_key = unsafe { kSecValueData };
    let q = query(name, &[(value_key, data.as_ref())]);
    // SAFETY: a valid attributes dictionary; no result requested.
    match unsafe { SecItemAdd(q.as_opaque(), ptr::null_mut()) } {
        0 => Ok(()),
        status => Err(format!("could not save to the Keychain (error {status})")),
    }
}

pub fn delete(name: &str) -> Result<(), String> {
    let q = query(name, &[]);
    // SAFETY: a valid query.
    match unsafe { SecItemDelete(q.as_opaque()) } {
        0 | ERR_SEC_ITEM_NOT_FOUND => Ok(()),
        status => Err(format!(
            "could not delete from the Keychain (error {status})"
        )),
    }
}

/// `keychain:NAME` values become the stored secret; others are returned
/// as they are.
pub fn resolve(value: &str) -> Result<String, String> {
    match value.strip_prefix("keychain:") {
        Some(name) => get(name.trim()).ok_or_else(|| {
            format!(
                "no Keychain secret named \"{name}\"; set it with: cleanshotz secret set {name}"
            )
        }),
        None => Ok(value.to_string()),
    }
}

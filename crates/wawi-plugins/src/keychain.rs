//! Where plugin secrets live: the macOS Keychain, one generic password per
//! secret, under the service `wawi2.plugin.<plugin>`. OAuth tokens go in
//! the account `oauth` (a small JSON), an API key in an account named like
//! the key (`LINEAR_API_KEY`).
//!
//! Everything goes through [`SecretStore`], so tests use [`MemoryStore`]
//! and never touch the real Keychain. A plugin's worker only gets a
//! [`Vault`] for its own service: it cannot ask for another plugin's item.
//!
//! The Mac store is raw FFI to Security.framework (`SecItemCopyMatching`,
//! `SecItemAdd`, `SecItemUpdate`, `SecItemDelete`) and the few
//! CoreFoundation calls they need: no new crate. Values never reach a log;
//! errors carry only the OSStatus.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The account that holds a plugin's OAuth grant.
pub const OAUTH_ACCOUNT: &str = "oauth";

/// The registered OAuth client (no tokens), kept when a grant is revoked
/// so connecting again does not register a new client.
pub const OAUTH_CLIENT_ACCOUNT: &str = "oauth-cliente";

/// `wawi2.plugin.<plugin>`.
pub fn service_for(plugin: &str) -> String {
    format!("wawi2.plugin.{plugin}")
}

/// A place for small secrets, by service and account. Calls may block (the
/// Keychain can ask the owner for access), so they are made on a plugin's
/// own thread or a short-lived one, never on the host or UI thread.
pub trait SecretStore: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, String>;
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String>;
    /// Deleting what is not there is fine.
    fn delete(&self, service: &str, account: &str) -> Result<(), String>;
}

/// One plugin's corner of the store.
#[derive(Clone)]
pub struct Vault {
    store: Arc<dyn SecretStore>,
    service: String,
}

impl Vault {
    pub fn new(store: Arc<dyn SecretStore>, plugin: &str) -> Self {
        Self {
            store,
            service: service_for(plugin),
        }
    }

    pub fn get(&self, account: &str) -> Result<Option<String>, String> {
        self.store.get(&self.service, account)
    }

    pub fn set(&self, account: &str, value: &str) -> Result<(), String> {
        self.store.set(&self.service, account, value)
    }

    pub fn delete(&self, account: &str) -> Result<(), String> {
        self.store.delete(&self.service, account)
    }
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Vault({})", self.service)
    }
}

/// In memory: for tests, and for systems with no Keychain (secrets then
/// last until Wawi closes; secrets.toml still works).
#[derive(Default)]
pub struct MemoryStore {
    items: Mutex<BTreeMap<(String, String), String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every `(service, account)` stored, for tests.
    pub fn keys(&self) -> Vec<(String, String)> {
        self.items
            .lock()
            .map(|items| items.keys().cloned().collect())
            .unwrap_or_default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
        let items = self.items.lock().map_err(|_| "almacén envenenado")?;
        Ok(items.get(&(service.into(), account.into())).cloned())
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String> {
        let mut items = self.items.lock().map_err(|_| "almacén envenenado")?;
        items.insert((service.into(), account.into()), value.into());
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), String> {
        let mut items = self.items.lock().map_err(|_| "almacén envenenado")?;
        items.remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

/// The system's store: the Keychain on macOS, memory elsewhere.
pub fn system() -> Arc<dyn SecretStore> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(mac::Keychain)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(MemoryStore::new())
    }
}

#[cfg(target_os = "macos")]
pub use mac::Keychain;

#[cfg(target_os = "macos")]
mod mac {
    //! Raw Security.framework. Every CF object created here is released
    //! before returning; the value bytes are copied out and the CFData
    //! released.

    use std::ffi::c_void;
    use std::ptr;

    use super::SecretStore;

    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFDataRef = *const c_void;
    type OSStatus = i32;

    #[repr(C)]
    struct CallBacks {
        _opaque: [u8; 0],
    }

    const UTF8: u32 = 0x0800_0100;
    const NOT_FOUND: OSStatus = -25300;
    const DUPLICATE: OSStatus = -25299;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: CallBacks;
        static kCFTypeDictionaryValueCallBacks: CallBacks;
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            key_callbacks: *const CallBacks,
            value_callbacks: *const CallBacks,
        ) -> CFDictionaryRef;
        fn CFStringCreateWithBytes(
            allocator: *const c_void,
            bytes: *const u8,
            count: isize,
            encoding: u32,
            external: u8,
        ) -> CFStringRef;
        fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> CFDataRef;
        fn CFDataGetLength(data: CFDataRef) -> isize;
        fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
        fn CFRelease(cf: CFTypeRef);
    }

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecClass: CFStringRef;
        static kSecClassGenericPassword: CFStringRef;
        static kSecAttrService: CFStringRef;
        static kSecAttrAccount: CFStringRef;
        static kSecAttrLabel: CFStringRef;
        static kSecValueData: CFStringRef;
        static kSecReturnData: CFStringRef;
        static kSecMatchLimit: CFStringRef;
        static kSecMatchLimitOne: CFStringRef;
        fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
        fn SecItemAdd(attributes: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
        fn SecItemUpdate(query: CFDictionaryRef, attributes: CFDictionaryRef) -> OSStatus;
        fn SecItemDelete(query: CFDictionaryRef) -> OSStatus;
    }

    /// Owns one CF object and releases it on drop.
    struct Owned(CFTypeRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: created by a CF *Create function (or returned
                // retained by SecItemCopyMatching) and released once.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn string(text: &str) -> Owned {
        // SAFETY: the bytes are valid for `len` and copied by CF.
        Owned(unsafe {
            CFStringCreateWithBytes(ptr::null(), text.as_ptr(), text.len() as isize, UTF8, 0)
        })
    }

    fn data(bytes: &[u8]) -> Owned {
        // SAFETY: as above; CF copies the bytes.
        Owned(unsafe { CFDataCreate(ptr::null(), bytes.as_ptr(), bytes.len() as isize) })
    }

    fn dictionary(pairs: &[(CFTypeRef, CFTypeRef)]) -> Owned {
        let keys: Vec<CFTypeRef> = pairs.iter().map(|p| p.0).collect();
        let values: Vec<CFTypeRef> = pairs.iter().map(|p| p.1).collect();
        // SAFETY: keys and values are live CF objects for the call; the
        // type callbacks retain them inside the dictionary.
        Owned(unsafe {
            CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                pairs.len() as isize,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        })
    }

    fn failed(what: &str, status: OSStatus) -> String {
        format!("el llavero no pudo {what} (OSStatus {status})")
    }

    /// The login Keychain, through Security.framework.
    pub struct Keychain;

    impl SecretStore for Keychain {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
            let (service, account) = (string(service), string(account));
            // SAFETY: the constants are CF strings owned by Security.framework.
            let query = unsafe {
                dictionary(&[
                    (kSecClass, kSecClassGenericPassword),
                    (kSecAttrService, service.0),
                    (kSecAttrAccount, account.0),
                    (kSecReturnData, kCFBooleanTrue),
                    (kSecMatchLimit, kSecMatchLimitOne),
                ])
            };
            let mut result: CFTypeRef = ptr::null();
            // SAFETY: `query` is a valid dictionary; `result` gets a retained CFData.
            let status = unsafe { SecItemCopyMatching(query.0, &mut result) };
            let result = Owned(result);
            match status {
                0 if !result.0.is_null() => {
                    // SAFETY: a CFData on success, alive while `result` is.
                    let bytes = unsafe {
                        let len = CFDataGetLength(result.0) as usize;
                        let ptr = CFDataGetBytePtr(result.0);
                        if ptr.is_null() {
                            Vec::new()
                        } else {
                            std::slice::from_raw_parts(ptr, len).to_vec()
                        }
                    };
                    String::from_utf8(bytes)
                        .map(Some)
                        .map_err(|_| "el llavero devolvió algo que no es texto".to_owned())
                }
                0 | NOT_FOUND => Ok(None),
                other => Err(failed("leer", other)),
            }
        }

        fn set(&self, service_name: &str, account_name: &str, value: &str) -> Result<(), String> {
            let (service, account) = (string(service_name), string(account_name));
            let value = data(value.as_bytes());
            let label = string(&format!("Wawi: {service_name} ({account_name})"));
            // SAFETY: as in `get`.
            let query = unsafe {
                dictionary(&[
                    (kSecClass, kSecClassGenericPassword),
                    (kSecAttrService, service.0),
                    (kSecAttrAccount, account.0),
                ])
            };
            // SAFETY: as in `get`.
            let change = unsafe { dictionary(&[(kSecValueData, value.0)]) };
            // SAFETY: valid dictionaries.
            let status = unsafe { SecItemUpdate(query.0, change.0) };
            match status {
                0 => return Ok(()),
                NOT_FOUND => {}
                other => return Err(failed("guardar", other)),
            }
            // SAFETY: as in `get`.
            let add = unsafe {
                dictionary(&[
                    (kSecClass, kSecClassGenericPassword),
                    (kSecAttrService, service.0),
                    (kSecAttrAccount, account.0),
                    (kSecAttrLabel, label.0),
                    (kSecValueData, value.0),
                ])
            };
            // SAFETY: a valid dictionary; no result asked for.
            let status = unsafe { SecItemAdd(add.0, ptr::null_mut()) };
            match status {
                0 => Ok(()),
                DUPLICATE => {
                    // Raced with another writer: update instead.
                    // SAFETY: valid dictionaries.
                    let status = unsafe { SecItemUpdate(query.0, change.0) };
                    if status == 0 {
                        Ok(())
                    } else {
                        Err(failed("guardar", status))
                    }
                }
                other => Err(failed("guardar", other)),
            }
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), String> {
            let (service, account) = (string(service), string(account));
            // SAFETY: as in `get`.
            let query = unsafe {
                dictionary(&[
                    (kSecClass, kSecClassGenericPassword),
                    (kSecAttrService, service.0),
                    (kSecAttrAccount, account.0),
                ])
            };
            // SAFETY: a valid dictionary.
            match unsafe { SecItemDelete(query.0) } {
                0 | NOT_FOUND => Ok(()),
                other => Err(failed("borrar", other)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vault_only_reaches_its_own_service() {
        let store = Arc::new(MemoryStore::new());
        let linear = Vault::new(store.clone(), "linear");
        let otro = Vault::new(store.clone(), "otro");
        linear.set(OAUTH_ACCOUNT, "{}").unwrap();
        assert_eq!(otro.get(OAUTH_ACCOUNT).unwrap(), None);
        assert_eq!(linear.get(OAUTH_ACCOUNT).unwrap().as_deref(), Some("{}"));
        assert_eq!(
            store.keys(),
            [("wawi2.plugin.linear".to_owned(), "oauth".to_owned())]
        );
        linear.delete(OAUTH_ACCOUNT).unwrap();
        linear.delete(OAUTH_ACCOUNT).unwrap();
        assert!(store.keys().is_empty());
        assert!(!format!("{linear:?}").contains('{'));
    }

    /// The real login Keychain, with a throwaway service name. Ignored by
    /// default: `cargo test -p wawi-plugins -- --ignored real_keychain`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn real_keychain_round_trip() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let service = format!("wawi2.test.{}.{nanos}", std::process::id());
        let keychain = Keychain;
        let cleanup = |k: &Keychain| k.delete(&service, "cuenta");
        assert_eq!(keychain.get(&service, "cuenta").unwrap(), None);
        keychain.set(&service, "cuenta", "uno").unwrap();
        assert_eq!(
            keychain.get(&service, "cuenta").unwrap().as_deref(),
            Some("uno")
        );
        keychain.set(&service, "cuenta", "dos ñ").unwrap();
        assert_eq!(
            keychain.get(&service, "cuenta").unwrap().as_deref(),
            Some("dos ñ")
        );
        cleanup(&keychain).unwrap();
        assert_eq!(keychain.get(&service, "cuenta").unwrap(), None);
    }
}

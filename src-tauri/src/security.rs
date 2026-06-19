use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Serialize, Deserialize)]
struct SecretPayload {
    providers: BTreeMap<String, String>,
    proxy_keys: BTreeMap<String, String>,
}

pub fn hydrate_config_secrets(config: &mut AppConfig, path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let encrypted = std::fs::read(path).map_err(|error| error.to_string())?;
    let plaintext = unprotect(&encrypted)?;
    let secrets: SecretPayload =
        serde_json::from_slice(&plaintext).map_err(|error| error.to_string())?;

    for provider in &mut config.providers {
        if provider.api_key.trim().is_empty() {
            if let Some(secret) = secrets.providers.get(&provider.id) {
                provider.api_key = secret.clone();
            }
        }
    }
    for api_key in &mut config.proxy_api_keys {
        if api_key.key.trim().is_empty() {
            if let Some(secret) = secrets.proxy_keys.get(&api_key.id) {
                api_key.key = secret.clone();
            }
        }
    }
    Ok(())
}

pub fn save_config_secrets(config: &AppConfig, path: &Path) -> Result<(), String> {
    let secrets = SecretPayload {
        providers: config
            .providers
            .iter()
            .filter(|provider| !provider.id.trim().is_empty() && !provider.api_key.is_empty())
            .map(|provider| (provider.id.clone(), provider.api_key.clone()))
            .collect(),
        proxy_keys: config
            .proxy_api_keys
            .iter()
            .filter(|api_key| !api_key.id.trim().is_empty() && !api_key.key.is_empty())
            .map(|api_key| (api_key.id.clone(), api_key.key.clone()))
            .collect(),
    };
    let plaintext = serde_json::to_vec(&secrets).map_err(|error| error.to_string())?;
    let encrypted = protect(&plaintext)?;
    atomic_write(path, &encrypted)
}

pub fn redacted_config(config: &AppConfig) -> AppConfig {
    let mut redacted = config.clone();
    redacted.proxy_api_key.clear();
    for provider in &mut redacted.providers {
        provider.api_key.clear();
    }
    for api_key in &mut redacted.proxy_api_keys {
        api_key.key.clear();
    }
    redacted
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, content).map_err(|error| error.to_string())?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

#[cfg(windows)]
fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: data
            .len()
            .try_into()
            .map_err(|_| "Secret payload is too large")?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let success = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let protected = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        bytes
    };
    Ok(protected)
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: data
            .len()
            .try_into()
            .map_err(|_| "Secret payload is too large")?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let success = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let plaintext = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        bytes
    };
    Ok(plaintext)
}

#[cfg(not(windows))]
fn protect(_data: &[u8]) -> Result<Vec<u8>, String> {
    Err("Secure API key storage currently requires Windows DPAPI".to_string())
}

#[cfg(not(windows))]
fn unprotect(_data: &[u8]) -> Result<Vec<u8>, String> {
    Err("Secure API key storage currently requires Windows DPAPI".to_string())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::config::{Provider, ProxyApiKey};

    #[test]
    fn dpapi_round_trip_does_not_store_plaintext() {
        let plaintext = b"sk-test-secret";
        let encrypted = protect(plaintext).unwrap();
        assert_ne!(encrypted, plaintext);
        assert_eq!(unprotect(&encrypted).unwrap(), plaintext);
    }

    #[test]
    fn config_secrets_are_redacted_and_restored() {
        let path = std::env::temp_dir().join(format!("api-nexus-{}.dpapi", uuid::Uuid::new_v4()));
        let mut config = AppConfig {
            providers: vec![Provider {
                id: "provider-1".to_string(),
                api_key: "upstream-secret".to_string(),
                ..Provider::default()
            }],
            proxy_api_keys: vec![ProxyApiKey {
                id: "client-1".to_string(),
                name: "client".to_string(),
                key: "client-secret".to_string(),
                enabled: true,
            }],
            ..AppConfig::default()
        };
        save_config_secrets(&config, &path).unwrap();
        let encrypted = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&encrypted).contains("upstream-secret"));

        config = redacted_config(&config);
        assert!(config.providers[0].api_key.is_empty());
        hydrate_config_secrets(&mut config, &path).unwrap();
        assert_eq!(config.providers[0].api_key, "upstream-secret");
        assert_eq!(config.proxy_api_keys[0].key, "client-secret");
        std::fs::remove_file(path).unwrap();
    }
}

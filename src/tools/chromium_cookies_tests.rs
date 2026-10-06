use super::*;
use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

/// Encrypt a value exactly as Chromium's OS-Crypt v10 does, so the test proves
/// our decrypt is byte-correct without needing a live browser.
fn encrypt_v10(plaintext: &str, key: &[u8; 16]) -> Vec<u8> {
    let iv = [0x20u8; 16];
    let mut out = b"v10".to_vec();
    let ct = Aes128CbcEnc::new(key.into(), &iv.into())
        .encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes());
    out.extend_from_slice(&ct);
    out
}

#[test]
fn v10_roundtrip_decrypts_to_original() {
    let key = derive_key(b"peanuts");
    let blob = encrypt_v10("session=abc123; secure", &key);
    let got = decrypt_value(&blob, &key, None).expect("v10 must decrypt with peanuts key");
    assert_eq!(got, "session=abc123; secure");
}

#[test]
fn v11_decrypts_only_with_keyring_key() {
    let v11_key = derive_key(b"a-real-keyring-secret");
    // Build a v11 blob (same scheme, different key + prefix).
    let iv = [0x20u8; 16];
    let mut blob = b"v11".to_vec();
    blob.extend_from_slice(
        &Aes128CbcEnc::new((&v11_key).into(), &iv.into())
            .encrypt_padded_vec_mut::<Pkcs7>(b"tok=xyz"),
    );
    let v10_key = derive_key(b"peanuts");
    // Without the keyring key, a v11 blob is unreadable (skipped, not garbage).
    assert!(decrypt_value(&blob, &v10_key, None).is_none());
    // With it, it decrypts.
    assert_eq!(
        decrypt_value(&blob, &v10_key, Some(&v11_key)).as_deref(),
        Some("tok=xyz")
    );
}

#[test]
fn strips_m130_domain_hash_prefix_when_present() {
    let key = derive_key(b"peanuts");
    // Simulate the 32-byte SHA-256 host prefix newer Chromium prepends.
    let mut payload = vec![0u8; 32];
    payload.extend_from_slice(b"realvalue=1");
    let iv = [0x20u8; 16];
    let mut blob = b"v10".to_vec();
    blob.extend_from_slice(
        &Aes128CbcEnc::new((&key).into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(&payload),
    );
    // The 32 leading NUL bytes aren't valid UTF-8 at the front, so the decoder
    // strips them and returns the tail.
    assert_eq!(
        decrypt_value(&blob, &key, None).as_deref(),
        Some("realvalue=1")
    );
}

#[test]
fn chromium_epoch_converts_to_unix_seconds() {
    // 13300000000000000 µs since 1601 → a plausible 2021-ish Unix time.
    let secs = chromium_expiry_to_unix_secs(13_300_000_000_000_000).unwrap();
    assert!(secs > 1_600_000_000.0 && secs < 1_800_000_000.0, "{secs}");
    assert_eq!(chromium_expiry_to_unix_secs(0), None); // session cookie
}

#[test]
fn source_recognition_matches_chromium_family_only() {
    for s in [
        "chrome", "brave", "chromium", "edge", "vivaldi", "opera", "Brave",
    ] {
        assert!(is_chromium_source(s), "{s} should be chromium");
    }
    for s in ["zen", "firefox", "librewolf", "", "none"] {
        assert!(!is_chromium_source(s), "{s} should NOT be chromium");
    }
}

#[test]
fn same_site_maps_chromium_ints() {
    assert_eq!(same_site(0).as_deref(), Some("None"));
    assert_eq!(same_site(1).as_deref(), Some("Lax"));
    assert_eq!(same_site(2).as_deref(), Some("Strict"));
    assert_eq!(same_site(-1), None);
}

// Live: decrypt the user's REAL Chrome cookie store end to end. Read-only.
//   cargo test --lib live_chromium_reads_real_chrome_cookies -- --ignored --nocapture
#[test]
#[ignore]
fn live_chromium_reads_real_chrome_cookies() {
    let cookies = match read_cookies("chrome") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no Chrome profile to read ({e}); skipping");
            return;
        }
    };
    eprintln!("decrypted {} chrome cookies", cookies.len());
    assert!(
        !cookies.is_empty(),
        "expected some decrypted chrome cookies"
    );
    // A decrypted value must be printable text (proves decryption, not garbage).
    let sample = cookies
        .iter()
        .find(|c| !c.value.is_empty())
        .expect("at least one non-empty cookie value");
    eprintln!(
        "sample: {}={} (secure={})",
        sample.domain, sample.name, sample.secure
    );
    assert!(
        sample
            .value
            .chars()
            .all(|ch| !ch.is_control() || ch == '\t'),
        "decrypted value should be clean text, got control chars"
    );
}

//! Android key-attestation `KeyDescription` extension parsing (OID
//! `1.3.6.1.4.1.11129.2.1.17`).
//!
//! The extension is a DER `KeyDescription` sequence (Keymaster v1–v4 and
//! KeyMint v100+ all keep the fields this service needs at the same
//! positions): attestation/keymaster security levels, the attestation
//! challenge, and two `AuthorizationList`s whose context-specific tags carry
//! `attestationApplicationId` ([709]) and `rootOfTrust` ([704]).
//!
//! Parsing is a small hand-rolled DER TLV reader: the interesting fields use
//! high-tag-number context tags (704/709) that generic parsers surface as
//! opaque blobs anyway, and the input is attacker-controlled — every read is
//! bounds-checked and nothing recurses unboundedly.

/// OID of the Android key-attestation extension.
pub const ANDROID_ATTESTATION_OID: &str = "1.3.6.1.4.1.11129.2.1.17";

/// AuthorizationList context tag for `attestationApplicationId`.
const TAG_ATTESTATION_APPLICATION_ID: u32 = 709;
/// AuthorizationList context tag for `rootOfTrust`.
const TAG_ROOT_OF_TRUST: u32 = 704;

/// The fields of the parsed `KeyDescription` this service validates.
#[derive(Debug)]
pub struct KeyDescription {
    /// `attestationSecurityLevel`: Software = 0, TEE = 1, StrongBox = 2.
    pub attestation_security_level: u64,
    /// `keymasterSecurityLevel` / `keyMintSecurityLevel` (same position).
    pub key_security_level: u64,
    /// `attestationChallenge` — the bytes the client minted the key with.
    pub attestation_challenge: Vec<u8>,
    /// Package names from `attestationApplicationId.packageInfos`.
    pub package_names: Vec<String>,
    /// SHA-256 signing-certificate digests from `attestationApplicationId`.
    pub signing_digests: Vec<Vec<u8>>,
    /// `rootOfTrust` from the hardware-enforced list, when present.
    pub root_of_trust: Option<RootOfTrust>,
}

/// The hardware-enforced `RootOfTrust` sequence.
#[derive(Debug)]
pub struct RootOfTrust {
    /// `verifiedBootKey` — hash of the OS-signing key (32 bytes in practice).
    pub verified_boot_key: Vec<u8>,
    /// `deviceLocked` — bootloader lock state.
    pub device_locked: bool,
    /// `verifiedBootState`: Verified = 0, SelfSigned = 1, Unverified = 2, Failed = 3.
    pub verified_boot_state: u64,
}

/// A decoded DER TLV.
struct Tlv<'a> {
    class: u8,
    constructed: bool,
    number: u32,
    content: &'a [u8],
}

/// Universal tag numbers this parser consumes.
const TAG_BOOLEAN: u32 = 1;
const TAG_OCTET_STRING: u32 = 4;
const TAG_ENUMERATED: u32 = 10;
const TAG_SEQUENCE: u32 = 16;
const TAG_SET: u32 = 17;

impl Tlv<'_> {
    /// Require a primitive universal type.
    fn expect_primitive(&self, number: u32, what: &str) -> Result<(), String> {
        if self.class != 0 || self.constructed || self.number != number {
            return Err(format!("{what}: unexpected DER type"));
        }
        Ok(())
    }

    /// Require a constructed universal type (SEQUENCE / SET).
    fn expect_constructed(&self, number: u32, what: &str) -> Result<(), String> {
        if self.class != 0 || !self.constructed || self.number != number {
            return Err(format!("{what}: unexpected DER type"));
        }
        Ok(())
    }
}

/// Read one TLV from `input`, returning it and the remaining bytes.
/// Enforces DER minimal encodings for the tag and length forms.
fn read_tlv(input: &[u8]) -> Result<(Tlv<'_>, &[u8]), String> {
    let b0 = *input.first().ok_or("truncated tag")?;
    let class = b0 >> 6;
    let constructed = b0 & 0x20 != 0;
    let mut idx = 1usize;
    let mut number = u32::from(b0 & 0x1F);
    if number == 0x1F {
        number = 0;
        let mut first = true;
        loop {
            let b = *input.get(idx).ok_or("truncated high-form tag")?;
            idx += 1;
            if first && b & 0x7F == 0 {
                return Err("non-minimal high-form tag".to_string());
            }
            first = false;
            number = number
                .checked_mul(128)
                .and_then(|n| n.checked_add(u32::from(b & 0x7F)))
                .ok_or("tag number overflow")?;
            if b & 0x80 == 0 {
                break;
            }
        }
        if number < 31 {
            return Err("non-minimal high-form tag".to_string());
        }
    }

    let l0 = *input.get(idx).ok_or("truncated length")?;
    idx += 1;
    let len = if l0 & 0x80 == 0 {
        usize::from(l0)
    } else {
        let n = usize::from(l0 & 0x7F);
        if n == 0 || n > 4 {
            return Err("unsupported DER length form".to_string());
        }
        let mut len = 0usize;
        for i in 0..n {
            let b = *input.get(idx).ok_or("truncated long-form length")?;
            idx += 1;
            if i == 0 && b == 0 {
                return Err("non-minimal long-form length".to_string());
            }
            len = (len << 8) | usize::from(b);
        }
        if len < 128 {
            return Err("non-minimal long-form length".to_string());
        }
        len
    };

    let end = idx.checked_add(len).ok_or("length overflow")?;
    let content = input.get(idx..end).ok_or("truncated content")?;
    Ok((
        Tlv {
            class,
            constructed,
            number,
            content,
        },
        &input[end..],
    ))
}

/// Split a constructed value's content into its child TLVs.
fn children(mut content: &[u8]) -> Result<Vec<Tlv<'_>>, String> {
    let mut out = Vec::new();
    while !content.is_empty() {
        let (tlv, rest) = read_tlv(content)?;
        out.push(tlv);
        content = rest;
    }
    Ok(out)
}

/// Interpret an INTEGER/ENUMERATED content as an unsigned value.
fn as_u64(tlv: &Tlv<'_>) -> Result<u64, String> {
    if tlv.content.is_empty() || tlv.content.len() > 8 {
        return Err("integer out of range".to_string());
    }
    let mut value = 0u64;
    for &b in tlv.content {
        value = (value << 8) | u64::from(b);
    }
    Ok(value)
}

/// Interpret a strict DER BOOLEAN (`00` / `ff` single byte).
fn as_bool(tlv: &Tlv<'_>) -> Result<bool, String> {
    match tlv.content {
        [0x00] => Ok(false),
        [0xFF] => Ok(true),
        _ => Err("non-canonical DER BOOLEAN".to_string()),
    }
}

/// Parse the DER `KeyDescription` from the extension value.
pub fn parse(extension_value: &[u8]) -> Result<KeyDescription, String> {
    let (top, rest) = read_tlv(extension_value)?;
    if !rest.is_empty() {
        return Err("trailing bytes after KeyDescription".to_string());
    }
    top.expect_constructed(TAG_SEQUENCE, "KeyDescription")?;
    let fields = children(top.content)?;
    if fields.len() < 8 {
        return Err(format!(
            "KeyDescription has {} fields, expected at least 8",
            fields.len()
        ));
    }

    fields[1].expect_primitive(TAG_ENUMERATED, "attestationSecurityLevel")?;
    let attestation_security_level = as_u64(&fields[1])?;
    fields[3].expect_primitive(TAG_ENUMERATED, "keymasterSecurityLevel")?;
    let key_security_level = as_u64(&fields[3])?;
    fields[4].expect_primitive(TAG_OCTET_STRING, "attestationChallenge")?;
    let attestation_challenge = fields[4].content.to_vec();

    let software = parse_authorization_list(&fields[6])?;
    let hardware = parse_authorization_list(&fields[7])?;

    // attestationApplicationId is populated by the framework into the
    // software-enforced list; fall back to hardware-enforced like the legacy
    // backend does.
    let app_id_der = software
        .application_id
        .or(hardware.application_id)
        .ok_or("missing attestationApplicationId")?;
    let (package_names, signing_digests) = parse_application_id(app_id_der)?;

    Ok(KeyDescription {
        attestation_security_level,
        key_security_level,
        attestation_challenge,
        package_names,
        signing_digests,
        root_of_trust: hardware.root_of_trust,
    })
}

/// The AuthorizationList fields this service reads.
struct AuthorizationList<'a> {
    application_id: Option<&'a [u8]>,
    root_of_trust: Option<RootOfTrust>,
}

/// Scan an `AuthorizationList` SEQUENCE for the tags we validate.
fn parse_authorization_list<'a>(list: &Tlv<'a>) -> Result<AuthorizationList<'a>, String> {
    list.expect_constructed(TAG_SEQUENCE, "AuthorizationList")?;
    let mut application_id = None;
    let mut root_of_trust = None;
    for entry in children(list.content)? {
        // Context-specific EXPLICIT tags: the content wraps the inner TLV.
        if entry.class != 2 {
            continue;
        }
        match entry.number {
            TAG_ATTESTATION_APPLICATION_ID => {
                let (inner, rest) = read_tlv(entry.content)?;
                if !rest.is_empty() {
                    return Err("attestationApplicationId: trailing bytes".to_string());
                }
                inner.expect_primitive(TAG_OCTET_STRING, "attestationApplicationId")?;
                application_id = Some(inner.content);
            }
            TAG_ROOT_OF_TRUST => {
                let (inner, rest) = read_tlv(entry.content)?;
                if !rest.is_empty() {
                    return Err("rootOfTrust: trailing bytes".to_string());
                }
                inner.expect_constructed(TAG_SEQUENCE, "rootOfTrust")?;
                root_of_trust = Some(parse_root_of_trust(inner.content)?);
            }
            _ => {}
        }
    }
    Ok(AuthorizationList {
        application_id,
        root_of_trust,
    })
}

/// Parse `RootOfTrust ::= SEQUENCE { verifiedBootKey, deviceLocked,
/// verifiedBootState, [verifiedBootHash] }`.
fn parse_root_of_trust(content: &[u8]) -> Result<RootOfTrust, String> {
    let fields = children(content)?;
    if fields.len() < 3 {
        return Err("rootOfTrust has fewer than 3 fields".to_string());
    }
    fields[0].expect_primitive(TAG_OCTET_STRING, "verifiedBootKey")?;
    fields[1].expect_primitive(TAG_BOOLEAN, "deviceLocked")?;
    fields[2].expect_primitive(TAG_ENUMERATED, "verifiedBootState")?;
    Ok(RootOfTrust {
        verified_boot_key: fields[0].content.to_vec(),
        device_locked: as_bool(&fields[1])?,
        verified_boot_state: as_u64(&fields[2])?,
    })
}

/// Parse the `AttestationApplicationId` DER carried inside the [709] octet
/// string: `SEQUENCE { SET OF { SEQUENCE { name OCTET STRING, version INTEGER } },
/// SET OF OCTET STRING digests }`.
fn parse_application_id(der: &[u8]) -> Result<(Vec<String>, Vec<Vec<u8>>), String> {
    let (top, rest) = read_tlv(der)?;
    if !rest.is_empty() {
        return Err("AttestationApplicationId: trailing bytes".to_string());
    }
    top.expect_constructed(TAG_SEQUENCE, "AttestationApplicationId")?;
    let sets = children(top.content)?;
    if sets.len() != 2 {
        return Err("AttestationApplicationId does not have 2 sets".to_string());
    }
    sets[0].expect_constructed(TAG_SET, "packageInfos")?;
    sets[1].expect_constructed(TAG_SET, "signatureDigests")?;

    let mut package_names = Vec::new();
    for info in children(sets[0].content)? {
        info.expect_constructed(TAG_SEQUENCE, "packageInfo")?;
        let fields = children(info.content)?;
        let name = fields.first().ok_or("packageInfo missing name")?;
        name.expect_primitive(TAG_OCTET_STRING, "packageName")?;
        package_names.push(
            String::from_utf8(name.content.to_vec())
                .map_err(|_| "package name is not UTF-8".to_string())?,
        );
    }

    let mut signing_digests = Vec::new();
    for digest in children(sets[1].content)? {
        digest.expect_primitive(TAG_OCTET_STRING, "signature digest")?;
        signing_digests.push(digest.content.to_vec());
    }

    Ok((package_names, signing_digests))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal DER builders for synthetic KeyDescription negatives.
    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        assert!(content.len() < 128, "test builder supports short form only");
        let mut out = vec![tag, content.len() as u8];
        out.extend_from_slice(content);
        out
    }
    fn integer(v: u8) -> Vec<u8> {
        tlv(0x02, &[v])
    }
    fn enumerated(v: u8) -> Vec<u8> {
        tlv(0x0A, &[v])
    }
    fn octet(content: &[u8]) -> Vec<u8> {
        tlv(0x04, content)
    }
    fn boolean(byte: u8) -> Vec<u8> {
        tlv(0x01, &[byte])
    }
    fn constructed(tag: u8, parts: &[Vec<u8>]) -> Vec<u8> {
        let content: Vec<u8> = parts.concat();
        let mut out = vec![tag];
        if content.len() < 128 {
            out.push(content.len() as u8);
        } else {
            assert!(content.len() < 256);
            out.extend_from_slice(&[0x81, content.len() as u8]);
        }
        out.extend(content);
        out
    }
    fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
        constructed(0x30, parts)
    }
    fn set(parts: &[Vec<u8>]) -> Vec<u8> {
        constructed(0x31, parts)
    }
    /// Context-specific EXPLICIT [709] / [704] wrapper (high-tag-number form).
    fn ctx(number: u32, inner: &[u8]) -> Vec<u8> {
        let mut out = vec![0xBF];
        assert!((128..16384).contains(&number));
        out.push(0x80 | (number / 128) as u8);
        out.push((number % 128) as u8);
        assert!(inner.len() < 128);
        out.push(inner.len() as u8);
        out.extend_from_slice(inner);
        out
    }

    fn application_id(package: &str) -> Vec<u8> {
        sequence(&[
            set(&[sequence(&[octet(package.as_bytes()), integer(1)])]),
            set(&[octet(&[0x11; 32])]),
        ])
    }

    fn key_description(security: Vec<u8>, challenge_field: Vec<u8>, locked: Vec<u8>) -> Vec<u8> {
        let root_of_trust = sequence(&[octet(&[0x22; 32]), locked, enumerated(0)]);
        sequence(&[
            integer(100),     // attestationVersion
            security.clone(), // attestationSecurityLevel
            integer(100),     // keymasterVersion
            security,         // keymasterSecurityLevel
            challenge_field,  // attestationChallenge
            octet(&[]),       // uniqueId
            sequence(&[ctx(
                TAG_ATTESTATION_APPLICATION_ID,
                &octet(&application_id("io.pcf.polkadotapp")),
            )]),
            sequence(&[ctx(TAG_ROOT_OF_TRUST, &root_of_trust)]),
        ])
    }

    #[test]
    fn synthetic_key_description_parses_and_enforces_types() {
        let valid = key_description(enumerated(1), octet(b"challenge"), boolean(0xFF));
        let parsed = parse(&valid).expect("valid synthetic description");
        assert_eq!(parsed.attestation_security_level, 1);
        assert_eq!(parsed.attestation_challenge, b"challenge");
        assert_eq!(parsed.package_names, vec!["io.pcf.polkadotapp".to_string()]);
        assert!(parsed.root_of_trust.expect("root of trust").device_locked);

        // Security level as INTEGER instead of ENUMERATED is type confusion.
        let wrong_type = key_description(integer(1), octet(b"challenge"), boolean(0xFF));
        assert!(parse(&wrong_type).is_err());

        // Challenge that is not an OCTET STRING is rejected.
        let wrong_challenge = key_description(enumerated(1), integer(7), boolean(0xFF));
        assert!(parse(&wrong_challenge).is_err());

        // Non-canonical BOOLEAN (0x01) is rejected.
        let sloppy_bool = key_description(enumerated(1), octet(b"challenge"), boolean(0x01));
        assert!(parse(&sloppy_bool).is_err());
    }

    #[test]
    fn non_minimal_der_encodings_are_rejected() {
        // Long-form length used for a short value (0x81 0x05).
        assert!(parse(&[0x30, 0x81, 0x05, 0x02, 0x01, 0x01, 0x04, 0x00]).is_err());
        // High-tag-number form for a tag that fits the low form (0x1F 0x05).
        assert!(read_tlv(&[0x1F, 0x05, 0x00]).is_err());
        // Leading zero in a high-form tag.
        assert!(read_tlv(&[0xBF, 0x80, 0x85, 0x45, 0x00]).is_err());
        // Truncated content.
        assert!(parse(&[0x30, 0x05, 0x02, 0x01]).is_err());
        // Trailing bytes after the top-level SEQUENCE.
        assert!(parse(&[0x30, 0x00, 0x00]).is_err());
    }
}

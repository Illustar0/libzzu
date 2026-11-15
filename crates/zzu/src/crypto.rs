use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
};
use ecb::cipher::{BlockDecryptMut, KeyInit, block_padding::Pkcs7};
use libsm::sm2::{encrypt::EncryptCtx, signature::SigCtx};
use rand::Rng;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey, pkcs1::DecodeRsaPublicKey, pkcs8::DecodePublicKey};
use serde::Deserialize;

use crate::{Error, Result};

pub(crate) fn rsa_key(text: &str) -> Result<RsaPublicKey> {
    let pem = pem::parse(text).map_err(Error::crypto_source)?;
    match pem.tag() {
        "PUBLIC KEY" => {
            RsaPublicKey::from_public_key_der(pem.contents()).map_err(Error::crypto_source)
        }
        "RSA PUBLIC KEY" => {
            RsaPublicKey::from_pkcs1_der(pem.contents()).map_err(Error::crypto_source)
        }
        _ => Err(Error::crypto("不支持的 RSA 公钥 PEM 标签".into())),
    }
}

pub(crate) fn rsa_encrypt(key: &RsaPublicKey, text: &str) -> Result<String> {
    let encrypted = key
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, text.as_bytes())
        .map_err(Error::crypto_source)?;
    Ok(format!("__RSA__{}", STANDARD.encode(encrypted)))
}

pub(crate) fn jwt_expiration(token: &str) -> Result<i64> {
    #[derive(Deserialize)]
    struct Claims {
        exp: serde_json::Value,
    }
    if token.is_empty() || token.len() > 16_384 {
        return Err(Error::decode_message("JWT 为空或超过长度限制".into()));
    }
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(Error::decode_message("JWT 结构无效".into()));
    }
    let decode = |part| {
        URL_SAFE_NO_PAD
            .decode(part)
            .or_else(|_| URL_SAFE.decode(part))
            .map_err(|_| Error::decode_message("JWT Base64URL 无效".into()))
    };
    let _: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&decode(parts[0])?)?;
    let claims: Claims = serde_json::from_slice(&decode(parts[1])?)?;
    decode(parts[2])?;
    let expiration = crate::http::parse_number(&claims.exp, "JWT exp")?;
    if !(0.0..=253_402_300_799.0).contains(&expiration) {
        return Err(Error::decode_message("JWT exp 超出日期范围".into()));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "JWT NumericDate permits fractional seconds; the range is checked above"
    )]
    let expiration = expiration as i64;
    Ok(expiration)
}

const SM4_KEY_ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_+-=";

pub(crate) fn sm2_encrypt(public_key: &str, plaintext: &[u8]) -> Result<String> {
    let mut key_bytes = hex::decode(public_key).map_err(Error::crypto_source)?;
    // sm-crypto 同时接受带与不带 `04` 前缀的未压缩公钥。
    if key_bytes.len() == 64 {
        key_bytes.insert(0, 0x04);
    }
    let key = SigCtx::new()
        .load_pubkey(&key_bytes)
        .map_err(|error| Error::crypto(error.to_string()))?;
    let encrypted = EncryptCtx::new(plaintext.len(), key)
        .encrypt(plaintext)
        .map_err(|error| Error::crypto(error.to_string()))?;
    // libsm 输出 C1C2C3，需改排为 C1C3C2 并去掉 C1 的 `0x04`。
    let (point, remainder) = encrypted.split_at(65);
    let (ciphertext, digest) = remainder.split_at(plaintext.len());
    Ok(hex::encode([&point[1..], digest, ciphertext].concat()))
}

pub(crate) fn random_sm4_key() -> String {
    let mut rng = rand::rngs::OsRng;
    (0..16)
        .map(|_| char::from(SM4_KEY_ALPHABET[rng.gen_range(0..SM4_KEY_ALPHABET.len())]))
        .collect()
}

pub(crate) fn sm4_decrypt(data: &str, key: &str) -> Result<Vec<u8>> {
    let ciphertext = STANDARD.decode(data).map_err(Error::crypto_source)?;
    let key: &[u8; 16] = key
        .as_bytes()
        .try_into()
        .map_err(|_| Error::crypto("SM4 密钥必须为 16 字节".into()))?;
    ecb::Decryptor::<sm4::Sm4>::new(key.into())
        .decrypt_padded_vec_mut::<Pkcs7>(&ciphertext)
        .map_err(|_| Error::crypto("SM4 密文或 PKCS#7 填充无效".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecb::cipher::BlockEncryptMut;
    use libsm::sm2::encrypt::DecryptCtx;

    #[test]
    fn rsa_key_accepts_cas_unwrapped_pem() {
        use rsa::{pkcs1::EncodeRsaPublicKey, pkcs8::EncodePublicKey};

        let key = rsa_key(include_str!("fixtures/cas-public-key.pem")).unwrap();
        let encrypted = rsa_encrypt(&key, "probe").unwrap();
        assert!(encrypted.starts_with("__RSA__"));
        let spki = key.to_public_key_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        let pkcs1 = key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        assert_eq!(rsa_key(&spki).unwrap(), key);
        assert_eq!(rsa_key(&pkcs1).unwrap(), key);
        assert!(rsa_key("-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----").is_err());
    }

    #[test]
    fn jwt_expiration_rejects_invalid_tokens() {
        let token = format!(
            "e30.{}.c2ln",
            URL_SAFE_NO_PAD.encode(br#"{"exp":2000000000}"#)
        );
        assert_eq!(jwt_expiration(&token).unwrap(), 2_000_000_000);
        for token in ["", "a.b", "e30.e30.***", "W10.e30.c2ln"] {
            assert!(jwt_expiration(token).is_err());
        }
    }

    #[test]
    fn sm2_ciphertext_matches_sm_crypto_c1c3c2_layout() {
        let context = SigCtx::new();
        let (public, secret) = context.new_keypair().unwrap();
        let key_hex = hex::encode(context.serialize_pubkey(&public, false).unwrap());
        let plaintext = br#"{"amt":"1"}"#;
        for key in [key_hex.as_str(), &key_hex[2..]] {
            let wire = hex::decode(sm2_encrypt(key, plaintext).unwrap()).unwrap();
            let (point, rest) = wire.split_at(64);
            let (digest, ciphertext) = rest.split_at(32);
            let native = [&[4][..], point, ciphertext, digest].concat();
            assert_eq!(
                DecryptCtx::new(plaintext.len(), secret.clone())
                    .decrypt(&native)
                    .unwrap(),
                plaintext
            );
        }
        assert!(sm2_encrypt("not hex", plaintext).is_err());
    }

    #[test]
    fn sm4_decrypt_uses_ascii_key_bytes() {
        let key = random_sm4_key();
        assert_eq!(key.len(), 16);
        let plaintext = br#"{"success":true}"#;
        let key_bytes: &[u8; 16] = key.as_bytes().try_into().unwrap();
        let ciphertext = ecb::Encryptor::<sm4::Sm4>::new(key_bytes.into())
            .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
        assert_eq!(
            sm4_decrypt(&STANDARD.encode(ciphertext), &key).unwrap(),
            plaintext
        );
        assert!(sm4_decrypt("AA==", &key).is_err());
        assert!(sm4_decrypt("AA==", "short").is_err());
    }
}

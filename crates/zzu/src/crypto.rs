use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
};
use ecb::cipher::{BlockDecryptMut, KeyInit, block_padding::Pkcs7};
use rand::Rng;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey, pkcs1::DecodeRsaPublicKey, pkcs8::DecodePublicKey};
use serde::Deserialize;
use sm2::{elliptic_curve::common::getrandom::SysRng, pke::EncryptingKey};

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
            .map_err(|error| Error::decode_with_source("JWT Base64URL 无效", error))
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
    let key = EncryptingKey::from_sec1_bytes(&key_bytes)
        .map_err(|error| Error::crypto_with_source("SM2 公钥无效", error))?;
    let encrypted = key
        .encrypt(&mut SysRng, plaintext)
        .map_err(|error| Error::crypto_with_source("SM2 加密失败", error))?;
    // sm2 默认按 C1C3C2 输出且 C1 带未压缩前缀 `0x04`，服务端按 sm-crypto 约定省略该前缀。
    let wire = encrypted
        .strip_prefix(&[0x04])
        .ok_or_else(|| Error::crypto("SM2 密文缺少未压缩点前缀".into()))?;
    Ok(hex::encode(wire))
}

pub(crate) fn random_sm4_key() -> String {
    let mut rng = rand::rngs::OsRng;
    (0..16)
        .map(|_| char::from(SM4_KEY_ALPHABET[rng.gen_range(0..SM4_KEY_ALPHABET.len())]))
        .collect()
}

pub(crate) fn sm4_decrypt(data: &str, key: &str) -> Result<Vec<u8>> {
    let ciphertext = STANDARD
        .decode(data)
        .map_err(|error| Error::crypto_with_source("SM4 密文 Base64 无效", error))?;
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
    use sm2::{
        NonZeroScalar,
        elliptic_curve::Generate,
        pke::{DecryptingKey, Mode},
    };

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
        let bad_b64 = jwt_expiration("e30.e30.***").unwrap_err();
        assert!(std::error::Error::source(&bad_b64).is_some());
    }

    #[test]
    fn sm2_ciphertext_matches_sm_crypto_c1c3c2_layout() {
        let secret = NonZeroScalar::try_generate_from_rng(&mut SysRng).unwrap();
        let decrypting = DecryptingKey::new_with_mode(secret, Mode::C1C3C2);
        let key_hex = hex::encode(decrypting.encrypting_key().to_sec1_bytes());
        let plaintext = br#"{"amt":"1"}"#;
        for key in [key_hex.as_str(), &key_hex[2..]] {
            let wire = hex::decode(sm2_encrypt(key, plaintext).unwrap()).unwrap();
            assert_eq!(wire.len(), 64 + 32 + plaintext.len());
            let sec1_cipher = [&[0x04][..], &wire].concat();
            assert_eq!(decrypting.decrypt(&sec1_cipher).unwrap(), plaintext);
        }
        assert!(sm2_encrypt("not hex", plaintext).is_err());
        let empty_err = sm2_encrypt(&key_hex, b"").unwrap_err();
        assert!(std::error::Error::source(&empty_err).is_some());
    }

    #[test]
    fn sm4_decrypt_uses_ascii_key_bytes() {
        let key = random_sm4_key();
        assert_eq!(key.len(), 16);
        assert!(key.bytes().all(|byte| SM4_KEY_ALPHABET.contains(&byte)));
        assert_eq!(
            hex::encode("rx4EG_5ziW3YMjZt".as_bytes()),
            "72783445475f357a695733594d6a5a74"
        );
        let plaintext = br#"{"success":true}"#;
        let key_bytes: &[u8; 16] = key.as_bytes().try_into().unwrap();
        let ciphertext = ecb::Encryptor::<sm4::Sm4>::new(key_bytes.into())
            .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
        assert_eq!(
            sm4_decrypt(&STANDARD.encode(ciphertext), &key).unwrap(),
            plaintext
        );
        assert!(sm4_decrypt("", &key).is_err());
        assert!(sm4_decrypt("AA==", &key).is_err());
        let b64_err = sm4_decrypt("***", &key).unwrap_err();
        assert!(std::error::Error::source(&b64_err).is_some());
        assert!(sm4_decrypt("AA==", "short").is_err());
    }
}

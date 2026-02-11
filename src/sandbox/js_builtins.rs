pub use xworkflow_sandbox_js::builtins::*;

#[cfg(test)]
mod tests {
    use super::*;
    use boa_engine::{Context, Source};
    use hmac::{Hmac, Mac};
    use regex::Regex;
    use sha2::Sha256;
    use uuid::Uuid;

    fn eval(context: &mut Context, code: &str) -> boa_engine::JsValue {
        context.eval(Source::from_bytes(code)).unwrap()
    }

    fn setup_context() -> Context {
        let mut ctx = Context::default();
        register_all(&mut ctx).unwrap();
        ctx
    }

    #[test]
    fn test_datetime_now_and_format() {
        let mut ctx = setup_context();
        let value = eval(&mut ctx, "datetime.now()");
        let number = value.to_number(&mut ctx).unwrap();
        assert!(number > 1_500_000_000_000.0);

        let formatted = eval(&mut ctx, "datetime.format()");
        let formatted_str = formatted
            .to_string(&mut ctx)
            .unwrap()
            .to_std_string_escaped();
        let re = Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$").unwrap();
        assert!(re.is_match(&formatted_str));
    }

    #[test]
    fn test_datetime_iso_string() {
        let mut ctx = setup_context();
        let value = eval(&mut ctx, "datetime.isoString()");
        let iso = value.to_string(&mut ctx).unwrap().to_std_string_escaped();
        assert!(iso.contains('T'));
        assert!(iso.ends_with('Z'));
    }

    #[test]
    fn test_crypto_hashes() {
        let mut ctx = setup_context();
        let md5_val = eval(&mut ctx, "crypto.md5('hello')")
            .to_string(&mut ctx)
            .unwrap()
            .to_std_string_escaped();
        assert_eq!(md5_val, "5d41402abc4b2a76b9719d911017c592");

        let sha1_val = eval(&mut ctx, "crypto.sha1('hello')")
            .to_string(&mut ctx)
            .unwrap()
            .to_std_string_escaped();
        assert_eq!(sha1_val, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");

        let sha256_val = eval(&mut ctx, "crypto.sha256('hello')")
            .to_string(&mut ctx)
            .unwrap()
            .to_std_string_escaped();
        assert_eq!(
            sha256_val,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_crypto_hmac() {
        let mut ctx = setup_context();
        let js_hmac = eval(&mut ctx, "crypto.hmacSha256('secret', 'hello')")
            .to_string(&mut ctx)
            .unwrap()
            .to_std_string_escaped();

        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(b"secret").unwrap();
        mac.update(b"hello");
        let expected = hex::encode(mac.finalize().into_bytes());
        assert_eq!(js_hmac, expected);
    }

    #[test]
    fn test_crypto_aes_round_trip() {
        let mut ctx = setup_context();
        let code = r#"(function() {
            var key = "000102030405060708090a0b0c0d0e0f000102030405060708090a0b0c0d0e0f";
            var enc = crypto.aesEncrypt("hello", key);
            return crypto.aesDecrypt(enc.ciphertext, key, enc.iv);
        })()"#;
        let value = eval(&mut ctx, code);
        let decrypted = value.to_string(&mut ctx).unwrap().to_std_string_escaped();
        assert_eq!(decrypted, "hello");
    }

    #[test]
    fn test_base64_round_trip() {
        let mut ctx = setup_context();
        let value = eval(&mut ctx, "atob(btoa('hello'))");
        let decoded = value.to_string(&mut ctx).unwrap().to_std_string_escaped();
        assert_eq!(decoded, "hello");
    }

    #[test]
    fn test_uuid_and_random() {
        let mut ctx = setup_context();
        let uuid_val = eval(&mut ctx, "uuidv4()")
            .to_string(&mut ctx)
            .unwrap()
            .to_std_string_escaped();
        assert!(Uuid::parse_str(&uuid_val).is_ok());

        let random_int = eval(&mut ctx, "randomInt(1, 10)")
            .to_number(&mut ctx)
            .unwrap();
        assert!(random_int >= 1.0 && random_int <= 10.0);

        let random_bytes = eval(&mut ctx, "randomBytes(16)")
            .to_string(&mut ctx)
            .unwrap()
            .to_std_string_escaped();
        assert_eq!(random_bytes.len(), 32);
        let re = Regex::new(r"^[0-9a-f]+$").unwrap();
        assert!(re.is_match(&random_bytes));
    }
}

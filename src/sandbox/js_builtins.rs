use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsNativeError, JsResult, JsValue, NativeFunction};
use chrono::{SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use md5::Md5;
use rand::Rng;
use rand::RngCore;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;

/// Register all built-in APIs into a boa context.
pub fn register_all(context: &mut Context) -> JsResult<()> {
    register_datetime(context)?;
    register_crypto(context)?;
    register_base64(context)?;
    register_uuid(context)?;
    register_random(context)?;
    Ok(())
}

fn register_datetime(context: &mut Context) -> JsResult<()> {
    let mut initializer = ObjectInitializer::new(context);
    initializer
        .function(NativeFunction::from_fn_ptr(datetime_now), js_string!("now"), 0)
        .function(
            NativeFunction::from_fn_ptr(datetime_timestamp),
            js_string!("timestamp"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(datetime_format),
            js_string!("format"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(datetime_iso_string),
            js_string!("isoString"),
            0,
        );
    let datetime = initializer.build();

    context.register_global_property(js_string!("datetime"), datetime, Attribute::all())?;
    Ok(())
}

fn register_crypto(context: &mut Context) -> JsResult<()> {
    let mut initializer = ObjectInitializer::new(context);
    initializer
        .function(NativeFunction::from_fn_ptr(crypto_md5), js_string!("md5"), 1)
        .function(NativeFunction::from_fn_ptr(crypto_sha1), js_string!("sha1"), 1)
        .function(
            NativeFunction::from_fn_ptr(crypto_sha256),
            js_string!("sha256"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(crypto_sha512),
            js_string!("sha512"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(crypto_hmac_sha256),
            js_string!("hmacSha256"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(crypto_hmac_sha512),
            js_string!("hmacSha512"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(crypto_aes_encrypt),
            js_string!("aesEncrypt"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(crypto_aes_decrypt),
            js_string!("aesDecrypt"),
            3,
        );
    let crypto = initializer.build();

    context.register_global_property(js_string!("crypto"), crypto, Attribute::all())?;
    Ok(())
}

fn register_base64(context: &mut Context) -> JsResult<()> {
    let btoa = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(base64_btoa))
        .name(js_string!("btoa"))
        .length(1)
        .constructor(false)
        .build();
    let atob = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(base64_atob))
        .name(js_string!("atob"))
        .length(1)
        .constructor(false)
        .build();

    context.register_global_property(js_string!("btoa"), btoa, Attribute::all())?;
    context.register_global_property(js_string!("atob"), atob, Attribute::all())?;
    Ok(())
}

fn register_uuid(context: &mut Context) -> JsResult<()> {
    let uuidv4 = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(uuid_v4))
        .name(js_string!("uuidv4"))
        .length(0)
        .constructor(false)
        .build();
    context.register_global_property(js_string!("uuidv4"), uuidv4, Attribute::all())?;
    Ok(())
}

fn register_random(context: &mut Context) -> JsResult<()> {
    let random_int_fn = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(random_int))
        .name(js_string!("randomInt"))
        .length(2)
        .constructor(false)
        .build();
    let random_float_fn = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(random_float))
        .name(js_string!("randomFloat"))
        .length(0)
        .constructor(false)
        .build();
    let random_bytes_fn = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(random_bytes))
        .name(js_string!("randomBytes"))
        .length(1)
        .constructor(false)
        .build();

    context.register_global_property(
        js_string!("randomInt"),
        random_int_fn,
        Attribute::all(),
    )?;
    context.register_global_property(
        js_string!("randomFloat"),
        random_float_fn,
        Attribute::all(),
    )?;
    context.register_global_property(
        js_string!("randomBytes"),
        random_bytes_fn,
        Attribute::all(),
    )?;
    Ok(())
}

fn type_error<T>(message: impl Into<String>) -> JsResult<T> {
    Err(JsNativeError::typ()
        .with_message(message.into())
        .into())
}

fn require_string_arg(
    args: &[JsValue],
    index: usize,
    context: &mut Context,
    name: &str,
) -> JsResult<String> {
    let value = match args.get(index) {
        Some(v) => v,
        None => return type_error(format!("Missing argument: {}", name)),
    };

    if value.is_undefined() || value.is_null() {
        return type_error(format!("Argument '{}' is required", name));
    }

    let js_str = value.to_string(context)?;
    Ok(js_str.to_std_string_escaped())
}

fn optional_string_arg(
    args: &[JsValue],
    index: usize,
    context: &mut Context,
) -> JsResult<Option<String>> {
    let value = match args.get(index) {
        Some(v) => v,
        None => return Ok(None),
    };

    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }

    let js_str = value.to_string(context)?;
    Ok(Some(js_str.to_std_string_escaped()))
}

fn require_number_arg(
    args: &[JsValue],
    index: usize,
    context: &mut Context,
    name: &str,
) -> JsResult<f64> {
    let value = match args.get(index) {
        Some(v) => v,
        None => return type_error(format!("Missing argument: {}", name)),
    };

    if value.is_undefined() || value.is_null() {
        return type_error(format!("Argument '{}' is required", name));
    }

    let number = value.to_number(context)?;
    if !number.is_finite() {
        return type_error(format!("Argument '{}' must be a finite number", name));
    }
    Ok(number)
}

// ------------------ datetime ------------------

fn datetime_now(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    let ts = Utc::now().timestamp_millis() as f64;
    Ok(JsValue::from(ts))
}

fn datetime_timestamp(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let ts = Utc::now().timestamp() as f64;
    Ok(JsValue::from(ts))
}

fn datetime_format(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let fmt = optional_string_arg(args, 0, context)?
        .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".to_string());
    let formatted = Utc::now().format(&fmt).to_string();
    Ok(JsValue::from(js_string!(formatted)))
}

fn datetime_iso_string(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let iso = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    Ok(JsValue::from(js_string!(iso)))
}

// ------------------ crypto ------------------

fn hash_hex<D: Digest + Default>(input: &str) -> String {
    let mut hasher = D::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

fn crypto_md5(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, context, "str")?;
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    let output = hex::encode(hasher.finalize());
    Ok(JsValue::from(js_string!(output)))
}

fn crypto_sha1(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, context, "str")?;
    let output = hash_hex::<Sha1>(&input);
    Ok(JsValue::from(js_string!(output)))
}

fn crypto_sha256(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, context, "str")?;
    let output = hash_hex::<Sha256>(&input);
    Ok(JsValue::from(js_string!(output)))
}

fn crypto_sha512(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, context, "str")?;
    let output = hash_hex::<Sha512>(&input);
    Ok(JsValue::from(js_string!(output)))
}

fn crypto_hmac_sha256(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = require_string_arg(args, 0, context, "key")?;
    let data = require_string_arg(args, 1, context, "data")?;

    let mut mac = match <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return type_error("Invalid HMAC key"),
    };
    mac.update(data.as_bytes());
    let result = mac.finalize().into_bytes();
    Ok(JsValue::from(js_string!(hex::encode(result))))
}

fn crypto_hmac_sha512(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let key = require_string_arg(args, 0, context, "key")?;
    let data = require_string_arg(args, 1, context, "data")?;

    let mut mac = match <Hmac<Sha512> as Mac>::new_from_slice(key.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return type_error("Invalid HMAC key"),
    };
    mac.update(data.as_bytes());
    let result = mac.finalize().into_bytes();
    Ok(JsValue::from(js_string!(hex::encode(result))))
}

fn crypto_aes_encrypt(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let plaintext = require_string_arg(args, 0, context, "plaintext")?;
    let key_hex = require_string_arg(args, 1, context, "key_hex")?;

    let key_bytes = match hex::decode(key_hex) {
        Ok(bytes) => bytes,
        Err(_) => return type_error("key_hex must be a valid hex string"),
    };
    if key_bytes.len() != 32 {
        return type_error("key_hex must be 32 bytes (64 hex chars)");
    }

    let cipher = match Aes256Gcm::new_from_slice(&key_bytes) {
        Ok(cipher) => cipher,
        Err(_) => return type_error("Failed to initialize AES-256-GCM"),
    };

    let mut iv = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut iv);
    let nonce = Nonce::from_slice(&iv);

    let ciphertext = match cipher.encrypt(nonce, plaintext.as_bytes()) {
        Ok(data) => data,
        Err(_) => return type_error("AES-256-GCM encryption failed"),
    };

    let ciphertext_b64 = BASE64_STANDARD.encode(ciphertext);
    let iv_b64 = BASE64_STANDARD.encode(iv);

    let mut initializer = ObjectInitializer::new(context);
    initializer
        .property(
            js_string!("ciphertext"),
            JsValue::from(js_string!(ciphertext_b64)),
            Attribute::all(),
        )
        .property(
            js_string!("iv"),
            JsValue::from(js_string!(iv_b64)),
            Attribute::all(),
        );
    let result = initializer.build();

    Ok(result.into())
}

fn crypto_aes_decrypt(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ciphertext_b64 = require_string_arg(args, 0, context, "ciphertext_base64")?;
    let key_hex = require_string_arg(args, 1, context, "key_hex")?;
    let iv_b64 = require_string_arg(args, 2, context, "iv_base64")?;

    let ciphertext = match BASE64_STANDARD.decode(ciphertext_b64.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return type_error("ciphertext_base64 must be valid base64"),
    };

    let iv = match BASE64_STANDARD.decode(iv_b64.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return type_error("iv_base64 must be valid base64"),
    };

    if iv.len() != 12 {
        return type_error("iv must be 12 bytes for AES-256-GCM");
    }

    let key_bytes = match hex::decode(key_hex) {
        Ok(bytes) => bytes,
        Err(_) => return type_error("key_hex must be a valid hex string"),
    };
    if key_bytes.len() != 32 {
        return type_error("key_hex must be 32 bytes (64 hex chars)");
    }

    let cipher = match Aes256Gcm::new_from_slice(&key_bytes) {
        Ok(cipher) => cipher,
        Err(_) => return type_error("Failed to initialize AES-256-GCM"),
    };

    let nonce = Nonce::from_slice(&iv);
    let plaintext = match cipher.decrypt(nonce, ciphertext.as_ref()) {
        Ok(data) => data,
        Err(_) => return type_error("AES-256-GCM decryption failed"),
    };

    let plaintext_str = match String::from_utf8(plaintext) {
        Ok(s) => s,
        Err(_) => return type_error("Decrypted data is not valid UTF-8"),
    };

    Ok(JsValue::from(js_string!(plaintext_str)))
}

// ------------------ base64 ------------------

fn base64_btoa(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, context, "str")?;
    let encoded = BASE64_STANDARD.encode(input.as_bytes());
    Ok(JsValue::from(js_string!(encoded)))
}

fn base64_atob(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = require_string_arg(args, 0, context, "base64_str")?;
    let decoded = match BASE64_STANDARD.decode(input.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return type_error("Invalid base64 string"),
    };

    let decoded_str = match String::from_utf8(decoded) {
        Ok(s) => s,
        Err(_) => return type_error("Decoded base64 is not valid UTF-8"),
    };

    Ok(JsValue::from(js_string!(decoded_str)))
}

// ------------------ uuid ------------------

fn uuid_v4(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    let id = Uuid::new_v4().to_string();
    Ok(JsValue::from(js_string!(id)))
}

// ------------------ random ------------------

fn random_int(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let min = require_number_arg(args, 0, context, "min")?;
    let max = require_number_arg(args, 1, context, "max")?;

    let min_int = min as i64;
    let max_int = max as i64;

    if min_int > max_int {
        return type_error("min must be <= max");
    }

    let value = rand::thread_rng().gen_range(min_int..=max_int);
    Ok(JsValue::from(value as f64))
}

fn random_float(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    let value: f64 = rand::thread_rng().gen();
    Ok(JsValue::from(value))
}

fn random_bytes(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let length = require_number_arg(args, 0, context, "length")?;
    if length < 0.0 {
        return type_error("length must be >= 0");
    }

    let len = length as usize;
    let mut bytes = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut bytes);
    let hex_str = hex::encode(bytes);
    Ok(JsValue::from(js_string!(hex_str)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use boa_engine::Source;
    use regex::Regex;

    fn eval(context: &mut Context, code: &str) -> JsValue {
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

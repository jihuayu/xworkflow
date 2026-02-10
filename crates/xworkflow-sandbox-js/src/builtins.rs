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
    let random_int_fn = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(random_int),
    )
    .name(js_string!("randomInt"))
    .length(2)
    .constructor(false)
    .build();
    let random_float_fn = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(random_float),
    )
    .name(js_string!("randomFloat"))
    .length(0)
    .constructor(false)
    .build();
    let random_bytes_fn = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(random_bytes),
    )
    .name(js_string!("randomBytes"))
    .length(1)
    .constructor(false)
    .build();

    context.register_global_property(js_string!("randomInt"), random_int_fn.clone(), Attribute::all())?;
    context.register_global_property(js_string!("randomFloat"), random_float_fn.clone(), Attribute::all())?;
    context.register_global_property(js_string!("randomBytes"), random_bytes_fn.clone(), Attribute::all())?;

    let mut initializer = ObjectInitializer::new(context);
    initializer
        .function(NativeFunction::from_fn_ptr(random_int), js_string!("randomInt"), 2)
        .function(NativeFunction::from_fn_ptr(random_float), js_string!("randomFloat"), 0)
        .function(NativeFunction::from_fn_ptr(random_bytes), js_string!("randomBytes"), 1);
    let random = initializer.build();
    context.register_global_property(js_string!("random"), random, Attribute::all())?;
    Ok(())
}

fn datetime_now(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(Utc::now().timestamp_millis()))
}

fn datetime_timestamp(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(Utc::now().timestamp_millis()))
}

fn datetime_format(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let fmt = args
        .get(0)
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".to_string());
    let formatted = Utc::now().format(&fmt).to_string();
    Ok(JsValue::from(js_string!(formatted)))
}

fn datetime_iso_string(
    _this: &JsValue,
    _args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let iso = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    Ok(JsValue::from(js_string!(iso)))
}

fn crypto_md5(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let input = js_arg_to_string(args.get(0));
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    Ok(JsValue::from(js_string!(hex::encode(result))))
}

fn crypto_sha1(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let input = js_arg_to_string(args.get(0));
    let mut hasher = Sha1::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    Ok(JsValue::from(js_string!(hex::encode(result))))
}

fn crypto_sha256(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let input = js_arg_to_string(args.get(0));
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    Ok(JsValue::from(js_string!(hex::encode(result))))
}

fn crypto_sha512(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let input = js_arg_to_string(args.get(0));
    let mut hasher = Sha512::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    Ok(JsValue::from(js_string!(hex::encode(result))))
}

fn crypto_hmac_sha256(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let key = js_arg_to_string(args.get(0));
    let msg = js_arg_to_string(args.get(1));
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes())
        .map_err(|_| JsNativeError::typ().with_message("Invalid HMAC key"))?;
    mac.update(msg.as_bytes());
    let result = mac.finalize().into_bytes();
    Ok(JsValue::from(js_string!(hex::encode(result))))
}

fn crypto_hmac_sha512(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let key = js_arg_to_string(args.get(0));
    let msg = js_arg_to_string(args.get(1));
    let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(key.as_bytes())
        .map_err(|_| JsNativeError::typ().with_message("Invalid HMAC key"))?;
    mac.update(msg.as_bytes());
    let result = mac.finalize().into_bytes();
    Ok(JsValue::from(js_string!(hex::encode(result))))
}

fn crypto_aes_encrypt(
    _this: &JsValue,
    args: &[JsValue],
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let plaintext = js_arg_to_string(args.get(0));
    let key_hex = js_arg_to_string(args.get(1));

    let key_bytes = hex::decode(key_hex)
        .map_err(|_| JsNativeError::typ().with_message("key_hex must be a valid hex string"))?;
    if key_bytes.len() != 32 {
        return Err(JsNativeError::typ()
            .with_message("key_hex must be 32 bytes (64 hex chars)")
            .into());
    }

    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|_| JsNativeError::typ().with_message("Failed to initialize AES-256-GCM"))?;

    let mut iv = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut iv);
    let nonce = Nonce::from_slice(&iv);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| JsNativeError::error().with_message("AES-256-GCM encryption failed"))?;

    let ciphertext_b64 = BASE64_STANDARD.encode(ciphertext);
    let iv_b64 = BASE64_STANDARD.encode(iv);

    let mut initializer = ObjectInitializer::new(_ctx);
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
    _ctx: &mut Context,
) -> JsResult<JsValue> {
    let ciphertext_b64 = js_arg_to_string(args.get(0));
    let key_hex = js_arg_to_string(args.get(1));
    let iv_b64 = js_arg_to_string(args.get(2));

    let ciphertext = BASE64_STANDARD
        .decode(ciphertext_b64.as_bytes())
        .map_err(|_| JsNativeError::typ().with_message("ciphertext_base64 must be valid base64"))?;

    let iv = BASE64_STANDARD
        .decode(iv_b64.as_bytes())
        .map_err(|_| JsNativeError::typ().with_message("iv_base64 must be valid base64"))?;

    if iv.len() != 12 {
        return Err(JsNativeError::typ().with_message("iv must be 12 bytes for AES-256-GCM").into());
    }

    let key_bytes = hex::decode(key_hex)
        .map_err(|_| JsNativeError::typ().with_message("key_hex must be a valid hex string"))?;
    if key_bytes.len() != 32 {
        return Err(JsNativeError::typ()
            .with_message("key_hex must be 32 bytes (64 hex chars)")
            .into());
    }

    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|_| JsNativeError::typ().with_message("Failed to initialize AES-256-GCM"))?;

    let nonce = Nonce::from_slice(&iv);
    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| JsNativeError::error().with_message("AES-256-GCM decryption failed"))?;

    let plaintext_str = String::from_utf8(plaintext)
        .map_err(|_| JsNativeError::typ().with_message("Decrypted data is not valid UTF-8"))?;

    Ok(JsValue::from(js_string!(plaintext_str)))
}

fn base64_btoa(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let input = js_arg_to_string(args.get(0));
    Ok(JsValue::from(js_string!(BASE64_STANDARD.encode(input.as_bytes()))))
}

fn base64_atob(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let input = js_arg_to_string(args.get(0));
    let decoded = BASE64_STANDARD
        .decode(input.as_bytes())
        .map_err(|_| JsNativeError::typ().with_message("Invalid base64 input"))?;
    let s = String::from_utf8(decoded)
        .map_err(|_| JsNativeError::typ().with_message("Invalid UTF-8 string"))?;
    Ok(JsValue::from(js_string!(s)))
}

fn uuid_v4(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let id = Uuid::new_v4().to_string();
    Ok(JsValue::from(js_string!(id)))
}

fn random_int(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let min = args
        .get(0)
        .and_then(|v| v.as_number())
        .unwrap_or(0.0) as i64;
    let max = args
        .get(1)
        .and_then(|v| v.as_number())
        .unwrap_or(100.0) as i64;

    if min > max {
        return Err(JsNativeError::range()
            .with_message("min should be <= max")
            .into());
    }

    let value = rand::thread_rng().gen_range(min..=max);
    Ok(JsValue::from(value as f64))
}

fn random_float(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let value: f64 = rand::thread_rng().gen();
    Ok(JsValue::from(value))
}

fn random_bytes(_this: &JsValue, args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let len = args
        .get(0)
        .and_then(|v| v.as_number())
        .unwrap_or(16.0) as usize;
    let mut bytes = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut bytes);
    Ok(JsValue::from(js_string!(hex::encode(bytes))))
}

fn js_arg_to_string(arg: Option<&JsValue>) -> String {
    arg.and_then(|v| v.as_string())
    .map(|s| s.to_std_string_escaped())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uuid() {
        let uuid_val = Uuid::new_v4().to_string();
        assert!(Uuid::parse_str(&uuid_val).is_ok());
    }
}

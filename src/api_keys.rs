//! API keys for the catch-up endpoints and transcription providers, held in the
//! secret-service keyring (oo7), one item per slot.

pub const SUMMARIZER: &str = "catchup.summarizer";
pub const FALLBACK: &str = "catchup.fallback";

/// The slot of the `[[catchup.stt]]` provider called `name`.
pub fn stt_slot(name: &str) -> String {
    format!("stt.{name}")
}

fn attrs(slot: &str) -> [(&str, &str); 3] {
    [("app", "moderntube"), ("kind", "api-key"), ("slot", slot)]
}

/// The key stored for `slot`.
pub async fn load(slot: &str) -> Option<String> {
    // On a private D-Bus session the secret-service activation never answers.
    if crate::hooks::flag("no-keyring") {
        return None;
    }
    let kr = oo7::Keyring::new().await.ok()?;
    let _ = kr.unlock().await;
    let items = kr.search_items(&attrs(slot)).await.ok()?;
    let secret = items.first()?.secret().await.ok()?;
    String::from_utf8(secret.as_bytes().to_vec()).ok().filter(|k| !k.is_empty())
}

/// Stores `key` for `slot`; an empty key deletes the item.
pub async fn save(slot: &str, key: &str) -> Result<(), String> {
    if crate::hooks::flag("no-keyring") {
        return Err("the keyring is off (--no-keyring)".into());
    }
    let kr = oo7::Keyring::new().await.map_err(|e| e.to_string())?;
    let _ = kr.unlock().await;
    let r = if key.is_empty() {
        kr.delete(&attrs(slot)).await
    } else {
        kr.create_item(&format!("moderntube API key ({slot})"), &attrs(slot), oo7::Secret::text(key), true)
            .await
            .map(drop)
    };
    r.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "live: needs a running secret service"]
    async fn a_saved_key_loads_back_and_an_empty_save_deletes_it() {
        let slot = "test.roundtrip";
        save(slot, "k1").await.unwrap();
        save(slot, "k2").await.unwrap();
        assert_eq!(load(slot).await.as_deref(), Some("k2"));
        save(slot, "").await.unwrap();
        assert_eq!(load(slot).await, None);
    }
}

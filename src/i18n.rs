use rust_embed::RustEmbed;
use serde_json::Value;

#[derive(RustEmbed)]
#[folder = "locales/"]
struct Asset;

pub struct I18n {
    texts: Value,
    pub current_lang: String,
}

impl I18n {
    pub fn new(lang: &str) -> Self {
        let path = format!("{}.json", lang);
        let content = if let Some(file) = Asset::get(&path) {
            std::str::from_utf8(file.data.as_ref())
                .expect("UTF-8")
                .to_string()
        } else {
            r#"{"processing": "...", "wait": "..."}"#.to_string()
        };
        I18n {
            texts: serde_json::from_str(&content).expect("JSON"),
            current_lang: lang.to_string(),
        }
    }

    pub fn get(&self, key: &str) -> String {
        self.texts
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(key)
            .to_string()
    }

    pub fn get_args(&self, key: &str, args: &[String]) -> String {
        let mut s = self.get(key);
        for (i, arg) in args.iter().enumerate() {
            let placeholder = format!("{{{}}}", i);
            s = s.replace(&placeholder, arg);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_i18n_translation() {
        let i18n = I18n::new("en");
        assert_eq!(i18n.get("processing"), "🤔 Processing...");
    }

    #[test]
    fn test_i18n_args_replacement() {
        let mut i18n = I18n::new("en");
        // 手動模擬帶參數的翻譯字串
        i18n.texts["test_key"] = serde_json::Value::String("Value: {0}, {1}".to_string());

        let result = i18n.get_args("test_key", &["A".into(), "B".into()]);
        assert_eq!(result, "Value: A, B");
    }

    #[test]
    fn test_i18n_fallback_to_key() {
        let i18n = I18n::new("en");
        assert_eq!(i18n.get("non_existent_key_123"), "non_existent_key_123");
    }
}

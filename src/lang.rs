//! Idioma da interface: `SSHDECK_LANG`, a escolha salva no cofre, ou o locale do sistema.

pub const LANGS: [&str; 3] = ["pt", "en", "es"];
/// Nomes na própria língua, para o popup de escolha.
pub const NAMES: [&str; 3] = ["Português", "English", "Español"];

/// `SSHDECK_LANG` tem prioridade sobre tudo, inclusive a escolha salva.
pub fn forced() -> Option<&'static str> {
    std::env::var("SSHDECK_LANG").ok().and_then(|v| parse(&v))
}

/// Idioma inicial: `SSHDECK_LANG`, senão LC_ALL / LC_MESSAGES / LANG; inglês se nada bater.
pub fn detect() -> &'static str {
    forced()
        .or_else(|| {
            ["LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .filter_map(|k| std::env::var(k).ok())
                .find(|v| !v.is_empty())
                .and_then(|v| parse(&v))
        })
        .unwrap_or("en")
}

/// "pt_BR.UTF-8" → "pt"
pub fn parse(value: &str) -> Option<&'static str> {
    let code = value.split(['_', '.', '-', '@']).next()?.to_lowercase();
    LANGS.into_iter().find(|l| *l == code)
}

pub fn set(code: &str) {
    rust_i18n::set_locale(code);
}

pub fn current() -> String {
    rust_i18n::locale().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_locales() {
        assert_eq!(parse("pt_BR.UTF-8"), Some("pt"));
        assert_eq!(parse("es_PY.UTF-8"), Some("es"));
        assert_eq!(parse("en_US"), Some("en"));
        assert_eq!(parse("C"), None);
        assert_eq!(parse("de_DE.UTF-8"), None);
    }
}

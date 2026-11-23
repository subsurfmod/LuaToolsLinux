//! Steam, SteamDB, Community, and `steam://` AppID extractor.

use std::sync::OnceLock;

use regex::Regex;

const MAX_APP_ID: u32 = 2_000_000_000;

fn link_patterns() -> &'static Vec<Regex> {
    static PATS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)steamdb\.info/(?:app|sub)/(\d+)").unwrap(),
            Regex::new(r"(?i)store\.steampowered\.com/(?:agecheck/)?(?:app|sub)/(\d+)").unwrap(),
            Regex::new(r"(?i)steamcommunity\.com/(?:app|sub)/(\d+)").unwrap(),
            Regex::new(r"(?i)steampowered\.com/(?:app|sub)/(\d+)").unwrap(),
            Regex::new(r"(?i)steam://(?:store|install|run|rungameid|launch)/(\d+)").unwrap(),
            Regex::new(r"(?i)^appid[:\s#]*(\d+)$").unwrap(),
        ]
    })
}

pub struct SteamLinkParser;

impl SteamLinkParser {
    pub fn app_id_from(text: &str) -> Option<u32> {
        let cleaned = text.trim();
        if cleaned.is_empty() {
            return None;
        }
        if cleaned.chars().all(|c| c.is_ascii_digit()) {
            let aid = cleaned.parse::<u32>().ok()?;
            return (aid > 0 && aid < MAX_APP_ID).then_some(aid);
        }

        for pat in link_patterns() {
            if let Some(cap) = pat.captures(cleaned) {
                if let Some(m) = cap.get(1) {
                    if let Ok(aid) = m.as_str().parse::<u32>() {
                        if aid > 0 && aid < MAX_APP_ID {
                            return Some(aid);
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_urls_and_ignores_game_title_numbers() {
        assert_eq!(SteamLinkParser::app_id_from("1245620"), Some(1245620));
        assert_eq!(
            SteamLinkParser::app_id_from("https://store.steampowered.com/app/1245620/ELDEN_RING/"),
            Some(1245620)
        );
        assert_eq!(
            SteamLinkParser::app_id_from("https://steamdb.info/app/730/depots/"),
            Some(730)
        );
        assert_eq!(
            SteamLinkParser::app_id_from("steam://install/570"),
            Some(570)
        );
        assert_eq!(SteamLinkParser::app_id_from("Warhammer 40000"), None);
        assert_eq!(SteamLinkParser::app_id_from("Battlefield 2042"), None);
    }
}

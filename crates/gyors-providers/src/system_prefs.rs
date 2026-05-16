//! MacOS System Settings deep-links. Open a specific pane by fuzzy-name
//!
//! Provides a full set of `SettingsPane` candidates that the main ranker
//! fuzzy-matches against user's query - no keyword needed. Typing
//! `wifi`, `display`, `bluetooth`, etc. surfaces matching pane
//!
//! Activation emits `Effect::OpenUrl(x-apple.systempreferences:...)` which
//! SwiftUI shell hands to `NSWorkspace.open`

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::sync::LazyLock;

pub struct SystemPrefsProvider;

/// Pre-built once at process start, never allocated in the hot path.
/// Per-query we just `.clone()` the whole Vec - still N `String`
/// clones, but avoids the 47 x (`format!`, `to_string`, `Action::new`,
/// `format!`) work the old implementation did every keystroke
static CACHED_CANDIDATES: LazyLock<Vec<Candidate>> =
    LazyLock::new(|| PANES.iter().map(to_candidate).collect());

#[derive(Debug, Clone, Copy)]
struct PrefsPane {
    id: &'static str,
    title: &'static str,
    keywords: &'static str,
    symbol: &'static str,
    url: &'static str,
}

const PANES: &[PrefsPane] = &[
    PrefsPane { id: "wifi",            title: "Settings: Wi-Fi",                     keywords: "wifi wi-fi network wireless internet",              symbol: "wifi",                    url: "x-apple.systempreferences:com.apple.wifi-settings-extension" },
    PrefsPane { id: "bluetooth",       title: "Settings: Bluetooth",                 keywords: "bluetooth bt airpods headphones pair",              symbol: "bluetooth",               url: "x-apple.systempreferences:com.apple.BluetoothSettings" },
    PrefsPane { id: "network",         title: "Settings: Network",                   keywords: "network ethernet internet dns proxy",               symbol: "network",                 url: "x-apple.systempreferences:com.apple.Network-Settings.extension" },
    PrefsPane { id: "vpn",             title: "Settings: VPN",                       keywords: "vpn virtual private network tunnel",                symbol: "shield.lefthalf.filled",  url: "x-apple.systempreferences:com.apple.Network-Settings.extension?VPN" },
    PrefsPane { id: "firewall",        title: "Settings: Firewall",                  keywords: "firewall incoming connections block network",       symbol: "flame.fill",              url: "x-apple.systempreferences:com.apple.Network-Settings.extension?Firewall" },

    PrefsPane { id: "display",         title: "Settings: Displays",                  keywords: "display screen monitor resolution brightness hdr",  symbol: "display",                 url: "x-apple.systempreferences:com.apple.Displays-Settings.extension" },
    PrefsPane { id: "sound",           title: "Settings: Sound",                     keywords: "sound audio volume output input speakers mic",      symbol: "speaker.wave.2.fill",     url: "x-apple.systempreferences:com.apple.preference.sound" },
    PrefsPane { id: "keyboard",        title: "Settings: Keyboard",                  keywords: "keyboard typing shortcuts layout key repeat",       symbol: "keyboard.fill",           url: "x-apple.systempreferences:com.apple.Keyboard-Settings.extension" },
    PrefsPane { id: "trackpad",        title: "Settings: Trackpad",                  keywords: "trackpad gestures tap scroll zoom",                 symbol: "hand.draw.fill",          url: "x-apple.systempreferences:com.apple.Trackpad-Settings.extension" },
    PrefsPane { id: "mouse",           title: "Settings: Mouse",                     keywords: "mouse pointer cursor scroll",                       symbol: "computermouse.fill",      url: "x-apple.systempreferences:com.apple.Mouse-Settings.extension" },
    PrefsPane { id: "printers",        title: "Settings: Printers & Scanners",       keywords: "printer scanner print fax airprint",                symbol: "printer.fill",            url: "x-apple.systempreferences:com.apple.Print-Scan-Settings.extension" },

    PrefsPane { id: "appearance",      title: "Settings: Appearance",                keywords: "appearance theme dark light mode accent",           symbol: "paintbrush.fill",         url: "x-apple.systempreferences:com.apple.Appearance-Settings.extension" },
    PrefsPane { id: "wallpaper",       title: "Settings: Wallpaper",                 keywords: "wallpaper background desktop picture",              symbol: "photo.fill",              url: "x-apple.systempreferences:com.apple.Wallpaper-Settings.extension" },
    PrefsPane { id: "screen-saver",    title: "Settings: Screen Saver",              keywords: "screen saver screensaver aerial idle",              symbol: "sparkle.magnifyingglass", url: "x-apple.systempreferences:com.apple.ScreenSaver-Settings.extension" },
    PrefsPane { id: "dock",            title: "Settings: Desktop & Dock",            keywords: "dock desktop menu bar hot corners mission control", symbol: "dock.rectangle",          url: "x-apple.systempreferences:com.apple.Desktop-Settings.extension" },
    PrefsPane { id: "control-center",  title: "Settings: Control Center",            keywords: "control center menu bar modules widgets",           symbol: "switch.2",                url: "x-apple.systempreferences:com.apple.ControlCenter-Settings.extension" },
    PrefsPane { id: "lock-screen",     title: "Settings: Lock Screen",               keywords: "lock screen login idle sleep password",             symbol: "lock.display",            url: "x-apple.systempreferences:com.apple.Lock-Screen-Settings.extension" },

    PrefsPane { id: "security",        title: "Settings: Privacy & Security",        keywords: "privacy security permissions firewall filevault gatekeeper", symbol: "lock.shield.fill",            url: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension" },
    PrefsPane { id: "privacy-location",title: "Privacy: Location Services",          keywords: "location services gps coordinates maps",            symbol: "location.fill",           url: "x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices" },
    PrefsPane { id: "privacy-camera",  title: "Privacy: Camera",                     keywords: "camera webcam video permission privacy",            symbol: "camera.fill",             url: "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera" },
    PrefsPane { id: "privacy-mic",     title: "Privacy: Microphone",                 keywords: "microphone mic audio permission privacy",           symbol: "mic.fill",                url: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone" },
    PrefsPane { id: "privacy-screen",  title: "Privacy: Screen Recording",           keywords: "screen recording capture share permission privacy", symbol: "rectangle.on.rectangle",  url: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture" },
    PrefsPane { id: "privacy-a11y",    title: "Privacy: Accessibility",              keywords: "accessibility control permission automation privacy",symbol: "figure.wave",             url: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" },
    PrefsPane { id: "privacy-files",   title: "Privacy: Full Disk Access",           keywords: "full disk access fda files permission privacy",     symbol: "externaldrive.fill",      url: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles" },
    PrefsPane { id: "privacy-input",   title: "Privacy: Input Monitoring",           keywords: "input monitoring keyboard watch keys permission",   symbol: "keyboard.badge.eye",      url: "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent" },
    PrefsPane { id: "privacy-auto",    title: "Privacy: Automation",                 keywords: "automation apple events applescript permission",    symbol: "gearshape.2.fill",        url: "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation" },
    PrefsPane { id: "privacy-dev",     title: "Privacy: Developer Tools",            keywords: "developer tools dev xcode permission privacy",      symbol: "hammer.fill",             url: "x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools" },
    PrefsPane { id: "passwords",       title: "Settings: Passwords",                 keywords: "passwords keychain credentials login 2fa otp",      symbol: "key.fill",                url: "x-apple.systempreferences:com.apple.Passwords-Settings.extension" },

    PrefsPane { id: "accessibility",   title: "Settings: Accessibility",             keywords: "accessibility a11y voiceover zoom contrast dictation",symbol: "figure.stand",            url: "x-apple.systempreferences:com.apple.preference.universalaccess" },
    PrefsPane { id: "focus",           title: "Settings: Focus",                     keywords: "focus do not disturb dnd schedules",                symbol: "moon.circle.fill",        url: "x-apple.systempreferences:com.apple.Focus-Settings.extension" },
    PrefsPane { id: "notifications",   title: "Settings: Notifications",             keywords: "notifications alerts banners badges",               symbol: "bell.badge.fill",         url: "x-apple.systempreferences:com.apple.Notifications-Settings.extension" },
    PrefsPane { id: "battery",         title: "Settings: Battery",                   keywords: "battery power energy lowpower charging",            symbol: "battery.100",             url: "x-apple.systempreferences:com.apple.Battery-Settings.extension" },
    PrefsPane { id: "storage",         title: "Settings: Storage",                   keywords: "storage disk space capacity manage",                symbol: "internaldrive.fill",      url: "x-apple.systempreferences:com.apple.settings.Storage" },
    PrefsPane { id: "users",           title: "Settings: Users & Groups",            keywords: "users groups login accounts admin guest",           symbol: "person.2.fill",           url: "x-apple.systempreferences:com.apple.Users-Groups-Settings.extension" },
    PrefsPane { id: "login-items",     title: "Settings: Login Items",               keywords: "login items startup autostart background",          symbol: "play.rectangle.fill",     url: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension" },
    PrefsPane { id: "date-time",       title: "Settings: Date & Time",               keywords: "date time clock timezone ntp",                      symbol: "clock.badge.fill",        url: "x-apple.systempreferences:com.apple.Date-Time-Settings.extension" },
    PrefsPane { id: "language",        title: "Settings: Language & Region",         keywords: "language region locale input keyboard",             symbol: "globe",                   url: "x-apple.systempreferences:com.apple.Localization-Settings.extension" },
    PrefsPane { id: "sharing",         title: "Settings: Sharing",                   keywords: "sharing screen file airdrop remote printer media",  symbol: "square.and.arrow.up.fill",url: "x-apple.systempreferences:com.apple.Sharing-Settings.extension" },
    PrefsPane { id: "time-machine",    title: "Settings: Time Machine",              keywords: "time machine backup restore",                       symbol: "clock.arrow.circlepath",  url: "x-apple.systempreferences:com.apple.Time-Machine-Settings.extension" },
    PrefsPane { id: "software-update", title: "Settings: Software Update",           keywords: "software update macos upgrade",                     symbol: "arrow.down.circle.fill",  url: "x-apple.systempreferences:com.apple.Software-Update-Settings.extension" },
    PrefsPane { id: "transfer-reset",  title: "Settings: Transfer or Reset",         keywords: "transfer reset erase factory migration",            symbol: "arrow.triangle.2.circlepath", url: "x-apple.systempreferences:com.apple.Transfer-Reset-Settings.extension" },
    PrefsPane { id: "extensions",      title: "Settings: Extensions",                keywords: "extensions plugins share actions",                  symbol: "puzzlepiece.extension.fill",  url: "x-apple.systempreferences:com.apple.Extensions-Settings.extension" },

    PrefsPane { id: "apple-id",        title: "Settings: Apple ID",                  keywords: "apple id icloud account signin",                    symbol: "applelogo",               url: "x-apple.systempreferences:com.apple.systempreferences.AppleIDSettings" },
    PrefsPane { id: "internet-accounts", title: "Settings: Internet Accounts",       keywords: "internet accounts mail calendar contacts google",   symbol: "at",                      url: "x-apple.systempreferences:com.apple.Internet-Accounts-Settings.extension" },
    PrefsPane { id: "game-center",     title: "Settings: Game Center",               keywords: "game center games",                                 symbol: "gamecontroller.fill",     url: "x-apple.systempreferences:com.apple.Game-Center-Settings.extension" },
    PrefsPane { id: "family",          title: "Settings: Family",                    keywords: "family sharing members children",                   symbol: "figure.2.and.child.holdinghands", url: "x-apple.systempreferences:com.apple.Family-Settings.extension" },
    PrefsPane { id: "screen-time",     title: "Settings: Screen Time",               keywords: "screen time app limits downtime",                   symbol: "hourglass",               url: "x-apple.systempreferences:com.apple.Screen-Time-Settings.extension" },

    PrefsPane { id: "siri",            title: "Settings: Siri & Spotlight",          keywords: "siri spotlight search assistant dictation",         symbol: "mic.and.signal.meter.fill", url: "x-apple.systempreferences:com.apple.Siri-Settings.extension" },
    PrefsPane { id: "general",         title: "Settings: General",                   keywords: "general about software update name",                symbol: "gearshape.fill",          url: "x-apple.systempreferences:com.apple.systempreferences" },
];

#[async_trait]
impl Provider for SystemPrefsProvider {
    fn id(&self) -> &str {
        "prefs"
    }

    async fn query(&self, _query: &Query) -> Vec<Candidate> {
        CACHED_CANDIDATES.clone()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let pane_id = id
            .strip_prefix("prefs::")
            .ok_or_else(|| anyhow::anyhow!("invalid prefs candidate id: {id}"))?;
        let pane = PANES
            .iter()
            .find(|p| p.id == pane_id)
            .ok_or_else(|| anyhow::anyhow!("unknown settings pane: {pane_id}"))?;
        Ok(Effect::OpenUrl(pane.url.to_string()))
    }
}

fn to_candidate(pane: &PrefsPane) -> Candidate {
    Candidate {
        id: format!("prefs::{}", pane.id),
        title: pane.title.to_string(),
        subtitle: Some("Open System Settings pane".into()),
        icon: Icon::SfSymbol(pane.symbol.into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Open")],
        search_text: format!("{} {}", pane.title, pane.keywords),
        bypass_rank: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn query_returns_all_panes() {
        let p = SystemPrefsProvider;
        let out = p.query(&Query::new("")).await;
        assert_eq!(out.len(), PANES.len());
    }

    #[tokio::test]
    async fn activate_yields_openurl_with_systempreferences_scheme() {
        let p = SystemPrefsProvider;
        let effect = p.activate(&"prefs::wifi".to_string(), "default").await.unwrap();
        match effect {
            Effect::OpenUrl(url) => {
                assert!(url.starts_with("x-apple.systempreferences:"));
                assert!(url.contains("wifi"));
            }
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_unknown_pane_errors() {
        let p = SystemPrefsProvider;
        assert!(p.activate(&"prefs::bogus".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = SystemPrefsProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn candidates_include_keywords_in_search_text() {
        let p = SystemPrefsProvider;
        let out = p.query(&Query::new("")).await;
        let wifi = out.iter().find(|c| c.id == "prefs::wifi").unwrap();
        assert!(wifi.search_text.contains("wireless"));
    }

    #[test]
    fn pane_ids_are_unique() {
        let mut ids: Vec<&str> = PANES.iter().map(|p| p.id).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before);
    }

    #[test]
    fn urls_use_systempreferences_scheme() {
        for pane in PANES {
            assert!(pane.url.starts_with("x-apple.systempreferences:"), "{}", pane.id);
        }
    }
}

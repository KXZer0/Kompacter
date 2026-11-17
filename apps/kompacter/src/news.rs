//! "What's new": a few short lines shown once, the first time a new version opens.
//! Add an entry (newest first) before tagging a release; a version with no entry shows
//! nothing. Keep it to 3 or 4 lines a person would actually care about.

pub const NOTES: &[(&str, &[&str])] = &[(
    "0.0.7",
    &[
        "Slack, WhatsApp, Telegram and Signal targets, each with its own limit",
        "The paste guard works in those apps too",
        "Zip and 7z files: the videos and photos inside shrink too",
        "Updates now install themselves",
    ],
)];

/// The lines for `version`, if it has any.
pub fn for_version(version: &str) -> Option<&'static [&'static str]> {
    NOTES.iter().find(|(v, _)| *v == version).map(|(_, l)| *l)
}

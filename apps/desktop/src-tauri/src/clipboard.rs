//! Put files on the clipboard the way Explorer does, so Ctrl+V in Discord attaches them.

use std::path::PathBuf;

#[cfg(windows)]
pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
    use clipboard_win::{formats, raw, Clipboard, Setter};
    let list: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let _open = Clipboard::new_attempts(10).map_err(|e| e.to_string())?;
    raw::empty().map_err(|e| e.to_string())?;
    formats::FileList
        .write_clipboard(list.as_slice())
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub fn copy_files(_paths: &[PathBuf]) -> Result<(), String> {
    Err("copying files to the clipboard isn't supported on this OS yet".into())
}

#[cfg(windows)]
pub fn read_files() -> Vec<String> {
    use clipboard_win::{formats, get_clipboard};
    get_clipboard::<Vec<String>, _>(formats::FileList).unwrap_or_default()
}

#[cfg(not(windows))]
pub fn read_files() -> Vec<String> {
    Vec::new()
}

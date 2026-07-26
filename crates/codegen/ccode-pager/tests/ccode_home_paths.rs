//! `CCODE_HOME` override tests in an isolated binary so `ccode_home()`'s
//! process-wide `OnceLock` initializes from the overridden env var.

use std::path::PathBuf;

#[test]
fn ccode_home_override_path_helpers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ccode_home = tmp.path().to_path_buf();
    unsafe {
        std::env::set_var("CCODE_HOME", &ccode_home);
    }

    assert_eq!(
        ccode_pager::util::pager_toml_path(),
        ccode_home.join("pager.toml")
    );
    assert_eq!(
        ccode_pager::util::display_ccode_home_prefix(),
        "$CCODE_HOME"
    );
    assert_eq!(
        ccode_pager::util::display_user_ccode_path("config.toml"),
        "$CCODE_HOME/config.toml"
    );

    let memory_path = ccode_home.join("memory/MEMORY.md");
    assert_eq!(
        ccode_pager::util::abbreviate_path(&memory_path.display().to_string()),
        "$CCODE_HOME/memory/MEMORY.md"
    );

    // Copy-toast paths follow the same abbreviation convention, so a custom
    // $CCODE_HOME outside $HOME still displays short.
    assert_eq!(
        ccode_pager::clipboard::display_copy_path(&ccode_home.join("last-copy.txt")),
        "$CCODE_HOME/last-copy.txt"
    );

    assert!(ccode_pager::util::is_under_user_ccode_home(&memory_path));
    assert!(!ccode_pager::util::is_under_user_ccode_home(
        PathBuf::from("/tmp/other").as_path()
    ));
}

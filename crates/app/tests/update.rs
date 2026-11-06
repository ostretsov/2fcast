//! Needs the internet: `cargo test -p twofcast --test update -- --ignored`.

use twofcast_core::update;

#[test]
#[ignore]
fn downloads_and_verifies_the_latest_release() {
    let json = ureq::get(update::LATEST_URL)
        .header("User-Agent", "2fcast-test")
        .call()
        .unwrap()
        .body_mut()
        .read_to_string()
        .unwrap();
    let mut r = update::parse_release(&json)
        .unwrap()
        .expect("a release with a .deb");
    assert!(r.sha256.is_some(), "GitHub reports the digest");
    let dir = tempfile::tempdir().unwrap();
    let mut last = 0;
    let deb = twofcast::update::download(&r, dir.path(), |n| last = n).unwrap();
    assert_eq!(last, r.deb_size);
    assert_eq!(std::fs::metadata(&deb).unwrap().len(), r.deb_size);

    // A wrong digest is refused.
    r.sha256 = Some("0".repeat(64));
    let err = twofcast::update::download(&r, dir.path(), |_| {}).unwrap_err();
    assert!(format!("{err:#}").contains("SHA-256"), "{err:#}");
}

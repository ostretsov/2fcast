//! Runtime dependency check: which Ubuntu packages provide what we need.

/// (GStreamer element, Ubuntu package providing it).  `a|b` means either.
pub const ELEMENTS: &[(&str, &str)] = &[
    ("videoconvert", "gstreamer1.0-plugins-base"),
    ("videoscale", "gstreamer1.0-plugins-base"),
    ("videorate", "gstreamer1.0-plugins-base"),
    ("compositor", "gstreamer1.0-plugins-base"),
    ("audiomixer", "gstreamer1.0-plugins-base"),
    ("audioconvert", "gstreamer1.0-plugins-base"),
    ("audioresample", "gstreamer1.0-plugins-base"),
    ("opusenc", "gstreamer1.0-plugins-base"),
    ("opusdec", "gstreamer1.0-plugins-base"),
    ("appsink", "gstreamer1.0-plugins-base"),
    ("appsrc", "gstreamer1.0-plugins-base"),
    ("videotestsrc", "gstreamer1.0-plugins-base"),
    ("v4l2src", "gstreamer1.0-plugins-good"),
    ("jpegdec", "gstreamer1.0-plugins-good"),
    ("cairooverlay", "gstreamer1.0-plugins-good"),
    ("matroskamux", "gstreamer1.0-plugins-good"),
    ("rtph264pay", "gstreamer1.0-plugins-good"),
    ("rtph264depay", "gstreamer1.0-plugins-good"),
    ("rtpopuspay", "gstreamer1.0-plugins-good"),
    ("rtpopusdepay", "gstreamer1.0-plugins-good"),
    ("rtpjitterbuffer", "gstreamer1.0-plugins-good"),
    ("udpsrc", "gstreamer1.0-plugins-good"),
    ("multiudpsink", "gstreamer1.0-plugins-good"),
    ("ximagesrc", "gstreamer1.0-plugins-good"),
    ("pulsesrc", "gstreamer1.0-pulseaudio"),
    ("pulsesink", "gstreamer1.0-pulseaudio"),
    ("intervideosrc", "gstreamer1.0-plugins-bad"),
    ("intervideosink", "gstreamer1.0-plugins-bad"),
    ("interaudiosrc", "gstreamer1.0-plugins-bad"),
    ("interaudiosink", "gstreamer1.0-plugins-bad"),
    ("h264parse", "gstreamer1.0-plugins-bad"),
    ("x264enc", "gstreamer1.0-plugins-ugly"),
    // Either H.264 decoder will do.
    ("avdec_h264|openh264dec", "gstreamer1.0-libav"),
];

/// (program, Ubuntu package).
pub const PROGRAMS: &[(&str, &str)] = &[("pactl", "pulseaudio-utils")];

/// Packages that need to be installed, sorted and without duplicates.
pub fn missing_packages(
    has_element: impl Fn(&str) -> bool,
    has_program: impl Fn(&str) -> bool,
) -> Vec<&'static str> {
    let mut pkgs: Vec<&str> = ELEMENTS
        .iter()
        .filter(|(e, _)| !e.split('|').any(&has_element))
        .map(|(_, p)| *p)
        .chain(
            PROGRAMS
                .iter()
                .filter(|(b, _)| !has_program(b))
                .map(|(_, p)| *p),
        )
        .collect();
    pkgs.sort();
    pkgs.dedup();
    pkgs
}

/// Shell commands that install `pkgs`: refresh the package lists first (stale
/// lists make dependencies look unavailable) and let apt repair a half-done
/// earlier install (`--fix-broken`).
pub fn install_script(pkgs: &[&str]) -> String {
    format!(
        "apt-get update && apt-get install -y --fix-broken {}",
        pkgs.join(" ")
    )
}

/// Runs [`install_script`] as root with a graphical password prompt.
pub fn install_command(pkgs: &[&str]) -> Vec<String> {
    vec![
        "pkexec".into(),
        "sh".into(),
        "-c".into(),
        install_script(pkgs),
    ]
}

/// The same, to type into a terminal.
pub fn terminal_command(pkgs: &[&str]) -> String {
    format!(
        "sudo apt update && sudo apt install --fix-broken {}",
        pkgs.join(" ")
    )
}

/// Explains a failed `apt-get` run in plain words, with the commands to fix it.
pub fn explain_apt_failure(output: &str) -> String {
    let mut help = vec![];
    // A dependency apt cannot find at all ("Depends: x" with nothing after it,
    // or "Unable to locate package") usually lives in "universe".
    let unavailable = output.lines().any(|l| {
        let l = l.trim();
        (l.contains("Depends:")
            && !l.contains("but it is not")
            && !l.contains("but it is not going to be installed"))
            || l.contains("Unable to locate package")
            || l.contains("has no installation candidate")
    });
    if unavailable {
        help.push(
            "Some packages are not available from your software sources. On Ubuntu they are in the \
             \"universe\" repository; enable it and refresh the package lists:\n    \
             sudo add-apt-repository universe\n    sudo apt update",
        );
    }
    if output.contains("--fix-broken") || output.contains("Unmet dependencies") {
        help.push(
            "An earlier installation was left incomplete (for example a .deb installed with \
             \"dpkg -i\"). Let apt finish it:\n    sudo apt --fix-broken install",
        );
    }
    if output.contains("Could not get lock") || output.contains("is held by process") {
        help.push("Another program (e.g. Software Updater) is installing right now. Wait for it to finish and try again.");
    }
    if help.is_empty() {
        help.push("Installation failed or was cancelled.");
    }
    help.push("Then start 2fcast again, or install the package with:\n    sudo apt install ./2fcast_*.deb");
    help.join("\n\n")
}

/// True if `program` is found in `$PATH`.
pub fn in_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternatives_satisfy_a_requirement() {
        let without_libav = missing_packages(|e| e != "avdec_h264", |_| true);
        assert!(
            without_libav.is_empty(),
            "openh264dec is enough: {without_libav:?}"
        );
        let neither = missing_packages(|e| e != "avdec_h264" && e != "openh264dec", |_| true);
        assert_eq!(neither, vec!["gstreamer1.0-libav"]);
    }

    #[test]
    fn nothing_missing() {
        assert!(missing_packages(|_| true, |_| true).is_empty());
    }

    #[test]
    fn missing_are_deduplicated() {
        let m = missing_packages(|e| !e.starts_with("rtp") && e != "x264enc", |_| false);
        assert_eq!(
            m,
            vec![
                "gstreamer1.0-plugins-good",
                "gstreamer1.0-plugins-ugly",
                "pulseaudio-utils"
            ]
        );
    }

    #[test]
    fn install_command_shape() {
        assert_eq!(
            install_command(&["a", "b"]),
            vec![
                "pkexec",
                "sh",
                "-c",
                "apt-get update && apt-get install -y --fix-broken a b"
            ]
        );
        assert_eq!(
            terminal_command(&["a"]),
            "sudo apt update && sudo apt install --fix-broken a"
        );
    }

    /// What apt printed on a real machine with universe missing and a
    /// half-installed .deb.
    const BROKEN: &str = "Reading package lists... Done
You might want to run 'apt --fix-broken install' to correct these.
The following packages have unmet dependencies:
 gstreamer1.0-plugins-bad : Depends: libde265-0 (>= 0.9) but it is not going to be installed
                            Depends: libopenh264-6 (>= 2.2.0+dfsg)
                            Depends: libzbar0 (>= 0.10) but it is not going to be installed
E: Unmet dependencies. Try 'apt --fix-broken install' with no packages (or specify a solution).";

    #[test]
    fn explains_missing_universe_and_broken_install() {
        let help = explain_apt_failure(BROKEN);
        assert!(help.contains("sudo add-apt-repository universe"), "{help}");
        assert!(help.contains("sudo apt --fix-broken install"), "{help}");
        assert!(help.contains("sudo apt install ./2fcast_*.deb"));
    }

    #[test]
    fn explains_other_failures() {
        let only_broken = "E: Unmet dependencies. Try 'apt --fix-broken install'\n x : Depends: y but it is not going to be installed";
        let help = explain_apt_failure(only_broken);
        assert!(
            help.contains("--fix-broken install") && !help.contains("universe"),
            "{help}"
        );
        assert!(explain_apt_failure("E: Unable to locate package foo").contains("universe"));
        assert!(
            explain_apt_failure("E: Could not get lock /var/lib/dpkg/lock-frontend")
                .contains("Wait")
        );
        assert!(
            explain_apt_failure("Error executing command as another user: Not authorized")
                .starts_with("Installation failed")
        );
    }

    #[test]
    fn finds_sh_in_path() {
        assert!(in_path("sh"));
        assert!(!in_path("definitely-not-a-program-2fcast"));
    }
}

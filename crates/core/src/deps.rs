//! Runtime dependency check: which Ubuntu packages provide what we need.

/// (GStreamer element, Ubuntu package providing it).
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
    ("avdec_h264", "gstreamer1.0-libav"),
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
        .filter(|(e, _)| !has_element(e))
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

/// Command that installs `pkgs` with a graphical password prompt.
pub fn install_command(pkgs: &[&str]) -> Vec<String> {
    ["pkexec", "apt-get", "install", "-y"]
        .iter()
        .map(|s| s.to_string())
        .chain(pkgs.iter().map(|s| s.to_string()))
        .collect()
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
            vec!["pkexec", "apt-get", "install", "-y", "a", "b"]
        );
    }

    #[test]
    fn finds_sh_in_path() {
        assert!(in_path("sh"));
        assert!(!in_path("definitely-not-a-program-2fcast"));
    }
}

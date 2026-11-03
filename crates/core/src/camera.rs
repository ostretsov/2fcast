//! Picking a capture mode for a webcam.

#[derive(Clone, Debug, PartialEq)]
pub struct CamMode {
    /// "jpeg" for MJPEG, otherwise the raw video format (e.g. "YUY2").
    pub format: String,
    pub width: i32,
    pub height: i32,
    pub fps_n: i32,
    pub fps_d: i32,
}

impl CamMode {
    pub fn fps(&self) -> f64 {
        self.fps_n as f64 / self.fps_d.max(1) as f64
    }

    pub fn is_jpeg(&self) -> bool {
        self.format == "jpeg"
    }

    pub fn caps(&self) -> String {
        let media = if self.is_jpeg() {
            "image/jpeg".to_string()
        } else {
            format!("video/x-raw,format={}", self.format)
        };
        format!(
            "{media},width={},height={},framerate={}/{}",
            self.width, self.height, self.fps_n, self.fps_d
        )
    }
}

/// Target camera resolution sent over the network and composited.
pub const TARGET_HEIGHT: i32 = 540;

/// Chooses the best mode: smooth frame rate first, then the smallest mode that
/// still covers the target height (less decoding), raw preferred over MJPEG.
pub fn choose(modes: &[CamMode]) -> Option<CamMode> {
    modes
        .iter()
        .filter(|m| m.width > 0 && m.height > 0 && m.fps_n > 0)
        .max_by_key(|m| {
            let smooth = m.fps() >= 24.0;
            let covers = m.height >= TARGET_HEIGHT;
            // Among modes covering the target prefer smaller, otherwise larger.
            let size = if covers { -m.height } else { m.height };
            let fps = (m.fps().min(30.0) * 100.0) as i64;
            (smooth, covers, size, !m.is_jpeg(), fps)
        })
        .cloned()
}

/// A webcam found on this machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// Device node, e.g. "/dev/video2".
    pub path: String,
    /// Name for people, e.g. "Logitech C920".
    pub name: String,
    pub modes: Vec<CamMode>,
}

/// A device name for people: UVC drivers often report "Name: Name" cut to
/// 31 characters ("Integrated Webcam: Integrated W").
pub fn clean_name(name: &str) -> String {
    if let Some((head, tail)) = name.split_once(": ")
        && !tail.is_empty()
        && head.starts_with(tail)
    {
        return head.to_string();
    }
    name.to_string()
}

/// Sorts devices by node number ("/dev/video2" before "/dev/video10") and
/// drops duplicates.
pub fn sort_devices(devices: &mut Vec<Device>) {
    fn key(path: &str) -> (String, u64) {
        let digits = path.len() - path.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let (stem, num) = path.split_at(path.len() - digits);
        (stem.to_string(), num.parse().unwrap_or(0))
    }
    devices.sort_by_key(|d| key(&d.path));
    devices.dedup_by(|a, b| a.path == b.path);
}

/// The real device node behind a path (`/dev/v4l/by-id/…` links point to
/// `/dev/videoN`); the path itself if it cannot be resolved.
pub fn resolve(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

/// The device a `camera` setting names: "auto" is the first one, a path
/// matches the device it is or links to.  `None` for "none", "test" and
/// devices that are not listed.
pub fn find<'a>(setting: &str, devices: &'a [Device]) -> Option<&'a Device> {
    match setting {
        "auto" => devices.first(),
        "none" | "test" => None,
        path => {
            let real = resolve(path);
            devices.iter().find(|d| d.path == path || d.path == real)
        }
    }
}

/// A path for `device` (e.g. "/dev/video2") that stays the same when the
/// numbering changes: a link in `/dev/v4l/by-id` (or `by-path`) pointing at
/// it, else the device itself.  `v4l_dir` is normally "/dev/v4l".
pub fn stable_path(device: &str, v4l_dir: &std::path::Path) -> String {
    let real = resolve(device);
    for sub in ["by-id", "by-path"] {
        let Ok(entries) = std::fs::read_dir(v4l_dir.join(sub)) else {
            continue;
        };
        let mut links: Vec<String> = entries
            .flatten()
            .map(|e| e.path().display().to_string())
            .filter(|p| resolve(p) == real)
            .collect();
        links.sort();
        if let Some(l) = links.into_iter().next() {
            return l;
        }
    }
    device.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(format: &str, width: i32, height: i32, fps: i32) -> CamMode {
        CamMode {
            format: format.into(),
            width,
            height,
            fps_n: fps,
            fps_d: 1,
        }
    }

    fn dev(path: &str) -> Device {
        Device {
            path: path.into(),
            name: format!("cam {path}"),
            modes: vec![m("YUY2", 640, 480, 30)],
        }
    }

    #[test]
    fn device_names_without_repetition() {
        assert_eq!(
            clean_name("Integrated Webcam: Integrated W"),
            "Integrated Webcam"
        );
        assert_eq!(clean_name("HD Pro Webcam C920"), "HD Pro Webcam C920");
        assert_eq!(clean_name("Cam: Other"), "Cam: Other");
        assert_eq!(clean_name("Cam: "), "Cam: ");
    }

    #[test]
    fn devices_sorted_by_number() {
        let mut d = vec![
            dev("/dev/video10"),
            dev("/dev/video2"),
            dev("/dev/video0"),
            dev("/dev/video2"),
        ];
        sort_devices(&mut d);
        let paths: Vec<&str> = d.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["/dev/video0", "/dev/video2", "/dev/video10"]);
    }

    #[test]
    fn settings_find_devices_also_through_links() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("video4");
        std::fs::write(&node, "").unwrap();
        let by_id = dir.path().join("v4l/by-id");
        std::fs::create_dir_all(&by_id).unwrap();
        let link = by_id.join("usb-Cam_123-video-index0");
        std::os::unix::fs::symlink(&node, &link).unwrap();
        let node = resolve(&node.display().to_string());
        let devices = vec![dev("/dev/video0"), dev(&node)];

        assert_eq!(find("auto", &devices).unwrap().path, "/dev/video0");
        assert_eq!(find("auto", &[]), None);
        assert_eq!(find("none", &devices), None);
        assert_eq!(find("test", &devices), None);
        assert_eq!(find("/dev/video9", &devices), None);
        assert_eq!(find(&node, &devices).unwrap().path, node);
        let link = link.display().to_string();
        assert_eq!(find(&link, &devices).unwrap().path, node);

        assert_eq!(stable_path(&node, &dir.path().join("v4l")), link);
        assert_eq!(
            stable_path("/dev/video0", &dir.path().join("v4l")),
            "/dev/video0"
        );
        assert_eq!(
            stable_path("/dev/video0", &dir.path().join("missing")),
            "/dev/video0"
        );
    }

    #[test]
    fn picks_mjpeg_540p_on_typical_laptop_cam() {
        // The modes of this machine's integrated webcam.
        let modes = vec![
            m("jpeg", 1280, 720, 30),
            m("jpeg", 848, 480, 30),
            m("jpeg", 960, 540, 30),
            m("YUY2", 640, 480, 30),
            m("YUY2", 640, 360, 30),
        ];
        assert_eq!(choose(&modes), Some(m("jpeg", 960, 540, 30)));
    }

    #[test]
    fn prefers_smooth_over_big() {
        let modes = vec![m("YUY2", 1920, 1080, 5), m("YUY2", 640, 480, 30)];
        assert_eq!(choose(&modes).unwrap().height, 480);
    }

    #[test]
    fn prefers_raw_when_same_size() {
        let modes = vec![m("jpeg", 1280, 720, 30), m("YUY2", 1280, 720, 30)];
        assert_eq!(choose(&modes).unwrap().format, "YUY2");
    }

    #[test]
    fn empty_and_caps() {
        assert_eq!(choose(&[]), None);
        assert_eq!(
            m("jpeg", 1280, 720, 30).caps(),
            "image/jpeg,width=1280,height=720,framerate=30/1"
        );
        assert_eq!(
            m("YUY2", 640, 480, 30).caps(),
            "video/x-raw,format=YUY2,width=640,height=480,framerate=30/1"
        );
    }
}

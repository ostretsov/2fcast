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

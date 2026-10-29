//! Prints the description of one pipeline (for debugging with gst-launch).
//!   cargo run --example describe -- camera /dev/video0

use twofcast_core::camera::CamMode;
use twofcast_core::pipelines::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ch = Channels::new("");
    let desc = match args.first().map(String::as_str) {
        Some("camera") => {
            let mode = CamMode {
                format: "jpeg".into(),
                width: 960,
                height: 540,
                fps_n: 30,
                fps_d: 1,
            };
            camera(
                &ch,
                &CamSource::Device {
                    path: args.get(1).cloned().unwrap_or("/dev/video0".into()),
                    mode: Some(mode),
                },
            )
        }
        Some("program") => program(&ch),
        Some("receive") => receive(&ch, &Ports::from_base(47100), true),
        _ => "usage: describe camera|program|receive".into(),
    };
    println!("{desc}");
}

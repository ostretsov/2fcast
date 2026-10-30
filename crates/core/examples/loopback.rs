//! Prints one gst-launch description containing every pipeline of one
//! machine, with its LAN output looped back to itself.  Used to smoke-test the
//! pipeline descriptions without the app:
//!
//!   gst-launch-1.0 -e $(cargo run -q --example loopback -- /tmp/out.mkv)

use twofcast_core::pipelines::*;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "loopback.mkv".into());
    let ch = Channels::new("");
    let ports = Ports::from_base(47900);
    let open = |s: String, net: &str, port: u16| {
        s.replace("drop=true", "drop=false").replace(
            &format!("name={net} "),
            &format!("name={net} clients=127.0.0.1:{port} "),
        )
    };
    let parts = [
        open(
            camera(&ch, &CamSource::Test(0x204060)),
            "net_cam",
            ports.cam,
        ),
        open(microphone(&ch, &MicSource::Test), "net_mic", ports.mic),
        open(
            open(
                screen(&ch, &ScreenSource::Test, Some("test")),
                "net_screen",
                ports.screen,
            ),
            "net_share",
            ports.share,
        ),
        receive(&ch, &ports, false, "avdec_h264"),
        receive_screen(&ch, &ports, false, "avdec_h264"),
        program(&ch),
        record(&ch, 4000, "ultrafast").replace(
            "filesink name=file",
            &format!("filesink name=file location={out}"),
        ),
    ];
    println!("{}", parts.join(" "));
}

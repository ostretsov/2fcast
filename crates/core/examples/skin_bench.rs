//! Time of the skin filter on a 960x540 frame:
//!   cargo run --release -p twofcast-core --example skin_bench
use std::time::Instant;
use twofcast_core::skin::{Planes, SkinFilter};

fn main() {
    let (w, h) = (960, 540);
    let mut y: Vec<u8> = (0..w * h).map(|i| (100 + (i * 7919) % 60) as u8).collect();
    let (u, v) = (vec![105u8; w * h / 4], vec![150u8; w * h / 4]);
    let mut f = SkinFilter::new();
    let n = 300;
    let t = Instant::now();
    for _ in 0..n {
        f.process(
            Planes {
                width: w,
                height: h,
                y: &mut y,
                y_stride: w,
                u: &u,
                v: &v,
                uv_stride: w / 2,
            },
            0.7,
        );
    }
    println!(
        "{:.2} ms per frame (all skin, worst case)",
        t.elapsed().as_secs_f64() * 1000.0 / n as f64
    );
}

//! Factory processing presets. They only touch the processing section.

use crate::config::{BandConfig, ProcessingConfig};

fn bands(thr: [f32; 5], ratio: f32, makeup: f32) -> [BandConfig; 5] {
    let att = [30.0, 20.0, 10.0, 5.0, 3.0];
    let rel = [300.0, 250.0, 200.0, 150.0, 120.0];
    std::array::from_fn(|i| BandConfig {
        threshold_db: thr[i],
        ratio,
        attack_ms: att[i],
        release_ms: rel[i],
        makeup_db: makeup,
        ..Default::default()
    })
}

pub fn factory() -> Vec<(&'static str, &'static str, ProcessingConfig)> {
    let mut gentle = ProcessingConfig::default();
    gentle.multiband.bands = bands([-18.0, -18.0, -18.0, -20.0, -22.0], 2.0, 2.0);
    gentle.clipper.drive_db = 0.0;
    gentle.limiter.drive_db = 0.0;

    let standard = ProcessingConfig::default();

    let mut loud = ProcessingConfig::default();
    loud.agc.target_db = -16.0;
    loud.multiband.bands = bands([-26.0, -26.0, -26.0, -28.0, -30.0], 4.0, 5.0);
    loud.limiter.drive_db = 3.0;
    loud.clipper.drive_db = 2.5;
    loud.clipper.knee = 0.75;

    let mut speech = ProcessingConfig::default();
    speech.multiband.crossovers_hz = [150.0, 600.0, 2500.0, 6500.0];
    speech.multiband.bands = bands([-20.0, -22.0, -24.0, -24.0, -24.0], 3.0, 3.0);
    speech.multiband.bands[0].gain_db = -2.0;
    speech.agc.release_db_s = 2.0;

    vec![
        ("Gentle", "Light processing, maximum dynamics", gentle),
        ("Standard", "Balanced density for music radio", standard),
        ("Loud", "Dense and loud, still within limits", loud),
        ("Speech", "Clear speech, less low end", speech),
    ]
}

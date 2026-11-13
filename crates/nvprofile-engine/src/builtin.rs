//! Presets compiled into the engine.
//!
//! All four target the author's RTX 3080 Ti (Palit Gaming Pro; manual fan floor
//! ~30% PWM, power range 100-365 W, default 350 W). Fan points never go below
//! 31% because the driver's PWM floor is 77/255 = 30.2%.

use crate::types::*;

/// Afterburner "Profile 5" V/F offsets: `[index, voltage_mv, offset_mhz]`.
static PROFILE5_VF: &str = include_str!("../data/profile5_vf.json");

fn profile5_offsets() -> Vec<VfOffset> {
    let rows: Vec<[i64; 3]> = serde_json::from_str(PROFILE5_VF).expect("embedded profile5_vf.json is valid");
    rows.into_iter()
        .map(|[index, voltage, offset]| VfOffset {
            index: index as u32,
            voltage_mv: Some(voltage as u32),
            offset_mhz: offset as i32,
        })
        .collect()
}

fn pts(v: &[(u32, f64)]) -> Vec<CurvePoint> {
    v.iter().map(|(t, s)| CurvePoint { temp_c: *t, speed: *s }).collect()
}

/// The built-in presets, in display order.
pub fn builtin_presets() -> Vec<Preset> {
    vec![
        Preset {
            id: "stock".into(),
            name: "Stock".into(),
            description: "Removes every nvprofile/LACT override: driver fan curve, driver power \
                limit (350 W on this card) and a cleared V/F curve. This is the reset target."
                .into(),
            power_cap_w: None,
            fan: FanSpec::Auto,
            vf: VfSpec::Stock,
            builtin: true,
        },
        Preset {
            id: "profile5".into(),
            name: "Profile 5 (24/7 gaming + AI)".into(),
            description: "Your MSI Afterburner Profile 5: 178.5 W cap (51% of 350 W), fixed 51% \
                fan, and the 127-point V/F curve that tops out near 1920 MHz. Measured ~177 W / \
                49 C under a 100% render load; the everyday gaming/AI/rendering profile."
                .into(),
            power_cap_w: Some(178.5),
            fan: FanSpec::Static { speed: 0.51 },
            vf: VfSpec::Offsets { points: profile5_offsets() },
            builtin: true,
        },
        Preset {
            id: "night".into(),
            name: "Night (silent desktop)".into(),
            description: "For quiet use at night with light desktop work, no rendering or \
                gaming. 130 W cap bounds worst-case heat; the fan stops (driver auto/zero-RPM) \
                below 50 C and otherwise sits at 31-33% up to 60 C, ramping gently to 70% only \
                at 85 C. 3 C hysteresis and a 10 s spin-down delay stop the fan from hunting. \
                V/F offsets reuse the Profile 5 curve, which is proven stable on this card, and \
                only the low-voltage points matter at 130 W. Tradeoff: heavy GPU loads will be \
                power-throttled far below stock performance; switch to Profile 5 before rendering or gaming."
                .into(),
            power_cap_w: Some(130.0),
            fan: FanSpec::Curve {
                points: pts(&[(50, 0.31), (60, 0.33), (70, 0.40), (78, 0.50), (85, 0.70)]),
                zero_rpm_below_c: Some(50),
                hysteresis_c: 3,
                spindown_delay_ms: 10_000,
            },
            vf: VfSpec::Offsets { points: profile5_offsets() },
            builtin: true,
        },
        Preset {
            id: "desk".into(),
            name: "Desk (quiet browsing/coding)".into(),
            description: "Middle ground for browsing, coding and light GPU work: 155 W cap, fan \
                stopped below 45 C, then 31% at 45 C rising to 42% at 65 C and 70% at 82 C. \
                Same proven Profile 5 V/F curve. Louder than Night under sustained load, much \
                quieter and cooler than stock."
                .into(),
            power_cap_w: Some(155.0),
            fan: FanSpec::Curve {
                points: pts(&[(45, 0.31), (55, 0.34), (65, 0.42), (75, 0.52), (82, 0.70)]),
                zero_rpm_below_c: Some(45),
                hysteresis_c: 2,
                spindown_delay_ms: 5_000,
            },
            vf: VfSpec::Offsets { points: profile5_offsets() },
            builtin: true,
        },
    ]
}

pub fn is_builtin_id(id: &str) -> bool {
    ["stock", "profile5", "night", "desk"].contains(&id)
}

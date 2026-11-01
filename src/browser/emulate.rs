//! `browser.zoom` and `browser.viewport`: the zoom ladder, the viewport
//! presets and their arguments, and the DevTools calls a viewport makes.
//! Pure: runtime.rs applies them to CEF.

// Default builds compile these rules only for their tests; the browser feature uses them.
#![cfg_attr(not(all(target_os = "macos", feature = "browser-pane")), allow(dead_code))]

use serde_json::{Value, json};

/// Chromium's own zoom steps (`zoom::kPresetZoomFactors`), as percents:
/// ⌘+ and ⌘- in Chrome move along the same ladder.
pub const STEPS: [f64; 17] =
    [25.0, 100.0 / 3.0, 50.0, 200.0 / 3.0, 75.0, 80.0, 90.0, 100.0, 110.0, 125.0, 150.0, 175.0, 200.0, 250.0, 300.0, 400.0, 500.0];

/// The least and most zoom `level` takes, in percent (the ladder's ends).
pub const MIN_ZOOM: f64 = 25.0;
pub const MAX_ZOOM: f64 = 500.0;

/// Chromium's zoom level for a percent: each level is 20% bigger (`1.2^level`).
#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub fn level_of(percent: f64) -> f64 {
    (percent / 100.0).ln() / 1.2f64.ln()
}

/// The percent for a Chromium zoom level.
#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub fn percent_of(level: f64) -> f64 {
    100.0 * 1.2f64.powf(level)
}

/// A zoom as `BrowserInfo.zoom` shows it: a whole percent (33, 67, 125).
pub fn shown(percent: f64) -> u32 {
    percent.round().clamp(1.0, 10_000.0) as u32
}

/// The next step on the ladder from `current` (in or out). A zoom between
/// two steps (a `level` someone set) goes to the next step past it; at an
/// end it stays.
pub fn step(current: f64, zoom_in: bool) -> f64 {
    // Half a percent of slack: a level read back from Chromium is never exact.
    if zoom_in {
        STEPS.iter().copied().find(|s| *s > current + 0.5).unwrap_or(MAX_ZOOM)
    } else {
        STEPS.iter().rev().copied().find(|s| *s < current - 0.5).unwrap_or(MIN_ZOOM)
    }
}

/// What `browser.zoom` asks for, as the percent to set, from the page's
/// `current` percent. `step`: "in", "out" or "reset" (100%); `level`: a
/// percent from 25 to 500. One of the two, never both. A `step` that is a
/// number is a level: the shorthand `browser.zoom:125` (keys.json, `do`)
/// fills `step`, the command's first argument.
pub fn zoom_target(args: &Value, current: f64) -> Result<f64, String> {
    let mut step_arg = args.get("step").filter(|v| !v.is_null());
    let mut level = args.get("level").filter(|v| !v.is_null());
    if level.is_none() && step_arg.and_then(Value::as_str).is_some_and(|s| s.trim().parse::<f64>().is_ok()) {
        level = step_arg.take();
    }
    match (step_arg, level) {
        (Some(_), Some(_)) => Err("browser.zoom takes step or level, not both".into()),
        (Some(s), None) => match s.as_str() {
            Some("in") => Ok(step(current, true)),
            Some("out") => Ok(step(current, false)),
            Some("reset") => Ok(100.0),
            _ => Err(format!("browser.zoom step is \"in\", \"out\" or \"reset\", not {s}")),
        },
        (None, Some(l)) => match number(l) {
            Some(p) if (MIN_ZOOM..=MAX_ZOOM).contains(&p) => Ok(p),
            _ => Err(format!("browser.zoom level is a percent from 25 to 500, not {l}")),
        },
        (None, None) => Err("browser.zoom needs step (\"in\", \"out\", \"reset\") or level (a percent, 25 to 500)".into()),
    }
}

/// A responsive viewport: the page lays out at this size, as on that device.
#[derive(Clone, Debug, PartialEq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    /// A mobile page: `mobile: true` (meta viewport, overlay scrollbars) and touch.
    pub mobile: bool,
    /// The device's pixel ratio; 0 keeps the screen's.
    pub scale_factor: f64,
    /// The preset it came from, if any.
    pub preset: Option<&'static str>,
}

impl Viewport {
    /// As `BrowserInfo.viewport` shows it.
    pub fn json(&self) -> Value {
        json!({ "width": self.width, "height": self.height, "mobile": self.mobile, "preset": self.preset })
    }
}

/// `preset` names: (name, width, height, pixel ratio). Both are mobile.
/// "fit" (no override: the page fills its pane) is the third.
pub const PRESETS: [(&str, u32, u32, f64); 2] = [("phone", 390, 844, 3.0), ("tablet", 820, 1180, 2.0)];

/// The largest width or height `browser.viewport` takes.
pub const MAX_SIDE: u32 = 10_000;

/// What `browser.viewport` asks for. None: "fit", the page fills its pane
/// again. `preset` ("fit", "phone", "tablet"), or `width` and `height`
/// (1 to 10000) with `mobile` (default false).
pub fn viewport(args: &Value) -> Result<Option<Viewport>, String> {
    let arg = |k: &str| args.get(k).filter(|v| !v.is_null());
    let (preset, width, height, mobile) = (arg("preset"), arg("width"), arg("height"), arg("mobile"));
    if let Some(p) = preset {
        if width.is_some() || height.is_some() || mobile.is_some() {
            return Err("browser.viewport takes preset, or width and height (and mobile), not both".into());
        }
        return match p.as_str() {
            Some("fit") => Ok(None),
            Some(name) => match PRESETS.iter().find(|(n, ..)| *n == name) {
                Some(&(n, w, h, ratio)) => Ok(Some(Viewport { width: w, height: h, mobile: true, scale_factor: ratio, preset: Some(n) })),
                None => Err(format!("browser.viewport preset is \"fit\", \"phone\" or \"tablet\", not {p}")),
            },
            None => Err(format!("browser.viewport preset is \"fit\", \"phone\" or \"tablet\", not {p}")),
        };
    }
    let side = |name: &str, v: Option<&Value>| -> Result<u32, String> {
        let v = v.ok_or_else(|| format!("browser.viewport needs {name} with {}", if name == "width" { "height" } else { "width" }))?;
        match number(v) {
            Some(n) if n.fract() == 0.0 && (1.0..=f64::from(MAX_SIDE)).contains(&n) => Ok(n as u32),
            _ => Err(format!("browser.viewport {name} is a whole number of pixels from 1 to {MAX_SIDE}, not {v}")),
        }
    };
    if width.is_none() && height.is_none() {
        return Err("browser.viewport needs preset (\"fit\", \"phone\", \"tablet\") or width and height".into());
    }
    let mobile = match mobile {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) if s == "true" || s == "false" => s == "true",
        Some(other) => return Err(format!("browser.viewport mobile is true or false, not {other}")),
    };
    Ok(Some(Viewport { width: side("width", width)?, height: side("height", height)?, mobile, scale_factor: 0.0, preset: None }))
}

/// How much to shrink an emulated viewport so all of it shows in a pane of
/// `pane` (width, height) points: 1 when it fits.
#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub fn fit_scale(v: &Viewport, pane: (f64, f64)) -> f64 {
    let (w, h) = pane;
    if w <= 0.0 || h <= 0.0 {
        return 1.0;
    }
    (w / f64::from(v.width)).min(h / f64::from(v.height)).min(1.0)
}

/// The DevTools calls (method, params) that put `v` on the page, or take
/// any override off it (None). `scale` < 1 shrinks the emulated page to
/// fit its pane (`fit_scale`).
#[cfg_attr(not(feature = "browser"), allow(dead_code))] // CEF only
pub fn calls(v: Option<&Viewport>, scale: f64) -> Vec<(&'static str, Value)> {
    match v {
        Some(v) => {
            let mut metrics = json!({ "width": v.width, "height": v.height, "deviceScaleFactor": v.scale_factor, "mobile": v.mobile });
            if scale < 1.0 {
                metrics["scale"] = json!(scale);
            }
            let touch = if v.mobile { json!({ "enabled": true, "maxTouchPoints": 5 }) } else { json!({ "enabled": false }) };
            vec![("Emulation.setDeviceMetricsOverride", metrics), ("Emulation.setTouchEmulationEnabled", touch)]
        }
        None => vec![("Emulation.clearDeviceMetricsOverride", json!({})), ("Emulation.setTouchEmulationEnabled", json!({ "enabled": false }))],
    }
}

/// A number, or a string that is one (`--level 125`, `{"level": "125"}`).
fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|n: &f64| n.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_percents_are_chromiums() {
        assert!(level_of(100.0).abs() < 1e-9);
        assert!((level_of(120.0) - 1.0).abs() < 1e-9);
        for p in STEPS {
            assert!((percent_of(level_of(p)) - p).abs() < 1e-9, "{p}");
        }
        assert_eq!(STEPS.map(shown), [25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500]);
    }

    #[test]
    fn zoom_steps_walk_chromiums_ladder_and_stop_at_its_ends() {
        assert_eq!(step(100.0, true), 110.0);
        assert_eq!(step(100.0, false), 90.0);
        assert_eq!(step(500.0, true), 500.0);
        assert_eq!(step(25.0, false), 25.0);
        // A level read back from Chromium is a hair off its step.
        assert_eq!(step(109.9999, true), 125.0);
        assert_eq!(step(66.6667, false), 50.0);
        // Between steps: to the next one past it.
        assert_eq!(step(117.0, true), 125.0);
        assert_eq!(step(117.0, false), 110.0);
        // Outside the ladder (a level set elsewhere): back onto it.
        assert_eq!(step(600.0, false), 500.0);
        assert_eq!(step(10.0, true), 25.0);
    }

    #[test]
    fn zoom_takes_a_step_or_a_level() {
        let at = |args: Value| zoom_target(&args, 100.0);
        assert_eq!(at(json!({ "step": "in" })), Ok(110.0));
        assert_eq!(at(json!({ "step": "out" })), Ok(90.0));
        assert_eq!(zoom_target(&json!({ "step": "reset" }), 250.0), Ok(100.0));
        assert_eq!(at(json!({ "level": 125 })), Ok(125.0));
        assert_eq!(at(json!({ "level": "150" })), Ok(150.0));
        assert_eq!(at(json!({ "level": 25 })), Ok(25.0));
        assert_eq!(at(json!({ "level": 500, "step": null })), Ok(500.0));
        // The shorthand `browser.zoom:125` puts the number in step.
        assert_eq!(at(json!({ "step": "125" })), Ok(125.0));
        assert!(at(json!({ "step": "600" })).is_err());
        assert!(at(json!({ "step": "125", "level": 150 })).is_err());
        for bad in [json!({}), json!({ "step": "up" }), json!({ "step": 1 }), json!({ "level": 20 }), json!({ "level": 501 }), json!({ "level": "big" }), json!({ "level": 110, "step": "in" })] {
            assert!(at(bad.clone()).is_err(), "{bad}");
        }
        assert_eq!(at(json!({ "level": 110, "step": "in" })).unwrap_err(), "browser.zoom takes step or level, not both");
    }

    #[test]
    fn viewport_presets_are_phone_tablet_and_fit() {
        assert_eq!(viewport(&json!({ "preset": "fit" })), Ok(None));
        let phone = viewport(&json!({ "preset": "phone" })).unwrap().unwrap();
        assert_eq!((phone.width, phone.height, phone.mobile, phone.scale_factor, phone.preset), (390, 844, true, 3.0, Some("phone")));
        let tablet = viewport(&json!({ "preset": "tablet" })).unwrap().unwrap();
        assert_eq!((tablet.width, tablet.height, tablet.mobile, tablet.preset), (820, 1180, true, Some("tablet")));
        assert_eq!(phone.json(), json!({ "width": 390, "height": 844, "mobile": true, "preset": "phone" }));
        assert!(viewport(&json!({ "preset": "watch" })).unwrap_err().contains("not \"watch\""));
        assert!(viewport(&json!({ "preset": "phone", "width": 100 })).unwrap_err().contains("not both"));
    }

    #[test]
    fn a_custom_viewport_needs_width_and_height() {
        let v = viewport(&json!({ "width": 1024, "height": "768" })).unwrap().unwrap();
        assert_eq!((v.width, v.height, v.mobile, v.scale_factor, v.preset), (1024, 768, false, 0.0, None));
        assert!(viewport(&json!({ "width": 360, "height": 640, "mobile": true })).unwrap().unwrap().mobile);
        assert!(viewport(&json!({ "width": 360, "height": 640, "mobile": "true" })).unwrap().unwrap().mobile);
        for bad in [json!({}), json!({ "width": 390 }), json!({ "height": 844 }), json!({ "width": 0, "height": 10 }), json!({ "width": 10001, "height": 10 }),
            json!({ "width": 390.5, "height": 844 }), json!({ "width": -1, "height": 844 }), json!({ "width": 390, "height": 844, "mobile": 1 })] {
            assert!(viewport(&bad).is_err(), "{bad}");
        }
        assert_eq!(viewport(&json!({ "width": 390 })).unwrap_err(), "browser.viewport needs height with width");
    }

    #[test]
    fn a_viewport_too_big_for_its_pane_shrinks_to_fit() {
        let tablet = viewport(&json!({ "preset": "tablet" })).unwrap().unwrap();
        assert_eq!(fit_scale(&tablet, (1640.0, 2360.0)), 1.0);
        assert_eq!(fit_scale(&tablet, (820.0, 590.0)), 0.5);
        assert_eq!(fit_scale(&tablet, (0.0, 0.0)), 1.0);
    }

    #[test]
    fn viewport_calls_set_metrics_and_touch_or_clear_both() {
        let phone = viewport(&json!({ "preset": "phone" })).unwrap().unwrap();
        assert_eq!(calls(Some(&phone), 1.0), vec![
            ("Emulation.setDeviceMetricsOverride", json!({ "width": 390, "height": 844, "deviceScaleFactor": 3.0, "mobile": true })),
            ("Emulation.setTouchEmulationEnabled", json!({ "enabled": true, "maxTouchPoints": 5 })),
        ]);
        assert_eq!(calls(Some(&phone), 0.5)[0].1["scale"], json!(0.5));
        let desk = viewport(&json!({ "width": 1280, "height": 800 })).unwrap().unwrap();
        assert_eq!(calls(Some(&desk), 1.0)[1], ("Emulation.setTouchEmulationEnabled", json!({ "enabled": false })));
        assert_eq!(calls(None, 1.0), vec![
            ("Emulation.clearDeviceMetricsOverride", json!({})),
            ("Emulation.setTouchEmulationEnabled", json!({ "enabled": false })),
        ]);
        // Herdr's own calls are methods the agent allowlist names too: they
        // are page-scoped emulation, nothing browser-wide.
        for (method, _) in calls(Some(&phone), 1.0).into_iter().chain(calls(None, 1.0)) {
            assert!(super::super::agent::allowed_method(method), "{method}");
        }
    }
}

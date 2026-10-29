//! Rounded, smoothed figures. The hot path adds one counter per frame, once.
//! A detector pass is costed at the module's rough size. Nothing walks the model.

#[derive(Clone, Debug)]
pub struct PcSample {
    pub name: String,
    pub cores: usize,
    pub cpu: f64,
    pub ram_used: f64,
    pub ram_total: f64,
    pub cameras: usize,
    pub on_time: usize,
    pub gated: f64,
    pub blur: f64,
    pub blank: f64,
    pub passes: f64,
    pub ops: f64,
    pub lacking: f64,
    pub missed: f64,
    pub same: f64,
    pub paced: f64,
    pub behind: f64,
    pub ms_pass: f64,
    pub warnings: usize,
    pub share: f64,
}

pub fn sig2(x: f64) -> f64 {
    if !x.is_finite() || x.abs() < 0.5 {
        return 0.0;
    }
    let ax = x.abs();
    let p = 10f64.powf(ax.log10().floor() - 1.0);
    (x / p).round() * p
}

pub fn fmt_num(x: f64) -> String {
    let v = sig2(x);
    if (v - v.round()).abs() < 0.05 || v.abs() >= 100.0 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.1}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub fn format_ops(ops: f64) -> String {
    if ops >= 1.0e9 {
        format!("{} billion", fmt_num(ops / 1.0e9))
    } else if ops >= 1.0e6 {
        format!("{} million", fmt_num(ops / 1.0e6))
    } else {
        fmt_num(ops)
    }
}

pub fn ema(prev: Option<f64>, sample: f64) -> f64 {
    match prev {
        None => sample,
        Some(p) => p * 0.6 + sample * 0.4,
    }
}

pub fn status(error: &str, seen: bool, gated: f64, blur: f64, blank: f64, lacking: f64) -> &'static str {
    if !error.is_empty() {
        return "no picture";
    }
    if !seen {
        return "starting";
    }
    if blank > 1.0 && blank >= blur {
        return "blank";
    }
    if lacking >= 1.0 {
        return "behind";
    }
    if gated > 1.0 && blur / gated >= 0.5 {
        return "blurry";
    }
    "on time"
}

pub fn people(n: f64) -> String {
    let n = n.round() as i64;
    if n <= 0 {
        String::new()
    } else if n == 1 {
        "1 person".into()
    } else {
        format!("{n} people")
    }
}

pub fn sentence(joined: usize, ops: f64, lacking: f64) -> String {
    let who = if joined == 0 {
        "No PC has joined yet. Any PC on this network can.".to_string()
    } else if joined == 1 {
        "One PC is looking. More can join at any time.".to_string()
    } else {
        format!("{joined} PCs are looking, and more can join.")
    };
    format!(
        "{who} About {} operations a second. About {} frames a second are lacking.",
        format_ops(ops),
        fmt_num(lacking)
    )
}

pub fn cam_warnings(name: &str, gated: f64, blur: f64, blank: f64, lacking: f64, error: &str) -> Vec<String> {
    let mut out = Vec::new();
    if !error.is_empty() {
        out.push(format!("{name} has no picture. {error}"));
        return out;
    }
    if blank > 1.0 && blank >= blur {
        out.push(format!("{name} shows nothing. The detector is paused."));
    } else if gated > 1.0 && blur / gated >= 0.5 {
        out.push(format!("{name} is blurry. The detector is paused."));
    }
    if lacking >= 1.0 {
        out.push(format!("{name} is lacking about {} frames a second.", fmt_num(lacking)));
    }
    out
}

pub fn matrix(pcs: &[PcSample]) -> (Vec<String>, Vec<(&'static str, Vec<String>)>) {
    let headers: Vec<String> = pcs.iter().map(|p| p.name.clone()).collect();
    let cols = headers.len();
    let cell = |f: &dyn Fn(&PcSample) -> String| -> Vec<String> {
        let mut v: Vec<String> = pcs.iter().map(f).collect();
        v.resize(cols, String::new());
        v
    };
    let rows = vec![
        ("Lacking / s", cell(&|p| fmt_num(p.lacking))),
        ("Operations / s", cell(&|p| format_ops(p.ops))),
        ("Share of the work", cell(&|p| format!("{}%", fmt_num(p.share)))),
        ("Cameras", cell(&|p| p.cameras.to_string())),
        ("On time", cell(&|p| format!("{} of {}", p.on_time, p.cameras))),
        ("Gated / s", cell(&|p| fmt_num(p.gated))),
        ("Blur / s", cell(&|p| fmt_num(p.blur))),
        ("Blank / s", cell(&|p| fmt_num(p.blank))),
        ("Detector passes / s", cell(&|p| fmt_num(p.passes))),
        ("Sharp missed / s", cell(&|p| fmt_num(p.missed))),
        ("Unchanged, skipped / s", cell(&|p| fmt_num(p.same))),
        ("Paced, skipped / s", cell(&|p| fmt_num(p.paced))),
        ("Behind, frames", cell(&|p| fmt_num(p.behind))),
        ("ms / pass", cell(&|p| fmt_num(p.ms_pass))),
        ("CPU", cell(&|p| format!("{}%", fmt_num(p.cpu)))),
        (
            "Memory",
            cell(&|p| {
                if p.ram_total <= 0.0 {
                    return String::new();
                }
                format!("{:.1} / {:.0} GB", p.ram_used / 1e9, p.ram_total / 1e9)
            }),
        ),
        ("Cores", cell(&|p| p.cores.to_string())),
        ("Warnings", cell(&|p| p.warnings.to_string())),
    ];
    (headers, rows)
}

pub const FOOTNOTE: &str = "Rates are sampled once a second and smoothed. One detector pass is costed at the module's rough size, so the operation count is not measured inside the model. Lacking is frames not gated, plus sharp frames the detector did not run.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_to_two_figures() {
        assert_eq!(sig2(0.4), 0.0);
        assert_eq!(sig2(1.26), 1.3);
        assert_eq!(sig2(18.2), 18.0);
        assert_eq!(fmt_num(18.2), "18");
        assert_eq!(fmt_num(1.26), "1.3");
        assert_eq!(format_ops(4.46e9), "4.5 billion");
    }

    #[test]
    fn one_pc_sentence_names_the_gap() {
        let s = sentence(1, 4.5e9, 18.2);
        assert!(s.contains("One PC is looking"), "{s}");
        assert!(s.contains("More can join"), "{s}");
        assert!(!s.contains("expected"), "{s}");
        assert!(s.contains("4.5 billion"), "{s}");
        assert!(s.contains("18"), "{s}");
    }

    #[test]
    fn matrix_shows_only_pcs_that_joined() {
        let pc = PcSample {
            name: "lab".into(),
            cores: 6,
            cpu: 40.0,
            ram_used: 2e9,
            ram_total: 8e9,
            cameras: 8,
            on_time: 2,
            gated: 40.0,
            blur: 1.0,
            blank: 0.0,
            passes: 20.0,
            ops: 9e10,
            lacking: 18.0,
            missed: 18.0,
            same: 4.0,
            paced: 2.0,
            behind: 2.0,
            ms_pass: 30.0,
            warnings: 1,
            share: 100.0,
        };
        let (headers, rows) = matrix(&[pc]);
        assert_eq!(headers, vec!["lab".to_string()]);
        assert_eq!(rows[0].1[0], "18");
        assert!(rows.iter().any(|r| r.0 == "Unchanged, skipped / s" && r.1[0] == "4"));
    }

    #[test]
    fn status_words() {
        assert_eq!(status("ffmpeg", true, 10.0, 0.0, 0.0, 0.0), "no picture");
        assert_eq!(status("", false, 0.0, 0.0, 0.0, 0.0), "starting");
        assert_eq!(status("", true, 10.0, 0.0, 8.0, 0.0), "blank");
        assert_eq!(status("", true, 20.0, 0.0, 0.0, 6.0), "behind");
        assert_eq!(status("", true, 20.0, 16.0, 0.0, 0.0), "blurry");
        assert_eq!(status("", true, 20.0, 1.0, 0.0, 0.0), "on time");
    }

    #[test]
    fn blur_and_lack_warnings_are_sentences() {
        let w = cam_warnings("Camera 2", 20.0, 18.0, 0.0, 0.2, "");
        assert!(w[0].contains("blurry"), "{w:?}");
        let w = cam_warnings("Camera 3", 20.0, 0.0, 0.0, 6.0, "");
        assert!(w[0].contains("lacking"), "{w:?}");
    }
}

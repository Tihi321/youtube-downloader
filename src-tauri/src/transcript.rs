//! json3 caption parsing and renderers (port of `formats.js` from
//! Tihi321/youtube-transcription-downloader).

use anyhow::{Context, Result};
use serde::{Serialize, Serializer};

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Segment {
    #[serde(serialize_with = "num")]
    pub start: f64,
    #[serde(serialize_with = "num")]
    pub dur: f64,
    pub text: String,
}

/// Serialize like JS `JSON.stringify`: whole numbers without a fraction.
fn num<S: Serializer>(v: &f64, s: S) -> std::result::Result<S::Ok, S::Error> {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Txt,
    Timestamped,
    Srt,
    Vtt,
    Json,
}

impl Format {
    pub fn parse(s: &str) -> Format {
        match s {
            "timestamped" => Format::Timestamped,
            "srt" => Format::Srt,
            "vtt" => Format::Vtt,
            "json" => Format::Json,
            _ => Format::Txt,
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Format::Txt | Format::Timestamped => "txt",
            Format::Srt => "srt",
            Format::Vtt => "vtt",
            Format::Json => "json",
        }
    }
}

/// Parse a yt-dlp `json3` subtitle file into segments.
pub fn parse_json3(input: &str) -> Result<Vec<Segment>> {
    let v: serde_json::Value = serde_json::from_str(input).context("invalid json3")?;
    let mut out = Vec::new();
    let Some(events) = v.get("events").and_then(|e| e.as_array()) else {
        return Ok(out);
    };
    for ev in events {
        let Some(segs) = ev.get("segs").and_then(|s| s.as_array()) else {
            continue;
        };
        let raw: String = segs
            .iter()
            .filter_map(|s| s.get("utf8").and_then(|u| u.as_str()))
            .collect();
        let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        let start = ev.get("tStartMs").and_then(|x| x.as_f64()).unwrap_or(0.0) / 1000.0;
        let dur = ev
            .get("dDurationMs")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0)
            / 1000.0;
        out.push(Segment { start, dur, text });
    }
    Ok(out)
}

fn pad(n: i64, w: usize) -> String {
    format!("{:0>w$}", n, w = w)
}

pub fn clock(sec: f64, sep: &str) -> String {
    let ms = (sec * 1000.0).round() as i64;
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    format!(
        "{}:{}:{}{}{}",
        pad(h, 2),
        pad(m, 2),
        pad(s, 2),
        sep,
        pad(ms % 1000, 3)
    )
}

pub fn short(sec: f64) -> String {
    let t = sec.floor() as i64;
    let h = t / 3600;
    let m = (t % 3600) / 60;
    let s = t % 60;
    if h != 0 {
        format!("{}:{}:{}", h, pad(m, 2), pad(s, 2))
    } else {
        format!("{}:{}", m, pad(s, 2))
    }
}

fn end(seg: &Segment, next: Option<&Segment>) -> f64 {
    let d = if seg.dur != 0.0 { seg.dur } else { 2.0 };
    let e = seg.start + d;
    match next {
        Some(n) => {
            let m = e.min(n.start);
            if m == 0.0 || m.is_nan() {
                e
            } else {
                m
            }
        }
        None => e,
    }
}

pub fn render(fmt: Format, segs: &[Segment]) -> String {
    match fmt {
        Format::Txt => {
            let joined = segs
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let norm = joined.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("{norm}\n")
        }
        Format::Timestamped => {
            let body = segs
                .iter()
                .map(|s| format!("[{}] {}", short(s.start), s.text))
                .collect::<Vec<_>>()
                .join("\n");
            format!("{body}\n")
        }
        Format::Srt => segs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    "{}\n{} --> {}\n{}\n",
                    i + 1,
                    clock(s.start, ","),
                    clock(end(s, segs.get(i + 1)), ","),
                    s.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Format::Vtt => {
            let body = segs
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    format!(
                        "{} --> {}\n{}\n",
                        clock(s.start, "."),
                        clock(end(s, segs.get(i + 1)), "."),
                        s.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("WEBVTT\n\n{body}")
        }
        Format::Json => serde_json::to_string_pretty(segs).unwrap_or_else(|_| "[]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"wireMagic":"pb3","events":[
        {"tStartMs":0,"dDurationMs":2000,"segs":[{"utf8":"Hello"},{"utf8":" world"}]},
        {"tStartMs":1500,"dDurationMs":1000,"segs":[{"utf8":"\n"}]},
        {"tStartMs":2000,"dDurationMs":3000,"segs":[{"utf8":"this is\na test"}]},
        {"tStartMs":4000,"dDurationMs":2500,"segs":[{"utf8":"overlap  here"}]},
        {"tStartMs":3661500,"segs":[{"utf8":"late"}]},
        {"tStartMs":9000}
    ]}"#;

    #[test]
    fn parses_and_skips_spacers() {
        let segs = parse_json3(FIXTURE).unwrap();
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].text, "Hello world");
        assert_eq!(segs[1].text, "this is a test");
        assert_eq!(segs[2].text, "overlap here");
        assert_eq!(segs[2].dur, 2.5);
        assert_eq!(segs[3].start, 3661.5);
        assert_eq!(segs[3].dur, 0.0);
    }

    #[test]
    fn clock_and_short_edges() {
        assert_eq!(clock(3661.5, ","), "01:01:01,500");
        assert_eq!(clock(0.0, "."), "00:00:00.000");
        assert_eq!(clock(59.9996, "."), "00:01:00.000");
        assert_eq!(short(3661.0), "1:01:01");
        assert_eq!(short(65.0), "1:05");
        assert_eq!(short(0.0), "0:00");
        assert_eq!(short(3599.9), "59:59");
    }

    #[test]
    fn txt_and_timestamped() {
        let segs = parse_json3(FIXTURE).unwrap();
        assert_eq!(
            render(Format::Txt, &segs),
            "Hello world this is a test overlap here late\n"
        );
        assert_eq!(
            render(Format::Timestamped, &segs),
            "[0:00] Hello world\n[0:02] this is a test\n[0:04] overlap here\n[1:01:01] late\n"
        );
    }

    #[test]
    fn srt_exact() {
        let segs = parse_json3(FIXTURE).unwrap();
        let expected = "1\n00:00:00,000 --> 00:00:02,000\nHello world\n\n\
2\n00:00:02,000 --> 00:00:04,000\nthis is a test\n\n\
3\n00:00:04,000 --> 00:00:06,500\noverlap here\n\n\
4\n01:01:01,500 --> 01:01:03,500\nlate\n";
        assert_eq!(render(Format::Srt, &segs), expected);
    }

    #[test]
    fn vtt_exact() {
        let segs = parse_json3(FIXTURE).unwrap();
        let expected = "WEBVTT\n\n\
00:00:00.000 --> 00:00:02.000\nHello world\n\n\
00:00:02.000 --> 00:00:04.000\nthis is a test\n\n\
00:00:04.000 --> 00:00:06.500\noverlap here\n\n\
01:01:01.500 --> 01:01:03.500\nlate\n";
        assert_eq!(render(Format::Vtt, &segs), expected);
    }

    #[test]
    fn json_matches_js_stringify() {
        let segs = vec![Segment {
            start: 0.0,
            dur: 2.5,
            text: "a".into(),
        }];
        assert_eq!(
            render(Format::Json, &segs),
            "[\n  {\n    \"start\": 0,\n    \"dur\": 2.5,\n    \"text\": \"a\"\n  }\n]"
        );
        assert_eq!(render(Format::Json, &[]), "[]");
    }
}

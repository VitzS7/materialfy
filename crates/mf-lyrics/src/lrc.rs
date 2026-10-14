//! Разбор и сборка строк (lyrics_sources.py: lines_from_lrc, build_from_timed_lines, interpolate_words,
//! word_from_parts, group_syllables, parse_yrc, parse_ttml, parse_elrc, add_gaps). Округление времени — round(x, 3).
use std::sync::LazyLock;

use regex::Regex;

use crate::text::{ends_with_cjk, is_meta_line, is_placeholder, py_isspace, py_splitlines, py_strip};
use crate::{GAP_MIN, Line, Word};

static LRC_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([0-9]+):([0-9]+(?:[.:][0-9]+)?)\]").unwrap());
static LRC_WORD_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[0-9]+:[0-9]+(?:[.:][0-9]+)?>").unwrap());
static ELRC_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([0-9]+):([0-9]+(?:[.:][0-9]+)?)>([^<\r\n]*)").unwrap());
static YRC_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[([0-9]+),([0-9]+)\](.*)$").unwrap());
static YRC_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(([0-9]+),([0-9]+),[0-9]+\)([^(]*)").unwrap());

static TTML_P: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<p\b([^>]*)>(.*?)</p>").unwrap());
static TTML_AUX_SPAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<span\b[^>]*\bttm:role\s*=\s*["']x-(?:translation|roman)["'][^>]*>.*?</span>"#).unwrap()
});
static TTML_BG_SPAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<span\b[^>]*\bttm:role\s*=\s*["']x-bg["'][^>]*>.*?</span>"#).unwrap()
});
static TTML_SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<span\b([^>]*)>([^<]*)</span>").unwrap());
static TTML_BEGIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\bbegin\s*=\s*["']([^"']+)["']"#).unwrap());
static TTML_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\bend\s*=\s*["']([^"']+)["']"#).unwrap());
static XML_TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

/// round(x, 3) Python: точное десятичное округление (половина — к чётному), а не (x·1000).round()/1000
pub fn round3(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.3}").parse().unwrap_or(x)
}

/// [mm:ss.xx] LRC → (время, текст), по времени; enhanced-теги <mm:ss.xx> вырезаются; несколько меток на строку — несколько пар.
pub fn lines_from_lrc(lrc: &str) -> Vec<(f64, String)> {
    let mut out = Vec::new();
    for raw in py_splitlines(lrc) {
        let stamps: Vec<(u64, f64)> = LRC_TAG
            .captures_iter(raw)
            .filter_map(|c| Some((c[1].parse().ok()?, c[2].replace(':', ".").parse().ok()?)))
            .collect();
        if stamps.is_empty() {
            continue;
        }
        let text = LRC_TAG.replace_all(raw, "");
        let text = LRC_WORD_TAG.replace_all(py_strip(&text), "");
        let text = py_strip(&text);
        for (m, s) in stamps {
            out.push(((m * 60) as f64 + s, text.to_owned()));
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// Enhanced LRC (`[mm:ss.xx]<mm:ss.xx>слово ...`) → строки по словам (`i = false`), если в LRC есть словесные теги.
pub fn parse_elrc(lrc: &str, duration: f64) -> Option<Vec<Line>> {
    if !ELRC_WORD.is_match(lrc) {
        return None;
    }
    let mut raw_lines: Vec<(f64, String)> = Vec::new();
    for raw in py_splitlines(lrc) {
        let stamps: Vec<(u64, f64)> = LRC_TAG
            .captures_iter(raw)
            .filter_map(|c| Some((c[1].parse().ok()?, c[2].replace(':', ".").parse().ok()?)))
            .collect();
        if stamps.is_empty() {
            continue;
        }
        let body = py_strip(&LRC_TAG.replace_all(raw, "")).to_owned();
        for (m, s) in stamps {
            raw_lines.push(((m * 60) as f64 + s, body.clone()));
        }
    }
    raw_lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    if raw_lines.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    let mut has_real_words = false;
    for (idx, (line_t, body)) in raw_lines.iter().enumerate() {
        if body.is_empty() {
            continue;
        }
        let next_t = raw_lines
            .get(idx + 1)
            .map(|(t, _)| *t)
            .unwrap_or_else(|| if duration > *line_t { duration } else { *line_t + 5.0 });
        let plain = LRC_WORD_TAG.replace_all(body, "");
        let plain = py_strip(&plain);
        if plain.is_empty() || plain == "\u{266a}" || is_meta_line(plain) {
            continue;
        }

        let caps: Vec<(f64, String)> = ELRC_WORD
            .captures_iter(body)
            .filter_map(|c| {
                let m: u64 = c[1].parse().ok()?;
                let s: f64 = c[2].replace(':', ".").parse().ok()?;
                Some(((m * 60) as f64 + s, c[3].to_owned()))
            })
            .collect();

        if !caps.is_empty() {
            let mut sylls: Vec<(String, f64, f64)> = Vec::new();
            for (i, (s, chunk)) in caps.iter().enumerate() {
                if chunk.is_empty() {
                    continue;
                }
                let e = match caps.get(i + 1) {
                    Some((ns, _)) => (*ns).max(*s + 0.01),
                    None => (next_t - 0.05).clamp(*s + 0.15, *s + 4.0),
                };
                if py_strip(chunk).is_empty()
                    && let Some(last) = sylls.last_mut()
                {
                    last.0.push_str(chunk);
                    last.2 = last.2.max(e);
                } else {
                    sylls.push((chunk.clone(), *s, e));
                }
            }
            let words = group_syllables(&sylls);
            if let (Some(first), Some(last)) = (words.first(), words.last()) {
                has_real_words = true;
                out.push(Line::Words {
                    t: round3(line_t.min(first.s)),
                    e: round3(last.e),
                    w: words,
                    i: false,
                });
            }
        } else {
            let n_chars = plain.chars().count() as f64;
            let sung = (n_chars * 0.085 + 0.4).clamp(1.2, 9.0);
            let end = (next_t - 0.05).min(*line_t + sung).max(*line_t + 0.5);
            out.push(Line::Words {
                t: round3(*line_t),
                e: round3(end),
                w: interpolate_words(plain, *line_t, end),
                i: true,
            });
        }
    }
    (has_real_words && !out.is_empty()).then_some(out)
}

/// Время TTML (`"00:01:23.456"`, `"01:23.45"`, `"12.345s"`, `"12345ms"`) → секунды.
pub fn parse_ttml_time(raw: &str) -> Option<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(ms) = s.strip_suffix("ms") {
        return ms.trim().parse::<f64>().ok().map(|v| v / 1000.0);
    }
    let s = s.strip_suffix('s').unwrap_or(s).trim();
    let parts: Vec<&str> = s.split(':').collect();
    match parts.as_slice() {
        [sec] => sec.parse::<f64>().ok(),
        [min, sec] => Some(min.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?),
        [hr, min, sec] => {
            Some(hr.parse::<f64>().ok()? * 3600.0 + min.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?)
        }
        _ => None,
    }
}

fn decode_xml_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    s.replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Разбор Apple Music / AMLL TTML (`.ttml`): возвращает `("word", lines)` при наличии слоговых `<span>`,
/// либо `("line", lines)` при построчной разметке `<p begin="..." end="...">`.
pub fn parse_ttml(ttml: &str, duration: f64) -> Option<(&'static str, Vec<Line>)> {
    let mut word_lines = Vec::new();
    let mut timed_pairs: Vec<(f64, String)> = Vec::new();

    for cap in TTML_P.captures_iter(ttml) {
        let p_attrs = &cap[1];
        let p_raw = &cap[2];
        let p_begin = TTML_BEGIN.captures(p_attrs).and_then(|c| parse_ttml_time(&c[1]));
        let p_end = TTML_END.captures(p_attrs).and_then(|c| parse_ttml_time(&c[1]));

        let without_aux = TTML_AUX_SPAN.replace_all(p_raw, "");
        let without_bg = TTML_BG_SPAN.replace_all(&without_aux, "");
        let body: &str = if TTML_SPAN.is_match(&without_bg) { &without_bg } else { &without_aux };

        let mut sylls: Vec<(String, f64, f64)> = Vec::new();
        let mut last_match_end: Option<usize> = None;
        for m in TTML_SPAN.captures_iter(body) {
            let whole = m.get(0).unwrap();
            let span_attrs = &m[1];
            let span_text = decode_xml_entities(&m[2]);
            let Some(s) = TTML_BEGIN.captures(span_attrs).and_then(|c| parse_ttml_time(&c[1])) else {
                continue;
            };
            let e = TTML_END
                .captures(span_attrs)
                .and_then(|c| parse_ttml_time(&c[1]))
                .unwrap_or(s + 0.15)
                .max(s);

            if let Some(prev_end) = last_match_end {
                let between = &body[prev_end..whole.start()];
                if between.chars().any(py_isspace)
                    && let Some(prev) = sylls.last_mut()
                    && !prev.0.chars().last().is_some_and(py_isspace)
                {
                    prev.0.push(' ');
                }
            }
            last_match_end = Some(whole.end());

            if span_text.is_empty() {
                continue;
            }
            if py_strip(&span_text).is_empty() {
                if let Some(prev) = sylls.last_mut()
                    && !prev.0.chars().last().is_some_and(py_isspace)
                {
                    prev.0.push(' ');
                }
            } else {
                sylls.push((span_text, s, e));
            }
        }

        if !sylls.is_empty() {
            let joined: String = sylls.iter().map(|s| s.0.as_str()).collect();
            let clean = py_strip(&joined);
            if clean.is_empty() || clean == "\u{266a}" || is_meta_line(clean) {
                continue;
            }
            let words = group_syllables(&sylls);
            let Some(last) = words.last() else { continue };
            let first_s = words.first().map(|w| w.s).unwrap_or(0.0);
            let t = round3(p_begin.unwrap_or(first_s).min(first_s));
            let e = round3(p_end.unwrap_or(last.e).max(last.e));
            word_lines.push(Line::Words { t, e, w: words, i: false });
        } else if let Some(t) = p_begin {
            let stripped = XML_TAGS.replace_all(&without_aux, " ");
            let decoded = decode_xml_entities(&stripped);
            let text = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
            if !text.is_empty() && text != "\u{266a}" && !is_meta_line(&text) {
                timed_pairs.push((t, text));
            }
        }
    }

    if !word_lines.is_empty() {
        let texts: Vec<String> = word_lines.iter().map(Line::text).collect();
        if !is_placeholder(texts.iter().map(String::as_str)) {
            return Some(("word", word_lines));
        }
    }
    if !timed_pairs.is_empty() && !is_placeholder(timed_pairs.iter().map(|(_, t)| t.as_str())) {
        let lines = build_from_timed_lines(&timed_pairs, duration);
        if !lines.is_empty() {
            return Some(("line", lines));
        }
    }
    None
}

/// Построчный текст → строки со словами, интерполированными по длине (i = true). Мета-строки (автор, композитор…) выкидываются.
pub fn build_from_timed_lines(pairs: &[(f64, String)], duration: f64) -> Vec<Line> {
    let pairs: Vec<&(f64, String)> = pairs.iter().filter(|(_, x)| !is_meta_line(x)).collect();
    let mut lines = Vec::new();
    for (i, &(t, text)) in pairs.iter().enumerate() {
        let t = *t;
        if text.is_empty() {
            continue;
        }
        let nxt = match pairs.get(i + 1) {
            Some(p) => p.0,
            None if duration > t => duration,
            None => t + 6.0,
        };
        let n_chars = text.chars().count() as f64;
        let sung = (n_chars * 0.085 + 0.4).clamp(1.2, 9.0);
        let mut end = (nxt - 0.05).min(t + sung);
        if end <= t {
            end = t + 0.5;
        }
        lines.push(Line::Words { t: round3(t), e: round3(end), w: interpolate_words(text, t, end), i: true });
    }
    lines
}

/// \S+\s* в смысле Python
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut in_space = false;
    for (i, c) in text.char_indices() {
        let space = py_isspace(c);
        match start {
            None if !space => {
                start = Some(i);
                in_space = false;
            }
            Some(s) if !space && in_space => {
                out.push(&text[s..i]);
                start = Some(i);
                in_space = false;
            }
            Some(_) if space => in_space = true,
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(&text[s..]);
    }
    out
}

/// Слова строки равномерно по весу len(слово)+1.5.
pub fn interpolate_words(text: &str, start: f64, end: f64) -> Vec<Word> {
    let tokens = tokens(text);
    if tokens.is_empty() {
        return Vec::new();
    }
    let weights: Vec<f64> = tokens.iter().map(|t| py_strip(t).chars().count() as f64 + 1.5).collect();
    let total: f64 = weights.iter().sum();
    let mut out = Vec::with_capacity(tokens.len());
    let mut t = start;
    for (tok, w) in tokens.into_iter().zip(weights) {
        let d = (end - start) * w / total;
        out.push(Word { x: tok.to_owned(), s: round3(t), e: round3(t + d), p: None });
        t += d;
    }
    out
}

/// Слоги одного слова (текст, начало, конец) → слово; у составного — "p" [[символов, s, e], …].
pub fn word_from_parts(parts: &[(String, f64, f64)]) -> Word {
    let x: String = parts.iter().map(|p| p.0.as_str()).collect();
    let p = (parts.len() > 1).then(|| {
        parts.iter().map(|(x, s, e)| (py_strip(x).chars().count().max(1) as u32, round3(*s), round3(*e))).collect()
    });
    Word { x, s: round3(parts[0].1), e: round3(parts[parts.len() - 1].2), p }
}

/// Слоги (текст, начало, конец) → слова: граница — пробел в конце слога или CJK-символ.
pub fn group_syllables(syllables: &[(String, f64, f64)]) -> Vec<Word> {
    let mut words = Vec::new();
    let mut cur: Vec<(String, f64, f64)> = Vec::new();
    for syl in syllables {
        let Some(last) = syl.0.chars().last() else { continue };
        cur.push(syl.clone());
        if py_isspace(last) || ends_with_cjk(&syl.0) {
            words.push(word_from_parts(&cur));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        words.push(word_from_parts(&cur));
    }
    words
}

/// NetEase YRC: строки "[start,dur](s,d,0)слог…"; JSON-строки с авторами пропускаются.
pub fn parse_yrc(yrc: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for raw in py_splitlines(yrc) {
        let Some(m) = YRC_LINE.captures(py_strip(raw)) else { continue };
        let (Ok(ls), Ok(ld)) = (m[1].parse::<u64>(), m[2].parse::<u64>()) else { continue };
        let (ls, ld) = (ls as f64 / 1000.0, ld as f64 / 1000.0);
        let syllables: Vec<(String, f64, f64)> = YRC_WORD
            .captures_iter(&m[3])
            .filter_map(|c| {
                let (s, d) = (c[1].parse::<u64>().ok()?, c[2].parse::<u64>().ok()?);
                Some((c[3].to_owned(), s as f64 / 1000.0, (s + d) as f64 / 1000.0))
            })
            .collect();
        let text: String = syllables.iter().map(|s| s.0.as_str()).collect();
        let text = py_strip(&text);
        if text.is_empty() || is_meta_line(text) {
            continue;
        }
        let words = group_syllables(&syllables);
        let Some(last) = words.last() else { continue };
        let e = round3((ls + ld).max(last.e));
        lines.push(Line::Words { t: round3(ls), e, w: words, i: false });
    }
    lines
}

/// Паузы ≥ GAP_MIN между строками → {"gap": true, "t": prev_end + (0.3 если prev_end), "e": t - 0.15}.
pub fn add_gaps(lines: Vec<Line>, duration: f64) -> Vec<Line> {
    // duration в Python тоже не используется — оставлен ради сигнатуры
    let _ = duration;
    let mut out = Vec::with_capacity(lines.len() + 4);
    let mut prev_end = 0.0_f64;
    for ln in lines {
        let (t, e) = (ln.start(), ln.end());
        if t - prev_end >= GAP_MIN {
            let start = prev_end + if prev_end != 0.0 { 0.3 } else { 0.0 };
            out.push(Line::Gap { gap: true, t: round3(start), e: round3(t - 0.15) });
        }
        out.push(ln);
        prev_end = prev_end.max(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &Line) -> &[Word] {
        match line {
            Line::Words { w, .. } => w,
            Line::Gap { .. } => &[],
        }
    }

    fn line(t: f64, e: f64) -> Line {
        Line::Words { t, e, w: Vec::new(), i: false }
    }

    // ── порт LyricsSourcesTest.kt ──

    #[test]
    fn lrc_is_parsed_and_sorted() {
        let pairs = lines_from_lrc("[00:12.50]вторая строка\n[00:05.00][01:10.00]первая и повтор\n[ar:кто-то]\nбез метки");
        assert_eq!(pairs.iter().map(|p| p.0).collect::<Vec<_>>(), vec![5.0, 12.5, 70.0]);
        assert_eq!(pairs[0].1, "первая и повтор");
    }

    #[test]
    fn words_are_interpolated_inside_the_line() {
        let pairs = [(10.0, "раз два три".to_owned()), (14.0, String::new()), (20.0, "конец".to_owned())];
        let lines = build_from_timed_lines(&pairs, 30.0);
        assert_eq!(lines.len(), 2); // пустая строка — не текст
        let w = words(&lines[0]);
        assert_eq!(w.iter().map(|w| w.x.as_str()).collect::<Vec<_>>(), vec!["раз ", "два ", "три"]);
        assert!((w[0].s - 10.0).abs() < 0.001);
        assert!(lines[0].end() <= 13.95 + 0.001);
        assert!(w.windows(2).all(|p| p[0].e <= p[1].s + 0.001));
    }

    #[test]
    fn credits_are_not_lyrics() {
        let pairs = [(0.0, "作词 : 某人".to_owned()), (1.0, "Lyrics: someone".to_owned()), (5.0, "настоящая строка".to_owned())];
        assert_eq!(build_from_timed_lines(&pairs, 10.0).len(), 1);
    }

    #[test]
    fn gaps_appear_where_nobody_sings() {
        let lines = add_gaps(vec![line(8.0, 10.0), line(11.0, 12.0), line(20.0, 22.0)], 0.0);
        let gaps: Vec<bool> = lines.iter().map(|l| matches!(l, Line::Gap { .. })).collect();
        assert_eq!(gaps, vec![true, false, false, true, false]);
        assert_eq!(lines[0], Line::Gap { gap: true, t: 0.0, e: 7.85 });
        assert_eq!(lines[3], Line::Gap { gap: true, t: 12.3, e: 19.85 });
    }

    // ── прочее ──

    #[test]
    fn round3_is_python_round() {
        assert_eq!(round3(0.0625), 0.062);
        assert_eq!(round3(2.675), 2.675);
        assert_eq!(round3(1.0005), 1.0);
        assert_eq!(round3(12.3456), 12.346);
    }

    #[test]
    fn enhanced_tags_are_stripped() {
        let pairs = lines_from_lrc("[00:01.00]<00:01.00>раз <00:01.50>два\r\n[00:02:50] три ");
        assert_eq!(pairs, vec![(1.0, "раз два".to_owned()), (2.5, "три".to_owned())]);
    }

    #[test]
    fn elrc_keeps_word_timestamps() {
        let raw = "[00:01.00]<00:01.00>Hel<00:01.50>lo <00:02.00>world\n[00:04.00]<00:04.00>end";
        let lines = parse_elrc(raw, 10.0).unwrap();
        assert_eq!(lines.len(), 2);
        let w = words(&lines[0]);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].x, "Hello ");
        assert_eq!((w[0].s, w[0].e), (1.0, 2.0));
        assert_eq!(w[1].x, "world");
    }

    #[test]
    fn ttml_is_parsed_to_words() {
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
          <body><div>
            <p begin="00:01.000" end="00:03.000">
              <span begin="00:01.000" end="00:01.500">Hel</span><span begin="00:01.500" end="00:02.000">lo</span>
              <span begin="00:02.000" end="00:03.000">world</span>
              <span ttm:role="x-translation">Привет мир</span>
            </p>
          </div></body>
        </tt>"#;
        let (kind, lines) = parse_ttml(xml, 10.0).unwrap();
        assert_eq!(kind, "word");
        assert_eq!(lines.len(), 1);
        let w = words(&lines[0]);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].x, "Hello ");
        assert_eq!(w[1].x, "world");
    }

    #[test]
    fn tokens_keep_trailing_space() {
        assert_eq!(tokens("  a  b\tc "), vec!["a  ", "b\t", "c "]);
        assert!(tokens("   ").is_empty());
    }

    #[test]
    fn yrc_is_parsed_to_words() {
        let yrc = "{\"t\":0,\"c\":[{\"tx\":\"作词: \"}]}\n[1000,2000](1000,500,0)Hel(1500,500,0)lo (2000,1000,0)world\n[4000,100](4000,100,0)你(4100,100,0)好";
        let lines = parse_yrc(yrc);
        assert_eq!(lines.len(), 2);
        let w = words(&lines[0]);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0], Word { x: "Hello ".into(), s: 1.0, e: 2.0, p: Some(vec![(3, 1.0, 1.5), (2, 1.5, 2.0)]) });
        assert_eq!(w[1], Word { x: "world".into(), s: 2.0, e: 3.0, p: None });
        assert_eq!(lines[0].end(), 3.0);
        // каждый иероглиф — отдельное слово; конец строки — по последнему слову
        assert_eq!(words(&lines[1]).len(), 2);
        assert_eq!(lines[1].end(), 4.2);
    }
}
